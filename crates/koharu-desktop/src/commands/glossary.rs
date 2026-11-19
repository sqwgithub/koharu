use std::{collections::BTreeSet, sync::Arc};

use anyhow::Context as _;
use koharu_pipeline::{PipelineConfig, Request, RunId, Stage, Target};
use koharu_scene::{
    AliasSuggestion, Edit, GlossaryEntry, GlossaryKind, Op, ProjectId, TermIssue, TermLocation,
    TextSuggestion,
};
use serde::Serialize;
use specta::Type;
use tauri::{State, Window};
use tauri_runtime_cef::CefRuntime;

use super::Result;
use crate::app::App;

#[derive(Serialize, Type)]
pub struct GlossaryView {
    project: ProjectId,
    entries: Vec<GlossaryEntry>,
    issues: Vec<TermIssue>,
    lines: usize,
    suggestions: Vec<AliasSuggestion>,
    corrections: Vec<TextSuggestion>,
    candidate_reviews: Vec<koharu_scene::CandidateReview>,
    uncertainties: Vec<koharu_scene::TranslationUncertainty>,
    inks: Vec<koharu_scene::InkSuggestion>,
    candidates: Option<Vec<TermCandidate>>,
}

#[tauri::command]
#[specta::specta]
pub fn get_glossary(app: State<'_, Arc<App>>) -> Result<GlossaryView> {
    let project = app.session()?.lock().project();
    let lines = project.transcript()?;
    let candidates = (!project.glossary_candidates().is_empty()).then(|| {
        let mut candidates: Vec<_> = project
            .glossary_candidates()
            .iter()
            .filter_map(|entries| {
                let first = entries.first()?;
                let active = project
                    .glossary()
                    .iter()
                    .find(|entry| entry.source == first.source);
                let matcher = entries
                    .iter()
                    .chain(active)
                    .find(|entry| entry.kind != GlossaryKind::PreserveBlock)
                    .unwrap_or(first);
                let locations: Vec<_> = lines
                    .iter()
                    .filter(|line| matcher.matches(&line.text))
                    .map(|line| TermLocation {
                        page: line.page,
                        number: line.number,
                        layer: line.layer,
                        text: line.text.clone(),
                    })
                    .collect();
                Some(TermCandidate {
                    manual: project
                        .manual_candidates(&first.source)
                        .iter()
                        .filter(|text| !entries.iter().any(|e| &e.translation == *text))
                        .cloned()
                        .collect(),
                    entries: entries.clone(),
                    locations,
                })
            })
            .collect();
        candidates.sort_by_key(|entry| std::cmp::Reverse(entry.locations.len()));
        candidates
    });
    let global = koharu_config::get::<PipelineConfig>()?.glossary;
    Ok(GlossaryView {
        project: project.id(),
        entries: project.glossary().to_vec(),
        issues: project.check_glossary(&global)?,
        lines: lines.len(),
        suggestions: project.alias_suggestions().to_vec(),
        corrections: project.text_suggestions().to_vec(),
        candidate_reviews: project.candidate_reviews(&global)?,
        uncertainties: project.translation_uncertainties()?,
        inks: project.ink_suggestions(&global)?,
        candidates,
    })
}

#[tauri::command]
#[specta::specta]
pub fn extract_source(app: State<'_, Arc<App>>, project: ProjectId) -> Result<Vec<RunId>> {
    let session = app.session()?;
    let snapshot = session.lock().project();
    if snapshot.id() != project {
        return Err(anyhow::anyhow!("the open project changed").into());
    }
    let mut pages = Vec::new();
    let mut blocks = Vec::new();
    for page in snapshot
        .pages()
        .filter(|page| !page.excluded && page.background.is_some())
    {
        let found = snapshot.blocks(page.id)?;
        if found.is_empty() {
            pages.push(page.id);
        } else {
            blocks.extend(found.into_iter().filter_map(|(id, block)| {
                block.source.content.text().trim().is_empty().then_some(id)
            }));
        }
    }
    let mut queued = Vec::new();
    for (target, stages) in [
        (!pages.is_empty()).then_some((
            Target::Pages(pages),
            BTreeSet::from([Stage::Detect, Stage::Recognize]),
        )),
        (!blocks.is_empty())
            .then_some((Target::Blocks(blocks), BTreeSet::from([Stage::Recognize]))),
    ]
    .into_iter()
    .flatten()
    {
        queued.push(app.pipeline.run(
            Arc::clone(&session),
            Request {
                target,
                stages,
                resume: false,
                config: None,
            },
        )?);
    }
    Ok(queued)
}

#[tauri::command]
#[specta::specta]
pub fn save_glossary(
    app: State<'_, Arc<App>>,
    project: ProjectId,
    previous: Vec<GlossaryEntry>,
    entries: Vec<GlossaryEntry>,
) -> Result<()> {
    let opened = app.project.lock();
    let opened = opened.as_ref().context("no project is open")?;
    let mut session = opened.session.lock();
    if session.id() != project {
        return Err(anyhow::anyhow!("the open project changed").into());
    }
    session.apply(Edit {
        label: "edit_glossary".to_owned(),
        ops: vec![Op::SetGlossary { previous, entries }],
    })?;
    Ok(())
}

#[derive(Serialize, Type)]
pub struct CandidateApplication {
    updated: usize,
    review: usize,
}

#[tauri::command]
#[specta::specta]
pub async fn apply_candidate(
    app: State<'_, Arc<App>>,
    project: ProjectId,
    entry: GlossaryEntry,
    locations: Vec<koharu_scene::LayerId>,
) -> Result<CandidateApplication> {
    let (snapshot, closing) = {
        let opened = app.project.lock();
        let opened = opened.as_ref().context("no project is open")?;
        let session = opened.session.lock();
        if session.id() != project {
            return Err(anyhow::anyhow!("the open project changed").into());
        }
        (session.project(), opened.closing.clone())
    };
    let config = koharu_config::get::<PipelineConfig>()?;
    let (config, entry, plan) = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let plan = snapshot.candidate_plan(&entry, &locations, &config.glossary)?;
        Ok((config, entry, plan))
    })
    .await??;
    let decisions = tokio::select! {
        biased;
        _ = closing.cancelled() => return Err(anyhow::anyhow!("the open project changed").into()),
        result = app.pipeline.locate_term(&config, &entry, &plan.lines) => result?,
    };
    let app = Arc::clone(&app);
    Ok(tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let opened = app.project.lock();
        let opened = opened.as_ref().context("no project is open")?;
        anyhow::ensure!(!closing.is_cancelled(), "the open project changed");
        let current = koharu_config::get::<PipelineConfig>()?;
        anyhow::ensure!(
            current.translator.target == config.translator.target,
            "the translation language changed"
        );
        let mut session = opened.session.lock();
        let (ops, updated, review) =
            session
                .project()
                .settle_candidate(&plan, &decisions, &current.glossary)?;
        if !ops.is_empty() {
            session.apply(Edit {
                label: "apply_candidate".into(),
                ops,
            })?;
        }
        Ok(CandidateApplication { updated, review })
    })
    .await??)
}

#[tauri::command]
#[specta::specta]
pub fn resolve_candidate_review(
    app: State<'_, Arc<App>>,
    project: ProjectId,
    suggestion: koharu_scene::CandidateReview,
    translation: Option<String>,
) -> Result<()> {
    let opened = app.project.lock();
    let opened = opened.as_ref().context("no project is open")?;
    let mut session = opened.session.lock();
    if session.id() != project {
        return Err(anyhow::anyhow!("the open project changed").into());
    }
    let global = koharu_config::get::<PipelineConfig>()?.glossary;
    let ops =
        session
            .project()
            .candidate_review_ops(&suggestion, translation.as_deref(), &global)?;
    if !ops.is_empty() {
        session.apply(Edit {
            label: "apply_candidate".into(),
            ops,
        })?;
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn edit_manual_candidate(
    app: State<'_, Arc<App>>,
    project: ProjectId,
    source: String,
    translation: String,
    remove: bool,
) -> Result<()> {
    resolve_review(
        &app,
        project,
        Op::EditManualCandidate {
            source,
            translation,
            remove,
        },
    )
}

#[tauri::command]
#[specta::specta]
pub fn resolve_alias(
    app: State<'_, Arc<App>>,
    project: ProjectId,
    suggestion: AliasSuggestion,
    translation: Option<String>,
) -> Result<()> {
    resolve_review(
        &app,
        project,
        Op::ResolveAlias {
            suggestion,
            translation,
        },
    )
}

#[tauri::command]
#[specta::specta]
pub fn resolve_uncertainty(
    app: State<'_, Arc<App>>,
    project: ProjectId,
    suggestion: koharu_scene::TranslationUncertainty,
    translation: String,
) -> Result<()> {
    let opened = app.project.lock();
    let mut session = opened
        .as_ref()
        .context("no project is open")?
        .session
        .lock();
    if session.id() != project {
        return Err(anyhow::anyhow!("open project changed").into());
    }
    if translation.trim().is_empty() {
        return Err(anyhow::anyhow!("empty translation").into());
    }
    if !session
        .project()
        .translation_uncertainties()?
        .contains(&suggestion)
    {
        return Err(anyhow::anyhow!("translation review changed").into());
    }
    let layer = suggestion.location.layer;
    let mut ops = Vec::new();
    if translation != suggestion.translation {
        ops.push(Op::SetText {
            layer: layer.into(),
            text: translation,
        });
    }
    ops.push(Op::SetAttribute {
        layer: layer.into(),
        namespace: "koharu".into(),
        key: "translation_review".into(),
        value: None,
    });
    session.apply(Edit {
        label: "set_text".into(),
        ops,
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn resolve_translation(
    app: State<'_, Arc<App>>,
    project: ProjectId,
    suggestion: TextSuggestion,
    translation: Option<String>,
) -> Result<()> {
    resolve_review(
        &app,
        project,
        Op::ResolveTranslation {
            suggestion,
            translation,
        },
    )
}

#[tauri::command]
#[specta::specta]
pub fn resolve_ink(
    app: State<'_, Arc<App>>,
    project: ProjectId,
    suggestion: koharu_scene::InkSuggestion,
    choice: koharu_scene::InkChoice,
) -> Result<()> {
    resolve_review(&app, project, Op::ResolveInk { suggestion, choice })
}

#[tauri::command]
#[specta::specta]
pub async fn normalize_ink(
    app: State<'_, Arc<App>>,
    project: ProjectId,
    settings: Option<koharu_scene::InkNormalization>,
) -> Result<usize> {
    let app = Arc::clone(&app);
    Ok(
        tokio::task::spawn_blocking(move || -> anyhow::Result<usize> {
            let opened = app.project.lock();
            let mut session = opened
                .as_ref()
                .context("no project is open")?
                .session
                .lock();
            anyhow::ensure!(session.id() == project, "the open project changed");
            let (count, ops) = session.project().normalize_ink(settings.as_ref())?;
            if !ops.is_empty() {
                session.apply(Edit {
                    label: "review_ink".into(),
                    ops,
                })?;
            }
            Ok(count)
        })
        .await??,
    )
}

fn resolve_review(app: &App, project: ProjectId, op: Op) -> Result<()> {
    let opened = app.project.lock();
    let mut session = opened
        .as_ref()
        .context("no project is open")?
        .session
        .lock();
    if session.id() != project {
        return Err(anyhow::anyhow!("the open project changed").into());
    }
    session.apply(Edit {
        label: match op {
            Op::ResolveInk { .. } => "review_ink",
            Op::ResolveTranslation { .. } => "set_text",
            _ => "edit_glossary",
        }
        .to_owned(),
        ops: vec![op],
    })?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn export_source(app: State<'_, Arc<App>>, window: Window<CefRuntime>) -> Result<()> {
    let session = app.session()?;
    let (project, suggested) = {
        let session = session.lock();
        let name = session
            .path()
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        (session.project(), format!("{name}-source.json"))
    };
    let Some(path) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .add_filter("JSON", &["json"])
        .set_file_name(suggested)
        .save_file()
        .await
    else {
        return Ok(());
    };
    let file = path.path().to_owned();
    Ok(tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let lines: Vec<_> = project
            .transcript()?
            .into_iter()
            .map(|line| {
                serde_json::json!({
                    "page": line.page, "number": line.number, "layer": line.layer, "text": line.text
                })
            })
            .collect();
        fs_err::write(file, serde_json::to_vec_pretty(&lines)?)?;
        Ok(())
    })
    .await??)
}

#[derive(Serialize, Type)]
pub struct TermCandidate {
    manual: Vec<String>,
    entries: Vec<GlossaryEntry>,
    locations: Vec<TermLocation>,
}

#[tauri::command]
#[specta::specta]
pub fn analyze_glossary(app: State<'_, Arc<App>>, project: ProjectId) -> Result<RunId> {
    let session = app.session()?;
    if session.lock().id() != project {
        return Err(anyhow::anyhow!("the open project changed").into());
    }
    Ok(app.pipeline.run(
        session,
        Request {
            target: Target::Project,
            stages: BTreeSet::from([Stage::Glossary]),
            resume: false,
            config: None,
        },
    )?)
}

#[tauri::command]
#[specta::specta]
pub fn get_sfx_glossary() -> Result<koharu_pipeline::SfxGlossary> {
    Ok(koharu_config::get()?)
}

/// Apply one explicit edit without overwriting other translations learned in the meantime.
#[tauri::command]
#[specta::specta]
pub fn edit_sfx_term(
    languages: String,
    source: String,
    previous: String,
    translation: Option<String>,
) -> Result<()> {
    if translation.as_ref().is_some_and(|v| v.trim().is_empty()) {
        return Err(anyhow::anyhow!("empty sound-effect translation").into());
    }
    let mut applied = false;
    koharu_config::update(|table: &mut koharu_pipeline::SfxGlossary| {
        let Some(entries) = table.languages.get_mut(&languages) else {
            return;
        };
        if entries.get(&source) != Some(&previous) {
            return;
        }
        if let Some(value) = translation {
            entries.insert(source, value.trim().into());
        } else {
            entries.remove(&source);
        }
        if entries.is_empty() {
            table.languages.remove(&languages);
        }
        applied = true;
    })?;
    if !applied {
        return Err(anyhow::anyhow!("sound-effect entry changed; refresh the glossary").into());
    }
    Ok(())
}
