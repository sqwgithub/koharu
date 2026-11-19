use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

use serde::{Deserialize, Serialize};
use specta::Type;

use super::{GlossaryEntry, GlossaryKind, TermLocation, matches, term_occurrences};
use crate::{Content, Error, LayerId, Op, Project, Result, RichText};

const MARKERS: &str = "koharu.glossary_application";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateLine {
    pub location: TermLocation,
    pub translation: String,
    pub occurrences: usize,
    pub alternatives: Vec<String>,
    pub protected: Vec<String>,
    pub context_before: Option<String>,
    pub context_after: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidatePlan {
    entry: GlossaryEntry,
    locations: BTreeSet<LayerId>,
    direct: Vec<CandidateLine>,
    pub lines: Vec<CandidateLine>,
}

#[derive(Debug, Clone)]
pub enum CandidateDecision {
    Located(Vec<Range<usize>>),
    Uncertain {
        reason: String,
        spans: Vec<Range<usize>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct CandidateReview {
    pub location: TermLocation,
    pub source: String,
    pub expected: String,
    pub previous: String,
    pub preview: Option<String>,
    pub reason: String,
}

// One marker per source term and layer. A closed marker suppresses a repeat
// request only while its OCR, target spelling and current translation agree.
#[derive(Serialize, Deserialize)]
struct Marker {
    source_text: String,
    previous: String,
    expected: String,
    reason: Option<String>,
    spans: Vec<Range<usize>>,
}

impl Marker {
    fn applies(&self, source: &str, translation: &str, expected: &str) -> bool {
        self.source_text == source && self.previous == translation && self.expected == expected
    }
    fn op(self, layer: LayerId, term: &str) -> Op {
        Op::SetAttribute {
            layer: layer.into(),
            namespace: MARKERS.into(),
            key: term.into(),
            value: Some(serde_json::json!(self)),
        }
    }
}

impl CandidateLine {
    fn edits(
        &self,
        ranges: &[Range<usize>],
        expected: &str,
        content: &RichText,
        source_term: &str,
    ) -> Option<RichText> {
        if ranges.is_empty() || ranges.len() != self.occurrences {
            return None;
        }
        let mut previous_end = 0;
        let mut updated = content.clone();
        for r in ranges {
            let part = self.translation.get(r.clone())?;
            if r.start < previous_end || part.is_empty() || part.trim() != part {
                return None;
            }
            if !super::at_word_boundaries(part, &self.translation, r.start, r.end) {
                return None;
            }
            let known = self.alternatives.iter().any(|s| s == part);
            if !known
                && (part.contains(['。', '！', '？', '!', '?', '\n', '\r'])
                    || r.len() == self.translation.len()
                        && self.location.text.trim() != source_term)
            {
                return None;
            }
            let protected = self
                .protected
                .iter()
                .flat_map(|s| matches(s, &self.translation))
                .any(|(a, b)| a < r.end && r.start < b);
            let cuts_name = self
                .alternatives
                .iter()
                .flat_map(|s| matches(s, &self.translation))
                .any(|(a, b)| (a < r.start && r.start < b) || (a < r.end && r.end < b));
            if protected || cuts_name {
                return None;
            }
            previous_end = r.end;
        }
        for r in ranges.iter().rev() {
            let part = &self.translation[r.clone()];
            if part != expected {
                let marks = content.marks_at(r.start + part.chars().next()?.len_utf8());
                updated = updated.spliced(r.clone(), expected, &marks);
            }
        }
        Some(updated)
    }
}

// Both the locator and the review dialog use the *current* overlap and
// preservation rules, including other terms whose spellings changed since analysis.
struct ApplicationRules {
    effective: Vec<GlossaryEntry>,
    spellings: BTreeMap<String, BTreeSet<String>>,
}

impl ApplicationRules {
    fn new(project: &Project, global: &[GlossaryEntry]) -> Self {
        let effective: Vec<_> = project
            .effective_glossary(global)
            .into_iter()
            .cloned()
            .collect();
        let mut spellings = BTreeMap::<String, BTreeSet<String>>::new();
        for entry in &effective {
            let alternatives = spellings.entry(entry.source.clone()).or_default();
            alternatives.insert(entry.translation.trim().to_owned());
            alternatives.extend(project.manual_candidates(&entry.source).iter().cloned());
        }
        for entry in project
            .glossary_candidates()
            .iter()
            .flatten()
            .filter(|e| e.is_term())
        {
            spellings
                .entry(entry.source.clone())
                .or_default()
                .insert(entry.translation.trim().to_owned());
        }
        Self {
            effective,
            spellings,
        }
    }

    fn line(
        &self,
        entry: &GlossaryEntry,
        location: TermLocation,
        translation: &str,
    ) -> Option<CandidateLine> {
        let id = location.layer;
        let terms = term_occurrences(&self.effective, id, &location.text);
        let occurrences = terms
            .iter()
            .filter(|&&i| self.effective[i].source == entry.source)
            .count();
        if occurrences == 0 || occurrences != entry.occurrences(&location.text).count() {
            return None;
        }
        let mut protected: BTreeSet<_> = self
            .effective
            .iter()
            .filter(|e| {
                e.kind == GlossaryKind::Preserve && e.applies_to(id) && e.matches(&location.text)
            })
            .map(|e| e.source.clone())
            .collect();
        for i in terms {
            let term = &self.effective[i];
            if term.source != entry.source {
                protected.extend(self.spellings[&term.source].iter().cloned());
            }
        }
        Some(CandidateLine {
            location,
            translation: translation.to_owned(),
            occurrences,
            alternatives: self.spellings[&entry.source].iter().cloned().collect(),
            protected: protected.into_iter().collect(),
            context_before: None,
            context_after: None,
        })
    }
}

impl Project {
    pub fn candidate_plan(
        &self,
        entry: &GlossaryEntry,
        locations: &[LayerId],
        global: &[GlossaryEntry],
    ) -> Result<CandidatePlan> {
        if !entry.is_term() || !self.glossary().contains(entry) {
            return Err(Error::Rejected("the selected glossary term changed".into()));
        }
        if !self
            .glossary_candidates()
            .iter()
            .any(|group| group.first().is_some_and(|e| e.source == entry.source))
        {
            return Err(Error::Rejected(
                "the glossary candidate group changed".into(),
            ));
        }
        let rules = ApplicationRules::new(self, global);
        let expected = entry.translation.trim();
        let mut plan = CandidatePlan {
            entry: entry.clone(),
            locations: locations.iter().copied().collect(),
            direct: vec![],
            lines: vec![],
        };
        let mut visible = BTreeSet::new();
        for page in self.pages().filter(|p| !p.excluded) {
            visible.extend(self.story(page.id)?);
        }
        let mut transcript = self.transcript()?;
        transcript.retain(|s| {
            visible.contains(&s.layer)
                && entry.applies_to(s.layer)
                && !self.keeps_original(global, s.layer, &s.text)
        });
        for (index, item) in transcript.iter().enumerate() {
            let id = item.layer;
            if !plan.locations.contains(&id) || self.locked_at(id).is_some() {
                continue;
            }
            let Some(translation) = item.translation.as_ref().filter(|t| !t.trim().is_empty())
            else {
                continue;
            };
            if self
                .layer(id)?
                .attributes
                .get(MARKERS)
                .and_then(|m| m.get(&entry.source))
                .and_then(|v| serde_json::from_value::<Marker>(v.clone()).ok())
                .is_some_and(|m| m.applies(&item.text, translation, expected))
            {
                continue;
            }
            let Some(mut line) = rules.line(entry, item.into(), translation) else {
                continue;
            };
            let context = |i: usize| {
                transcript
                    .get(i)
                    .filter(|s| s.page == item.page)
                    .map(|s| s.text.clone())
            };
            line.context_before = index.checked_sub(1).and_then(context);
            line.context_after = context(index + 1);
            if item.text.trim() == entry.source && line.alternatives.contains(translation) {
                if translation != expected {
                    plan.direct.push(line);
                }
            } else {
                plan.lines.push(line);
            }
        }
        Ok(plan)
    }

    /// Recheck all evidence at commit time. Returning ops lets the caller apply
    /// both local replacements and review markers as one undoable edit.
    pub fn settle_candidate(
        &self,
        plan: &CandidatePlan,
        decisions: &BTreeMap<LayerId, CandidateDecision>,
        global: &[GlossaryEntry],
    ) -> Result<(Vec<Op>, usize, usize)> {
        if self.candidate_plan(
            &plan.entry,
            &plan.locations.iter().copied().collect::<Vec<_>>(),
            global,
        )? != *plan
        {
            return Err(Error::Rejected(
                "the glossary candidate locations changed".into(),
            ));
        }
        if decisions.len() != plan.lines.len()
            || !plan
                .lines
                .iter()
                .all(|line| decisions.contains_key(&line.location.layer))
        {
            return Err(Error::Rejected(
                "the locator returned different glossary locations".into(),
            ));
        }
        let expected = plan.entry.translation.trim();
        let mut ops = Vec::new();
        let mut changed = 0;
        let mut reviewed = 0;
        for line in plan.direct.iter().chain(&plan.lines) {
            let id = line.location.layer;
            let Content::Block(block) = &self.layer(id)?.content else {
                unreachable!()
            };
            let content = &block
                .translation
                .as_ref()
                .expect("validated candidate translation")
                .content;
            let direct =
                CandidateDecision::Located(std::iter::once(0..line.translation.len()).collect());
            let decision = decisions.get(&id).unwrap_or(&direct);
            let mut marker = Marker {
                source_text: line.location.text.clone(),
                previous: line.translation.clone(),
                expected: expected.into(),
                reason: None,
                spans: vec![],
            };
            match decision {
                CandidateDecision::Located(spans)
                    if let Some(new) = line.edits(spans, expected, content, &plan.entry.source) =>
                {
                    if new != *content {
                        marker.previous = new.text();
                        ops.push(Op::SetRichText {
                            layer: id.into(),
                            spans: new.0,
                        });
                        changed += 1;
                    }
                }
                CandidateDecision::Uncertain { reason, spans } => {
                    marker.reason = Some(reason.clone());
                    if line
                        .edits(spans, expected, content, &plan.entry.source)
                        .is_some()
                    {
                        marker.spans = spans.clone();
                    }
                }
                _ => {
                    marker.reason = Some("glossary_location_conflict".into());
                }
            }
            reviewed += usize::from(marker.reason.is_some());
            ops.push(marker.op(id, &plan.entry.source));
        }
        Ok((ops, changed, reviewed))
    }

    pub fn candidate_reviews(&self, global: &[GlossaryEntry]) -> Result<Vec<CandidateReview>> {
        let mut result = Vec::new();
        let rules = ApplicationRules::new(self, global);
        for (index, page) in self.pages().enumerate() {
            if page.excluded {
                continue;
            }
            for id in self.story(page.id)? {
                let layer = self.layer(id)?;
                let Content::Block(block) = &layer.content else {
                    continue;
                };
                let Some(markers) = layer.attributes.get(MARKERS) else {
                    continue;
                };
                let Some(translated) = &block.translation else {
                    continue;
                };
                let text = block.source.content.text();
                if self.locked_at(id).is_some() || self.keeps_original(global, id, &text) {
                    continue;
                }
                let previous = translated.content.text();
                for (source, value) in markers {
                    let Ok(marker) = serde_json::from_value::<Marker>(value.clone()) else {
                        continue;
                    };
                    let Some(reason) = marker.reason.as_ref() else {
                        continue;
                    };
                    let Some(entry) = self
                        .glossary()
                        .iter()
                        .find(|e| e.source == *source && e.is_term())
                    else {
                        continue;
                    };
                    if !marker.applies(&text, &previous, entry.translation.trim()) {
                        continue;
                    }
                    let location = TermLocation {
                        page: page.id,
                        number: index + 1,
                        layer: id,
                        text: text.clone(),
                    };
                    let Some(line) = rules.line(entry, location, &previous) else {
                        continue;
                    };
                    let preview = line
                        .edits(&marker.spans, &marker.expected, &translated.content, source)
                        .map(|new| new.text())
                        .filter(|new| new != &previous);
                    result.push(CandidateReview {
                        location: line.location,
                        source: source.clone(),
                        expected: marker.expected,
                        previous: previous.clone(),
                        preview,
                        reason: reason.clone(),
                    });
                }
            }
        }
        Ok(result)
    }

    pub fn candidate_review_ops(
        &self,
        item: &CandidateReview,
        translation: Option<&str>,
        global: &[GlossaryEntry],
    ) -> Result<Vec<Op>> {
        if !self.candidate_reviews(global)?.contains(item) {
            return Err(Error::Rejected("the glossary review changed".into()));
        }
        let layer = self.layer(item.location.layer)?;
        let value = &layer.attributes[MARKERS][&item.source];
        let mut marker: Marker =
            serde_json::from_value(value.clone()).map_err(|e| Error::Rejected(e.to_string()))?;
        let mut ops = Vec::new();
        if let Some(new) = translation.filter(|&new| new != item.previous) {
            if new.trim().is_empty() {
                return Err(Error::Rejected("the translation is empty".into()));
            }
            let Content::Block(block) = &layer.content else {
                unreachable!()
            };
            let original = &block
                .translation
                .as_ref()
                .expect("validated review")
                .content;
            let updated = if item.preview.as_deref() == Some(new) {
                let mut rich = original.clone();
                for r in marker.spans.iter().rev() {
                    let first = item.previous[r.clone()]
                        .chars()
                        .next()
                        .ok_or_else(|| Error::Rejected("invalid review range".into()))?;
                    rich = rich.spliced(
                        r.clone(),
                        &item.expected,
                        &original.marks_at(r.start + first.len_utf8()),
                    );
                }
                rich
            } else {
                original.replaced(new)
            };
            ops.push(Op::SetRichText {
                layer: layer.id.into(),
                spans: updated.0,
            });
            marker.previous = new.to_owned();
        }
        marker.reason = None;
        marker.spans.clear();
        ops.push(marker.op(layer.id, &item.source));
        Ok(ops)
    }
}
