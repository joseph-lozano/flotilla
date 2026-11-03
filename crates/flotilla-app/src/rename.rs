//! The rename dialog's body: the name field, and a button that asks the preview model for a
//! name from the thread's latest prompts and replies (`thread.suggest_name`).

use gpui_kit::assets::IconName;
use gpui_kit::component::button::Button;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme, Disableable as _, h_flex, v_flex};
use gpui_kit::*;

/// Generate was pressed: the app asks for a name, and hands it back with `generated`.
pub struct Generate;

pub struct RenameView {
    pub title: Entity<InputState>,
    /// The preview model; empty when titles and previews are off.
    model: String,
    generating: bool,
    error: Option<String>,
}

impl EventEmitter<Generate> for RenameView {}

impl RenameView {
    pub fn new(
        current: String,
        model: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = cx.new(|cx| InputState::new(window, cx).default_value(current));
        RenameView {
            title,
            model: model.trim().to_string(),
            generating: false,
            error: None,
        }
    }

    pub(crate) fn generate(&mut self, cx: &mut Context<Self>) {
        if self.generating || self.model.is_empty() {
            return;
        }
        self.generating = true;
        self.error = None;
        cx.emit(Generate);
        cx.notify();
    }

    /// The name asked for, or why there isn't one.
    pub(crate) fn generated(
        &mut self,
        result: Result<flotilla_proto::Reply, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.generating = false;
        match result {
            Ok(flotilla_proto::Reply::Name(name)) => self.title.update(cx, |t, cx| {
                t.set_value(name, window, cx);
                t.focus(window, cx);
            }),
            Ok(_) => {}
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }
}

impl Render for RenameView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let note = match &self.error {
            Some(e) => Some((format!("Couldn't generate a name: {e}"), theme.danger)),
            None if self.model.is_empty() => Some((
                "Needs Titles and previews in Settings".to_string(),
                theme.muted_foreground,
            )),
            None => None,
        };
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Input::new(&self.title)))
                    .child(
                        Button::new("generate-name")
                            .outline()
                            .icon(IconName::Sparkles)
                            .label("Generate")
                            .loading(self.generating)
                            .disabled(self.model.is_empty())
                            .on_click(cx.listener(|v, _, _, cx| v.generate(cx))),
                    ),
            )
            .children(note.map(|(note, color)| div().text_xs().text_color(color).child(note)))
    }
}
