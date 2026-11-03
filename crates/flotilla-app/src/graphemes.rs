//! Moving and deleting in a composer a whole character at a time, as you see one (an emoji
//! family, a flag, a letter and its accent), where the text box steps through code points and
//! would split them.

use gpui_kit::component::input::TextareaState;
use gpui_kit::*;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Clone, Copy)]
pub enum Key {
    Left,
    Right,
    Backspace,
    Delete,
}

/// The character `key` steps over from `at`: the whole of it, found in the whole text (so a
/// caret left inside one, between a letter and its accent, takes all of it), if it's more than
/// one code point (one alone, the text box's own step is the same).
fn span(text: &str, at: usize, key: Key) -> Option<std::ops::Range<usize>> {
    let mut found = None;
    for (i, g) in text.grapheme_indices(true) {
        let end = i + g.len();
        let hit = match key {
            Key::Left | Key::Backspace => i < at && at <= end,
            Key::Right | Key::Delete => i <= at && at < end,
        };
        if hit {
            found = Some(i..end);
            break;
        }
    }
    let range = found?;
    (text[range.clone()].chars().count() > 1).then_some(range)
}

/// Do `key` by whole characters when the caret, with nothing selected, stands by one made of
/// several code points. Returns whether it did (the text box does the rest).
pub fn handle(
    input: &mut TextareaState,
    key: Key,
    window: &mut Window,
    cx: &mut Context<TextareaState>,
) -> bool {
    let text = input.value().to_string();
    let selected = input.selected_range();
    if !selected.is_empty() {
        // Deleting a selection that cuts a character takes all of it.
        if !matches!(key, Key::Backspace | Key::Delete) {
            return false;
        }
        let whole = widen(&text, selected.clone());
        if whole == selected {
            return false;
        }
        input.set_selected_range(whole, cx);
        input.replace("", window, cx);
        cx.notify();
        return true;
    }
    let Some(range) = span(&text, input.cursor(), key) else {
        return false;
    };
    match key {
        Key::Left => input.set_selected_range(range.start..range.start, cx),
        Key::Right => input.set_selected_range(range.end..range.end, cx),
        Key::Backspace | Key::Delete => {
            input.set_selected_range(range, cx);
            input.replace("", window, cx);
        }
    }
    cx.notify();
    true
}

/// `range` grown to the edges of the characters it cuts into.
fn widen(text: &str, range: std::ops::Range<usize>) -> std::ops::Range<usize> {
    let mut start = range.start;
    let mut end = range.end;
    for (i, g) in text.grapheme_indices(true) {
        let g_end = i + g.len();
        if i < range.start && range.start < g_end {
            start = i;
        }
        if i < range.end && range.end < g_end {
            end = g_end;
        }
    }
    start..end
}

#[cfg(test)]
mod tests {
    use super::{Key, span, widen};

    #[test]
    fn selections_widen_to_whole_characters() {
        let family = "a👨‍👩‍👧‍👦b";
        let end = family.len() - 1;
        // Shift+Left from the end of the family selects its last code point only.
        assert_eq!(widen(family, end - 4..end), 1..end);
        assert_eq!(widen(family, 0..2), 0..end);
        assert_eq!(widen("abc", 1..2), 1..2);
        assert_eq!(widen("xe\u{301}y", 2..4), 1..4);
    }

    #[test]
    fn whole_characters() {
        let family = "a👨‍👩‍👧‍👦b";
        let end = family.len() - 1;
        assert_eq!(span(family, end, Key::Backspace), Some(1..end));
        assert_eq!(span(family, end, Key::Left), Some(1..end));
        assert_eq!(span(family, 1, Key::Right), Some(1..end));
        let flag = "c🇦🇺d";
        assert_eq!(
            span(flag, flag.len() - 1, Key::Backspace),
            Some(1..flag.len() - 1)
        );
        let accent = "xe\u{301}y";
        assert_eq!(span(accent, 4, Key::Backspace), Some(1..4));
        // One code point: left to the text box.
        assert_eq!(span("abc", 2, Key::Backspace), None);
        assert_eq!(span("é", 0, Key::Delete), None);
        assert_eq!(span("", 0, Key::Left), None);
        // A caret left between a letter and its accent: the whole character either way.
        assert_eq!(span(accent, 2, Key::Backspace), Some(1..4));
        assert_eq!(span(accent, 2, Key::Delete), Some(1..4));
    }
}
