//! Archiving idle threads (`flotilla_core::tidy` does it).

/// What Settings offers for idle days (0: Never).
pub const IDLE_DAY_CHOICES: [u32; 5] = [0, 3, 7, 14, 30];
