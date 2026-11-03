//! The scripts a repo checks in under `.agents/`, which Flotilla runs at fixed points
//! (`flotilla_core::agents` runs them): their paths, and how long each gets.

use std::time::Duration;

pub const SETUP: &str = ".agents/setup";
pub const RESUME: &str = ".agents/resume";
pub const ARCHIVE: &str = ".agents/archive";
/// `.agents/setup` is stopped after this long (Amp: 20 min).
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(20 * 60);
/// `.agents/resume` gets this long; one still running then is stopped (Amp waits 10 s too).
pub const RESUME_TIMEOUT: Duration = Duration::from_secs(10);
/// `.agents/archive` gets this long: remote APIs can be slow, but archiving can't wait forever.
pub const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(2 * 60);

/// A hook Flotilla runs in a thread's VM and waits for, keeping how it went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hook {
    Resume,
    Archive,
}

impl Hook {
    pub fn path(self) -> &'static str {
        match self {
            Hook::Resume => RESUME,
            Hook::Archive => ARCHIVE,
        }
    }

    pub fn timeout(self) -> Duration {
        match self {
            Hook::Resume => RESUME_TIMEOUT,
            Hook::Archive => ARCHIVE_TIMEOUT,
        }
    }

    /// "10 s", "2 min": how long it gets, for "timed out after …".
    pub fn timeout_label(self) -> String {
        let secs = self.timeout().as_secs();
        if secs.is_multiple_of(60) {
            format!("{} min", secs / 60)
        } else {
            format!("{secs} s")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeouts_read_as_people_say_them() {
        assert_eq!(Hook::Archive.timeout_label(), "2 min");
        assert_eq!(Hook::Resume.timeout_label(), "10 s");
    }
}
