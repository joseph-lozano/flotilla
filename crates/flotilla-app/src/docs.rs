//! Links into the docs at useflotilla.com, where a first-run screen leaves a real question:
//! where a pairing link or a boat key comes from.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Sizable as _, h_flex};
use gpui_kit::*;

/// Run a server → Pair your devices: `flotilla pair`, and Add device.
pub const PAIR: &str = "https://useflotilla.com/docs/main/server.html#pair";
/// Getting started → Connect boat.dev: the command that makes a key.
pub const BOAT_KEY: &str = "https://useflotilla.com/docs/main/getting-started.html#boat-key";

/// A small link out to the docs, with the arrow that says it opens the browser.
pub fn link(id: &'static str, label: &'static str, url: &'static str) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .child(
            h_flex()
                .gap_1()
                .child(label)
                .child(Icon::new(IconName::ArrowUpRight).xsmall()),
        )
        .on_click(move |_, _, cx| cx.open_url(url))
}

#[cfg(test)]
mod tests {
    /// Each link lands on a page the docs have, at a section that's there.
    #[test]
    fn links_land_on_sections() {
        let docs = concat!(env!("CARGO_MANIFEST_DIR"), "/../../www/docs/main/");
        for url in [super::PAIR, super::BOAT_KEY] {
            let (page, section) = url
                .strip_prefix("https://useflotilla.com/docs/main/")
                .and_then(|p| p.split_once('#'))
                .unwrap_or_else(|| panic!("{url}: a docs page and a section"));
            let html = std::fs::read_to_string(format!("{docs}{page}"))
                .unwrap_or_else(|e| panic!("{url}: {e}"));
            assert!(
                html.contains(&format!("id=\"{section}\"")),
                "{url}: no #{section}"
            );
        }
    }
}
