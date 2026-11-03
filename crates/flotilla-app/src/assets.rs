//! The app's assets: provider logos (from svgl) on top of gpui-kit's Lucide icons.

use gpui_kit::{AssetSource, Result, SharedString};
use std::borrow::Cow;

macro_rules! provider_logos {
    ($($name:literal),* $(,)?) => {
        &[$((concat!("providers/", $name, ".svg"), include_bytes!(concat!("../assets/providers/", $name, ".svg")) as &[u8])),*]
    };
}

const PROVIDER_LOGOS: &[(&str, &[u8])] = provider_logos![
    "claude-code",
    "codex-light",
    "codex-dark",
    "mistral",
    "kimi",
    "opencode-light",
    "opencode-dark",
    "pi-light",
    "pi-dark",
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = PROVIDER_LOGOS.iter().find(|(p, _)| *p == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit::assets::AllAssets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut out = gpui_kit::assets::AllAssets.list(path)?;
        out.extend(
            PROVIDER_LOGOS
                .iter()
                .filter(|(p, _)| p.starts_with(path))
                .map(|(p, _)| SharedString::from(*p)),
        );
        Ok(out)
    }
}

/// The logo for a provider key, picked for the current appearance. `None` means monogram.
pub fn provider_logo(provider: &str, dark: bool) -> Option<&'static str> {
    Some(match (provider, dark) {
        ("claude-code", _) => "providers/claude-code.svg",
        ("codex", false) => "providers/codex-light.svg",
        ("codex", true) => "providers/codex-dark.svg",
        ("mistral", _) => "providers/mistral.svg",
        ("kimi", _) => "providers/kimi.svg",
        ("opencode", false) => "providers/opencode-light.svg",
        ("opencode", true) => "providers/opencode-dark.svg",
        ("pi", false) => "providers/pi-light.svg",
        ("pi", true) => "providers/pi-dark.svg",
        _ => return None,
    })
}
