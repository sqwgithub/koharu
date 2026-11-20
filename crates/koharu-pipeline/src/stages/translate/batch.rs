//! Multi-page text translation. Model-facing page/block labels belong to the
//! input snapshot. Only that snapshot's database IDs can address an edit.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use anyhow::{Context as _, Result, ensure};
use icu_locale_core::LanguageIdentifier;
use koharu_llm::rig::{
    completion::{Message, Reasoning, Usage},
    driver::DynModel,
    operation::Completion,
};
use koharu_scene::{Content, Edit, GlossaryEntry, LayerId, PageId, Project};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{Conversation, Member, Stopped, Task, name, rules::Rules, written};
use crate::{PipelineConfig, Stage};

#[derive(Clone)]
struct Line {
    page: PageId,
    layer: LayerId,
    id: String,
    source: String,
    saved: Option<String>,
    language: LanguageIdentifier,
    keep: bool,
}

fn lines(project: &Project, members: &[Member], global: &[GlossaryEntry]) -> Result<Vec<Line>> {
    let selected: BTreeMap<_, _> = members
        .iter()
        .map(|m| (m.page, m.blocks.as_deref()))
        .collect();
    let mut result = Vec::new();
    for (p, page) in project.pages().enumerate() {
        let Some(only) = selected.get(&page.id) else {
            continue;
        };
        if page.excluded {
            continue;
        }
        let story: BTreeMap<_, _> = project
            .story(page.id)?
            .into_iter()
            .enumerate()
            .map(|(index, layer)| (layer, index + 1))
            .collect();
        for (b, (layer, block)) in project.blocks(page.id)?.into_iter().enumerate() {
            if only.is_some_and(|ids| !ids.contains(&layer)) {
                continue;
            }
            let source = block.source.content.text();
            if source.trim().is_empty() {
                continue;
            }
            result.push(Line {
                page: page.id,
                layer,
                // Story also numbers standalone text layers. Hidden blocks
                // remain translatable, but cannot borrow a visible badge.
                id: match story.get(&layer) {
                    Some(number) => format!("P{:03}-B{number:03}", p + 1),
                    None => format!("P{:03}-H{:03}", p + 1, b + 1),
                },
                keep: project.keeps_original(global, layer, &source),
                saved: block.translation.as_ref().map(|text| text.content.text()),
                source,
                language: block.source.language.clone(),
            });
        }
    }
    Ok(result)
}

/// Existing target-language dialogue around a selection, including earlier
/// pages when reopening a project with no in-memory conversation. Old answers
/// for any block in this run are excluded even when it is in a later batch.
fn story_reference(
    project: &Project,
    current: &[Line],
    targets: &BTreeSet<LayerId>,
    translator: &super::TranslatorConfig,
    preceding: bool,
    mut room: usize,
) -> Result<Vec<serde_json::Value>> {
    let first = current[0].page;
    let last = current[current.len() - 1].page;
    let selected: BTreeSet<_> = current.iter().map(|line| line.page).collect();
    let mut pages = Vec::new();
    let mut started = false;
    for (index, page) in project.pages().enumerate() {
        started |= page.id == first;
        if (selected.contains(&page.id) || (preceding && !started)) && !page.excluded {
            pages.push((index + 1, page.id));
        }
        if page.id == last {
            break;
        }
    }
    let mut rows = Vec::new();
    for (number, page) in pages.into_iter().rev() {
        for (index, layer) in project.story(page)?.into_iter().enumerate().rev() {
            if targets.contains(&layer) {
                continue;
            }
            let Content::Block(block) = &project.layer(layer)?.content else {
                continue;
            };
            if translator.fast_sfx && block.role == koharu_scene::Role::Sfx {
                continue;
            }
            let Some(text) = block
                .translation
                .as_ref()
                .filter(|text| text.language == translator.target)
            else {
                continue;
            };
            let source = block.source.content.text();
            let translation = text.content.text();
            if source.trim().is_empty() || translation.trim().is_empty() {
                continue;
            }
            let row = json!({"id": format!("P{number:03}-B{:03}", index+1), "source":source, "translation":translation});
            let cost = tokens(&row.to_string()) + 1;
            if cost > room {
                continue;
            }
            room -= cost;
            rows.push(row);
            if room < 24 {
                rows.reverse();
                return Ok(rows);
            }
        }
    }
    rows.reverse();
    Ok(rows)
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Row {
    id: String,
    source: String,
    translation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Reply {
    texts: Vec<Row>,
}

fn reply_schema(rows: usize) -> schemars::Schema {
    let mut schema = schemars::schema_for!(Reply);
    let texts = &mut schema.ensure_object()["properties"]["texts"];
    texts["minItems"] = json!(rows);
    texts["maxItems"] = json!(rows);
    schema
}

struct Batch<'a> {
    system: String,
    fields: Vec<(String, &'a str)>,
    input: serde_json::Value,
    rules: Rules<'a>,
}

impl<'a> Batch<'a> {
    fn new(lines: &'a [Line], glossary: &[GlossaryEntry], mut system: String) -> Self {
        let selected: Vec<_> = lines.iter().filter(|line| !line.keep).collect();
        let (rules, fields) = Rules::prepare(
            glossary,
            selected
                .iter()
                .map(|line| (line.layer, (&line.id, &line.source))),
            &mut system,
        );
        let mut input = json!({"texts": fields.iter().map(|(id, source)| json!({ "id": id, "source": source })).collect::<Vec<_>>()});
        // Keep the whole line visible wherever a glossary rule removes text.
        let intact: BTreeSet<_> = fields.iter().map(|(id, _)| id.as_str()).collect();
        let context: Vec<_> = lines
            .iter()
            .filter(|line| !intact.contains(line.id.as_str()))
            .map(|line| json!({"id": line.id, "source": line.source}))
            .collect();
        if !context.is_empty() {
            input["context_only"] = json!(context);
        }
        Self {
            system,
            fields,
            input,
            rules,
        }
    }

    async fn ask(
        &self,
        model: &DynModel<Completion>,
        conversation: &mut Conversation,
        reasoning: Option<Reasoning>,
        cancel: &CancellationToken,
        pending_tokens: usize,
        max_output: u32,
    ) -> Result<(Vec<super::TextAnswer>, Usage)> {
        if self.fields.is_empty() {
            return Ok((self.rules.restore(&[]), Usage::default()));
        }
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(Stopped.into()),
            fit = conversation.context.fit_request(model, pending_tokens as u64) => fit?,
        }
        let mut pending: BTreeMap<&str, &str> = self
            .fields
            .iter()
            .map(|(id, source)| (id.as_str(), *source))
            .collect();
        let mut accepted = BTreeMap::new();
        let mut requests = VecDeque::from([pending.clone()]);
        let mut first = true;
        while let Some(requested) = requests.pop_front() {
            let mut input = self.input.clone();
            if !first {
                input["texts"] = json!(
                    self.fields
                        .iter()
                        .filter(|(id, _)| requested.contains_key(id.as_str()))
                        .map(|(id, source)| json!({"id": id, "source": source}))
                        .collect::<Vec<_>>()
                );
            }
            let request = koharu_llm::answers(model, reply_schema(requested.len()), reasoning)
                .max_tokens(u64::from(max_output))
                .preamble(format!(
                    "{}\nThis request has {} target IDs in texts. Return one row for every target ID exactly once.",
                    self.system, requested.len()
                ))
                .build()
                .prompt(input.to_string())
                .history(
                    conversation
                        .context
                        .messages()
                        .iter()
                        .map(|m| Message::clone(m)),
                );
            let response = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(Stopped.into()),
                response = koharu_llm::complete(model, request) => response?,
            };
            let output = response.output();
            let call = response.completion_calls.last();
            tracing::info!(
                retry = !first, requested = requested.len(), first_id = ?requested.keys().next(),
                last_id = ?requested.keys().next_back(), response_bytes = output.len(),
                finish_reason = ?call.and_then(|call| call.finish_reason.as_ref()),
                usage = ?call.map(|call| call.usage),
                "received project translation reply"
            );
            match aligned(&output, &requested) {
                Ok(rows) => {
                    for (id, translation) in rows {
                        pending.remove(id.as_str());
                        accepted.insert(id, translation);
                    }
                }
                Err(error) if first => {
                    tracing::warn!(%error, "retrying malformed translation batch")
                }
                Err(error) => {
                    tracing::warn!(%error, "invalid translation retry reply");
                    return Err(error);
                }
            }
            if pending.is_empty() {
                let translations: Vec<_> = self
                    .fields
                    .iter()
                    .map(|(id, _)| {
                        accepted
                            .remove(id)
                            .expect("every requested ID was accepted")
                    })
                    .collect();
                let usage = response
                    .completion_calls
                    .last()
                    .map_or_else(Usage::default, |call| call.usage);
                return Ok((self.rules.restore(&translations), usage));
            }
            if first {
                let mut pages = BTreeMap::<_, BTreeMap<_, _>>::new();
                for (&id, &source) in &pending {
                    let page = id
                        .split_once('-')
                        .expect("project block ID includes its page")
                        .0;
                    pages.entry(page).or_default().insert(id, source);
                }
                tracing::warn!(
                    missing = pending.len(),
                    fields = self.fields.len(),
                    pages = pages.len(),
                    "retrying unverified translation rows by page"
                );
                requests.extend(pages.into_values());
                first = false;
            }
        }
        anyhow::bail!(
            "missing or misaligned translations after retry: {}",
            pending.keys().copied().collect::<Vec<_>>().join(", ")
        )
    }
}

/// A duplicate invalidates both occurrences, even when the first looked valid.
fn aligned(
    output: &str,
    expected: &BTreeMap<&str, &str>,
) -> Result<BTreeMap<String, super::TextAnswer>> {
    let reply: Reply = serde_json::from_str(output).context("invalid translation JSON")?;
    let mut missing: BTreeSet<_> = expected.keys().copied().collect();
    let mut rejected = BTreeMap::new();
    let mut unexpected = 0;
    let mut valid = BTreeMap::new();
    let returned = reply.texts.len();
    for row in reply.texts {
        let Some((&id, &source)) = expected.get_key_value(row.id.as_str()) else {
            unexpected += 1;
            continue;
        };
        let issue = if !missing.remove(id) {
            Some("duplicate_id")
        } else if row.source != source {
            Some("source_mismatch")
        } else if row.translation.trim().is_empty() {
            Some("blank_translation")
        } else if row.reason.as_ref().is_some_and(|r| r.trim().is_empty()) {
            Some("blank_review_reason")
        } else {
            None
        };
        if let Some(issue) = issue {
            valid.remove(&row.id);
            rejected.insert(id, issue);
        } else {
            valid.insert(
                row.id,
                super::TextAnswer {
                    text: row.translation,
                    reason: row.reason,
                },
            );
        }
    }
    if valid.len() != expected.len() || unexpected > 0 {
        tracing::warn!(
            requested = expected.len(),
            returned,
            matched = valid.len(),
            ?missing,
            ?rejected,
            unexpected,
            "project translation reply did not match its targets"
        );
    }
    ensure!(
        unexpected == 0,
        "translation reply contains {unexpected} unexpected IDs"
    );
    Ok(valid)
}

fn tokens(text: &str) -> usize {
    tiktoken_rs::o200k_base_singleton()
        .encode_ordinary(text)
        .len()
}

/// End at a page boundary when possible. A page that exceeds the budget on
/// its own can span consecutive turns in the same conversation.
fn page_end(lines: &[Line], end: usize) -> usize {
    if end == lines.len() || lines[end - 1].page != lines[end].page {
        return end;
    }
    lines[..end]
        .iter()
        .rposition(|line| line.page != lines[end].page)
        .map_or(end, |i| i + 1)
}

struct Budget {
    input: usize,
    reply: usize,
    total: usize,
}

impl Budget {
    fn next<'a>(
        &self,
        lines: &'a [Line],
        glossary: &[GlossaryEntry],
        system: &str,
    ) -> Result<(usize, Batch<'a>, usize)> {
        // Bound the first sizing pass on book-length runs. Then measure the
        // actual JSON and glossary instructions before sending anything.
        let mut end = 0;
        let mut estimate = 0;
        for line in lines {
            let cost = 20 + 3 * tokens(&line.source);
            if end > 0 && estimate + cost > self.reply {
                break;
            }
            end += 1;
            estimate += cost;
        }
        end = page_end(lines, end);
        loop {
            let batch = Batch::new(&lines[..end], glossary, system.to_owned());
            let input = tokens(&batch.system) + tokens(&batch.input.to_string()) + 256;
            let output = batch
                .fields
                .iter()
                .map(|(id, source)| 18 + tokens(id) + 3 * tokens(source))
                .sum::<usize>();
            if batch.fields.is_empty()
                || (input <= self.input && output <= self.reply && input + output <= self.total)
            {
                return Ok((end, batch, input + output));
            }
            ensure!(
                end > 1,
                "{} exceeds the model's translation budget",
                lines[0].id
            );
            end = page_end(lines, end / 2);
        }
    }
}

pub(super) async fn translate(
    task: &Task,
    config: &PipelineConfig,
    conversation: &mut Conversation,
) -> Result<()> {
    let member = &task.members[0];
    let project = member.session.lock().project();
    let lines = lines(&project, &task.members, &config.glossary)?;
    if lines.is_empty() {
        return Ok(());
    }
    let glossary: Vec<_> = project
        .effective_glossary(&config.glossary)
        .into_iter()
        .cloned()
        .collect();
    let translator = &config.translator;
    conversation.keep(translator, &glossary);
    let model = if lines.iter().any(|line| !line.keep) {
        Some(conversation.hosted(translator).await?)
    } else {
        None
    };
    let spec = model.as_ref().and_then(|model| model.spec());
    // Keep room for conversation history and for reasoning. The reply echoes
    // each source and can expand in translation, even with very little input.
    let window = spec.and_then(|s| s.input_limit()).unwrap_or(32768) as usize;
    let max_output = super::model::batch_output_limit(spec);
    let reference_budget = 1536.min(window / 24);
    let budget = Budget {
        input: window / 3 - reference_budget,
        reply: max_output as usize / 2,
        total: window / 2 - reference_budget,
    };
    let targets = lines.iter().map(|line| line.layer).collect();
    let system = format!(
        "Translate the supplied manga dialogue into {}. Read the texts across pages in story order (P = page, B = block) and use the project history for names, voice and terminology. Translate each text only from its own source. Return JSON with a texts array of objects containing id, source copied exactly, and a nonempty translation. Preserve separate bubbles. Only texts entries are translation targets. Read context_only and previously saved reference entries for context and consistent terminology. The current source takes precedence. Return exactly the IDs in texts. {} Put the reason in the optional reason field of the relevant row; use null or omit it for confident rows.\n\n{}",
        name(&translator.target)?,
        super::REVIEW_INSTRUCTION,
        translator.instructions
    );
    let mut split_page = None;
    let mut remaining = VecDeque::from(lines);
    while !remaining.is_empty() {
        if member.cancel.is_cancelled() {
            return Err(Stopped.into());
        }
        let live = member.session.lock().project();
        remaining.retain(|line| live.page(line.page).is_ok_and(|page| !page.excluded));
        if remaining.is_empty() {
            break;
        }
        let pending = remaining.make_contiguous();
        let (end, mut batch, mut request_tokens) = budget.next(pending, &glossary, &system)?;
        let current = &pending[..end];
        let pages: BTreeSet<_> = current.iter().map(|line| line.page).collect();
        let starting = task
            .members
            .iter()
            .filter(|m| pages.contains(&m.page) && Some(m.page) != split_page)
            .collect::<Vec<_>>();
        task.started(&starting);
        split_page = pending
            .get(end)
            .map(|line| line.page)
            .filter(|page| pages.contains(page));
        if !batch.fields.is_empty() {
            let reference = story_reference(
                &live,
                current,
                &targets,
                translator,
                conversation.context.messages().is_empty(),
                reference_budget,
            )?;
            if !reference.is_empty() {
                batch.input["reference"] = json!(reference);
                request_tokens += reference_budget;
            }
        }
        tracing::info!(first = %current[0].id, last = %current[end-1].id, blocks = end, "translating project batch");
        let (answers, usage) = match &model {
            Some(model) => {
                batch
                    .ask(
                        model,
                        conversation,
                        translator.reasoning,
                        &member.cancel,
                        request_tokens,
                        max_output,
                    )
                    .await?
            }
            None => (Vec::new(), Usage::default()),
        };
        let mut answers = answers.into_iter();
        let rows: Vec<_> = current
            .iter()
            .map(|line| {
                let answer = if line.keep {
                    super::TextAnswer::from(line.source.clone())
                } else {
                    answers.next().expect("one answer per selected line")
                };
                Row {
                    id: line.id.clone(),
                    source: line.source.clone(),
                    translation: answer.text,
                    reason: answer.reason,
                }
            })
            .collect();
        let current: Vec<_> = remaining.drain(..end).collect();
        let glossary = project.glossary().to_vec();
        let config = config.clone();
        let member = member.clone();
        let committed = tokio::task::spawn_blocking(move || {
            commit_batch(&member, &config, &glossary, current.into_iter().zip(rows))
        })
        .await??;
        // A page split across requests finishes only after its last rows are committed.
        for page in pages.into_iter().filter(|page| Some(*page) != split_page) {
            task.events.send(crate::Event::Finished {
                run: task.members[0].run,
                page,
                stage: Stage::Translate,
            });
        }
        if !committed.is_empty() {
            let sources: Vec<_> = committed
                .iter()
                .map(|row| json!({"id":row.id, "source":row.source}))
                .collect();
            let messages = vec![
                Message::user(json!({"texts": sources}).to_string()),
                Message::assistant(json!({"texts": committed}).to_string()),
            ];
            conversation.context.record([(messages, usage)]);
        }
    }
    Ok(())
}

/// Check membership and write the remaining replies under the same session lock.
fn commit_batch(
    member: &Member,
    config: &PipelineConfig,
    glossary: &[GlossaryEntry],
    rows: impl ExactSizeIterator<Item = (Line, Row)>,
) -> Result<Vec<Row>> {
    let mut session = member.session.lock();
    if member.cancel.is_cancelled() {
        return Err(Stopped.into());
    }
    let project = session.project();
    ensure!(
        project.glossary() == glossary,
        "the project's glossary changed during translation"
    );
    let mut translated = Vec::with_capacity(rows.len());
    let mut committed = Vec::new();
    for (line, row) in rows {
        if !project.page(line.page).is_ok_and(|page| !page.excluded) {
            continue;
        }
        let (page, _) = project.parent(line.layer)?;
        ensure!(page.id == line.page, "{} moved to another page", line.id);
        let Content::Block(block) = &project.layer(line.layer)?.content else {
            anyhow::bail!("{} is no longer a text block", line.id);
        };
        let saved = block.translation.as_ref().map(|text| text.content.text());
        ensure!(
            block.source.content.text() == line.source
                && block.source.language == line.language
                && saved == line.saved,
            "the source or translation of {} changed during translation",
            line.id
        );
        translated.push((
            line.layer,
            line.language,
            super::TextAnswer {
                text: row.translation.clone(),
                reason: row.reason.clone(),
            },
        ));
        committed.push(row);
    }
    if committed.is_empty() {
        return Ok(committed);
    }
    let ops = written(&project, translated, &config.translator, &config.glossary)?;
    if member.cancel.is_cancelled() {
        return Err(Stopped.into());
    }
    session.apply(Edit {
        label: Stage::Translate.to_string(),
        ops,
    })?;
    Ok(committed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use koharu_scene::{Session, Uuid};

    #[test]
    fn skipping_a_page_during_a_request_commits_only_the_other_pages() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut session = Session::create(directory.path().join("batch.khr"))?;
        let mut ops = Vec::new();
        for index in 0..2 {
            let page = format!("$p{index}");
            ops.push(
                json!({"op":"add_page","id":page,"index":index,"size":{"width":100,"height":100}}),
            );
            ops.push(json!({"op":"add_layer","page":page,"index":0,"layer":{
                "kind":"block","source":{"content":"Source"},"translation":{"content":"Saved","language":"en"}
            }}));
        }
        session.apply(Edit {
            label: "fixture".into(),
            ops: ops
                .into_iter()
                .map(serde_json::from_value)
                .collect::<Result<_, _>>()?,
        })?;
        let project = session.project();
        let session = std::sync::Arc::new(parking_lot::Mutex::new(session));
        let members: Vec<_> = project
            .pages()
            .enumerate()
            .map(|(index, page)| Member {
                run: crate::RunId(0),
                job: 0.into(),
                page: page.id,
                index,
                blocks: None,
                session: session.clone(),
                cancel: CancellationToken::new(),
                resume: false,
            })
            .collect();
        let requested = lines(&project, &members, &[])?;
        let skipped = requested[0].layer;
        let included = requested[1].layer;
        session.lock().apply(Edit {
            label: "skip first".into(),
            ops: vec![serde_json::from_value(
                json!({"op":"set_page_options","page":members[0].page,"excluded":true}),
            )?],
        })?;
        let rows = requested.into_iter().map(|line| {
            let row = Row {
                id: line.id.clone(),
                source: line.source.clone(),
                translation: "New".into(),
                reason: Some("Verify this reading".into()),
            };
            (line, row)
        });
        let committed = commit_batch(&members[0], &PipelineConfig::default(), &[], rows)?;
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0].id, "P002-B001");
        let after = session.lock().project();
        assert_eq!(after.layer(skipped)?, project.layer(skipped)?);
        let Content::Block(block) = &after.layer(included)?.content else {
            unreachable!()
        };
        assert_eq!(block.translation.as_ref().unwrap().content.text(), "New");
        let reviews = after.translation_uncertainties()?;
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].location.layer, included);
        assert_eq!(reviews[0].reason, "Verify this reading");
        Ok(())
    }

    #[test]
    fn selected_block_uses_its_story_badge_and_neighbors_saved_translation() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut session = Session::create(directory.path().join("story.khr"))?;
        let ops = [
            json!({"op":"add_page", "id":"$p", "index":0, "size":{"width":100,"height":100}}),
            json!({"op":"add_layer","page":"$p","index":0,"layer":{"kind":"text","content":[{"insert":"Title"}]}}),
            json!({"op":"add_layer","page":"$p","index":1,"layer":{"kind":"block","source":{"content":[{"insert":"Neighbor"}]}, "translation":{"language":"en","content":[{"insert":"Saved neighbor"}]}}}),
            json!({"op":"add_layer","page":"$p","index":2,"layer":{"kind":"block","source":{"content":[{"insert":"Target"}]}, "translation":{"language":"en","content":[{"insert":"Old target"}]}}}),
        ];
        session.apply(Edit {
            label: "fixture".into(),
            ops: ops
                .into_iter()
                .map(serde_json::from_value)
                .collect::<Result<_, _>>()?,
        })?;
        let project = session.project();
        let page = project.pages().next().unwrap().id;
        let layer = project.story(page)?[2];
        let member = Member {
            run: crate::RunId(0),
            job: 0.into(),
            page,
            index: 0,
            blocks: Some(vec![layer].into()),
            session: std::sync::Arc::new(parking_lot::Mutex::new(session)),
            cancel: CancellationToken::new(),
            resume: false,
        };
        let selected = lines(&project, &[member], &[])?;
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, "P001-B003");
        assert_eq!(selected[0].layer, layer);
        let reference = story_reference(
            &project,
            &selected,
            &BTreeSet::from([layer]),
            &super::super::TranslatorConfig::default(),
            true,
            256,
        )?;
        assert_eq!(
            reference,
            [json!({"id":"P001-B002","source":"Neighbor","translation":"Saved neighbor"})]
        );
        Ok(())
    }

    #[test]
    fn two_short_pages_share_one_request_and_replies_align_by_id() -> Result<()> {
        let mut lines = Vec::new();
        for p in 5..=6 {
            for b in 1..=11 {
                lines.push(Line {
                    page: PageId(Uuid::from_u128(p)),
                    layer: LayerId(Uuid::from_u128(p * 100 + b)),
                    id: format!("P{p:03}-B{b:03}"),
                    source: format!("Dialogue {p}/{b}"),
                    language: icu_locale_core::langid!("en"),
                    keep: false,
                    saved: None,
                });
            }
        }
        let budget = Budget {
            input: 8192,
            reply: 4096,
            total: 16384,
        };
        let (end, batch, _) = budget.next(&lines, &[], "Translate")?;
        assert_eq!(end, 22);
        assert_eq!(batch.input["texts"][11]["id"], "P006-B001");
        assert_eq!(page_end(&lines, 15), 11);
        assert_eq!(page_end(&lines, 3), 3);
        let expected = batch
            .fields
            .iter()
            .map(|(id, source)| (id.as_str(), *source))
            .collect();
        let shuffled = lines
            .iter()
            .rev()
            .map(|line| json!({"id":line.id, "source":line.source, "translation":line.id}))
            .collect::<Vec<_>>();
        let valid = aligned(&json!({"texts":shuffled}).to_string(), &expected)?;
        assert_eq!(valid.len(), 22);
        assert!(valid.iter().all(|(id, text)| id == &text.text));
        Ok(())
    }

    #[test]
    fn retries_only_missing_duplicate_or_misaligned_ids() -> Result<()> {
        let expected = BTreeMap::from([("a", "Hello"), ("b", "Hi"), ("c", "Bye"), ("d", "Hello")]);
        for size in [expected.len(), 1] {
            let schema = serde_json::to_value(reply_schema(size))?;
            let texts = &schema["properties"]["texts"];
            assert_eq!(texts["minItems"], size);
            assert_eq!(texts["maxItems"], size);
            assert!(texts["items"].is_object());
        }
        let output = json!({"texts":[
            {"id":"a","source":"Hello","translation":"A","reason":"Unknown speaker"},
            {"id":"b","source":"Hi","translation":"B"},
            {"id":"b","source":"Hi","translation":"Again"},
            {"id":"c","source":"Hi","translation":"C"}
        ]});
        assert_eq!(
            aligned(&output.to_string(), &expected)?,
            BTreeMap::from([(
                "a".into(),
                super::super::TextAnswer {
                    text: "A".into(),
                    reason: Some("Unknown speaker".into())
                }
            )])
        );
        let extra = json!({"texts":[{"id":"z","source":"Hello","translation":"Extra"}]});
        assert!(aligned(&extra.to_string(), &expected).is_err());
        Ok(())
    }
}
