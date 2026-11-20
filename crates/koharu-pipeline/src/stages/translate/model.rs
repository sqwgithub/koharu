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

/// The model `reference` names, connected, with its projector when it reads
/// `images`; a hosted model has no room to size.
pub(in crate::stages) async fn load(
    reference: &str,
    images: bool,
) -> anyhow::Result<DynModel<Completion>> {
    koharu_llm::connect(reference, Size { images, ..LANGUAGE }).await
}
