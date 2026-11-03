//! Pairing: this Mac with a server ("Connect to a server…": a link, or its address and a code),
//! and another device with the server this Mac uses (Settings → Devices → Add device: a code,
//! its link and a QR code of it).

use crate::device::SavedServer;
use flotilla_proto::PairingCode;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::*;

/// Paired: the server to use now (its token is in the Keychain, or a file off a Mac).
pub struct Paired(pub SavedServer);

pub struct ConnectView {
    /// A pairing link, or the server's address.
    pub link: Entity<InputState>,
    pub code: Entity<InputState>,
    connecting: bool,
    error: Option<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Paired> for ConnectView {}

impl ConnectView {
    pub fn new(prefill: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let link = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("flotilla://pair?u=…  or  https://box.tail1234.ts.net")
        });
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("ABCD-EFGH-JKMN"));
        // A pasted link brings its code. Enter is the dialog's (`form`): it connects.
        let subs = vec![
            cx.subscribe_in(&link, window, |this, _, event, window, cx| {
                if let InputEvent::Change = event {
                    this.take_code(window, cx);
                }
            }),
        ];
        let mut view = ConnectView {
            link,
            code,
            connecting: false,
            error: None,
            _subscriptions: subs,
        };
        if let Some(text) = prefill {
            view.link.update(cx, |l, cx| l.set_value(text, window, cx));
            view.take_code(window, cx);
        }
        view
    }

    /// Where typing starts: the code, when a link without one is filled in; else the link.
    pub fn first_field(&self, cx: &App) -> Entity<InputState> {
        if self.code.read(cx).value().is_empty() && !self.link.read(cx).value().is_empty() {
            self.code.clone()
        } else {
            self.link.clone()
        }
    }

    /// The link's code, into the code field.
    fn take_code(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.link.read(cx).value().to_string();
        if let Ok(flotilla_proto::pair::Target {
            code: Some(code), ..
        }) = flotilla_proto::pair::read(&text)
        {
            self.code.update(cx, |c, cx| {
                c.set_value(flotilla_proto::pair::display_code(&code), window, cx)
            });
        }
    }

    #[cfg(feature = "snapshot")]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Pair: swap the code for a token (off the main thread), keep the token (the Keychain, or
    /// a file only this user can read off a Mac),
    /// then say which server.
    pub fn submit(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.connecting {
            return;
        }
        let target = match flotilla_proto::pair::read(self.link.read(cx).value().as_ref()) {
            Ok(t) => t,
            Err(e) => return self.fail(e, cx),
        };
        let typed = self.code.read(cx).value().trim().to_string();
        let Some(code) = (if typed.is_empty() {
            target.code.clone()
        } else {
            Some(typed)
        }) else {
            return self.fail("Enter the pairing code too".into(), cx);
        };
        self.connecting = true;
        self.error = None;
        cx.notify();
        let server = target.server;
        let task = cx.background_executor().spawn(async move {
            let paired =
                flotilla_client::pair(&server, &code, &device_name(), std::env::consts::OS)?;
            flotilla_client::tokens::set_device_token(&paired.server.id, &paired.token.0).map_err(
                |e| {
                    if cfg!(target_os = "macos") {
                        format!("Couldn't keep this device's token in the Keychain: {e}")
                    } else {
                        format!("Couldn't keep this device's token: {e}")
                    }
                },
            )?;
            Ok::<_, String>(SavedServer {
                id: paired.server.id,
                url: server,
                device: paired.device,
            })
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |v, cx| {
                v.connecting = false;
                match result {
                    Ok(saved) => cx.emit(Paired(saved)),
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn fail(&mut self, e: String, cx: &mut Context<Self>) {
        self.error = Some(e);
        cx.notify();
    }
}

/// What this Mac is called, for the server's list of devices.
#[cfg(target_os = "macos")]
fn device_name() -> String {
    let named = |cmd: &str, args: &[&str]| {
        std::process::Command::new(cmd)
            .args(args)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|n| !n.is_empty())
    };
    named("/usr/sbin/scutil", &["--get", "ComputerName"])
        .or_else(|| named("hostname", &[]))
        .unwrap_or_else(|| "Mac".into())
}

/// What this computer is called, for the server's list of devices: its pretty name if it has
/// one (`hostnamectl set-hostname --pretty`), else its host name.
#[cfg(not(target_os = "macos"))]
fn device_name() -> String {
    let pretty = std::fs::read_to_string("/etc/machine-info")
        .ok()
        .and_then(|info| {
            info.lines()
                .find_map(|l| l.strip_prefix("PRETTY_HOSTNAME="))
                .map(|v| v.trim().trim_matches('"').to_string())
                .filter(|n| !n.is_empty())
        });
    pretty.unwrap_or_else(flotilla_model::util::machine_name)
}

impl Render for ConnectView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let label = |t: &'static str| div().text_xs().font_semibold().child(t);
        v_flex()
            .gap_2()
            // Where the link comes from; the docs' Pair your devices from the button row.
            .child(
                div()
                    .pb_1()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(
                        "Get a pairing link by running flotilla pair on your server, or from \
                         Settings → Devices on another device.",
                    ),
            )
            .child(label("Pairing link or server address"))
            .child(Input::new(&self.link))
            .child(label("Code"))
            .child(
                h_flex().child(
                    div()
                        .w(px(200.))
                        .child(Input::new(&self.code).font_family(theme.mono_font_family.clone())),
                ),
            )
            .children(
                self.error
                    .clone()
                    .map(|e| div().text_xs().text_color(theme.danger).child(e)),
            )
            .child(
                h_flex()
                    .pt_2()
                    .justify_between()
                    .child(crate::docs::link(
                        "connect-docs",
                        "How to pair",
                        crate::docs::PAIR,
                    ))
                    .child(
                        Button::new("connect")
                            .primary()
                            .label("Connect")
                            .loading(self.connecting)
                            .disabled(self.connecting)
                            .on_click(cx.listener(|v, _, window, cx| v.submit(window, cx))),
                    ),
            )
    }
}

/// A code just made for another device: the QR code to scan, the link, and the code to type.
pub struct AddDeviceView {
    code: PairingCode,
    server: String,
    qr: Option<Qr>,
}

impl AddDeviceView {
    pub fn new(code: PairingCode) -> Self {
        let qr = Qr::new(&code.link);
        let server = flotilla_proto::pair::read(&code.link)
            .map(|t| t.server)
            .unwrap_or_default();
        AddDeviceView { code, server, qr }
    }
}

impl Render for AddDeviceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mono = theme.mono_font_family.clone();
        let expires =
            flotilla_model::util::local_when(self.code.expires_ms, flotilla_model::util::now_ms());
        let link = self.code.link.clone();
        v_flex()
            .gap_3()
            .items_center()
            .children(self.qr.clone().map(|qr| qr.element(px(208.))))
            .child(
                div()
                    .text_xl()
                    .font_semibold()
                    .font_family(mono.clone())
                    .child(flotilla_proto::pair::display_code(&self.code.code)),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("for {} · expires {expires}", self.server)),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_family(mono)
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(link.clone()),
                    )
                    .child(
                        Button::new("copy-link")
                            .small()
                            .outline()
                            .icon(IconName::Copy)
                            .label("Copy link")
                            .on_click(move |_, window, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(link.clone()));
                                window.push_notification("Link copied", cx);
                            }),
                    ),
            )
    }
}

use gpui_kit::component::WindowExt as _;

/// A QR code's modules, row by row (true: dark).
#[derive(Clone)]
struct Qr {
    width: usize,
    dark: std::rc::Rc<Vec<bool>>,
}

impl Qr {
    fn new(text: &str) -> Option<Qr> {
        let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
        let dark = code
            .to_colors()
            .into_iter()
            .map(|c| c == qrcode::Color::Dark)
            .collect();
        Some(Qr {
            width: code.width(),
            dark: std::rc::Rc::new(dark),
        })
    }

    /// Drawn `size` square, dark on white with its quiet zone, in whole pixels so the modules
    /// meet without seams.
    fn element(self, side: Pixels) -> impl IntoElement {
        const QUIET: usize = 3;
        div().size(side).rounded_md().overflow_hidden().child(
            canvas(
                |_, _, _| (),
                move |bounds, _, window, _| {
                    let n = self.width + 2 * QUIET;
                    let scale = window.scale_factor();
                    let device = (f32::from(bounds.size.width) * scale).floor();
                    let cell = (device / n as f32).floor().max(1.) / scale;
                    let drawn = cell * n as f32;
                    let origin = point(
                        bounds.origin.x + px((f32::from(bounds.size.width) - drawn) / 2.),
                        bounds.origin.y + px((f32::from(bounds.size.height) - drawn) / 2.),
                    );
                    window.paint_quad(fill(bounds, gpui_kit::white()));
                    for (i, dark) in self.dark.iter().enumerate() {
                        if !*dark {
                            continue;
                        }
                        let (x, y) = (
                            (i % self.width + QUIET) as f32 * cell,
                            (i / self.width + QUIET) as f32 * cell,
                        );
                        window.paint_quad(fill(
                            Bounds::new(
                                point(origin.x + px(x), origin.y + px(y)),
                                size(px(cell), px(cell)),
                            ),
                            gpui_kit::black(),
                        ));
                    }
                },
            )
            .size_full(),
        )
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: gpui_kit's glob brings GPUI's own `test` attribute.
    use super::Qr;

    #[test]
    fn a_qr_code_is_square_and_has_its_finder_corners() {
        let qr =
            Qr::new("flotilla://pair?u=https%3A%2F%2Fbox.tail1234.ts.net#ABCDEFGHJKMN").unwrap();
        assert_eq!(qr.dark.len(), qr.width * qr.width);
        // The top-left finder pattern's outer ring.
        assert!(qr.dark[0] && qr.dark[6] && qr.dark[6 * qr.width] && !qr.dark[qr.width + 1]);
    }
}
