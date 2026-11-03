//! Flotilla: a desktop app for coding-agent threads that each run in their own boat VM.

mod add_project;
mod app;
// Linux only; its tests run everywhere.
#[cfg(any(target_os = "linux", test))]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod appimage;
mod ask_card;
mod assets;
mod attachments;
mod backend;
mod branch_picker;
mod device;
mod docs;
mod drafts;
mod fmt;
mod form;
mod graphemes;
#[cfg(target_os = "linux")]
mod instance;
mod listing;
mod mention_menu;
mod meter;
mod new_thread;
mod notify;
mod pairing;
mod palette;
mod picker;
mod pixel;
mod project_view;
mod rename;
mod schedule_editor;
mod schedules_view;
mod settings;
mod sidebar;
#[cfg(feature = "snapshot")]
mod snapshot;
mod subagents;
mod theme;
mod thread_menu;
mod thread_view;

use gpui_kit::*;

gpui_kit::actions!(
    flotilla,
    [
        NewThread,
        OpenPalette,
        OpenSettings,
        ConnectServer,
        Quit,
        CloseWindow,
        SyncNow,
        UndoArchive,
        ArchiveAndNext,
        NextThread,
        PreviousThread,
        ToggleSidebar,
        Thread1,
        Thread2,
        Thread3,
        Thread4,
        Thread5,
        Thread6,
        Thread7,
        Thread8,
        Thread9
    ]
);

fn main() {
    // A debug build is Flotilla Dev: its own database, key, VMs and environments.
    let name = flotilla_model::flavor::init(cfg!(debug_assertions)).app_name();
    // `flotilla-app --version`: the version, and no window (the release checks run it).
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("{name} {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    // FLOTILLA_DB points a build at a scratch database.
    let db = std::env::var_os("FLOTILLA_DB")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(flotilla_core::lifecycle::default_db_path);
    // `flotilla://pair…` links opened on this computer, for the window to pair with. An empty
    // one (Linux: the app started again with no link) only brings the window forward.
    let (links, opened) = smol::channel::unbounded::<String>();
    // Linux: `xdg-open` runs the app with the link. The first app on this database takes it
    // (and later ones' links); a later start hands its own over and goes.
    #[cfg(target_os = "linux")]
    {
        appimage::integrate();
        let given = instance::links_in(std::env::args().skip(1));
        let forward = links.clone();
        if let instance::Start::Other = instance::start(&db, &given, move |link| {
            let _ = forward.send_blocking(link);
        }) {
            return;
        }
        for link in given {
            instance::send(link, &|link| {
                let _ = links.send_blocking(link);
            });
        }
    }
    let application = gpui_kit::application().with_assets(assets::Assets);
    application.on_open_urls(move |urls| {
        for url in urls {
            let _ = links.send_blocking(url);
        }
    });
    application.run(move |cx| {
        gpui_kit::init(cx);
        theme::install(cx);
        notify::init(cx);
        // ⌘ on a Mac, Ctrl elsewhere.
        macro_rules! key {
            ($k:literal) => {
                if cfg!(target_os = "macos") {
                    concat!("cmd-", $k)
                } else {
                    concat!("ctrl-", $k)
                }
            };
        }
        cx.bind_keys([
            KeyBinding::new(key!("n"), NewThread, None),
            // Search threads and messages, and run commands (T3 Code's palette). Global, so it
            // opens from a text field too; gpui-kit's fields don't use it.
            KeyBinding::new(key!("k"), OpenPalette, None),
            KeyBinding::new(key!(","), OpenSettings, None),
            KeyBinding::new(key!("q"), Quit, None),
            KeyBinding::new(key!("w"), CloseWindow, None),
            KeyBinding::new(key!("r"), SyncNow, None),
            // In the app's own context, not global: a global binding beat text fields' own
            // ⌘Z. With a field focused its Undo runs, and the app root captures that while an
            // archive can be undone (see FlotillaApp::render).
            KeyBinding::new(key!("z"), UndoArchive, Some("Flotilla")),
            // Archive the open thread and open the next that needs you (T3 Code's settle and
            // advance). Nothing else here, in gpui-kit or its text fields uses it.
            KeyBinding::new(key!("shift-s"), ArchiveAndNext, None),
            // Thread switching, as in T3 Code.
            KeyBinding::new(key!("shift-]"), NextThread, None),
            KeyBinding::new(key!("shift-["), PreviousThread, None),
            // macOS's Show/Hide Sidebar; F9 shows and hides GNOME apps' sidebars.
            KeyBinding::new(
                if cfg!(target_os = "macos") {
                    "ctrl-cmd-s"
                } else {
                    "f9"
                },
                ToggleSidebar,
                None,
            ),
            KeyBinding::new(key!("1"), Thread1, None),
            KeyBinding::new(key!("2"), Thread2, None),
            KeyBinding::new(key!("3"), Thread3, None),
            KeyBinding::new(key!("4"), Thread4, None),
            KeyBinding::new(key!("5"), Thread5, None),
            KeyBinding::new(key!("6"), Thread6, None),
            KeyBinding::new(key!("7"), Thread7, None),
            KeyBinding::new(key!("8"), Thread8, None),
            KeyBinding::new(key!("9"), Thread9, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.set_menus([
            Menu::new(name).items([
                MenuItem::action("Settings…", OpenSettings),
                MenuItem::action("Connect to a Server…", ConnectServer),
                MenuItem::separator(),
                MenuItem::action(format!("Quit {name}"), Quit),
            ]),
            Menu::new("View").items([
                MenuItem::action("Search…", OpenPalette),
                MenuItem::separator(),
                MenuItem::action("Show/Hide Sidebar", ToggleSidebar),
            ]),
            Menu::new("Thread").items([
                MenuItem::action("New Thread", NewThread),
                MenuItem::action("Sync Now", SyncNow),
                MenuItem::separator(),
                MenuItem::action("Archive and Open Next", ArchiveAndNext),
                MenuItem::separator(),
                MenuItem::action("Next Thread", NextThread),
                MenuItem::action("Previous Thread", PreviousThread),
            ]),
        ]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let (w, h) = window_size();
        let bounds = Bounds::centered(None, size(px(w), px(h)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(760.), px(480.))),
            titlebar: Some(TitlebarOptions {
                title: Some(name.into()),
                ..Default::default()
            }),
            // Wayland: the desktop finds the app's .desktop file (its icon, its name) by this.
            app_id: cfg!(target_os = "linux")
                .then(|| flotilla_model::flavor::get().app_id().to_string()),
            ..Default::default()
        };
        let (window, app) = gpui_kit::open_window(options, cx, |window, cx| {
            // X11 takes it from the window's class, set here.
            #[cfg(target_os = "linux")]
            window.set_app_id(flotilla_model::flavor::get().app_id());
            cx.new(|cx| app::FlotillaApp::new(db.clone(), window, cx))
        })
        .expect("failed to open the Flotilla window");
        let app = app.downgrade();
        // ⌘K from inside a dialog (the palette's own field, Settings): the app's root, which
        // hears it otherwise, isn't on the way there, so it comes here.
        let palette = app.clone();
        cx.on_action(move |_: &OpenPalette, cx| {
            let palette = palette.clone();
            cx.defer(move |cx| {
                let _ = window.update(cx, |_, window, cx| {
                    let _ = palette.update(cx, |app, cx| app.open_palette(window, cx));
                });
            });
        });
        // A banner clicked: the window comes forward with its thread.
        let clicked = app.clone();
        notify::on_click(cx, move |tag, cx| {
            let _ = window.update(cx, |_, window, cx| {
                let _ = clicked.update(cx, |app, cx| app.open_notification(&tag, window, cx));
                cx.activate(true);
                #[cfg(target_os = "linux")]
                window.activate_window();
            });
        });
        cx.spawn(async move |cx| {
            while let Ok(url) = opened.recv().await {
                let _ = window.update(cx, |_, window, cx| {
                    let _ = app.update(cx, |app, cx| app.open_link(url, window, cx));
                    cx.activate(true);
                    // Linux's GPUI doesn't bring an app forward; its window can be. A later
                    // start that handed this over hears that it was taken.
                    #[cfg(target_os = "linux")]
                    {
                        window.activate_window();
                        instance::took();
                    }
                });
            }
        })
        .detach();
        cx.activate(true);
    });
}

/// The window's first size: 1180×780, or `FLOTILLA_SNAPSHOT_SIZE=WxH` in snapshot builds
/// (screenshots at other widths).
fn window_size() -> (f32, f32) {
    #[cfg(feature = "snapshot")]
    if let Some(size) = std::env::var("FLOTILLA_SNAPSHOT_SIZE").ok().and_then(|v| {
        let (w, h) = v.split_once('x')?;
        Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
    }) {
        return size;
    }
    (1180., 780.)
}
