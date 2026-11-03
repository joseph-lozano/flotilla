//! boat's model catalog (`GET https://boat.dev/api/provider-models`) and checking a thread's
//! settings against it.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Catalog {
    pub providers: Vec<Provider>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Provider {
    /// Canonical key, e.g. `claude-code`. This is what prompts and labels carry.
    pub key: String,
    pub cli_name: Option<String>,
    pub aliases: Vec<String>,
    pub default_model: Option<String>,
    pub models: Vec<Model>,
    /// Where boat lists it (lowest first).
    pub order: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Model {
    /// Sent as `model`. Response events report `sdk_id` instead.
    pub id: String,
    pub label: Option<String>,
    pub sdk_id: Option<String>,
    pub fast_mode: bool,
    pub effort: Option<EffortOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EffortOptions {
    pub supported: Vec<String>,
    pub default: Option<String>,
}

/// A validated, canonical set of the four settings a thread locks at creation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ThreadSettings {
    pub provider: String,
    pub model: String,
    pub effort: Option<String>,
    pub fast: bool,
}

impl Catalog {
    pub fn from_value(v: &Value) -> Result<Catalog, String> {
        let obj = v
            .as_object()
            .ok_or_else(|| "model catalog is not an object".to_string())?;
        let s = |v: Option<&Value>| v.and_then(Value::as_str).map(str::to_string);
        let mut providers = Vec::new();
        for (key, p) in obj {
            // Skip envelope-ish fields if boat ever adds them.
            let Some(models) = p.get("models").and_then(Value::as_array) else {
                continue;
            };
            let cli = p.get("cli");
            providers.push(Provider {
                key: key.clone(),
                cli_name: s(cli.and_then(|c| c.get("name"))),
                aliases: cli
                    .and_then(|c| c.get("aliases"))
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default(),
                default_model: s(p.get("default")),
                order: cli
                    .and_then(|c| c.get("order"))
                    .and_then(Value::as_i64)
                    .unwrap_or(i64::MAX),
                models: models
                    .iter()
                    .filter_map(|m| {
                        Some(Model {
                            id: s(m.get("id"))?,
                            label: s(m.get("label")),
                            sdk_id: s(m.get("sdkId")),
                            fast_mode: m.get("fastMode").and_then(Value::as_bool).unwrap_or(false),
                            effort: m.get("reasoningEffort").filter(|e| e.is_object()).map(|e| {
                                EffortOptions {
                                    supported: e
                                        .get("supported")
                                        .and_then(Value::as_array)
                                        .map(|a| {
                                            a.iter()
                                                .filter_map(Value::as_str)
                                                .map(str::to_string)
                                                .collect()
                                        })
                                        .unwrap_or_default(),
                                    default: s(e.get("default")),
                                }
                            }),
                        })
                    })
                    .collect(),
            });
        }
        providers.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.key.cmp(&b.key)));
        Ok(Catalog { providers })
    }

    /// Find a provider by canonical key, CLI name or alias.
    pub fn provider(&self, name: &str) -> Option<&Provider> {
        self.providers.iter().find(|p| p.key == name).or_else(|| {
            self.providers.iter().find(|p| {
                p.cli_name.as_deref() == Some(name) || p.aliases.iter().any(|a| a == name)
            })
        })
    }

    /// Check the settings against the catalog and return them in canonical form. A model with
    /// effort levels gets its default when none is given, so the effort is always locked
    /// explicitly; a model without them takes none.
    pub fn validate(
        &self,
        provider: &str,
        model: &str,
        effort: Option<&str>,
        fast: bool,
    ) -> Result<ThreadSettings, String> {
        let p = self.provider(provider).ok_or_else(|| {
            let keys: Vec<_> = self.providers.iter().map(|p| p.key.as_str()).collect();
            format!("unknown provider {provider:?}; known: {}", keys.join(", "))
        })?;
        let m = p.models.iter().find(|m| m.id == model).ok_or_else(|| {
            let ids: Vec<_> = p.models.iter().map(|m| m.id.as_str()).collect();
            format!(
                "{} has no model {model:?}; models: {}",
                p.key,
                ids.join(", ")
            )
        })?;
        let effort = match (&m.effort, effort) {
            (Some(opts), Some(e)) if opts.supported.iter().any(|s| s == e) => Some(e.to_string()),
            (Some(opts), Some(e)) => {
                return Err(format!(
                    "{}/{} doesn't support effort {e:?}; supported: {}",
                    p.key,
                    m.id,
                    opts.supported.join(", ")
                ));
            }
            (Some(opts), None) => opts
                .default
                .clone()
                .or_else(|| opts.supported.first().cloned()),
            (None, Some(e)) => {
                return Err(format!(
                    "{}/{} takes no effort level (got {e:?})",
                    p.key, m.id
                ));
            }
            (None, None) => None,
        };
        if fast && !m.fast_mode {
            return Err(format!("{}/{} has no fast mode", p.key, m.id));
        }
        Ok(ThreadSettings {
            provider: p.key.clone(),
            model: m.id.clone(),
            effort,
            fast,
        })
    }

    /// `model` with an effort and fast that suit it, as near `effort` and `fast` as it allows:
    /// the same level if it has it, else the nearest it has (the lower on a tie), else its
    /// default; fast only if it has fast mode. For one prompt started on several models.
    pub fn fit(
        &self,
        provider: &str,
        model: &str,
        effort: Option<&str>,
        fast: bool,
    ) -> Result<ThreadSettings, String> {
        let m = self
            .provider(provider)
            .and_then(|p| p.models.iter().find(|m| m.id == model));
        let effort = match (m.and_then(|m| m.effort.as_ref()), effort) {
            (Some(opts), Some(e)) => nearest_effort(e, &opts.supported),
            _ => None,
        };
        let fast = fast && m.is_some_and(|m| m.fast_mode);
        self.validate(provider, model, effort.as_deref(), fast)
    }
}

/// Effort levels from least to most, as boat's catalog names them.
const EFFORT_ORDER: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

/// The level of `supported` nearest `wanted` (itself if there; the lower on a tie). None when
/// either isn't a level Flotilla knows the order of: the model's default then applies.
fn nearest_effort(wanted: &str, supported: &[String]) -> Option<String> {
    if supported.iter().any(|s| s == wanted) {
        return Some(wanted.to_string());
    }
    let rank = |e: &str| EFFORT_ORDER.iter().position(|o| *o == e);
    let want = rank(wanted)? as i64;
    supported
        .iter()
        .filter_map(|s| Some((rank(s)? as i64, s)))
        .min_by_key(|(r, _)| ((r - want).abs(), *r))
        .map(|(_, s)| s.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog() -> Catalog {
        let v = serde_json::json!({
            "claude-code": {"cli": {"name": "claude", "order": 1}, "default": "opus", "models": [
                {"id": "opus", "label": "Opus 5.5", "fastMode": true, "reasoningEffort": {"supported": ["low", "medium", "high", "max"], "default": "high"}},
                {"id": "haiku", "label": "Haiku 5.5", "reasoningEffort": {"supported": ["low", "medium", "high"], "default": "medium"}}
            ]},
            "codex": {"cli": {"name": "codex", "order": 2}, "models": [
                {"id": "gpt-terra", "label": "GPT-6.1 Terra", "reasoningEffort": {"supported": ["minimal", "low", "medium", "high", "xhigh"], "default": "medium"}},
                {"id": "plain", "label": "Plain"}
            ]}
        });
        Catalog::from_value(&v).unwrap()
    }

    #[test]
    fn several_models_each_get_an_effort_and_fast_that_suit_them() {
        let c = catalog();
        let fit = |p: &str, m: &str, e: Option<&str>, fast: bool| {
            c.fit(p, m, e, fast).map(|s| (s.effort, s.fast)).unwrap()
        };
        // The same level where the model has it.
        assert_eq!(
            fit("codex", "gpt-terra", Some("high"), false),
            (Some("high".into()), false)
        );
        // Max on a model without it: the nearest it has.
        assert_eq!(
            fit("codex", "gpt-terra", Some("max"), false),
            (Some("xhigh".into()), false)
        );
        assert_eq!(
            fit("claude-code", "haiku", Some("max"), false),
            (Some("high".into()), false)
        );
        assert_eq!(
            fit("claude-code", "opus", Some("minimal"), false),
            (Some("low".into()), false)
        );
        // A level nothing knows the order of: the model's default.
        assert_eq!(
            fit("claude-code", "haiku", Some("turbo"), false),
            (Some("medium".into()), false)
        );
        // No effort asked: the default; a model without levels takes none.
        assert_eq!(
            fit("claude-code", "haiku", None, false),
            (Some("medium".into()), false)
        );
        assert_eq!(fit("codex", "plain", Some("high"), false), (None, false));
        // Fast only where the model has it.
        assert_eq!(
            fit("claude-code", "opus", Some("high"), true),
            (Some("high".into()), true)
        );
        assert_eq!(
            fit("claude-code", "haiku", Some("high"), true),
            (Some("high".into()), false)
        );
        // An unknown model is still an error.
        assert!(c.fit("codex", "nope", None, false).is_err());
    }
}
