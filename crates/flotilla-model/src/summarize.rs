//! Titles and previews (`flotilla_core::summarize` writes them).

/// The summary model used when the setting has never been changed. An empty setting turns
/// summaries off. A full id rather than the `haiku` alias, which boat's catalog maps to an
/// older Haiku.
pub const DEFAULT_MODEL: &str = "claude-haiku-5-5";
