//! Exact sound-effect translations shared across projects, scoped to a language pair.
use anyhow::{Result, ensure};
use icu_locale_core::LanguageIdentifier;
use koharu_scene::{Content, Edit, LayerId, PageId, Project, Role};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::{
    Conversation, Member, PipelineConfig, Stage, Stopped, Task, Turn, name, translation_model,
    written,
};

#[derive(Default, Serialize, Deserialize, specta::Type)]
#[serde(default)]
pub struct SfxGlossary {
    pub languages: BTreeMap<String, BTreeMap<String, String>>,
}

impl koharu_config::Section for SfxGlossary {
    const NAME: &'static str = "sfx_glossary";
}

#[derive(Clone)]
struct Occurrence {
    page: PageId,
    id: LayerId,
    saved: Option<String>,
}

type Groups = HashMap<LanguageIdentifier, BTreeMap<String, Vec<Occurrence>>>;

fn pair(source: &LanguageIdentifier, target: &LanguageIdentifier) -> String {
    format!("{source} → {target}")
}

fn collect(
    members: &[Member],
    config: &PipelineConfig,
    project: &Project,
    cache: &SfxGlossary,
) -> Result<Groups> {
    let glossary = project.effective_glossary(&config.glossary);
    let mut groups = Groups::new();
    for member in members {
        if project.page(member.page)?.excluded {
            continue;
        }
        for (id, block) in member.blocks(project) {
            if block.role != Role::Sfx {
                continue;
            }
            let source = block.source.content.text();
            // Explicit term, preserve and per-occurrence rules stay on the normal path.
            if source.trim().is_empty() || glossary.iter().any(|e| e.matches(&source)) {
                continue;
            }
            groups
                .entry(block.source.language.clone())
                .or_default()
                .entry(source)
                .or_default()
                .push(Occurrence {
                    page: member.page,
                    id,
                    saved: block.translation.as_ref().map(|t| t.content.text()),
                });
        }
    }
    // Full-page selections already contain every eligible occurrence on those pages.
    let full: BTreeSet<_> = members
        .iter()
        .filter(|m| m.blocks.is_none())
        .map(|m| m.page)
        .collect();
    let mut singles = HashMap::new();
    for (language, words) in &groups {
        let known = cache
            .languages
            .get(&pair(language, &config.translator.target));
        for (source, rows) in words {
            if rows.len() == 1 && !known.is_some_and(|entries| entries.contains_key(source)) {
                singles.insert((language.clone(), source.clone()), rows[0].id);
            }
        }
    }
    for page in project
        .pages()
        .filter(|p| !p.excluded && !full.contains(&p.id))
    {
        if singles.is_empty() {
            break;
        }
        for (id, block) in project.blocks(page.id)? {
            if block.role == Role::Sfx {
                let word = (block.source.language.clone(), block.source.content.text());
                if singles.get(&word).is_some_and(|first| *first != id) {
                    singles.remove(&word);
                }
            }
        }
    }
    for ((language, source), _) in singles {
        if let Some(words) = groups.get_mut(&language) {
            words.remove(&source);
        }
    }
    groups.retain(|_, words| !words.is_empty());
    Ok(groups)
}

pub(super) async fn translate(
    task: &mut Task,
    config: &PipelineConfig,
    conversation: &mut Conversation,
) -> Result<()> {
    let member = task.members[0].clone();
    let project = member.session.lock().project();
    let cache: SfxGlossary = koharu_config::get()?;
    let groups = collect(&task.members, config, &project, &cache)?;
    let mut handled = BTreeSet::new();
    if groups.is_empty() {
        return Ok(());
    }
    let mut model = None;
    for (language, words) in groups {
        let key = pair(&language, &config.translator.target);
        let known = cache.languages.get(&key);
        let (hits, misses): (Vec<_>, Vec<_>) = words
            .into_iter()
            .partition(|(text, _)| known.is_some_and(|m| m.contains_key(text)));
        // Cache hits in this language pair need no model.
        for (cached, list) in [(true, hits), (false, misses)] {
            let mut rest = list.as_slice();
            while !rest.is_empty() {
                if member.cancel.is_cancelled() {
                    return Err(Stopped.into());
                }
                let mut n = 0;
                let mut chars = 0;
                for (word, _) in rest.iter().take(32) {
                    let length = word.chars().count();
                    if n > 0 && chars + length > 1200 {
                        break;
                    }
                    n += 1;
                    chars += length;
                }
                let (batch, tail) = rest.split_at(n);
                rest = tail;
                let texts: Vec<_> = batch.iter().map(|(text, _)| text.clone()).collect();
                let answers = if cached {
                    texts
                        .iter()
                        .map(|text| known.expect("cache hits")[text].clone())
                        .collect()
                } else {
                    if model.is_none() {
                        let glossary = project
                            .effective_glossary(&config.glossary)
                            .into_iter()
                            .cloned()
                            .collect::<Vec<_>>();
                        conversation.keep(&config.translator, &glossary);
                        model = Some(translation_model(task, config, conversation).await?);
                    }
                    let prompt = format!(
                        "Translate these independent manga sound effects from {language} into {}. Return a JSON object with exactly the supplied keys and a concise sound effect for each key. Preserve expressive spelling and punctuation.\n{}",
                        name(&config.translator.target)?,
                        config.translator.instructions
                    );
                    let turn = Turn::new(
                        prompt,
                        &texts,
                        batch.iter().map(|(_, rows)| rows[0].id),
                        &[],
                        None,
                        config.translator.reasoning,
                        &member.cancel,
                    );
                    turn.take(None, model.as_ref().expect("loaded for a cache miss"))
                        .await?
                        .ok_or(Stopped)?
                };
                let member = member.clone();
                let glossary = project.glossary().to_vec();
                let batch = batch.to_vec();
                let language = language.clone();
                let config = config.clone();
                let ids = tokio::task::spawn_blocking(move || {
                    commit(
                        &member, &config, &glossary, &language, cached, &batch, answers,
                    )
                })
                .await??;
                handled.extend(ids);
            }
        }
    }
    for m in &mut task.members {
        let remaining: Vec<_> = m
            .blocks(&project)
            .into_iter()
            .map(|(id, _)| id)
            .filter(|id| !handled.contains(id))
            .collect();
        m.blocks = Some(remaining.into());
    }
    task.members.retain(|m| {
        if m.blocks.as_ref().is_some_and(|b| b.is_empty()) {
            task.events.send(crate::Event::Finished {
                run: m.run,
                page: m.page,
                stage: Stage::Translate,
            });
            false
        } else {
            true
        }
    });
    Ok(())
}

fn commit(
    member: &Member,
    config: &PipelineConfig,
    glossary: &[koharu_scene::GlossaryEntry],
    language: &LanguageIdentifier,
    cached: bool,
    batch: &[(String, Vec<Occurrence>)],
    answers: Vec<String>,
) -> Result<Vec<LayerId>> {
    let mut session = member.session.lock();
    if member.cancel.is_cancelled() {
        return Err(Stopped.into());
    }
    let live = session.project();
    ensure!(
        glossary == live.glossary(),
        "the project glossary changed during translation"
    );
    ensure!(
        batch.len() == answers.len(),
        "incomplete sound-effect batch"
    );
    let mut accepted = BTreeMap::new();
    let mut rows = Vec::new();
    for ((source, occurrences), answer) in batch.iter().zip(answers) {
        for row in occurrences {
            if !live.page(row.page).is_ok_and(|page| !page.excluded) {
                continue;
            }
            let (owner, _) = live.parent(row.id)?;
            let Content::Block(block) = &live.layer(row.id)?.content else {
                anyhow::bail!("sound effect was removed")
            };
            ensure!(
                owner.id == row.page
                    && block.role == Role::Sfx
                    && block.source.language == *language
                    && block.source.content.text() == *source
                    && block.translation.as_ref().map(|t| t.content.text()) == row.saved,
                "sound effect changed during translation"
            );
            accepted.insert(source.clone(), answer.clone());
            rows.push((row.id, source));
        }
    }
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let key = pair(language, &config.translator.target);
    if cached {
        let current: SfxGlossary = koharu_config::get()?;
        for (source, answer) in &mut accepted {
            *answer = current
                .languages
                .get(&key)
                .and_then(|entries| entries.get(source))
                .ok_or_else(|| anyhow::anyhow!("sound-effect glossary entry was removed"))?
                .clone();
        }
    } else {
        // An edit in another project or the global glossary takes precedence.
        koharu_config::update(|global: &mut SfxGlossary| {
            let entries = global.languages.entry(key).or_default();
            for (source, answer) in &mut accepted {
                *answer = entries
                    .entry(source.clone())
                    .or_insert_with(|| answer.clone())
                    .clone();
            }
        })?;
    }
    ensure!(
        accepted.values().all(|v| !v.trim().is_empty()),
        "empty sound-effect translation"
    );
    let translations = rows
        .iter()
        .map(|(id, text)| (*id, language.clone(), accepted[*text].clone()))
        .collect();
    let ops = written(&live, translations, &config.translator, &config.glossary)?;
    if member.cancel.is_cancelled() {
        return Err(Stopped.into());
    }
    session.apply(Edit {
        label: Stage::Translate.to_string(),
        ops,
    })?;
    Ok(rows.into_iter().map(|(id, _)| id).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use koharu_scene::Session;
    use serde_json::json;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    fn members(session: Session) -> Vec<Member> {
        let project = session.project();
        let session = Arc::new(parking_lot::Mutex::new(session));
        project
            .pages()
            .enumerate()
            .map(|(index, page)| Member {
                run: crate::RunId(0),
                job: petgraph::graph::NodeIndex::new(index),
                page: page.id,
                index,
                blocks: None,
                session: session.clone(),
                cancel: CancellationToken::new(),
                resume: false,
            })
            .collect()
    }

    #[test]
    fn groups_exact_sound_effects_across_included_pages_and_language_pairs() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut session = Session::create(dir.path().join("book.khr"))?;
        let mut ops = vec![];
        for (i, items) in [
            vec![
                ("sfx", "ドン", "ja"),
                ("text", "ドン", "ja"),
                ("sfx", "ドン！", "ja"),
                ("sfx", "啪", "zh"),
                ("sfx", "bang", "en"),
                ("sfx", "ガン", "ja"),
            ],
            vec![("sfx", "ドン", "ja"), ("sfx", "ガン", "ja")],
            vec![("sfx", "ドン！", "ja")],
        ]
        .into_iter()
        .enumerate()
        {
            let page = format!("$p{i}");
            ops.push(json!({"op":"add_page","id":page,"index":i,"size":{"width":64,"height":64}}));
            for (index, (role, source, language)) in items.into_iter().enumerate() {
                ops.push(json!({"op":"add_layer","page":page,"index":index,"layer":{"kind":"block","visible":false,"role":role,"source":{"language":language,"content":[{"insert":source}]}}}));
            }
        }
        ops.push(json!({"op":"set_page_options","page":"$p2","excluded":true}));
        session.apply(Edit {
            label: "fixture".into(),
            ops: ops
                .into_iter()
                .map(serde_json::from_value)
                .collect::<std::result::Result<_, _>>()?,
        })?;
        let project = session.project();
        let members = members(session);
        let mut config = PipelineConfig::default();
        config.glossary = serde_json::from_value(
            json!([{"kind":"preserve","source":"ガン","translation":"","note":""}]),
        )?;
        let mut cache = SfxGlossary::default();
        cache.languages.insert(
            "zh → en".into(),
            BTreeMap::from([("啪".into(), "pop".into())]),
        );
        cache.languages.insert(
            "en → fr".into(),
            BTreeMap::from([("bang".into(), "boum".into())]),
        );
        let groups = collect(&members[..1], &config, &project, &cache)?;
        assert_eq!(groups.values().map(BTreeMap::len).sum::<usize>(), 2);
        let japanese = &groups[&"ja".parse()?];
        assert_eq!(japanese["ドン"].len(), 1);
        assert!(groups[&"zh".parse()?].contains_key("啪"));
        let all = collect(&members, &config, &project, &cache)?;
        let language = "ja".parse()?;
        let occurrences = &all[&language]["ドン"];
        assert_eq!(occurrences.len(), 2);
        let batch = vec![("ドン".into(), occurrences.clone())];
        let first = &members[0];
        first.session.lock().apply(Edit {
            label: "remove layer".into(),
            ops: vec![serde_json::from_value(
                json!({"op":"remove_layer","layer":occurrences[1].id}),
            )?],
        })?;
        let apply = || {
            commit(
                first,
                &config,
                &[],
                &language,
                false,
                &batch,
                vec!["boom".into()],
            )
        };
        assert!(apply().is_err());
        first.cancel.cancel();
        assert!(apply().unwrap_err().is::<Stopped>());
        let live = first.session.lock().project();
        assert!(
            live.blocks(first.page)?
                .iter()
                .all(|(_, b)| b.translation.is_none())
        );
        Ok(())
    }
}
