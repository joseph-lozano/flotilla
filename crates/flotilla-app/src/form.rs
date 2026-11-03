//! Dialogs that hold a form. A dialog takes focus as it opens, and binds Enter to its own
//! confirm, which closes it; a one-line field passes Enter on to it. So a form opened with no
//! field to type in, and Enter in a field closed it without doing what it's for. A form's
//! dialog here gives its first field focus once it's open, and Enter does what the form's main
//! button does (or nothing), never closing the dialog by itself, so an error stays in view.
//! The form closes it when it's done.

use gpui_kit::component::WindowExt as _;
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::input::InputState;
use gpui_kit::*;
use std::rc::Rc;

/// Open `build`'s dialog with `first` focused (None: a form with no field to start in), Enter
/// running `enter`.
pub fn open(
    window: &mut Window,
    cx: &mut App,
    first: Option<Entity<InputState>>,
    enter: impl Fn(&mut Window, &mut App) + 'static,
    build: impl Fn(Dialog, &mut Window, &mut App) -> Dialog + 'static,
) {
    let enter = Rc::new(enter);
    window.open_dialog(cx, move |dialog, window, cx| {
        let enter = enter.clone();
        build(dialog, window, cx).on_ok(move |_, window, cx| {
            enter(window, cx);
            false
        })
    });
    // After: opening it focused the dialog itself.
    if let Some(first) = first {
        #[cfg(feature = "snapshot")]
        FIRST.with(|f| *f.borrow_mut() = Some(first.focus_handle(cx)));
        first.update(cx, |f, cx| f.focus(window, cx));
    }
}

#[cfg(feature = "snapshot")]
thread_local! {
    /// The field the last form opened with, for the harness to check it has focus.
    pub static FIRST: std::cell::RefCell<Option<FocusHandle>> = const { std::cell::RefCell::new(None) };
}
