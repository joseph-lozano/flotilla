//! What a client works out from the wire types without a database or the network: the
//! prompts and labels the UI shows, boat's limits, the text hand-offs are written in and read
//! back from, and the files on the device it runs on (attachments and skills it picks). A
//! client needs only this, flotilla-proto and flotilla-client; flotilla-core re-exports each
//! item where it used to live, for the server's own code.
//!
//! serde, chrono, jiff (scheduled prompts' zones) and std only: no SQLite, HTTP or GPUI.

pub mod agents;
pub mod attach;
pub mod devices;
pub mod fix;
pub mod flavor;
pub mod github;
pub mod lifecycle;
pub mod limits;
pub mod lineage;
pub mod mentions;
pub mod normalize;
pub mod orchestrate;
pub mod schedule;
pub mod secrets;
pub mod setup;
pub mod side;
pub mod skills;
pub mod spend;
pub mod subagent;
pub mod summarize;
pub mod tidy;
pub mod util;
