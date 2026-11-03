//! What a thread's messages say in place of what boat doesn't keep (`flotilla_core::normalize`
//! turns boat's events into messages).

/// Shown in place of a turn's prompt when boat replaced it with its steer wrapper and this
/// machine didn't send it (decision a).
pub const PROMPT_NOT_KEPT: &str = "(original prompt not kept by boat)";

/// The status of a message boat never got.
pub const NOT_SENT: &str = "not sent";
