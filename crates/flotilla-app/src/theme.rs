//! Flotilla's own light and dark themes (Day and Night), in the app icon's plum, coral, amber
//! and lavender, in place of gpui-kit's neutral defaults. See DESIGN.md.

use gpui_kit::component::{Theme, ThemeSet};
use gpui_kit::*;
use std::rc::Rc;

const THEMES: &str = include_str!("flotilla-theme.json");

/// Make Day and Night the themes the light and dark modes load, then reload the mode showing.
pub fn install(cx: &mut App) {
    let set: ThemeSet =
        serde_json::from_str(THEMES).expect("flotilla-theme.json is a valid theme set");
    for config in set.themes {
        let theme = Theme::global_mut(cx);
        if config.mode.is_dark() {
            theme.dark_theme = Rc::new(config);
        } else {
            theme.light_theme = Rc::new(config);
        }
    }
    let mode = Theme::global(cx).mode;
    Theme::change(mode, None, cx);
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_theme_set_parses_with_a_light_and_a_dark_theme() {
        let set: gpui_kit::component::ThemeSet = serde_json::from_str(super::THEMES).unwrap();
        assert_eq!(set.themes.len(), 2);
        assert!(
            set.themes.iter().any(|t| t.mode.is_dark())
                && set.themes.iter().any(|t| !t.mode.is_dark())
        );
    }
}
