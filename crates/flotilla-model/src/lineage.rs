//! Subagents as the harness itself records them in the VM (`flotilla_core::lineage` reads
//! them there).

/// Whether `flotilla_core::lineage::read` understands this provider's records.
pub fn reads(provider: &str) -> bool {
    matches!(provider, "codex" | "claude-code" | "opencode")
}
