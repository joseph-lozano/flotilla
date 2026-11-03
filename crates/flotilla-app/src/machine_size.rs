//! The machine size menu (boat's small, default, large and xlarge VMs), in New thread's bar
//! and a project's settings.

use flotilla_proto::{MACHINE_TYPES, machine_type_label};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Sizable};
use gpui_kit::*;
use std::rc::Rc;

/// Why the plan won't start size `t` (boat's name; `None`: its default), if it won't.
/// `unavailable`: what boat's limits say (`Usage::sizes_unavailable`).
pub fn unavailable<'a>(unavailable: &'a [(String, String)], t: Option<&str>) -> Option<&'a str> {
    let t = t.unwrap_or("default");
    unavailable
        .iter()
        .find(|(u, _)| u == t)
        .map(|(_, why)| why.as_str())
}

/// A ghost button showing `current` (boat's name; `None`: its default) that opens the sizes,
/// those the plan won't start (`unavailable`) greyed out with why; `pick` gets boat's name
/// for the one chosen.
pub fn menu(
    id: &'static str,
    current: Option<&str>,
    unavailable_sizes: &[(String, String)],
    disabled: bool,
    pick: impl Fn(String, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let current = current.unwrap_or("default").to_string();
    let pick = Rc::new(pick);
    let unavailable_sizes = unavailable_sizes.to_vec();
    Button::new(id)
        .ghost()
        .xsmall()
        .icon(IconName::Cpu)
        .label(machine_type_label(Some(&current)).to_string())
        .tooltip("Machine size")
        .disabled(disabled)
        .dropdown_caret(true)
        .dropdown_menu(move |menu, _, _| {
            MACHINE_TYPES.iter().fold(menu, |menu, (t, label, detail)| {
                let pick = pick.clone();
                let why = unavailable(&unavailable_sizes, Some(t));
                menu.item(
                    PopupMenuItem::new(format!("{label}  ·  {}", why.unwrap_or(detail)))
                        .checked(current == *t)
                        .disabled(why.is_some())
                        .on_click(move |_, window, cx| pick(t.to_string(), window, cx)),
                )
            })
        })
}
