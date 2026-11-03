//! Flotilla's core: the boat client, the SQLite cache, the sync engine and the thread
//! lifecycle. No GPUI dependency; the UI reads SQLite and calls [`lifecycle::Core`].

pub mod agents;
pub mod asks;
pub mod attach;
pub mod blobs;
pub mod boat;
pub mod catalog;
pub mod devices;
pub mod doctor;
pub mod error;
pub mod event;
pub mod github;
pub mod lifecycle;
pub mod lineage;
pub mod lock;
pub mod mentions;
pub mod meter;
pub mod normalize;
pub mod orchestrate;
pub mod owner;
pub mod prices;
pub mod projects;
pub mod provision;
pub mod public;
pub mod push;
pub mod remote_mcp;
pub mod search;
pub mod secrets;
pub mod services;
pub mod setup;
pub mod side;
pub mod skills;
pub mod spend;
pub mod store;
pub mod summarize;
pub mod sync;
pub mod tailscale;
pub mod tidy;
pub mod tools;

// Moved whole to flotilla-model; kept here at their old paths.
pub use flotilla_model::{fix, flavor, subagent, util};

pub use error::{ApiError, BoatError, Error, Result};
pub use lifecycle::Core;
