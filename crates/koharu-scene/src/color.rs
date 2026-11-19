//! Colours as `[r, g, b, a]`, as Figma's `Color` is. `color`'s own serde writes
//! `{"components": [...]}` and has no schemars or specta support, so layers
//! hold the components and convert at the edge.

use color::{AlphaColor, Srgb};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use specta::Type;

/// An sRGB colour with straight alpha.
pub type Color = AlphaColor<Srgb>;

/// An sRGB colour with straight alpha, each component from 0 to 1.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Type, JsonSchema)]
#[serde(transparent)]
pub struct Rgba(pub [f32; 4]);

impl From<Rgba> for Color {
    fn from(Rgba(components): Rgba) -> Self {
        Self::new(components)
    }
}

impl From<Color> for Rgba {
    fn from(color: Color) -> Self {
        Self(color.components)
    }
}

/// A reader's decision about the detector's ink. The source measurement remains evidence.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, Type, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InkChoice {
    Keep,
    Black,
    White,
}

impl InkChoice {
    pub(crate) fn replacement(self, detected: Rgba) -> Option<Rgba> {
        let level = match self {
            Self::Keep => return None,
            Self::Black => 0.0,
            Self::White => 1.0,
        };
        Some(Rgba([level, level, level, detected.0[3]]))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type, JsonSchema)]
pub struct InkSuggestion {
    pub location: crate::TermLocation,
    pub hidden: bool,
    pub block_number: Option<usize>,
    pub detected: Rgba,
    pub translation_fill: Option<Rgba>,
}

/// Channel cutoffs are inclusive sRGB bytes. Alpha is retained.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(deny_unknown_fields)]
pub struct InkNormalization {
    pub black_max: u8,
    pub white_min: u8,
    pub black: Rgba,
    pub white: Rgba,
}

impl InkNormalization {
    fn mapped(&self, current: Rgba) -> Option<Rgba> {
        let [r, g, b, alpha] = current.0;
        let rgb = [r, g, b].map(|c| (c * 255.0).round());
        let target = if rgb.iter().all(|c| *c <= f32::from(self.black_max)) {
            self.black
        } else if rgb.iter().all(|c| *c >= f32::from(self.white_min)) {
            self.white
        } else {
            return None;
        };
        Some(Rgba([target.0[0], target.0[1], target.0[2], alpha]))
    }
}

impl Default for InkNormalization {
    fn default() -> Self {
        Self {
            black_max: 24,
            white_min: 231,
            black: Rgba([0., 0., 0., 1.]),
            white: Rgba([1.; 4]),
        }
    }
}

impl crate::Layer {
    pub fn detected_ink(&self) -> Option<Rgba> {
        let crate::Content::Block(b) = &self.content else {
            return None;
        };
        self.attributes.get("c2pa")?.get("detection")?;
        b.source.style.chars.fill
    }

    fn ink_override(&self) -> Option<Rgba> {
        serde_json::from_value(self.attributes.get("koharu")?.get("ink_override")?.clone()).ok()
    }

    fn ink_choice(&self, detected: Rgba) -> Option<InkChoice> {
        let value = self.attributes.get("koharu")?.get("ink_review")?;
        let (reviewed, choice): (Rgba, InkChoice) = serde_json::from_value(value.clone()).ok()?;
        (reviewed == detected).then_some(choice)
    }

    /// A correction made before translation also sets its initial fill.
    pub fn reviewed_ink(&self, detected: Rgba) -> Rgba {
        self.ink_override()
            .or_else(|| {
                self.ink_choice(detected)
                    .and_then(|choice| choice.replacement(detected))
            })
            .unwrap_or(detected)
    }

    pub(crate) fn pending_ink(&self) -> Option<(Rgba, Option<Rgba>)> {
        let crate::Content::Block(block) = &self.content else {
            return None;
        };
        let detected = self.detected_ink()?;
        let translation = block.translation.as_ref();
        let fill = translation.and_then(|t| t.style.chars.fill);
        if InkNormalization::default().mapped(detected).is_some()
            || self.ink_choice(detected).is_some()
            || self.ink_override().is_some()
            || fill.is_some_and(|c| c != detected)
            || translation.is_some_and(|t| t.content.0.iter().any(|s| s.attributes.fill.is_some()))
        {
            return None;
        }
        Some((detected, fill))
    }
}

impl crate::Project {
    /// A single undo step for every eligible block, using the untouched detector
    /// measurement to restore both the paragraph fill and any character overrides.
    pub fn normalize_ink(
        &self,
        settings: Option<&InkNormalization>,
    ) -> crate::Result<(usize, Vec<crate::Op>)> {
        use crate::Op;
        use serde_json::json;
        if let Some(s) = settings
            && (s.black_max >= s.white_min
                || s.black
                    .0
                    .iter()
                    .chain(&s.white.0)
                    .any(|c| !(0.0..=1.0).contains(c)))
        {
            return Err(crate::Error::Rejected(
                "invalid ink thresholds or target color".into(),
            ));
        }
        let mut ops = Vec::new();
        let mut changed = 0;
        for page in self.pages().filter(|p| !p.excluded) {
            for layer in self.layers_of(page.id)? {
                let Some(detected) = layer.detected_ink() else {
                    continue;
                };
                let crate::Content::Block(block) = &layer.content else {
                    continue;
                };
                if self.locked_at(layer.id).is_some() {
                    continue;
                }
                let text = block.translation.as_ref();
                let fill = text
                    .and_then(|t| t.style.chars.fill)
                    .unwrap_or_else(|| layer.reviewed_ink(detected));
                let replacement = |color| match settings {
                    Some(s) => s.mapped(color),
                    None => Some(detected),
                };
                let target = replacement(fill);
                let next = target.unwrap_or(fill);
                let start = ops.len();
                if target.is_some() && text.is_some_and(|t| t.style.chars.fill != target) {
                    ops.push(Op::UpdateLayer {
                        layer: layer.id.into(),
                        patch: json!({"translation":{"style":{"chars":{"fill":next}}}}),
                    });
                }
                if let Some(text) = text
                    && text.content.0.iter().any(|span| {
                        span.attributes.fill.is_some_and(|f| {
                            settings.is_none() || replacement(f).is_some_and(|c| c != f)
                        })
                    })
                {
                    let mut spans = text.content.0.clone();
                    for span in &mut spans {
                        span.attributes.fill = span
                            .attributes
                            .fill
                            .map(|f| replacement(f).unwrap_or(f))
                            .filter(|c| *c != next);
                    }
                    ops.push(Op::SetRichText {
                        layer: layer.id.into(),
                        spans,
                    });
                }
                if next != fill
                    || ops.len() != start
                    || (settings.is_none() && layer.reviewed_ink(detected) != detected)
                {
                    ops.push(Op::SetAttribute {
                        layer: layer.id.into(),
                        namespace: "koharu".into(),
                        key: "ink_override".into(),
                        value: Some(json!(next)),
                    });
                    changed += 1;
                }
            }
        }
        Ok((changed, ops))
    }

    pub fn ink_suggestions(
        &self,
        global: &[crate::GlossaryEntry],
    ) -> crate::Result<Vec<InkSuggestion>> {
        let mut pending = Vec::new();
        for (i, page) in self.pages().enumerate() {
            let numbers: std::collections::HashMap<_, _> = self
                .story(page.id)?
                .into_iter()
                .enumerate()
                .map(|(i, id)| (id, i + 1))
                .collect();
            for layer in self.layers_of(page.id)? {
                let Some((detected, translation_fill)) = layer.pending_ink() else {
                    continue;
                };
                let crate::Content::Block(block) = &layer.content else {
                    continue;
                };
                pending.push(InkSuggestion {
                    location: crate::TermLocation {
                        page: page.id,
                        number: i + 1,
                        layer: layer.id,
                        text: block.source.content.text(),
                    },
                    hidden: page.excluded
                        || !numbers.contains_key(&layer.id)
                        || self.keeps_original(global, layer.id, &block.source.content.text())
                        || layer
                            .attributes
                            .get("koharu")
                            .is_some_and(|a| a.contains_key("translation_unchanged"))
                            && block.translation.is_none(),
                    block_number: numbers.get(&layer.id).copied(),
                    detected,
                    translation_fill,
                });
            }
        }
        Ok(pending)
    }
}
