//! The machine size menu (boat's small, default, large and xlarge VMs), in New thread's bar
//! and a project's settings.

use flotilla_proto::{MACHINE_TYPES, machine_type_label};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::{Disableable as _, Sizable};
use gpui_kit::*;
use std::rc::Rc;

/// A ghost button showing `current` (boat's name; `None`: its default) that opens the sizes;
/// `pick` gets boat's name for the one chosen.
pub fn menu(
    id: &'static str,
    current: Option<&str>,
    disabled: bool,
    pick: impl Fn(String, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let current = current.unwrap_or("default").to_string();
    let pick = Rc::new(pick);
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
                menu.item(
                    PopupMenuItem::new(format!("{label}  ·  {detail}"))
                        .checked(current == *t)
                        .on_click(move |_, window, cx| pick(t.to_string(), window, cx)),
                )
            })
        })
}
