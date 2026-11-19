use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::{Content, LayerId, Op, PageId, Project};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum GlossaryKind {
    Person,
    Place,
    Term,
    Preserve,
    PreserveBlock,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct GlossaryEntry {
    pub source: String,
    pub translation: String,
    pub kind: GlossaryKind,
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wiki_url: Option<url::Url>,
    /// Project text blocks where this rule is ignored.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub excluded_layers: BTreeSet<LayerId>,
}

impl GlossaryEntry {
    pub fn applies_to(&self, layer: LayerId) -> bool {
        !self.excluded_layers.contains(&layer)
    }

    pub fn matches(&self, text: &str) -> bool {
        if self.kind == GlossaryKind::PreserveBlock {
            text.trim() == self.source
        } else {
            matches(&self.source, text).next().is_some()
        }
    }

    pub fn is_term(&self) -> bool {
        matches!(
            self.kind,
            GlossaryKind::Person | GlossaryKind::Place | GlossaryKind::Term
        )
    }

    pub fn occurrences<'a>(&'a self, text: &'a str) -> impl Iterator<Item = (usize, usize)> + 'a {
        matches(&self.source, text)
    }
}

fn matches<'a>(term: &'a str, text: &'a str) -> impl Iterator<Item = (usize, usize)> {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    text.match_indices(term)
        .filter_map(move |(start, matched)| {
            let end = start + matched.len();
            let inside_left = term.starts_with(word) && text[..start].ends_with(word);
            let inside_right = term.ends_with(word) && text[end..].starts_with(word);
            (!inside_left && !inside_right).then_some((start, end))
        })
}

/// Terms present in the text, using the longest match when spellings overlap.
pub fn matching_terms<'a>(
    entries: &'a [GlossaryEntry],
    layer: LayerId,
    text: &str,
) -> Vec<&'a GlossaryEntry> {
    term_occurrences(entries, layer, text)
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|index| &entries[index])
        .collect()
}

/// Index of the rule owning each unprotected occurrence, in reading order.
fn term_occurrences(entries: &[GlossaryEntry], layer: LayerId, text: &str) -> Vec<usize> {
    let protected: Vec<_> = entries
        .iter()
        .filter(|entry| entry.kind == GlossaryKind::Preserve && entry.applies_to(layer))
        .flat_map(|entry| entry.occurrences(text))
        .collect();
    let mut matches: Vec<_> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.is_term() && entry.applies_to(layer))
        .flat_map(|(index, entry)| {
            entry
                .occurrences(text)
                .map(move |(start, end)| (start, end, index))
        })
        .filter(|&(start, end, _)| {
            !protected
                .iter()
                .any(|&(left, right)| start < right && left < end)
        })
        .collect();
    matches.sort_unstable_by_key(|&(start, end, _)| (start, std::cmp::Reverse(end)));
    let mut used = Vec::new();
    let mut last = 0;
    for (start, end, index) in matches {
        if start >= last {
            used.push(index);
            last = end;
        }
    }
    used
}

pub fn valid_glossary(entries: &[GlossaryEntry]) -> bool {
    let mut sources = BTreeSet::new();
    entries.iter().all(|entry| {
        !entry.source.is_empty()
            && entry.source == entry.source.trim()
            && (entry.is_term() != entry.translation.trim().is_empty())
            && entry
                .wiki_url
                .as_ref()
                .is_none_or(|url| matches!(url.scheme(), "https" | "http") && url.has_host())
            && sources.insert(&entry.source)
    })
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Type, schemars::JsonSchema)]
pub struct TermLocation {
    pub page: PageId,
    pub number: usize,
    pub layer: LayerId,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct TranslationUncertainty {
    pub location: TermLocation,
    pub translation: String,
    pub reason: String,
}

pub const JAPANESE_HONORIFICS: [&str; 6] = ["ちゃん", "さん", "くん", "君", "さま", "様"];

/// The address suffix is not part of the name used as the glossary key.
pub fn japanese_address(source: &str) -> (&str, &str) {
    JAPANESE_HONORIFICS
        .into_iter()
        .find_map(|suffix| source.strip_suffix(suffix).map(|name| (name, suffix)))
        .unwrap_or((source, ""))
}

impl From<&SourceLine> for TermLocation {
    fn from(line: &SourceLine) -> Self {
        Self {
            page: line.page,
            number: line.number,
            layer: line.layer,
            text: line.text.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Type, schemars::JsonSchema)]
pub struct AliasSuggestion {
    pub source: String,
    pub canonical: String,
    pub translation: String,
    pub reason: String,
    pub locations: Vec<TermLocation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, Type, schemars::JsonSchema)]
pub struct TextSuggestion {
    pub location: TermLocation,
    /// The exact translation the reviewer saw, for compare-and-set.
    pub previous: String,
    pub translation: String,
    pub reason: String,
}

impl TextSuggestion {
    pub(crate) fn key(&self) -> (LayerId, String, String, String) {
        (
            self.location.layer,
            self.location.text.clone(),
            self.previous.clone(),
            self.translation.clone(),
        )
    }
}

/// Closed pairs survive another analysis, even if the model rephrases its reason.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct SuggestionReviews {
    /// Alternatives explicitly entered for a project candidate group.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub manual_candidates: BTreeMap<String, Vec<String>>,
    pub aliases: Vec<AliasSuggestion>,
    pub closed_aliases: BTreeSet<(String, String)>,
    pub corrections: Vec<TextSuggestion>,
    pub closed_corrections: BTreeSet<(LayerId, String, String, String)>,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct SourceLine {
    pub page: PageId,
    pub number: usize,
    pub layer: LayerId,
    pub text: String,
    pub translation: Option<String>,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct TermIssue {
    pub page: PageId,
    pub number: usize,
    pub layer: LayerId,
    pub source: String,
    pub translation: Option<String>,
    pub expected: String,
    pub kind: GlossaryKind,
}

impl Project {
    pub fn translation_uncertainties(&self) -> crate::Result<Vec<TranslationUncertainty>> {
        let mut result = Vec::new();
        for (index, page) in self.pages().enumerate() {
            for (id, block) in self.blocks(page.id)? {
                let Some(marker) = self
                    .layer(id)?
                    .attributes
                    .get("koharu")
                    .and_then(|attrs| attrs.get("translation_review"))
                else {
                    continue;
                };
                let Some(reason) = marker["reason"].as_str() else {
                    continue;
                };
                let Some(text) = &block.translation else {
                    continue;
                };
                let source = block.source.content.text();
                let translation = text.content.text();
                if marker["source"] == source && marker["translation"] == translation {
                    result.push(TranslationUncertainty {
                        location: TermLocation {
                            page: page.id,
                            number: index + 1,
                            layer: id,
                            text: source,
                        },
                        translation,
                        reason: reason.to_owned(),
                    });
                }
            }
        }
        Ok(result)
    }

    pub fn alias_suggestions(&self) -> &[AliasSuggestion] {
        &self.suggestion_reviews.aliases
    }

    pub fn text_suggestions(&self) -> &[TextSuggestion] {
        &self.suggestion_reviews.corrections
    }

    pub(crate) fn check_translation_evidence(&self, item: &TextSuggestion) -> bool {
        self.index.get(&item.location.layer) == Some(&item.location.page)
            && self.layer(item.location.layer).is_ok_and(|layer| {
                matches!(&layer.content, Content::Block(b)
                if b.source.content.text() == item.location.text
                    && b.translation.as_ref().is_some_and(|t| t.content.text() == item.previous))
            })
    }

    pub(crate) fn check_alias_evidence(&self, suggestion: &AliasSuggestion) -> bool {
        let at = &suggestion.locations;
        let nonempty = |s: &str| !s.is_empty() && s.trim() == s;
        nonempty(&suggestion.source) && nonempty(&suggestion.canonical)
            && nonempty(&suggestion.translation) && nonempty(&suggestion.reason)
            && suggestion.source != suggestion.canonical
            && at.iter().any(|l| l.text.contains(&suggestion.source))
            && at.iter().any(|l| l.text.contains(&suggestion.canonical))
            && at.iter().all(|l| self.index.get(&l.layer) == Some(&l.page)
                && self.layer(l.layer).is_ok_and(|layer| matches!(&layer.content, Content::Block(b) if b.source.content.text() == l.text)))
    }
    pub fn glossary(&self) -> &[GlossaryEntry] {
        &self.glossary
    }

    pub fn manual_candidates(&self, source: &str) -> &[String] {
        self.suggestion_reviews
            .manual_candidates
            .get(source)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn glossary_candidates(&self) -> &[Vec<GlossaryEntry>] {
        &self.glossary_candidates
    }

    pub fn effective_glossary<'a>(&'a self, global: &'a [GlossaryEntry]) -> Vec<&'a GlossaryEntry> {
        let sources: BTreeSet<_> = self
            .glossary
            .iter()
            .map(|entry| entry.source.as_str())
            .collect();
        self.glossary
            .iter()
            .chain(
                global
                    .iter()
                    .filter(|entry| !sources.contains(entry.source.as_str())),
            )
            .collect()
    }

    pub fn keeps_original(&self, global: &[GlossaryEntry], layer: LayerId, text: &str) -> bool {
        let has_text = |entry: &&GlossaryEntry| entry.source == text.trim();
        self.glossary
            .iter()
            .find(has_text)
            .or_else(|| global.iter().find(has_text))
            .is_some_and(|entry| {
                entry.kind == GlossaryKind::PreserveBlock && entry.applies_to(layer)
            })
    }

    /// Exact, offline replacements in the displayed candidate locations. Each
    /// op is scoped to one block so the caller can commit the set in one edit.
    pub fn apply_candidate(
        &self,
        entry: &GlossaryEntry,
        locations: &[LayerId],
        global: &[GlossaryEntry],
    ) -> crate::Result<Vec<Op>> {
        if !entry.is_term() || !self.glossary().contains(entry) {
            return Err(crate::Error::Rejected(
                "the selected glossary term changed".into(),
            ));
        }
        let groups: BTreeMap<_, _> = self
            .glossary_candidates()
            .iter()
            .filter_map(|group| Some((group.first()?.source.as_str(), group)))
            .collect();
        if !groups.contains_key(entry.source.as_str()) {
            return Err(crate::Error::Rejected(
                "the glossary candidate group changed".into(),
            ));
        }
        let spellings = |term: &GlossaryEntry| -> BTreeSet<String> {
            groups
                .get(term.source.as_str())
                .into_iter()
                .flat_map(|group| group.iter())
                .filter(|e| e.is_term())
                .map(|e| e.translation.trim().to_owned())
                .chain(self.manual_candidates(&term.source).iter().cloned())
                .chain(std::iter::once(term.translation.trim().to_owned()))
                .collect()
        };
        let names = spellings(entry);
        let chosen = entry.translation.trim();
        let effective: Vec<_> = self
            .effective_glossary(global)
            .into_iter()
            .cloned()
            .collect();
        let mut ops = Vec::new();
        for id in locations.iter().copied().collect::<BTreeSet<_>>() {
            if !entry.applies_to(id) || self.locked_at(id).is_some() {
                continue;
            }
            let Ok(layer) = self.layer(id) else {
                continue;
            };
            let Content::Block(block) = &layer.content else {
                continue;
            };
            let Some(translation) = &block.translation else {
                continue;
            };
            let source = block.source.content.text();
            if self.parent(id)?.0.excluded || self.keeps_original(global, id, &source) {
                continue;
            }
            let terms = term_occurrences(&effective, id, &source);
            let count = terms
                .iter()
                .filter(|&&i| effective[i].source == entry.source)
                .count();
            if count == 0 || count != entry.occurrences(&source).count() {
                continue;
            }
            let mut protected: BTreeSet<_> = effective
                .iter()
                .filter(|e| {
                    e.kind == GlossaryKind::Preserve && e.applies_to(id) && e.matches(&source)
                })
                .map(|e| e.source.clone())
                .collect();
            for i in terms.into_iter().collect::<BTreeSet<_>>() {
                if effective[i].source != entry.source {
                    protected.extend(spellings(&effective[i]));
                }
            }
            let old = translation.content.text();
            let ranges = |text: &str| {
                let protected: Vec<_> = protected.iter().flat_map(|s| matches(s, text)).collect();
                // Prefer the longest spelling starting at each position, including
                // the active spelling itself (e.g. 哥哥 inside 大哥哥).
                let mut found: Vec<_> = names.iter().flat_map(|name| matches(name, text)).collect();
                found.sort_unstable_by_key(|&(a, b)| (a, std::cmp::Reverse(b)));
                let mut end = 0;
                let mut occurrences = 0;
                let mut replacements = Vec::new();
                for (a, b) in found {
                    if a < end || protected.iter().any(|&(l, r)| a < r && l < b) {
                        continue;
                    }
                    end = b;
                    occurrences += 1;
                    if &text[a..b] != chosen {
                        replacements.push(a..b);
                    }
                }
                if occurrences > count {
                    replacements.clear();
                }
                replacements
            };
            let edits = ranges(&old);
            if edits.is_empty() {
                continue;
            }
            let mut updated = translation.content.clone();
            for range in edits.into_iter().rev() {
                let first = old[range.clone()]
                    .chars()
                    .next()
                    .expect("nonempty candidate");
                let style = translation.content.marks_at(range.start + first.len_utf8());
                updated = updated.spliced(range, chosen, &style);
            }
            // A replacement can form another old spelling across its boundary.
            // Such a block is ambiguous and must not change on repeated clicks.
            if ranges(&updated.text()).is_empty() {
                ops.push(Op::SetRichText {
                    layer: id.into(),
                    spans: updated.0,
                });
            }
        }
        Ok(ops)
    }

    /// Candidate alternatives survive re-analysis and selection. Existing
    /// project entries keep their explicit selection until edited.
    pub fn analyzed_glossary(
        &self,
        groups: Vec<Vec<GlossaryEntry>>,
        selected: Vec<GlossaryEntry>,
        suggestions: Vec<AliasSuggestion>,
    ) -> Vec<Op> {
        let mut candidates: BTreeMap<_, _> = self
            .glossary_candidates
            .iter()
            .filter_map(|entries| Some((entries.first()?.source.clone(), entries.clone())))
            .collect();
        for mut group in groups {
            let Some(source) = group.first().map(|entry| entry.source.clone()) else {
                continue;
            };
            if let Some(previous) = candidates.remove(&source) {
                for item in previous {
                    if !group.iter().any(|entry| {
                        entry.kind == item.kind && entry.translation == item.translation
                    }) {
                        group.push(item);
                    }
                }
            }
            candidates.insert(source, group);
        }
        let mut entries = self.glossary().to_vec();
        let held: BTreeSet<_> = entries.iter().map(|entry| entry.source.clone()).collect();
        // A later coarse extraction cannot overturn a human decision (or bypass
        // pending review) even if it includes the honorific with the name.
        let reviewed: BTreeSet<_> = self
            .suggestion_reviews
            .closed_aliases
            .iter()
            .map(|(source, _)| source.as_str())
            .chain(
                self.suggestion_reviews
                    .aliases
                    .iter()
                    .chain(&suggestions)
                    .map(|s| s.source.as_str()),
            )
            .collect();
        entries.extend(selected.into_iter().filter(|entry| {
            let source = entry.source.as_str();
            !held.contains(source)
                && !reviewed.contains(source)
                && !reviewed.contains(japanese_address(source).0)
        }));
        let candidates: Vec<_> = candidates.into_values().collect();
        let mut ops = Vec::new();
        if candidates.as_slice() != self.glossary_candidates() {
            ops.push(Op::SetGlossaryCandidates { candidates });
        }
        if entries.as_slice() != self.glossary() {
            ops.push(Op::SetGlossary {
                previous: self.glossary().to_vec(),
                entries,
            });
        }
        if !suggestions.is_empty() {
            ops.push(Op::SuggestAliases { suggestions });
        }
        ops
    }

    /// The original text, in the same order used by the translator.
    pub fn transcript(&self) -> crate::Result<Vec<SourceLine>> {
        let mut lines = Vec::new();
        for (index, page) in self.pages().enumerate() {
            for (layer, block) in self.blocks(page.id)? {
                let text = block.source.content.text();
                if !text.trim().is_empty() {
                    lines.push(SourceLine {
                        page: page.id,
                        number: index + 1,
                        layer,
                        text,
                        translation: block.translation.as_ref().map(|text| text.content.text()),
                    });
                }
            }
        }
        Ok(lines)
    }

    pub fn check_glossary(&self, global: &[GlossaryEntry]) -> crate::Result<Vec<TermIssue>> {
        let mut issues = Vec::new();
        let glossary: Vec<_> = self
            .effective_glossary(global)
            .into_iter()
            .cloned()
            .collect();
        for line in self.transcript()? {
            let Content::Block(block) = &self.layer(line.layer)?.content else {
                continue;
            };
            let original = self.keeps_original(global, line.layer, &line.text);
            let terms = if original {
                Vec::new()
            } else {
                matching_terms(&glossary, line.layer, &line.text)
            };
            let relevant = glossary
                .iter()
                .filter(|entry| {
                    !entry.is_term() && entry.applies_to(line.layer) && entry.matches(&line.text)
                })
                .chain(terms);
            for entry in relevant {
                let failed = match (entry.kind, &line.translation) {
                    (GlossaryKind::PreserveBlock, translation) => {
                        translation.is_some() || block.cleanup.visible
                    }
                    (_, _) if original => false,
                    (GlossaryKind::Preserve, Some(text)) => {
                        entry.occurrences(text).count() < entry.occurrences(&line.text).count()
                    }
                    (_, Some(text)) => matches(entry.translation.trim(), text).next().is_none(),
                    (_, None) => false,
                };
                if failed {
                    issues.push(TermIssue {
                        page: line.page,
                        number: line.number,
                        layer: line.layer,
                        source: entry.source.clone(),
                        translation: line.translation.clone(),
                        expected: if entry.is_term() {
                            entry.translation.clone()
                        } else {
                            entry.source.clone()
                        },
                        kind: entry.kind,
                    });
                }
            }
        }
        Ok(issues)
    }
}

pub(crate) fn valid_candidates(groups: &[Vec<GlossaryEntry>]) -> bool {
    let mut sources = BTreeSet::new();
    groups.iter().all(|group| {
        let Some(first) = group.first() else {
            return false;
        };
        sources.insert(&first.source)
            && group.iter().enumerate().all(|(index, entry)| {
                entry.source == first.source
                    && valid_glossary(std::slice::from_ref(entry))
                    && !group[..index].iter().any(|other| {
                        other.kind == entry.kind && other.translation == entry.translation
                    })
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longer_names_and_protected_phrases_take_priority() {
        let entry = |source: &str, translation: &str, kind| GlossaryEntry {
            source: source.to_owned(),
            translation: translation.to_owned(),
            kind,
            note: String::new(),
            wiki_url: None,
            excluded_layers: BTreeSet::new(),
        };
        let entries = [
            entry("青木", "Aoki", GlossaryKind::Person),
            entry("青木山", "Mount Aoki", GlossaryKind::Place),
            entry("青木印", "", GlossaryKind::Preserve),
            entry("Cat", "猫", GlossaryKind::Term),
        ];
        let layer = LayerId(crate::Uuid::nil());
        let terms = |text| {
            matching_terms(&entries, layer, text)
                .into_iter()
                .map(|entry| entry.source.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(terms("青木山で青木印を押す"), ["青木山"]);
        assert_eq!(terms("青木山に青木が着いた"), ["青木", "青木山"]);
        assert!(terms("Cathedral").is_empty());
        assert_eq!(terms("Cat!"), ["Cat"]);
    }
}
