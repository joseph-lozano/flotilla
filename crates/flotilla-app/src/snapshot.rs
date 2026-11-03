//! Dev only (`--features snapshot`): walk the window through its screens and save a PNG of
//! each to `FLOTILLA_SNAPSHOT_DIR`, then quit. Rendering goes through GPUI's own Metal
//! renderer, so no screen-recording permission is needed.

use crate::app::FlotillaApp;
use gpui_kit::component::WindowExt as _;
use gpui_kit::*;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

const STEPS: &[&str] = &[
    "sidebar",
    "hover",
    "context-menu",
    "archived",
    "thread",
    "deleted",
    "thread",
    "tool-open",
    "working",
    "new-thread",
    "new-thread-typed",
    "settings",
    "dark",
];

pub fn start(dir: PathBuf, window: &mut Window, cx: &mut Context<FlotillaApp>) {
    if std::env::var_os("FLOTILLA_SNAPSHOT_LIVE").is_some() {
        return live(dir, window, cx);
    }
    if std::env::var_os("FLOTILLA_SNAPSHOT_SUMMARY").is_some() {
        return summary(dir, window, cx);
    }
    if let Some(script) = std::env::var_os("FLOTILLA_SNAPSHOT_SCRIPT") {
        return script_run(dir, script.to_string_lossy().into_owned(), window, cx);
    }
    if std::env::var_os("FLOTILLA_SNAPSHOT_RENAME").is_some() {
        return rename(dir, window, cx);
    }
    if std::env::var_os("FLOTILLA_SNAPSHOT_EXTERNAL_DELETE").is_some() {
        return external_delete(dir, window, cx);
    }
    cx.spawn_in(window, async move |this, cx| {
        cx.background_executor()
            .timer(Duration::from_millis(1500))
            .await;
        // The first row under the first group header, in window coordinates.
        let row = point(px(140.), px(108.));
        for step in STEPS {
            // Synthetic input goes to the window, outside any entity update.
            let _ = cx.update(|window, cx| match *step {
                "hover" => {
                    window.dispatch_event(
                        PlatformInput::MouseMove(MouseMoveEvent {
                            position: row,
                            pressed_button: None,
                            modifiers: Modifiers::default(),
                        }),
                        cx,
                    );
                }
                "context-menu" => {
                    let (button, modifiers) = (MouseButton::Right, Modifiers::default());
                    window.dispatch_event(
                        PlatformInput::MouseDown(MouseDownEvent {
                            button,
                            position: row,
                            modifiers,
                            click_count: 1,
                            first_mouse: false,
                        }),
                        cx,
                    );
                    window.dispatch_event(
                        PlatformInput::MouseUp(MouseUpEvent {
                            button,
                            position: row,
                            modifiers,
                            click_count: 1,
                        }),
                        cx,
                    );
                }
                "archived" => {
                    let escape = Keystroke::parse("escape").unwrap();
                    window.dispatch_event(
                        PlatformInput::KeyDown(KeyDownEvent {
                            keystroke: escape,
                            is_held: false,
                            prefer_character_input: false,
                        }),
                        cx,
                    );
                }
                _ => {}
            });
            if this
                .update_in(cx, |app, window, cx| app.snapshot_step(step, window, cx))
                .is_err()
            {
                return;
            }
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;
            settle(cx).await;
            match cx.update(capture) {
                Ok(Ok(img)) => {
                    let path = dir.join(format!("{step}.png"));
                    if let Err(e) = img.save(&path) {
                        eprintln!("snapshot {step}: {e}");
                    }
                }
                Ok(Err(e)) => eprintln!("snapshot {step}: {e}"),
                Err(_) => return,
            }
        }
        let _ = cx.update(|_, cx| cx.quit());
    })
    .detach();
}

/// Against real boat (scratch database): create a thread, follow it, send a follow-up and
/// steer it, then delete it. Saves a PNG at each stage.
fn live(dir: PathBuf, window: &mut Window, cx: &mut Context<FlotillaApp>) {
    use flotilla_proto::{Activity, Request};

    cx.spawn_in(window, async move |this, cx| {
        macro_rules! shot {
            ($name:expr) => {{
                cx.background_executor().timer(Duration::from_millis(600)).await;
                settle(cx).await;
                match cx.update(capture) {
                    Ok(Ok(img)) => {
                        let _ = img.save(dir.join(format!("{}.png", $name)));
                        eprintln!("[live] saved {}", $name);
                    }
                    Ok(Err(e)) => eprintln!("[live] {}: {e}", $name),
                    Err(_) => return,
                }
            }};
        }
        // Wait (up to `secs`) until the selected thread's activity satisfies `want`.
        macro_rules! wait_for {
            ($secs:expr, $want:expr) => {{
                let mut ok = false;
                for _ in 0..$secs {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    let Ok(a) = this.update(cx, |app, _| {
                        let sel = app.selected().map(String::from);
                        app.summaries.iter().find(|s| Some(&s.thread.id) == sel.as_ref()).map(|s| s.activity)
                    }) else {
                        return;
                    };
                    let want: fn(Option<Activity>) -> bool = $want;
                    if want(a) {
                        ok = true;
                        break;
                    }
                }
                eprintln!("[live] wait {}: {}", stringify!($want), if ok { "ok" } else { "timed out" });
            }};
        }

        // Give the first sync time to load projects, as a person typing would.
        cx.background_executor().timer(Duration::from_secs(10)).await;
        // Through the new-thread page, as a user would: type, then send.
        let _ = this.update_in(cx, |app, window, cx| app.snapshot_new_thread(
            "Remember the codeword PLUM. Run `uname -sm` with Bash, then write a short paragraph (about 80 words) about sailing boats.",
            window,
            cx,
        ));
        // The thread opens before boat has made its VM.
        if let Ok(Ok(img)) = cx.update(capture) {
            let _ = img.save(dir.join("live-0a-opened.png"));
            eprintln!("[live] saved live-0a-opened (the next frame after Send)");
        }
        shot!("live-0-page");
        wait_for!(30, |a| a == Some(Activity::Working));
        cx.background_executor().timer(Duration::from_secs(6)).await;
        shot!("live-1-working");
        wait_for!(120, |a| matches!(a, Some(Activity::Idle) | Some(Activity::Unread)));
        shot!("live-2-done");

        let Ok(Some(id)) = this.update(cx, |app, _| app.selected().map(String::from)) else { return };

        // Archive from the sidebar: the thread moves down at once, then its VM stops.
        let _ = this.update_in(cx, |app, window, cx| {
            app.collapsed.remove(&Activity::Archived);
            app.run_command(id.clone(), crate::thread_menu::ThreadCommand::Archive, window, cx);
        });
        shot!("live-3-archiving");
        let mut stopped = false;
        for _ in 0..60 {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let Ok(state) = this.update(cx, |app, _| app.summaries.iter().find(|s| s.thread.id == id).map(|s| (s.activity, s.thread.state))) else { return };
            if let Some((Activity::Archived, flotilla_proto::ThreadState::Stopped)) = state {
                stopped = true;
                break;
            }
        }
        eprintln!("[live] archived and stopped: {stopped}");
        shot!("live-4-archived");

        // A follow-up to the stopped VM: it shows at once, then the VM resumes and boat takes it.
        let follow_up = "Reply with just the word PONG.";
        let sent = Instant::now();
        let _ = this.update_in(cx, |app, window, cx| app.send_message(id.clone(), follow_up.to_string(), Vec::new(), window, cx));
        match cx.update(capture) {
            Ok(Ok(img)) => {
                let _ = img.save(dir.join("live-4b-sending.png"));
                eprintln!("[live] saved live-4b-sending (the next frame after Send)");
            }
            _ => eprintln!("[live] live-4b-sending: capture failed"),
        }
        wait_for!(60, |a| a == Some(Activity::Working));
        eprintln!("[live] turn started {:.1}s after Send", sent.elapsed().as_secs_f32());
        wait_for!(120, |a| matches!(a, Some(Activity::Idle) | Some(Activity::Unread)));
        shot!("live-4c-replied");
        let _ = this.update(cx, |app, _| {
            if let Some(d) = app.snapshot_detail().filter(|d| d.id == id) {
                let shown = d.messages.iter().filter(|m| m.kind == "prompt" && m.text.as_deref() == Some(follow_up)).count();
                let ids: Vec<&str> = d.sent.iter().map(|s| s.id.as_str()).collect();
                eprintln!("[live] follow-up shown {shown} time(s); sent_prompts ids: {ids:?}");
                eprintln!("[live] last reply: {:?}", d.messages.iter().rev().find(|m| m.kind == "text").and_then(|m| m.text.clone()));
            }
        });

        // Delete: the VM and its disk on boat, and the thread here.
        let _ = this.update(cx, |app, _| {
            if let Some(s) = app.summaries.iter().find(|s| s.thread.id == id) {
                eprintln!("[live] thread title now: {:?}; reply preview: {:?}", s.thread.title, s.last_reply);
            }
        });
        let _ = this.update(cx, |app, _| app.ask(Request::Delete(id.clone())));
        cx.background_executor().timer(Duration::from_secs(5)).await;
        shot!("live-5-deleted");
        let _ = cx.update(|_, cx| cx.quit());
    })
    .detach();
}

/// Draw once and give animations time to finish. A window that is hidden or behind others
/// gets no frames of its own, so an animation (a dialog fading in, the old one fading out)
/// would otherwise start on the capture's own draw and be caught at its first frame.
async fn settle(cx: &mut AsyncWindowContext) {
    let _ = cx.update(|window, cx| window.draw(cx).clear(cx));
    cx.background_executor()
        .timer(Duration::from_millis(500))
        .await;
}

/// Draw a fresh frame and read it back. Drawing first means the capture doesn't depend on
/// the window being on screen (macOS stops repainting hidden windows).
#[cfg(target_os = "macos")]
fn capture(window: &mut Window, cx: &mut App) -> Result<image::RgbaImage> {
    window.draw(cx).clear(cx);
    window.render_to_image()
}

/// Linux (X11, e.g. under Xvfb): GPUI's window can't read its frame back, so take the window's
/// pixels from the X server. That's the frame last presented: [`settle`] draws one and waits
/// for it to show.
#[cfg(target_os = "linux")]
fn capture(window: &mut Window, _: &mut App) -> Result<image::RgbaImage> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{ConnectionExt, ImageFormat};

    thread_local! {
        static X: std::cell::OnceCell<Option<x11rb::rust_connection::RustConnection>> = const { std::cell::OnceCell::new() };
    }
    let id = match window
        .window_handle()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .as_raw()
    {
        RawWindowHandle::Xcb(h) => h.window.get(),
        RawWindowHandle::Xlib(h) => h.window as u32,
        _ => {
            anyhow::bail!("snapshots on Linux need X11 (run under Xvfb, or unset WAYLAND_DISPLAY)")
        }
    };
    X.with(|x| {
        let conn = x
            .get_or_init(|| x11rb::connect(None).ok().map(|(c, _)| c))
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("couldn't reach the X server"))?;
        let geometry = conn.get_geometry(id)?.reply()?;
        let got = conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                id,
                0,
                0,
                geometry.width,
                geometry.height,
                !0,
            )?
            .reply()?;
        let bpp = conn
            .setup()
            .pixmap_formats
            .iter()
            .find(|f| f.depth == got.depth)
            .map(|f| f.bits_per_pixel);
        anyhow::ensure!(
            matches!(got.depth, 24 | 32) && bpp == Some(32),
            "an X visual of depth {} isn't supported",
            got.depth
        );
        // 32 bits a pixel, blue first: BGRX.
        let rgba = got
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[2], p[1], p[0], 255])
            .collect();
        image::RgbaImage::from_raw(geometry.width.into(), geometry.height.into(), rgba)
            .ok_or_else(|| anyhow::anyhow!("the X server sent a short image"))
    })
}

/// Settings' summary status with a model that fails, then with the default, then the model
/// list as a user opens it. No VM needed.
fn summary(dir: PathBuf, window: &mut Window, cx: &mut Context<FlotillaApp>) {
    // The summary model dropdown in Settings, in window coordinates.
    let model_select = point(px(SUMMARY_SELECT.0), px(SUMMARY_SELECT.1));
    cx.spawn_in(window, async move |this, cx| {
        macro_rules! shot {
            ($name:expr) => {{
                settle(cx).await;
                match cx.update(capture) {
                    Ok(Ok(img)) => {
                        let _ = img.save(dir.join(format!("{}.png", $name)));
                        eprintln!("[summary] saved {}", $name);
                    }
                    Ok(Err(e)) => eprintln!("[summary] {}: {e}", $name),
                    Err(_) => return,
                }
            }};
        }
        cx.background_executor().timer(Duration::from_secs(3)).await;
        for (model, name) in [
            ("not-a-real-model", "summary-failing"),
            (flotilla_model::summarize::DEFAULT_MODEL, "summary-working"),
        ] {
            let _ = this.update_in(cx, |app, window, cx| {
                window.close_all_dialogs(cx);
                app.open_settings(window, cx);
                app.snapshot_summary_model(model, window, cx);
            });
            cx.background_executor()
                .timer(Duration::from_secs(12))
                .await;
            shot!(name);
        }
        // Reopened, Settings lists the catalog's Claude models with the saved one chosen.
        let _ = this.update_in(cx, |app, window, cx| {
            window.close_all_dialogs(cx);
            app.open_settings(window, cx);
        });
        cx.background_executor().timer(Duration::from_secs(1)).await;
        shot!("summary-settings");
        let _ = cx.update(|window, cx| {
            let (button, modifiers) = (MouseButton::Left, Modifiers::default());
            window.dispatch_event(
                PlatformInput::MouseMove(MouseMoveEvent {
                    position: model_select,
                    pressed_button: None,
                    modifiers,
                }),
                cx,
            );
            window.draw(cx).clear(cx);
            window.dispatch_event(
                PlatformInput::MouseDown(MouseDownEvent {
                    button,
                    position: model_select,
                    modifiers,
                    click_count: 1,
                    first_mouse: false,
                }),
                cx,
            );
            window.dispatch_event(
                PlatformInput::MouseUp(MouseUpEvent {
                    button,
                    position: model_select,
                    modifiers,
                    click_count: 1,
                }),
                cx,
            );
        });
        cx.background_executor().timer(Duration::from_secs(1)).await;
        shot!("summary-menu");
        let _ = cx.update(|_, cx| cx.quit());
    })
    .detach();
}

const SUMMARY_SELECT: (f32, f32) = (390., 468.);

/// The rename dialog on the first thread, then after Generate. Runs the local `claude`; no VM.
/// Nothing is renamed: the dialog is never submitted.
fn rename(dir: PathBuf, window: &mut Window, cx: &mut Context<FlotillaApp>) {
    cx.spawn_in(window, async move |this, cx| {
        macro_rules! shot {
            ($name:expr) => {{
                settle(cx).await;
                match cx.update(capture) {
                    Ok(Ok(img)) => {
                        let _ = img.save(dir.join(format!("{}.png", $name)));
                        eprintln!("[rename] saved {}", $name);
                    }
                    Ok(Err(e)) => eprintln!("[rename] {}: {e}", $name),
                    Err(_) => return,
                }
            }};
        }
        cx.background_executor().timer(Duration::from_secs(2)).await;
        let Ok(Some(view)) = this.update_in(cx, |app, window, cx| app.snapshot_rename(window, cx))
        else {
            eprintln!("[rename] no thread to rename");
            let _ = cx.update(|_, cx| cx.quit());
            return;
        };
        shot!("rename");
        let _ = cx.update(|_, cx| view.update(cx, |v, cx| v.generate(cx)));
        cx.background_executor()
            .timer(Duration::from_millis(300))
            .await;
        shot!("rename-generating");
        cx.background_executor()
            .timer(Duration::from_secs(15))
            .await;
        if let Ok(name) = cx.update(|_, cx| view.read(cx).title.read(cx).value().to_string()) {
            eprintln!("[rename] field now: {name}");
        }
        shot!("rename-generated");
        let _ = cx.update(|_, cx| cx.quit());
    })
    .detach();
}

/// A VM deleted outside Flotilla (as from boat's dashboard), while its thread is open.
fn external_delete(dir: PathBuf, window: &mut Window, cx: &mut Context<FlotillaApp>) {
    use flotilla_proto::Activity;
    cx.spawn_in(window, async move |this, cx| {
        macro_rules! shot {
            ($name:expr) => {{
                settle(cx).await;
                match cx.update(capture) {
                    Ok(Ok(img)) => {
                        let _ = img.save(dir.join(format!("{}.png", $name)));
                    }
                    Ok(Err(e)) => eprintln!("[ext] {}: {e}", $name),
                    Err(_) => return,
                }
            }};
        }
        macro_rules! report {
            ($label:expr) => {{
                let _ = this.update(cx, |app, _| {
                    let sel = app.selected().map(String::from);
                    let s = app
                        .summaries
                        .iter()
                        .find(|s| Some(&s.thread.id) == sel.as_ref());
                    eprintln!(
                        "[ext] {}: selected={:?} state={:?} activity={:?} sync_error={:?}",
                        $label,
                        sel.as_deref().map(|s| &s[..8]),
                        s.map(|s| s.thread.state),
                        s.map(|s| s.activity),
                        app.status.error
                    );
                });
            }};
        }
        cx.background_executor()
            .timer(Duration::from_secs(10))
            .await;
        let _ = this.update_in(cx, |app, window, cx| {
            app.snapshot_new_thread("Reply with one word: ready.", window, cx)
        });
        for _ in 0..120 {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let Ok(done) = this.update(cx, |app, _| {
                let sel = app.selected().map(String::from);
                app.summaries.iter().any(|s| {
                    Some(&s.thread.id) == sel.as_ref()
                        && matches!(s.activity, Activity::Idle | Activity::Unread)
                })
            }) else {
                return;
            };
            if done {
                break;
            }
        }
        report!("before");
        // Delete the VM behind Flotilla's back.
        let Ok(Some(sandbox)) = this.update(cx, |app, _| {
            let sel = app.selected().map(String::from);
            app.summaries
                .iter()
                .find(|s| Some(&s.thread.id) == sel.as_ref())
                .and_then(|s| s.thread.sandbox_id.clone())
        }) else {
            return;
        };
        let deleted = cx
            .background_executor()
            .spawn(async move {
                let boat = flotilla_core::boat::BoatClient::new(
                    flotilla_core::secrets::api_key().map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                boat.delete_sandbox(&sandbox).map_err(|e| e.to_string())
            })
            .await;
        eprintln!("[ext] deleted outside Flotilla: {deleted:?}");
        for (secs, name) in [(3, "ext-3s"), (10, "ext-13s"), (20, "ext-33s")] {
            cx.background_executor()
                .timer(Duration::from_secs(secs))
                .await;
            report!(name);
            shot!(name);
        }
        let _ = cx.update(|_, cx| cx.quit());
    })
    .detach();
}

/// Drive the window from a script of `;`-separated steps, for checking one interaction:
/// `open:<title text>`, `step:<app snapshot step>`, `move:x,y`, `click:x,y`, `dclick:x,y`, `rclick:x,y`, `press:x,y` / `release:x,y`, `key:<keystroke>`
/// (e.g. `key:cmd-z`), `wait:<ms>`, `shot:<name>`, `fastshot:<name>` (no settling, for a
/// frame mid-animation), `clip` (print the clipboard), `dark`, `light`. The usage popover:
/// `step:usage;step:usage-open` (`usage-tight` for the near-limit counts).
/// Coordinates are window points.
fn script_run(dir: PathBuf, script: String, window: &mut Window, cx: &mut Context<FlotillaApp>) {
    cx.spawn_in(window, async move |this, cx| {
        cx.background_executor().timer(Duration::from_millis(1500)).await;
        let point_of = |arg: &str| -> Option<Point<Pixels>> {
            let mut parts = arg.split(',');
            let (x, y) = (parts.next()?, parts.next()?);
            Some(point(px(x.trim().parse().ok()?), px(y.trim().parse().ok()?)))
        };
        let recorder = Rc::new(RefCell::new(Recorder::default()));
        let (mut expected, mut failed) = (0, Vec::new());
        let mut pointer = point(px(700.), px(400.));
        let steps: Vec<String> = script.split(';').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
        for step in steps.iter().map(String::as_str) {
            let (cmd, arg) = step.split_once(':').unwrap_or((step, ""));
            eprintln!("[script] {step}");
            match cmd {
                "open" => {
                    let needle = arg.to_string();
                    let _ = this.update_in(cx, |app, window, cx| app.snapshot_open(&needle, window, cx));
                }
                // How many rows the open thread shows, and its oldest message's seq.
                "rows" => {
                    let _ = this.update(cx, |app, cx| {
                        let (rows, first) = app.snapshot_rows_first(cx);
                        eprintln!("[script] rows: {rows}, oldest seq shown {first:?}");
                    });
                }
                // Open a thread and time it: until its view has arrived and its rows are built,
                // then the first frame drawn with them (`[script] opened … in …`).
                "open-timed" => {
                    let needle = arg.to_string();
                    let started = Instant::now();
                    let _ = this.update_in(cx, |app, window, cx| app.snapshot_open(&needle, window, cx));
                    let deadline = started + Duration::from_secs(60);
                    let rows = loop {
                        let Ok(rows) = this.update(cx, |app, cx| app.snapshot_rows(cx)) else { return };
                        if rows > 0 || Instant::now() > deadline {
                            break rows;
                        }
                        cx.background_executor().timer(Duration::from_millis(2)).await;
                    };
                    let shown = started.elapsed();
                    let _ = cx.update(|window, cx| window.draw(cx).clear(cx));
                    eprintln!("[script] opened {needle:?} in {:.0} ms: {rows} rows built in {:.0} ms, first frame drawn", started.elapsed().as_secs_f64() * 1000., shown.as_secs_f64() * 1000.);
                }
                // Type a prompt on the new-thread page and send it.
                "newthread" => {
                    let prompt = arg.to_string();
                    let _ = this.update_in(cx, |app, window, cx| app.snapshot_new_thread(&prompt, window, cx));
                }
                // As if the engine saw a turn end on this thread.
                "finished" => {
                    let needle = arg.to_string();
                    let _ = this.update_in(cx, |app, window, cx| app.snapshot_finished(&needle, window, cx));
                }
                // As if the server sent an alert that this thread's turn finished.
                "alert" => {
                    let needle = arg.to_string();
                    let _ = this.update_in(cx, |app, window, cx| app.snapshot_alert(&needle, window, cx));
                }
                // Move the pointer smoothly: `glide:x,y,ms` (default 400 ms).
                "glide" => {
                    let Some(to) = point_of(arg) else { continue };
                    let ms: f32 = arg.split(',').nth(2).and_then(|m| m.trim().parse().ok()).unwrap_or(400.);
                    let steps = (ms / 16.).max(1.) as usize;
                    let from = pointer;
                    for i in 1..=steps {
                        let t = i as f32 / steps as f32;
                        let t = t * t * (3. - 2. * t);
                        let position = point(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t);
                        let _ = this.update(cx, |app, cx| {
                            app.demo_pointer = Some((position, None));
                            cx.notify();
                        });
                        // Outside the update: hover handlers update the app themselves.
                        let _ = cx.update(|window, cx| {
                            window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }), cx);
                        });
                        cx.background_executor().timer(Duration::from_millis(16)).await;
                    }
                    pointer = to;
                }
                // Hover at a point without drawing the demo pointer, for still screenshots:
                // `hover:x,y` (hover cards and highlights show, with no pointer over them).
                "hover" => {
                    let Some(position) = point_of(arg) else { continue };
                    pointer = position;
                    let _ = this.update(cx, |app, cx| {
                        app.demo_pointer = None;
                        cx.notify();
                    });
                    let _ = cx.update(|window, cx| {
                        window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }), cx);
                    });
                }
                "caption" => recorder.borrow_mut().caption(arg),
                "rec" if arg == "pause" => {
                    let mut r = recorder.borrow_mut();
                    if r.paused.is_none() {
                        r.paused = Some(Instant::now());
                    }
                }
                "rec" if arg == "resume" => {
                    let mut r = recorder.borrow_mut();
                    if let Some(p) = r.paused.take() {
                        r.paused_total += p.elapsed().as_secs_f64();
                    }
                }
                // Type into New thread's prompt a few characters at a time.
                "type" => {
                    let chars: Vec<char> = arg.chars().collect();
                    for end in (0..=chars.len()).step_by(3).chain([chars.len()]) {
                        let text: String = chars[..end].iter().collect();
                        let _ = this.update_in(cx, |app, window, cx| app.snapshot_type(&text, window, cx));
                        cx.background_executor().timer(Duration::from_millis(30)).await;
                    }
                }
                // Wait (up to `secs`, default 600) until the app shows `what`: `idle` (the open
                // thread's turn is over), `asked` (its agent asked a question that waits),
                // `checks-fail` / `checks-pass` (its PR's checks),
                // `woken` (a child's answer arrived and its turn is over), `pr` (it has a PR), `conflicts` / `mergeable`, `merged`, `setup-running` / `setup-done` / `setup-failed` (the newest setup run of
                // the first project), `repos` (Add project's list arrived), `subagents` (some are
                // running in the open thread).
                "until" => {
                    let (what, secs) = arg.split_once(',').map(|(w, s)| (w, s.trim().parse().unwrap_or(600))).unwrap_or((arg, 600u64));
                    let what = what.to_string();
                    let deadline = Instant::now() + Duration::from_secs(secs);
                    loop {
                        let Ok(met) = this.update(cx, |app, cx| app.snapshot_until(&what, cx)) else { return };
                        if met {
                            break;
                        }
                        if Instant::now() > deadline {
                            eprintln!("[script] until:{what} timed out");
                            break;
                        }
                        cx.background_executor().timer(Duration::from_millis(500)).await;
                    }
                }
                "rec" if arg.starts_with("start") => {
                    let fps: u64 = arg.split(':').nth(1).and_then(|f| f.parse().ok()).unwrap_or(15);
                    record(recorder.clone(), dir.join("frames"), fps, this.clone(), cx);
                }
                "rec" => {
                    recorder.borrow_mut().on = false;
                    // Let the last frame land, then write the timings ffmpeg needs.
                    cx.background_executor().timer(Duration::from_millis(400)).await;
                    if let Err(e) = recorder.borrow().write(&dir) {
                        eprintln!("[script] rec: {e}");
                    }
                }
                // Press without letting go, and let go: `press:x,y` … `release:x,y`, for a
                // press-and-hold (shots in between catch it part-way).
                "press" | "release" => {
                    let Some(position) = point_of(arg) else { continue };
                    pointer = position;
                    let pressed = (cmd == "press").then(std::time::Instant::now);
                    let _ = this.update(cx, |app, cx| {
                        app.demo_pointer = Some((position, pressed));
                        cx.notify();
                    });
                    let (button, modifiers) = (MouseButton::Left, Modifiers::default());
                    let _ = cx.update(|window, cx| {
                        window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers }), cx);
                        if cmd == "press" {
                            window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button, position, modifiers, click_count: 1, first_mouse: false }), cx);
                        } else {
                            window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button, position, modifiers, click_count: 1 }), cx);
                        }
                    });
                }
                // Drag with the button held: `drag:x1,y1,x2,y2[,ms]` (default 400 ms).
                "drag" => {
                    let n: Vec<f32> = arg.split(',').filter_map(|v| v.trim().parse().ok()).collect();
                    let [x1, y1, x2, y2] = n[..4.min(n.len())] else { continue };
                    let ms = n.get(4).copied().unwrap_or(400.);
                    let (button, modifiers) = (MouseButton::Left, Modifiers::default());
                    let from = point(px(x1), px(y1));
                    let _ = cx.update(|window, cx| {
                        window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position: from, pressed_button: None, modifiers }), cx);
                        window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button, position: from, modifiers, click_count: 1, first_mouse: false }), cx);
                    });
                    let steps = (ms / 16.).max(1.) as usize;
                    for i in 1..=steps {
                        let t = i as f32 / steps as f32;
                        let position = point(px(x1 + (x2 - x1) * t), px(y1 + (y2 - y1) * t));
                        let _ = this.update(cx, |app, cx| {
                            app.demo_pointer = Some((position, Some(std::time::Instant::now())));
                            cx.notify();
                        });
                        let _ = cx.update(|window, cx| {
                            window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: Some(button), modifiers }), cx);
                        });
                        cx.background_executor().timer(Duration::from_millis(16)).await;
                    }
                    let to = point(px(x2), px(y2));
                    pointer = to;
                    let _ = cx.update(|window, cx| {
                        window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button, position: to, modifiers, click_count: 1 }), cx);
                    });
                }
                "move" | "click" | "dclick" | "rclick" | "sclick" => {
                    let Some(position) = point_of(arg) else { continue };
                    pointer = position;
                    let pressed = (cmd != "move").then(std::time::Instant::now);
                    let _ = this.update(cx, |app, cx| {
                        app.demo_pointer = Some((position, pressed));
                        cx.notify();
                    });
                    // `sclick`: a ⇧-click.
                    let modifiers = Modifiers { shift: cmd == "sclick", ..Modifiers::default() };
                    let _ = cx.update(|window, cx| {
                        window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers }), cx);
                        window.draw(cx).clear(cx);
                        let button = if cmd == "rclick" { MouseButton::Right } else { MouseButton::Left };
                        let clicks: usize = if cmd == "dclick" { 2 } else { 1 };
                        if cmd != "move" {
                            for click_count in 1..=clicks {
                                window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button, position, modifiers, click_count, first_mouse: false }), cx);
                                window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button, position, modifiers, click_count }), cx);
                            }
                        }
                    });
                }
                // Scroll the wheel at a point: `scroll:x,y,dy` (dy in points, negative scrolls down).
                "scroll" => {
                    let Some(position) = point_of(arg) else { continue };
                    let dy: f32 = arg.split(',').nth(2).and_then(|d| d.trim().parse().ok()).unwrap_or(-200.);
                    let steps = 12;
                    for _ in 0..steps {
                        let delta = ScrollDelta::Pixels(point(px(0.), px(dy / steps as f32)));
                        let _ = cx.update(|window, cx| {
                            window.dispatch_event(PlatformInput::Scroll(ScrollEvent { position, delta, modifiers: Modifiers::default(), touch_phase: TouchPhase::Moved, source: ScrollSource::Pointer }), cx);
                        });
                        cx.background_executor().timer(Duration::from_millis(16)).await;
                    }
                }
                "key" => {
                    // `parse` wants a static str; a leaked step string is fine in a dev harness.
                    let Ok(keystroke) = Keystroke::parse(Box::leak(arg.to_string().into_boxed_str())) else { continue };
                    let _ = cx.update(|window, cx| {
                        window.dispatch_event(PlatformInput::KeyDown(KeyDownEvent { keystroke: keystroke.clone(), is_held: false, prefer_character_input: false }), cx);
                        window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
                    });
                }
                "wait" => cx.background_executor().timer(Duration::from_millis(arg.parse().unwrap_or(500))).await,
                // Check something now (see `FlotillaApp::snapshot_expect`): the run fails if it
                // isn't so.
                "expect" => {
                    let what = arg.to_string();
                    let Ok(met) = this.update_in(cx, |app, window, cx| app.snapshot_expect(&what, window, cx)) else { return };
                    expected += 1;
                    eprintln!("[script] expect {what}: {}", if met { "ok" } else { "FAILED" });
                    if !met {
                        failed.push(what);
                    }
                }
                // Paste text where the focus is, as ⌘V (Ctrl+V off a Mac) would.
                "paste" => {
                    let text = arg.to_string();
                    let paste = if cfg!(target_os = "macos") { "cmd-v" } else { "ctrl-v" };
                    let _ = cx.update(|window, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        window.dispatch_keystroke(Keystroke::parse(paste).unwrap(), cx);
                    });
                }
                // One of the app's named snapshot steps (see `FlotillaApp::snapshot_step`).
                "step" => {
                    let name = arg.to_string();
                    let _ = this.update_in(cx, |app, window, cx| app.snapshot_step(&name, window, cx));
                }
                // Print the open thread's composer text and caret (line, column).
                "composer" => {
                    let _ = this.update_in(cx, |app, window, cx| eprintln!("[script] composer (text, caret, focused): {:?}", app.snapshot_composer(window, cx)));
                }
                "clip" => {
                    let text = cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text())).ok().flatten();
                    eprintln!("[script] clipboard: {:?}", text.unwrap_or_default());
                }
                "dark" | "light" => {
                    let mode = if cmd == "dark" { gpui_kit::component::ThemeMode::Dark } else { gpui_kit::component::ThemeMode::Light };
                    let _ = cx.update(|window, cx| gpui_kit::component::Theme::change(mode, Some(window), cx));
                }
                // Capture this frame as it is, for catching an animation part-way.
                "fastshot" => match cx.update(capture) {
                    Ok(Ok(img)) => {
                        let _ = img.save(dir.join(format!("{arg}.png")));
                    }
                    Ok(Err(e)) => eprintln!("[script] {arg}: {e}"),
                    Err(_) => return,
                },
                "shot" => {
                    settle(cx).await;
                    match cx.update(capture) {
                        Ok(Ok(img)) => {
                            let _ = img.save(dir.join(format!("{arg}.png")));
                        }
                        Ok(Err(e)) => eprintln!("[script] {arg}: {e}"),
                        Err(_) => return,
                    }
                }
                other => eprintln!("[script] unknown step {other:?}"),
            }
            // Opening a thread loads it; other steps say how long to wait themselves.
            if cmd == "open" {
                cx.background_executor().timer(Duration::from_millis(150)).await;
            }
        }
        if expected > 0 {
            eprintln!("[script] {} of {expected} expectations held{}", expected - failed.len(), if failed.is_empty() { String::new() } else { format!("; failed: {}", failed.join(", ")) });
        }
        if !failed.is_empty() {
            std::process::exit(1);
        }
        let _ = cx.update(|_, cx| cx.quit());
    })
    .detach();
}

/// A scripted demo's recording: frame files with the times they were taken, and captions
/// with the times they changed, for ffmpeg to put together.
#[derive(Default)]
struct Recorder {
    on: bool,
    start: Option<Instant>,
    frames: Vec<(String, f64)>,
    captions: Vec<(f64, String)>,
    /// Since when it's paused (`rec:pause`), and how long all pauses so far took: the
    /// recording's clock skips them, so a wait for boat or GitHub is cut.
    paused: Option<Instant>,
    paused_total: f64,
    /// When it started, in seconds since 1970, to line it up with another recording.
    start_unix: f64,
}

impl Recorder {
    fn now(&self) -> f64 {
        let Some(start) = self.start else { return 0. };
        let paused =
            self.paused_total + self.paused.map(|p| p.elapsed().as_secs_f64()).unwrap_or(0.);
        start.elapsed().as_secs_f64() - paused
    }

    fn caption(&mut self, text: &str) {
        let t = self.now();
        self.captions.push((t, text.to_string()));
    }

    /// `frames.txt` (ffmpeg concat list with each frame's duration), `captions.tsv` (start
    /// time, text) and `rec-start.txt` (when it started, Unix seconds), next to the frames.
    fn write(&self, dir: &Path) -> std::io::Result<()> {
        let end = self.now();
        let mut list = String::new();
        for (i, (name, t)) in self.frames.iter().enumerate() {
            let next = self.frames.get(i + 1).map(|f| f.1).unwrap_or(end);
            list.push_str(&format!(
                "file 'frames/{name}'\nduration {:.4}\n",
                (next - t).max(0.001)
            ));
        }
        if let Some((name, _)) = self.frames.last() {
            list.push_str(&format!("file 'frames/{name}'\n"));
        }
        std::fs::write(dir.join("frames.txt"), list)?;
        std::fs::write(
            dir.join("rec-start.txt"),
            format!("{:.3}\n", self.start_unix),
        )?;
        let caps: String = self
            .captions
            .iter()
            .map(|(t, c)| format!("{t:.4}\t{c}\n"))
            .collect();
        std::fs::write(dir.join("captions.tsv"), format!("{caps}{end:.4}\t\n"))
    }
}

/// Capture a frame `fps` times a second until the recorder is switched off. Each frame is
/// drawn first, so the recording doesn't depend on the window being on screen, and encoded off
/// the main thread.
fn record(
    recorder: Rc<RefCell<Recorder>>,
    frames: PathBuf,
    fps: u64,
    this: WeakEntity<FlotillaApp>,
    cx: &mut AsyncWindowContext,
) {
    let _ = std::fs::create_dir_all(&frames);
    {
        let mut r = recorder.borrow_mut();
        r.on = true;
        r.start = Some(Instant::now());
        r.start_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0., |d| d.as_secs_f64());
    }
    let period = Duration::from_millis(1000 / fps.max(1));
    cx.spawn(async move |cx| {
        let mut n = 0u32;
        while recorder.borrow().on {
            let started = Instant::now();
            // Keep the pointer's press flash short even when nothing else redraws.
            let _ = this.update(cx, |_, cx| cx.notify());
            if recorder.borrow().paused.is_some() {
                cx.background_executor().timer(period).await;
                continue;
            }
            if let Ok(Ok(img)) = cx.update(capture) {
                n += 1;
                let name = format!("f{n:05}.png");
                let t = recorder.borrow().now();
                recorder.borrow_mut().frames.push((name.clone(), t));
                let path = frames.join(name);
                cx.background_spawn(async move {
                    let _ = img.save(path);
                })
                .detach();
            }
            let spent = started.elapsed();
            cx.background_executor()
                .timer(period.saturating_sub(spent))
                .await;
        }
    })
    .detach();
}
