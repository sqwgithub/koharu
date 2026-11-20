use koharu_llm::{
    Size,
    rig::{catalog::ModelSpec, driver::DynModel, operation::Completion},
};

/// A language model's room: one conversation at a time, of text unless
/// translation shows it pages.
const LANGUAGE: Size = Size {
    sequences: 1,
    context: 32_768,
    images: false,
};

/// Reserve at least half the context for the batch input and its instructions.
pub(in crate::stages) fn batch_output_limit(spec: Option<&ModelSpec>) -> u32 {
    spec.and_then(|s| s.max_output_tokens)
        .unwrap_or(8192)
        .min(16384)
        .min(spec.and_then(|s| s.context_window).unwrap_or(32768) / 2)
}

impl super::TranslatorConfig {
    pub(super) fn output_limit(&self, spec: Option<&ModelSpec>) -> Option<u32> {
        let requested = self.max_output_tokens?.get();
        Some(
            requested
                .min(spec.and_then(|s| s.max_output_tokens).unwrap_or(requested))
                .min(
                    spec.and_then(|s| s.context_window)
                        .map_or(requested, |window| window / 2),
                ),
        )
    }
}

/// The model `reference` names, connected, with its projector when it reads
/// `images`; a hosted model has no room to size.
pub(in crate::stages) async fn load(
    reference: &str,
    images: bool,
) -> anyhow::Result<DynModel<Completion>> {
    koharu_llm::connect(reference, Size { images, ..LANGUAGE }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stages::translate::TranslatorConfig;
    use koharu_llm::rig::providers::registry::ProviderId;

    #[test]
    fn configured_translation_limit_can_exceed_the_default_within_model_capacity() {
        let spec = ModelSpec::new(ProviderId::declare("test").unwrap(), "translator")
            .with_context_window(131072)
            .with_max_output_tokens(49152);
        let config: TranslatorConfig =
            serde_json::from_str(r#"{"max_output_tokens":32768}"#).unwrap();
        assert_eq!(batch_output_limit(Some(&spec)), 16384);
        assert_eq!(config.output_limit(Some(&spec)), Some(32768));
        assert_eq!(config.output_limit(None), Some(32768));
        assert_eq!(
            config.output_limit(Some(&spec.with_max_output_tokens(8192))),
            Some(8192)
        );
        assert!(serde_json::from_str::<TranslatorConfig>(r#"{"max_output_tokens":0}"#).is_err());
    }
}
