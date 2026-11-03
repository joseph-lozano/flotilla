//! The model catalog (`GET https://boat.dev/api/provider-models`) and settings validation, now
//! in flotilla-proto (a client checks settings too).

pub use flotilla_proto::{Catalog, EffortOptions, Model, Provider, ThreadSettings};

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        let raw = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../m0-samples/provider-models.json"
        ))
        .unwrap();
        Catalog::from_value(&serde_json::from_str(&raw).unwrap()).unwrap()
    }

    #[test]
    fn validates_settings() {
        let c = catalog();
        let s = c
            .validate("claude-code", "sonnet", Some("low"), false)
            .unwrap();
        assert_eq!(s.effort.as_deref(), Some("low"));
        // Default effort is filled in so it's locked explicitly.
        assert!(
            c.validate("claude-code", "sonnet", None, false)
                .unwrap()
                .effort
                .is_some()
        );
        assert!(
            c.validate("claude-code", "sonnet", Some("turbo"), false)
                .is_err()
        );
        assert!(c.validate("claude-code", "nope", None, false).is_err());
        assert!(
            c.validate("opencode", "claude-sonnet-5", Some("low"), false)
                .is_err()
        );
        assert_eq!(
            c.validate("opencode", "claude-sonnet-5", None, false)
                .unwrap()
                .effort,
            None
        );
        assert!(c.validate("claude-code", "opus-5-5", None, true).is_ok());
        assert!(
            c.validate("claude-code", "claude-fable-5-1", None, true)
                .is_err()
        );
    }
}
