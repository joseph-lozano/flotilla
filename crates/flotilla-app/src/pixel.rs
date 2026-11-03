//! Soft pixel accents after the app icon: its stepped sail over a hull, the three-sail mark,
//! and square status pips. Drawn from exact cell grids (one run of cells per row), never as
//! images, and used sparingly: empty states, the new-thread page, status marks. See DESIGN.md.

use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// The icon's three sail colours. Brand art, the same in Day and Night; never for text.
pub const CORAL: u32 = 0xF37173;
pub const AMBER: u32 = 0xF6BC63;
pub const LAVENDER: u32 = 0xC2A7E2;

/// One sail as the icon draws it: a right triangle against its mast, stepping down one cell a
/// row, a cell of water, then a hull reaching past the mast. 8 cells wide, 9 tall.
const SAIL: [(u8, u8); 9] = [
    (2, 1),
    (2, 2),
    (2, 3),
    (2, 4),
    (2, 5),
    (2, 6),
    (0, 0),
    (0, 8),
    (0, 8),
];

/// A sail in `color`, `cell` wide per grid cell.
pub fn sail(color: Hsla, cell: f32) -> Div {
    v_flex()
        .flex_none()
        .children(SAIL.iter().map(move |&(start, len)| {
            div().h(px(cell)).child(
                div()
                    .ml(px(start as f32 * cell))
                    .w(px(len as f32 * cell))
                    .h_full()
                    .when(len > 0, |el| el.bg(color)),
            )
        }))
}

/// The icon's flotilla: coral, amber (riding two cells higher) and lavender.
pub fn mark(cell: f32) -> Div {
    let boat = |color: u32, lift: f32| {
        div()
            .pb(px(lift * cell))
            .child(sail(rgb(color).into(), cell))
    };
    h_flex()
        .flex_none()
        .items_end()
        .gap(px(2. * cell))
        .child(boat(CORAL, 0.))
        .child(boat(AMBER, 2.))
        .child(boat(LAVENDER, 0.))
}

/// A status pip: a square with softened corners, the pixel take on a status dot.
pub fn pip(color: Hsla, size: f32) -> Div {
    div()
        .flex_none()
        .size(px(size))
        .rounded(px(size / 4.))
        .bg(color)
}
