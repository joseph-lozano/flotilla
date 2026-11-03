//! Files to send with a prompt: the paperclip's file picker, files dropped on a composer, and
//! the tiles that show them (in the composer, and on sent prompts).

use crate::fmt;
use flotilla_model::attach;
use flotilla_proto::{Attached, Kind};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::{ActiveTheme, Icon, Sizable, Theme, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::path::{Path, PathBuf};

/// The files waiting to go with the next prompt, and why the last one added was left out.
#[derive(Default)]
pub struct Pending {
    pub files: Vec<Attached>,
    pub error: Option<String>,
}

impl Pending {
    pub fn add(&mut self, paths: &[PathBuf]) {
        self.error = attach::add(&mut self.files, paths);
    }

    pub fn remove(&mut self, i: usize) {
        if i < self.files.len() {
            self.files.remove(i);
        }
        self.error = None;
    }

    pub fn clear(&mut self) {
        self.files.clear();
        self.error = None;
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Open the file picker; `then` gets the files chosen. One that can't open says why (on Linux
/// it's the desktop portal's, which may not be running).
pub fn pick<V: 'static>(
    window: &mut Window,
    cx: &mut Context<V>,
    then: impl FnOnce(&mut V, Vec<PathBuf>, &mut Context<V>) + 'static,
) {
    let chosen = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: true,
        prompt: None,
    });
    cx.spawn_in(window, async move |this, cx| match chosen.await {
        Ok(Ok(Some(paths))) => {
            let _ = this.update(cx, |v, cx| then(v, paths, cx));
        }
        Ok(Err(e)) => {
            let note = gpui_kit::component::notification::Notification::error(e.to_string())
                .title("Couldn't open the file picker");
            let _ = cx.update(|window, cx| window.push_notification(note, cx));
        }
        _ => {}
    })
    .detach();
}

/// The paperclip button.
pub fn button(
    id: &'static str,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .icon(IconName::Paperclip)
        .tooltip("Attach files")
        .on_click(on_click)
}

type OnClick = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// How tall a file shows; an image's thumbnail is this square.
const TILE: f32 = 56.;

/// The icon for a file that shows no thumbnail.
pub fn icon(kind: Kind) -> IconName {
    match kind {
        Kind::Image => IconName::FileImage,
        Kind::Text => IconName::FileText,
        Kind::Code => IconName::FileCode,
        Kind::Data => IconName::FileBraces,
        Kind::Sheet => IconName::FileSpreadsheet,
        Kind::Archive => IconName::FileArchive,
        Kind::Video => IconName::FileVideoCamera,
        Kind::Audio => IconName::FileMusic,
        Kind::Binary => IconName::FileDigit,
        Kind::Other => IconName::File,
    }
}

/// The line under a file's name: its extension ("PDF"), or what it holds when it has none.
fn label(name: &str, kind: Kind) -> String {
    match Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_uppercase())
    {
        Some(ext) if !ext.is_empty() => ext,
        _ => match kind {
            Kind::Text => "Text",
            Kind::Binary => "Binary",
            _ => "File",
        }
        .into(),
    }
}

/// One file, still to send or sent: an image as its thumbnail, anything else as its type's
/// icon, name and size. `file` is the file on this Mac, if it was sent from here: it opens on
/// a click. × takes it off.
pub fn tile(
    id: SharedString,
    name: &str,
    file: Option<&Attached>,
    on_remove: Option<OnClick>,
    theme: &Theme,
) -> AnyElement {
    // Files sent before kinds were kept go by their names.
    let kind = match file.map(|f| f.kind) {
        Some(k) if k != Kind::Other => k,
        _ => Kind::of_name(name),
    };
    let size = file.map(|f| f.size);
    let tip = SharedString::from(match size {
        Some(s) => format!("{name} · {}", fmt::bytes(s)),
        None => name.to_string(),
    });
    let path = file.map(|f| f.path.clone());
    let base = div()
        .id(id.clone())
        .relative()
        .flex_none()
        .h(px(TILE))
        .rounded(px(8.))
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .tooltip(move |window, cx| {
            gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
        })
        .when_some(path.clone(), |el, p| {
            el.cursor_pointer().on_click(move |_, _, cx| {
                if p.exists() {
                    cx.open_with_system(&p)
                }
            })
        });
    let muted = theme.muted_foreground;
    let placeholder = move || {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(
                Icon::new(IconName::FileImage)
                    .size(px(20.))
                    .text_color(muted),
            )
            .into_any_element()
    };
    let body = match path.filter(|_| kind == Kind::Image) {
        Some(p) => base.w(px(TILE)).child(
            img(p)
                .size_full()
                .rounded(px(7.))
                .object_fit(ObjectFit::Cover)
                .with_loading(placeholder)
                .with_fallback(placeholder),
        ),
        None => {
            base.max_w(px(220.))
                .flex()
                .items_center()
                .gap_2()
                .pl(px(9.))
                .pr(px(if on_remove.is_some() { 26. } else { 12. }))
                .child(
                    div()
                        .flex_none()
                        .size(px(36.))
                        .rounded(px(6.))
                        .bg(theme.muted)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::new(icon(kind))
                                .size(px(18.))
                                .text_color(theme.muted_foreground),
                        ),
                )
                .child(
                    v_flex()
                        .min_w_0()
                        .text_xs()
                        .child(div().truncate().child(name.to_string()))
                        .child(div().truncate().text_color(theme.muted_foreground).child(
                            match size {
                                Some(s) => format!("{} · {}", label(name, kind), fmt::bytes(s)),
                                None => label(name, kind),
                            },
                        )),
                )
        }
    };
    body.children(on_remove.map(|f| {
        div()
            .id(SharedString::from(format!("{id}-remove")))
            .absolute()
            .top(px(4.))
            .right(px(4.))
            .size(px(16.))
            .rounded_full()
            .flex()
            .items_center()
            .justify_center()
            // A dark badge with a light ring, to show on any thumbnail in either theme.
            .bg(hsla(0., 0., 0.15, 0.85))
            .border_1()
            .border_color(hsla(0., 0., 1., 0.85))
            .cursor_pointer()
            .child(Icon::new(IconName::X).size(px(9.)).text_color(white()))
            .tooltip(|window, cx| {
                gpui_kit::component::tooltip::Tooltip::new("Remove").build(window, cx)
            })
            .on_click(move |e, window, cx| {
                cx.stop_propagation();
                f(e, window, cx)
            })
    }))
    .into_any_element()
}

/// The files still to send, each with ×, and why the last one added was left out.
pub fn pending_row<V: 'static>(
    pending: &Pending,
    cx: &mut Context<V>,
    remove: fn(&mut V, usize, &mut Context<V>),
) -> Option<AnyElement> {
    if pending.files.is_empty() && pending.error.is_none() {
        return None;
    }
    let theme = cx.theme().clone();
    let tiles = pending.files.iter().enumerate().map(|(i, f)| {
        let on_remove = cx.listener(move |this, _: &ClickEvent, _, cx| {
            remove(this, i, cx);
            cx.notify();
        });
        tile(
            SharedString::from(format!("attached-{i}")),
            &f.name,
            Some(f),
            Some(Box::new(on_remove)),
            &theme,
        )
    });
    Some(
        h_flex()
            .flex_wrap()
            .gap_1p5()
            .children(tiles)
            .children(
                pending
                    .error
                    .clone()
                    .map(|e| div().text_xs().text_color(theme.danger).child(e)),
            )
            .into_any_element(),
    )
}
