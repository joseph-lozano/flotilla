//! Settings: which server (This Mac, or one it's paired with) and its devices (This Mac's
//! only while it lets them in); what the
//! engine's side found set up (the boat key, GitHub, summaries, Tailscale, the database), with
//! what to run there; defaults for new threads, projects, skills, the plan link, the data.
//! A paired Mac never enters a secret: only This Mac takes the boat key here.

use crate::device::SavedServer;
use crate::device::{THIS_MACHINE, THIS_MACHINE_LOWER};
use crate::picker::{PickerEvent, SummaryModelList, ThreadSettingsPicker, summary_model_choices};
use flotilla_model::lifecycle::IDLE_MINUTE_CHOICES;
use flotilla_model::setup::short;
use flotilla_proto::{
    Catalog, CheckOf, Doctor, Environment, GhStatus, Health, Skill, ThreadSettings, UserSkill,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariant, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme, Disableable as _, Icon, Selectable as _, Sizable, StyledExt as _, Theme,
    WindowExt, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::rc::Rc;

/// Where boat shows the plan, usage and billing.
pub const BILLING_URL: &str = "https://boat.dev/dashboard/billing";

pub enum SettingsEvent {
    /// This Mac's boat key (never sent from a paired Mac).
    SaveKey(String),
    Appearance(Appearance),
    DefaultsChanged(ThreadSettings),
    /// Save the summary model (empty turns summaries off) and test it.
    SummaryModel(String),
    /// Stop idle VMs after this many minutes.
    IdleMinutes(u32),
    /// Archive a thread when its pull request merges.
    ArchiveOnMerge(bool),
    /// Archive threads idle for this many days (0: never).
    ArchiveIdleDays(u32),
    /// Start the names of new branches with this (already cleaned; empty: no prefix).
    BranchPrefix(String),
    /// Run the setup checks again (and gh's).
    RunDoctor,
    AddProject,
    /// Open a project's settings.
    OpenProject(String),
    Rebuild,
    /// Your instructions for every agent.
    Instructions(String),
    /// Pack these skills' folders and give them to every agent (Update: one already there).
    AddSkills(Vec<Skill>),
    RemoveSkill(String),
    /// "Connect to a server…" (or a different one).
    ConnectServer,
    /// Back to this Mac's own engine.
    UseThisMac,
    /// The server this Mac is paired with.
    UseServer,
    /// Forget the server this Mac is paired with (in use or not).
    ForgetServer,
    /// A code for another device to pair with the server.
    AddDevice,
    RevokeDevice(String),
    /// Name a device: what Devices lists it as.
    RenameDevice {
        id: String,
        name: String,
    },
    /// This Mac lets other devices connect to it (and over Tailscale), or no longer.
    LetDevicesIn {
        on: bool,
        tailscale: bool,
    },
    /// Banners and the unread count on the app's icon, on this device.
    Notifications(bool),
}

/// Light or dark: follow macOS, or fix one. Kept on this device (`device.json`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    const ALL: [Appearance; 3] = [Appearance::System, Appearance::Light, Appearance::Dark];

    pub fn as_str(self) -> &'static str {
        match self {
            Appearance::System => "system",
            Appearance::Light => "light",
            Appearance::Dark => "dark",
        }
    }

    pub fn parse(s: &str) -> Appearance {
        Appearance::ALL
            .into_iter()
            .find(|a| a.as_str() == s)
            .unwrap_or_default()
    }

    fn label(self) -> &'static str {
        match self {
            Appearance::System => "System",
            Appearance::Light => "Light",
            Appearance::Dark => "Dark",
        }
    }
}

/// What Settings shows when it opens.
pub struct SettingsInit {
    /// Your settings as the server last sent them. None: they haven't come (it can't be
    /// reached), so only what's kept on this device shows (the server, appearance,
    /// notifications).
    pub settings: Option<flotilla_proto::Settings>,
    pub catalog: Option<Catalog>,
    pub environments: Vec<Environment>,
    pub summary_state: SummaryState,
    pub appearance: Appearance,
    pub gh_status: GhStatus,
    /// The skills on this Mac, to offer.
    pub mac_skills: Vec<Skill>,
    /// The device this Mac is to the server (none: This Mac), for what it added itself.
    pub me: Option<String>,
    /// The server this Mac uses (None: its own engine), and the one it's paired with.
    pub server: Option<SavedServer>,
    pub paired: Option<SavedServer>,
    /// That server's devices, once they've come.
    pub devices: Option<flotilla_proto::Devices>,
    /// On This Mac: whether it lets other devices in, over Tailscale, and how that's going.
    pub devices_in: (bool, bool, crate::app::DevicesIn),
    /// Notifications switched on here, and what the computer allows.
    pub notifications: (bool, crate::notify::Permission),
}

/// How summaries are doing, for the Settings line.
#[derive(Clone, Debug, PartialEq)]
pub enum SummaryState {
    Off,
    /// Never run on this Mac yet.
    Unknown,
    Testing,
    /// Last worked at this time (RFC 3339).
    Working(String),
    Failed(String),
}

pub struct SettingsView {
    /// Your settings as the server has them, with what was just chosen here shown at once
    /// (until the server sends them back). None: they hadn't come when Settings opened.
    settings: Option<flotilla_proto::Settings>,
    environments: Vec<Environment>,
    key: Entity<InputState>,
    defaults: Option<Entity<ThreadSettingsPicker>>,
    summary_model: Entity<SelectState<SummaryModelList>>,
    summary_state: SummaryState,
    appearance: Appearance,
    /// The branch prefix field, what was last saved, and a note when saving changed it.
    prefix: Entity<InputState>,
    saved_prefix: String,
    prefix_note: Option<String>,
    gh_status: GhStatus,
    /// Check again was pressed; the checks haven't come back yet.
    checking: bool,
    /// The setup checks shown in full (None: as their health says: open when one needs you).
    setup_open: Option<bool>,
    me: Option<String>,
    /// Your instructions for every agent, and what was last saved from here (the field is
    /// a draft: what another device saves doesn't replace it).
    instructions: Entity<TextareaState>,
    saved_instructions: String,
    /// Skills being packed and uploaded: their name, and when the one listed was added (0 for
    /// a new one), until the list has a newer one.
    adding: Vec<(String, i64)>,
    mac_skills: Vec<Skill>,
    /// A chosen folder held no skill.
    skills_note: Option<String>,
    server: Option<SavedServer>,
    paired: Option<SavedServer>,
    devices: Option<flotilla_proto::Devices>,
    /// Add device was pressed; the code hasn't come yet.
    adding_device: bool,
    devices_in: (bool, bool, crate::app::DevicesIn),
    notifications: (bool, crate::notify::Permission),
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl SettingsView {
    pub fn new(init: SettingsInit, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let SettingsInit {
            settings,
            catalog,
            environments,
            summary_state,
            appearance,
            gh_status,
            mac_skills,
            me,
            server,
            paired,
            devices,
            devices_in,
            notifications,
        } = init;
        let saved = |f: fn(&flotilla_proto::Settings) -> &String| {
            settings.as_ref().map(|s| f(s).clone()).unwrap_or_default()
        };
        let (saved_instructions, branch_prefix, summary_model) = (
            saved(|s| &s.instructions),
            saved(|s| &s.branch_prefix),
            saved(|s| &s.summary_model),
        );
        let defaults = settings.as_ref().and_then(|s| s.defaults.clone());
        let skills_note = settings.as_ref().and_then(|s| s.skills_error.clone());
        let instructions = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 12)
                .placeholder("e.g. Use pnpm, not npm. Run the tests before you finish.")
                .default_value(saved_instructions.clone())
        });
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Paste a new key to replace it")
                .masked(true)
        });
        let mut subs = vec![cx.subscribe_in(&key, window, |this, _, event, _, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.save(cx);
            }
        })];
        subs.push(
            cx.subscribe_in(&instructions, window, |this, _, event, _, cx| {
                // As they're written, so a quit with the box still open loses nothing.
                if let InputEvent::Change | InputEvent::Blur = event {
                    this.save_instructions(cx);
                }
            }),
        );
        let prefix = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("No prefix")
                .default_value(branch_prefix.clone())
        });
        subs.push(
            cx.subscribe_in(&prefix, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.save_prefix(window, cx);
                }
            }),
        );
        let (choices, index) = summary_model_choices(catalog.as_ref(), &summary_model);
        let summary_model =
            cx.new(|cx| SelectState::new(choices, index, window, cx).searchable(true));
        subs.push(cx.subscribe_in(
            &summary_model,
            window,
            |this, _, event: &SelectEvent<SummaryModelList>, _, cx| {
                if let SelectEvent::Confirm(Some(_)) = event {
                    this.save_summary_model(cx);
                }
            },
        ));
        let picker =
            catalog.map(|c| cx.new(|cx| ThreadSettingsPicker::new(c, defaults, false, window, cx)));
        if let Some(p) = &picker {
            subs.push(
                cx.subscribe_in(p, window, |_, picker, _: &PickerEvent, _, cx| {
                    if let Some(s) = picker.read(cx).settings(cx) {
                        cx.emit(SettingsEvent::DefaultsChanged(s));
                    }
                }),
            );
        }
        SettingsView {
            settings,
            environments,
            key,
            defaults: picker,
            summary_model,
            summary_state,
            appearance,
            prefix,
            saved_prefix: branch_prefix,
            prefix_note: None,
            gh_status,
            checking: false,
            setup_open: None,
            me,
            instructions,
            saved_instructions,
            adding: Vec::new(),
            mac_skills,
            skills_note,
            server,
            paired,
            devices,
            adding_device: false,
            devices_in,
            notifications,
            _subscriptions: subs,
        }
    }

    /// Save the instructions if they changed (on leaving the box).
    fn save_instructions(&mut self, cx: &mut Context<Self>) {
        let text = self.instructions.read(cx).value().trim().to_string();
        if text != self.saved_instructions {
            self.saved_instructions = text.clone();
            cx.emit(SettingsEvent::Instructions(text));
        }
    }

    /// Your settings as the server has them now: from here, another device, or the CLI.
    /// The fields being written (instructions, branch prefix) keep what's typed, and the
    /// model pickers stay as they are. Opened before they'd come, Settings stays as it was.
    pub fn set_settings(&mut self, settings: flotilla_proto::Settings, cx: &mut Context<Self>) {
        let Some(current) = &mut self.settings else {
            return;
        };
        // A skill being added is done once the list has a newer one of its name.
        self.adding.retain(|(name, before)| {
            !settings
                .skills
                .iter()
                .any(|s| &s.name == name && s.added_ms > *before)
        });
        if settings.skills_error.is_some() {
            self.skills_note = settings.skills_error.clone();
        }
        *current = settings;
        cx.notify();
    }

    /// What was just chosen here, shown at once (the server sends it back).
    fn chose(
        &mut self,
        change: impl FnOnce(&mut flotilla_proto::Settings),
        event: SettingsEvent,
        cx: &mut Context<Self>,
    ) {
        if let Some(s) = &mut self.settings {
            change(s);
        }
        cx.emit(event);
        cx.notify();
    }

    fn skills(&self) -> &[UserSkill] {
        self.settings.as_ref().map_or(&[], |s| s.skills.as_slice())
    }

    /// Packing or uploading `name` failed (the app says why).
    pub fn skill_failed(&mut self, name: &str, cx: &mut Context<Self>) {
        self.adding.retain(|(n, _)| n != name);
        cx.notify();
    }

    /// The checks Check again asked for are done.
    pub fn doctor_done(&mut self, cx: &mut Context<Self>) {
        self.checking = false;
        cx.notify();
    }

    /// Pack and upload these (each in place of any of its name).
    fn add_skills(&mut self, add: Vec<Skill>, cx: &mut Context<Self>) {
        if add.is_empty() {
            return;
        }
        for s in &add {
            let before = self
                .skills()
                .iter()
                .find(|k| k.name == s.name)
                .map_or(0, |k| k.added_ms);
            self.adding.retain(|(n, _)| *n != s.name);
            self.adding.push((s.name.clone(), before));
        }
        self.skills_note = None;
        cx.emit(SettingsEvent::AddSkills(add));
        cx.notify();
    }

    /// Whether this Mac added `skill`, and still has its folder: it can pack it again.
    fn updatable(&self, skill: &UserSkill) -> Option<Skill> {
        (skill.from == self.me)
            .then(|| flotilla_model::skills::read(&skill.folder))
            .flatten()
    }

    /// Pick folders: each a skill (it has a SKILL.md), or a folder of skills.
    fn choose_skill_folders(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add".into()),
        });
        cx.spawn(async move |this, cx| {
            let paths = match chosen.await {
                Ok(Ok(Some(paths))) => paths,
                // Linux: the desktop portal's picker, which may not be running.
                Ok(Err(e)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.skills_note = Some(format!("Couldn't open the folder picker: {e}"));
                        cx.notify();
                    });
                    return;
                }
                _ => return,
            };
            let _ = this.update(cx, |this, cx| {
                let mut found = Vec::new();
                for p in &paths {
                    match flotilla_model::skills::read(p) {
                        Some(s) => found.push(s),
                        None => {
                            let mut inside: Vec<Skill> = std::fs::read_dir(p)
                                .into_iter()
                                .flatten()
                                .flatten()
                                .filter_map(|e| flotilla_model::skills::read(&e.path()))
                                .collect();
                            inside.sort_by(|a, b| a.name.cmp(&b.name));
                            found.extend(inside);
                        }
                    }
                }
                if found.is_empty() {
                    this.skills_note = Some("No SKILL.md there".to_string());
                    cx.notify();
                }
                this.add_skills(found, cx);
            });
        })
        .detach();
    }

    /// This Mac letting other devices in, as it goes.
    pub fn set_devices_in(
        &mut self,
        state: (bool, bool, crate::app::DevicesIn),
        cx: &mut Context<Self>,
    ) {
        self.devices_in = state;
        cx.notify();
    }

    /// The server's devices, as they connect, drop, pair and go.
    pub fn set_devices(
        &mut self,
        devices: Option<flotilla_proto::Devices>,
        cx: &mut Context<Self>,
    ) {
        self.devices = devices;
        cx.notify();
    }

    /// The Add device code came (or didn't).
    pub fn device_added(&mut self, cx: &mut Context<Self>) {
        self.adding_device = false;
        cx.notify();
    }

    /// Forget the server this Mac is paired with, with its token (in use, back to This Mac).
    fn confirm_forget(
        &mut self,
        name: String,
        in_use: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.weak_entity();
        let says = if in_use {
            format!(
                "{THIS_MACHINE} forgets its pairing and goes back to its own threads. Pair again with a new code."
            )
        } else {
            format!("{THIS_MACHINE} forgets its pairing. Pair again with a new code.")
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .title(format!("Forget {name}?"))
                .description(says.clone())
                .confirm()
                .ok_text("Forget")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.paired = None;
                        cx.emit(SettingsEvent::ForgetServer);
                        cx.notify();
                    });
                    true
                })
        });
    }

    fn confirm_revoke(
        &mut self,
        id: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let (this, id) = (this.clone(), id.clone());
            alert
                .title(format!("Revoke {name}?"))
                .description("It disconnects now, and has to pair again to connect.")
                .confirm()
                .ok_text("Revoke")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let _ =
                        this.update(cx, |_, cx| cx.emit(SettingsEvent::RevokeDevice(id.clone())));
                    true
                })
        });
    }

    /// Rename a device, its name filled in.
    fn open_rename_device(
        &mut self,
        id: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        let submit = Rc::new({
            let (this, input) = (cx.weak_entity(), input.clone());
            move |window: &mut Window, cx: &mut App| {
                let name = input.read(cx).value().trim().to_string();
                if name.is_empty() {
                    return;
                }
                let _ = this.update(cx, |_, cx| {
                    cx.emit(SettingsEvent::RenameDevice {
                        id: id.clone(),
                        name,
                    })
                });
                window.close_dialog(cx);
            }
        });
        let enter = submit.clone();
        let field = input.clone();
        crate::form::open(
            window,
            cx,
            Some(input),
            move |window, cx| enter(window, cx),
            move |dialog, _, _| {
                let submit = submit.clone();
                dialog
                    .title("Rename device")
                    .width(px(380.))
                    .child(Input::new(&field))
                    .footer(
                        h_flex().justify_end().child(
                            Button::new("rename-device")
                                .primary()
                                .label("Rename")
                                .on_click(move |_, window, cx| submit(window, cx)),
                        ),
                    )
            },
        );
    }

    /// Which server, and its devices.
    fn render_server(&self, cx: &mut Context<Self>) -> (AnyElement, Option<AnyElement>) {
        let theme = cx.theme().clone();
        let heading = |t: &'static str| div().text_sm().font_semibold().child(t);
        let section = || {
            v_flex()
                .gap_2()
                .py_3()
                .border_b_1()
                .border_color(theme.border)
        };
        let row = |icon: IconName, name: String| {
            h_flex()
                .gap_2()
                .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                .child(div().flex_1().min_w_0().text_sm().truncate().child(name))
        };
        // Where threads can live, a row each: This Mac, and the server it's paired with (to
        // forget, in use or not); the one in use ticked, the other to use. Then connecting to a
        // server (in place of the paired one), on a line of its own.
        let tick = || {
            div()
                .flex_none()
                .child(Icon::new(IconName::Check).small().text_color(theme.primary))
        };
        let use_ = |id: &'static str, event: fn() -> SettingsEvent| {
            Button::new(id)
                .small()
                .outline()
                .label("Use")
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(event())))
        };
        let mut server = section().pt_0().child(heading("Server"));
        server = server.child(match &self.server {
            None => row(IconName::Laptop, THIS_MACHINE.into()).child(tick()),
            Some(_) => row(IconName::Laptop, THIS_MACHINE.into())
                .text_color(theme.muted_foreground)
                .child(use_("use-this-mac", || SettingsEvent::UseThisMac)),
        });
        if let Some(p) = &self.paired {
            let in_use = self.server.as_ref().is_some_and(|s| s.id == p.id);
            let name = p.name().to_string();
            let forget = {
                let name = name.clone();
                cx.listener(move |this, _, window, cx| {
                    this.confirm_forget(name.clone(), in_use, window, cx)
                })
            };
            let line = row(IconName::Server, name).child(
                Button::new("forget-server")
                    .small()
                    .ghost()
                    .label("Forget")
                    .on_click(forget),
            );
            server = server.child(if in_use {
                line.child(tick())
            } else {
                line.text_color(theme.muted_foreground)
                    .child(use_("use-server", || SettingsEvent::UseServer))
            });
        }
        // Dev: a server named by FLOTILLA_SERVER, in use but not paired.
        if let Some(s) = self
            .server
            .as_ref()
            .filter(|s| self.paired.as_ref().is_none_or(|p| p.id != s.id))
        {
            server = server.child(row(IconName::Server, s.name().to_string()).child(tick()));
        }
        server = server.child(
            h_flex().child(
                Button::new("connect-server")
                    .small()
                    .ghost()
                    .icon(IconName::Plus)
                    .label(crate::app::connect_label(self.paired.as_ref()))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::ConnectServer))),
            ),
        );
        let add = || {
            Button::new("add-device")
                .small()
                .outline()
                .icon(IconName::Plus)
                .label("Add device")
                .loading(self.adding_device)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.adding_device = true;
                    cx.emit(SettingsEvent::AddDevice);
                    cx.notify();
                }))
        };
        let mut list = section();
        let devices = match &self.server {
            Some(_) => {
                let Some(devices) = self.devices.as_ref() else {
                    return (server.into_any_element(), None);
                };
                list = list.child(
                    h_flex()
                        .child(div().flex_1().child(heading("Devices")))
                        .child(add()),
                );
                devices
            }
            // This Mac: other devices connect only while it lets them.
            None => {
                let (on, tailscale, state) = self.devices_in.clone();
                // Without Tailscale it listens on this Mac alone: no address to give out.
                let reachable =
                    matches!(&state, crate::app::DevicesIn::Open(o) if !loopback(&o.url));
                list = list.child(
                    h_flex()
                        .child(div().flex_1().child(heading("Devices")))
                        .when(reachable, |el| el.child(add())),
                );
                list = list.child(
                    Switch::new("let-devices-in")
                        .small()
                        .label("Let other devices connect")
                        .checked(on)
                        .on_click(cx.listener(move |_, on: &bool, _, cx| {
                            cx.emit(SettingsEvent::LetDevicesIn { on: *on, tailscale });
                        })),
                );
                if on {
                    list = list.child(
                        Switch::new("over-tailscale")
                            .small()
                            .label("Over Tailscale")
                            .checked(tailscale)
                            .on_click(cx.listener(move |_, tailscale: &bool, _, cx| {
                                cx.emit(SettingsEvent::LetDevicesIn {
                                    on: true,
                                    tailscale: *tailscale,
                                });
                            })),
                    );
                    let line =
                        |text: String, color: Hsla| div().text_xs().text_color(color).child(text);
                    list = match &state {
                        crate::app::DevicesIn::Off => list,
                        crate::app::DevicesIn::Opening => {
                            list.child(line("Opening…".into(), theme.muted_foreground))
                        }
                        crate::app::DevicesIn::Failed(e) => {
                            list.child(line(e.clone(), theme.danger))
                        }
                        crate::app::DevicesIn::Open(o) if !loopback(&o.url) => list.child(
                            div()
                                .text_xs()
                                .font_family(theme.mono_font_family.clone())
                                .text_color(theme.muted_foreground)
                                .child(o.url.clone()),
                        ),
                        // Tailscale asked for but not forwarding: why.
                        crate::app::DevicesIn::Open(o) if o.tailscale.is_some() => {
                            list.child(line(
                                format!("Tailscale: {}", o.tailscale.clone().unwrap_or_default()),
                                theme.warning,
                            ))
                        }
                        crate::app::DevicesIn::Open(_) => list.child(line(
                            format!(
                                "Only {THIS_MACHINE_LOWER} can connect until Over Tailscale is on."
                            ),
                            theme.muted_foreground,
                        )),
                    };
                }
                match self.devices.as_ref().filter(|_| on) {
                    Some(d) => d,
                    None => return (server.into_any_element(), Some(list.into_any_element())),
                }
            }
        };
        let this_device = self
            .server
            .as_ref()
            .map(|s| s.device.clone())
            .unwrap_or_default();
        let now = flotilla_model::util::now_ms();
        for d in &devices.devices {
            let mine = !this_device.is_empty() && d.id == this_device;
            let (status, color) = if d.connected {
                ("Connected".to_string(), theme.success)
            } else {
                (
                    format!("Last seen {}", crate::fmt::ago_phrase(d.last_seen_ms, now)),
                    theme.muted_foreground,
                )
            };
            let agent = d.scope == flotilla_proto::Scope::Agent;
            let icon = if agent {
                IconName::Bot
            } else if matches!(d.platform.as_str(), "ios" | "android") {
                IconName::Smartphone
            } else {
                IconName::Laptop
            };
            let kind = if agent {
                "Agent"
            } else {
                platform_name(&d.platform)
            };
            let (id, name) = (d.id.clone(), d.name.clone());
            let (rename_id, rename_name) = (d.id.clone(), d.name.clone());
            list = list.child(
                h_flex()
                    .gap_2()
                    .py_1()
                    .child(Icon::new(icon).small().text_color(theme.muted_foreground))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .text_sm()
                                            .font_semibold()
                                            .truncate()
                                            .child(d.name.clone()),
                                    )
                                    .when(mine, |el| {
                                        el.child(
                                            div()
                                                .flex_none()
                                                .text_xs()
                                                .text_color(theme.muted_foreground)
                                                .child("This device"),
                                        )
                                    }),
                            )
                            .child(
                                h_flex()
                                    .gap_1()
                                    .text_xs()
                                    .child(
                                        div()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("{kind} ·")),
                                    )
                                    .child(div().text_color(color).child(status)),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("rename-{}", d.id)))
                            .small()
                            .ghost()
                            .label("Rename")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_rename_device(
                                    rename_id.clone(),
                                    rename_name.clone(),
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .when(!mine, |el| {
                        el.child(
                            Button::new(SharedString::from(format!("revoke-{}", d.id)))
                                .small()
                                .outline()
                                .label("Revoke")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.confirm_revoke(id.clone(), name.clone(), window, cx)
                                })),
                        )
                    }),
            );
        }
        (server.into_any_element(), Some(list.into_any_element()))
    }

    pub fn set_gh_status(&mut self, status: GhStatus, cx: &mut Context<Self>) {
        self.gh_status = status;
        cx.notify();
    }

    /// What the computer allows banners now.
    pub fn set_notification_permission(
        &mut self,
        p: crate::notify::Permission,
        cx: &mut Context<Self>,
    ) {
        if self.notifications.1 != p {
            self.notifications.1 = p;
            cx.notify();
        }
    }

    /// Light, dark, or as the system is.
    fn render_appearance(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        v_flex()
            .gap_2()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(div().text_sm().font_semibold().child("Appearance"))
            .child(
                h_flex().child(
                    ButtonGroup::new("appearance")
                        .outline()
                        .small()
                        .children(Appearance::ALL.map(|a| {
                            segment(
                                Button::new(SharedString::from(format!(
                                    "appearance-{}",
                                    a.as_str()
                                )))
                                .label(a.label()),
                                a == self.appearance,
                                &theme,
                            )
                        }))
                        .on_click(cx.listener(|this, picked: &Vec<usize>, _, cx| {
                            if let Some(&a) = picked.first().and_then(|&i| Appearance::ALL.get(i)) {
                                this.appearance = a;
                                cx.emit(SettingsEvent::Appearance(a));
                                cx.notify();
                            }
                        })),
                ),
            )
            .into_any_element()
    }

    /// The switch, and a word when the computer has turned Flotilla's banners off.
    fn render_notifications(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let (on, permission) = self.notifications;
        let label = if cfg!(target_os = "macos") {
            "Banners and the Dock's unread count"
        } else {
            "Notifications and the launcher's unread count"
        };
        v_flex()
            .gap_2()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(div().text_sm().font_semibold().child("Notifications"))
            .child(
                Switch::new("notifications")
                    .small()
                    .label(label)
                    .checked(on)
                    .on_click(cx.listener(|this, on: &bool, _, cx| {
                        this.notifications.0 = *on;
                        cx.emit(SettingsEvent::Notifications(*on));
                        cx.notify();
                    })),
            )
            .when(
                on && permission == crate::notify::Permission::Denied,
                |el| {
                    el.child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(theme.warning)
                                    .child(format!(
                                        "{} can't show banners: they're off in System Settings.",
                                        flotilla_model::flavor::app_name()
                                    )),
                            )
                            .child(
                                Button::new("notification-settings")
                                    .small()
                                    .outline()
                                    .label("Open System Settings")
                                    .on_click(|_, _, cx| {
                                        cx.open_url(&crate::notify::settings_url())
                                    }),
                            ),
                    )
                },
            )
            .into_any_element()
    }

    /// What the engine's side found, with what to run there; This Mac's boat key field under
    /// its check (a paired Mac has none: the server's own setup sets it).
    fn render_setup(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let remote = self.server.is_some();
        let doctor = self.settings.as_ref().and_then(|s| s.doctor.as_ref());
        // One line, how many of the checks pass ("6/7"), until it's opened; open by itself when
        // one fails or warns (it needs you), or This Mac has no key to check yet.
        // Optional parts that are off (push, remote MCP) aren't counted.
        let (ok, all) = doctor.map_or((0, 0), |d| d.tally());
        let needs_you = doctor.is_some_and(|d| {
            d.checks
                .iter()
                .any(|c| matches!(c.health, Health::Fail | Health::Warn))
        }) || (!remote
            && doctor.is_some_and(|d| !d.checks.iter().any(|c| c.check == CheckOf::BoatKey)));
        let open = self.setup_open.unwrap_or(needs_you);
        let mut section = v_flex()
            .gap_2()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .child(
                        h_flex()
                            .id("setup-toggle")
                            .flex_1()
                            .gap_1()
                            .cursor_pointer()
                            .child(
                                Icon::new(if open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .xsmall()
                                .text_color(theme.muted_foreground),
                            )
                            .child(div().text_sm().font_semibold().child(if remote {
                                "Server setup"
                            } else {
                                "Setup"
                            }))
                            .when(all > 0, |el| {
                                el.child(
                                    div()
                                        .text_sm()
                                        .text_color(if ok == all {
                                            theme.muted_foreground
                                        } else {
                                            theme.foreground
                                        })
                                        .child(format!("{ok}/{all}")),
                                )
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.setup_open = Some(!open);
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("run-doctor")
                            .small()
                            .outline()
                            .label("Check again")
                            .loading(self.checking)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.checking = true;
                                cx.emit(SettingsEvent::RunDoctor);
                                cx.notify();
                            })),
                    ),
            );
        if doctor.is_some() && !open {
            return section.into_any_element();
        }
        let key_field = || {
            h_flex()
                .gap_2()
                .pl_6()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&self.key).small()),
                )
                .child(
                    div().flex_none().child(
                        Button::new("save-key")
                            .small()
                            .label("Replace")
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
                )
                .into_any_element()
        };
        let Some(doctor) = doctor else {
            section = section.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("Checking…"),
            );
            if !remote {
                section = section.child(key_row(key_field(), &theme));
            }
            return section.into_any_element();
        };
        let key_field: &dyn Fn() -> AnyElement = &key_field;
        section
            .children(check_rows(doctor, (!remote).then_some(key_field), &theme))
            .into_any_element()
    }

    pub fn set_summary_state(&mut self, state: SummaryState, cx: &mut Context<Self>) {
        self.summary_state = state;
        cx.notify();
    }

    #[cfg(feature = "snapshot")]
    pub(crate) fn save_summary_model_for_test(
        &mut self,
        model: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Any model, even one the list doesn't offer, so a failing one can be shown.
        let (choices, index) = summary_model_choices(None, model);
        self.summary_model.update(cx, |s, cx| {
            s.set_items(choices, window, cx);
            s.set_selected_index(index, window, cx);
        });
        self.save_summary_model(cx);
    }

    /// Dev: the idle minutes it shows chosen.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_idle_minutes(&self) -> Option<u32> {
        self.settings.as_ref().map(|s| s.idle_minutes)
    }

    /// Dev: into the branch prefix field, as a click would.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_focus_prefix(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.prefix.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Dev: type `text` in the branch prefix field and press Return.
    #[cfg(feature = "snapshot")]
    pub(crate) fn snapshot_prefix(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.prefix
            .update(cx, |s, cx| s.set_value(text.to_string(), window, cx));
        self.save_prefix(window, cx);
    }

    /// Save the branch prefix as git will take it, and say so if that isn't what was typed.
    fn save_prefix(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.prefix.read(cx).value().trim().to_string();
        let clean = flotilla_model::lifecycle::clean_branch_prefix(&typed);
        // The trailing slash it adds isn't worth a note.
        let changed = typed.trim_end_matches('/') != clean.trim_end_matches('/');
        self.prefix_note = changed.then(|| match clean.as_str() {
            "" => "Git doesn't allow that; no prefix".to_string(),
            c => format!("Saved as {c} (git doesn't allow those characters)"),
        });
        if typed != clean {
            self.prefix
                .update(cx, |s, cx| s.set_value(clean.clone(), window, cx));
        }
        if clean != self.saved_prefix {
            self.saved_prefix = clean.clone();
            cx.emit(SettingsEvent::BranchPrefix(clean));
        }
        cx.notify();
    }

    /// Save the chosen model ("Off" is empty) and test it.
    fn save_summary_model(&mut self, cx: &mut Context<Self>) {
        let model = self
            .summary_model
            .read(cx)
            .selected_value()
            .map(|(_, id)| id.clone())
            .unwrap_or_default();
        cx.emit(SettingsEvent::SummaryModel(model));
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let key = self.key.read(cx).value().trim().to_string();
        if !key.is_empty() {
            cx.emit(SettingsEvent::SaveKey(key));
        }
    }

    fn confirm_rebuild(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let this = this.clone();
            alert
                .title("Rebuild from boat?")
                .description("Downloads every thread again. The old database is kept as a backup.")
                .confirm()
                .ok_text("Rebuild")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, _, cx| {
                    let _ = this.update(cx, |_, cx| cx.emit(SettingsEvent::Rebuild));
                    true
                })
        });
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let heading = |t: &'static str| div().text_sm().font_semibold().child(t);
        let note = |t: &'static str| div().text_xs().text_color(theme.muted_foreground).child(t);
        let section = || {
            v_flex()
                .gap_2()
                .py_3()
                .border_b_1()
                .border_color(theme.border)
        };
        let scroll = || {
            v_flex()
                .id("settings-scroll")
                .max_h(window.viewport_size().height - px(200.))
                .overflow_y_scroll()
                .pr_2()
        };
        let Some(settings) = &self.settings else {
            let (server, devices) = self.render_server(cx);
            return scroll()
                .child(server)
                .children(devices)
                .child(self.render_appearance(cx))
                .child(self.render_notifications(cx));
        };

        let mut environments = section().child(heading("boat environments"));
        for p in &self.environments {
            environments = environments.child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(
                        div()
                            .font_family(theme.mono_font_family.clone())
                            .child(p.name.clone()),
                    )
                    .when(p.is_default, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("default"),
                        )
                    }),
            );
        }

        let mut projects_section = section().child(
            h_flex()
                .child(div().flex_1().child(heading("Projects")))
                .child(
                    Button::new("add-project")
                        .small()
                        .outline()
                        .icon(IconName::Plus)
                        .label("Add project")
                        .tooltip("Add a GitHub repo")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::AddProject))),
                ),
        );
        if settings.projects.is_empty() {
            projects_section = projects_section.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("No projects yet."),
            );
        }
        for row in &settings.projects {
            let p = &row.project;
            let id = p.id.clone();
            let branch = &p.default_branch;
            let (status, color) = match (&row.last_run, &p.template_sha) {
                (Some(r), _) if r.running() => (
                    format!("Setting up {branch}@{}…", short(&r.sha)),
                    theme.warning,
                ),
                (Some(r), _) if r.state == "failed" => (
                    format!(
                        "Setup failed at {}",
                        r.step.clone().unwrap_or_else(|| "setup".into())
                    ),
                    theme.danger,
                ),
                (_, Some(sha)) => (format!("Ready on {branch}@{}", short(sha)), theme.success),
                _ => ("Not set up yet".to_string(), theme.muted_foreground),
            };
            projects_section = projects_section.child(
                h_flex()
                    .gap_2()
                    .py_1()
                    .child(
                        Icon::new(IconName::Box)
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().font_semibold().child(p.repo.clone()))
                            .child(
                                h_flex()
                                    .gap_1()
                                    .text_xs()
                                    .child(
                                        div()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("{} ·", p.full_name())),
                                    )
                                    .child(div().text_color(color).child(status)),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("project-{}", p.id)))
                            .small()
                            .outline()
                            .label("Settings")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(SettingsEvent::OpenProject(id.clone()))
                            })),
                    ),
            );
        }

        projects_section = projects_section.child(
            v_flex()
                .gap_1()
                .pt_2()
                .child(heading("Branch prefix"))
                .child(h_flex().child(div().w(px(240.)).child(Input::new(&self.prefix).small())))
                .children(
                    self.prefix_note
                        .clone()
                        .map(|n| div().text_xs().text_color(theme.warning).child(n)),
                ),
        );

        let (server, devices) = self.render_server(cx);
        // Taller than a small window: the content scrolls inside the dialog.
        scroll()
            .child(server)
            .children(devices)
            .child(self.render_setup(cx))
            .child(projects_section)
            .child(self.render_appearance(cx))
            .child(self.render_notifications(cx))
            .child(
                section()
                    .child(heading("New threads"))
                    .child(match &self.defaults {
                        Some(p) => h_flex()
                            .child(
                                div()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(theme.border)
                                    .p_1()
                                    .child(p.clone()),
                            )
                            .into_any_element(),
                        None => note("The model catalog hasn't loaded yet.").into_any_element(),
                    }),
            )
            .child({
                let weak = cx.weak_entity();
                let offered: Vec<Skill> = self
                    .mac_skills
                    .iter()
                    .filter(|m| !settings.skills.iter().any(|s| s.name == m.name))
                    .cloned()
                    .collect();
                let add = Button::new("add-skill")
                    .small()
                    .outline()
                    .icon(IconName::Plus)
                    .label("Add skill")
                    .dropdown_menu(move |menu, _, _| {
                        let menu = offered.iter().fold(menu, |menu, s| {
                            let (weak, skill) = (weak.clone(), s.clone());
                            menu.item(PopupMenuItem::new(s.name.clone()).on_click(
                                move |_, _, cx| {
                                    let skill = skill.clone();
                                    let _ = weak
                                        .update(cx, |this, cx| this.add_skills(vec![skill], cx));
                                },
                            ))
                        });
                        let weak = weak.clone();
                        let menu = if offered.is_empty() {
                            menu
                        } else {
                            menu.separator()
                        };
                        let menu = menu.scrollable(true).max_h(px(320.));
                        menu.item(PopupMenuItem::new("Choose a folder…").on_click(
                            move |_, _, cx| {
                                let _ = weak.update(cx, |this, cx| this.choose_skill_folders(cx));
                            },
                        ))
                    });
                section()
                    .child(heading("Instructions for every agent"))
                    .child(
                        div()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme.border)
                            .px_2()
                            .py_1()
                            .child(Textarea::new(&self.instructions).appearance(false)),
                    )
                    .child(div().pt_2().child(heading("Skills for every agent")))
                    .children(settings.skills.iter().map(|s| {
                        let name = s.name.clone();
                        let update = self.updatable(s);
                        let now = flotilla_model::util::now_ms();
                        let who = if s.from == self.me {
                            THIS_MACHINE_LOWER.to_string()
                        } else {
                            s.from_name.clone()
                        };
                        let adding = self.adding.iter().any(|(n, _)| *n == s.name);
                        h_flex()
                            .gap_2()
                            .items_start()
                            .text_sm()
                            .child(
                                div().flex_none().pt(px(3.)).child(
                                    Icon::new(IconName::Sparkles)
                                        .xsmall()
                                        .text_color(theme.muted_foreground),
                                ),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .font_medium()
                                                    .child(s.name.clone()),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(s.description.clone()),
                                            ),
                                    )
                                    .child(
                                        div().text_xs().text_color(theme.muted_foreground).child(
                                            if adding {
                                                "Updating…".to_string()
                                            } else {
                                                format!(
                                                    "From {who} · {}",
                                                    crate::fmt::ago_phrase(s.added_ms, now)
                                                )
                                            },
                                        ),
                                    ),
                            )
                            .when_some(update.filter(|_| !adding), |el, skill| {
                                el.child(
                                    Button::new(SharedString::from(format!("update-skill-{name}")))
                                        .ghost()
                                        .xsmall()
                                        .icon(IconName::RefreshCw)
                                        .tooltip("Update from its folder")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.add_skills(vec![skill.clone()], cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new(SharedString::from(format!("remove-skill-{name}")))
                                    .ghost()
                                    .xsmall()
                                    .icon(IconName::X)
                                    .tooltip("Remove")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let name = name.clone();
                                        this.chose(
                                            |s| s.skills.retain(|s| s.name != name),
                                            SettingsEvent::RemoveSkill(name.clone()),
                                            cx,
                                        );
                                    })),
                            )
                    }))
                    .children(
                        self.adding
                            .iter()
                            .filter(|(n, _)| !settings.skills.iter().any(|s| &s.name == n))
                            .map(|(name, _)| {
                                h_flex()
                                    .gap_2()
                                    .text_sm()
                                    .child(
                                        Icon::new(IconName::Sparkles)
                                            .xsmall()
                                            .text_color(theme.muted_foreground),
                                    )
                                    .child(div().flex_none().font_medium().child(name.clone()))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("Adding…"),
                                    )
                            }),
                    )
                    .child(h_flex().child(add))
                    .children(
                        self.skills_note
                            .clone()
                            .map(|n| div().text_xs().text_color(theme.warning).child(n)),
                    )
            })
            .child(
                section().child(heading("Stop idle VMs after")).child(
                    h_flex().child(
                        ButtonGroup::new("idle-minutes")
                            .outline()
                            .small()
                            .children(IDLE_MINUTE_CHOICES.map(|m| {
                                segment(
                                    Button::new(SharedString::from(format!("idle-{m}")))
                                        .label(format!("{m} min")),
                                    m == settings.idle_minutes,
                                    &theme,
                                )
                            }))
                            .on_click(cx.listener(|this, picked: &Vec<usize>, _, cx| {
                                if let Some(&m) =
                                    picked.first().and_then(|&i| IDLE_MINUTE_CHOICES.get(i))
                                {
                                    this.chose(
                                        |s| s.idle_minutes = m,
                                        SettingsEvent::IdleMinutes(m),
                                        cx,
                                    );
                                }
                            })),
                    ),
                ),
            )
            .child(
                section()
                    .child(heading("Archiving"))
                    .child(
                        Switch::new("archive-on-merge")
                            .small()
                            .label("Archive a thread when its pull request merges")
                            .checked(settings.archive_on_merge)
                            .on_click(cx.listener(|this, on: &bool, _, cx| {
                                let on = *on;
                                this.chose(
                                    |s| s.archive_on_merge = on,
                                    SettingsEvent::ArchiveOnMerge(on),
                                    cx,
                                );
                            })),
                    )
                    .child(div().pt_1().text_sm().child("Archive threads idle for"))
                    .child(
                        h_flex().child(
                            ButtonGroup::new("archive-idle-days")
                                .outline()
                                .small()
                                .children(flotilla_model::tidy::IDLE_DAY_CHOICES.map(|d| {
                                    segment(
                                        Button::new(SharedString::from(format!(
                                            "archive-idle-{d}"
                                        )))
                                        .label(
                                            if d == 0 {
                                                "Never".to_string()
                                            } else {
                                                format!("{d} days")
                                            },
                                        ),
                                        d == settings.archive_idle_days,
                                        &theme,
                                    )
                                }))
                                .on_click(cx.listener(|this, picked: &Vec<usize>, _, cx| {
                                    if let Some(&d) = picked.first().and_then(|&i| {
                                        flotilla_model::tidy::IDLE_DAY_CHOICES.get(i)
                                    }) {
                                        this.chose(
                                            |s| s.archive_idle_days = d,
                                            SettingsEvent::ArchiveIdleDays(d),
                                            cx,
                                        );
                                    }
                                })),
                        ),
                    ),
            )
            .child({
                let (icon, color, text) = match &self.summary_state {
                    SummaryState::Off => (
                        IconName::CircleX,
                        theme.muted_foreground,
                        "Off: no model-written titles".to_string(),
                    ),
                    SummaryState::Unknown => (
                        IconName::Clock,
                        theme.muted_foreground,
                        "Not run yet".to_string(),
                    ),
                    SummaryState::Testing => (
                        IconName::Clock,
                        theme.muted_foreground,
                        "Testing…".to_string(),
                    ),
                    SummaryState::Working(at) => (
                        IconName::CircleCheck,
                        theme.success,
                        format!(
                            "Working · last ran {}",
                            crate::fmt::clock(at).unwrap_or_default()
                        ),
                    ),
                    SummaryState::Failed(e) => (
                        IconName::TriangleAlert,
                        theme.danger,
                        format!("Failing: {e}"),
                    ),
                };
                section()
                    .child(heading("Titles and previews"))
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(theme.border)
                                    .p_1()
                                    .child(
                                        Select::new(&self.summary_model)
                                            .appearance(false)
                                            .small()
                                            .menu_width(px(320.))
                                            .search_placeholder("Search models"),
                                    ),
                            )
                            .when(self.summary_state != SummaryState::Off, |el| {
                                el.child(
                                    Button::new("test-summary-model")
                                        .small()
                                        .outline()
                                        .label("Test again")
                                        .disabled(self.summary_state == SummaryState::Testing)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.save_summary_model(cx)
                                        })),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .items_start()
                            .text_xs()
                            .text_color(color)
                            .child(div().flex_none().pt(px(1.)).child(Icon::new(icon).xsmall()))
                            .child(div().flex_1().min_w_0().child(text)),
                    )
            })
            // Nothing to list (projects' own environments aren't shown): no section.
            .when(!self.environments.is_empty(), |el| el.child(environments))
            .child(
                section().child(heading("Plan and billing")).child(
                    h_flex().child(
                        Button::new("billing")
                            .small()
                            .outline()
                            .icon(IconName::ExternalLink)
                            .label("Open boat.dev billing")
                            .on_click(|_, _, cx| cx.open_url(BILLING_URL)),
                    ),
                ),
            )
            .child(
                v_flex()
                    .gap_2()
                    .pt_3()
                    .child(heading(if self.server.is_some() {
                        "Data on the server"
                    } else {
                        "Local data"
                    }))
                    .child(
                        div()
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .child(settings.database.clone()),
                    )
                    .child(
                        h_flex().child(
                            Button::new("rebuild")
                                .small()
                                .outline()
                                .icon(IconName::RefreshCw)
                                .label("Rebuild from boat…")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.confirm_rebuild(window, cx)
                                })),
                        ),
                    ),
            )
    }
}

/// `macos` → macOS, as people write it.
fn platform_name(platform: &str) -> &str {
    match platform {
        "macos" => "macOS",
        "linux" => "Linux",
        "ios" => "iOS",
        "android" => "Android",
        "windows" => "Windows",
        other => other,
    }
}

/// A segmented control's segment: the chosen one filled, as a switch that's on is, so it
/// reads at a glance in Day and Night.
fn segment(button: Button, selected: bool, theme: &gpui_kit::component::Theme) -> Button {
    button.selected(selected).when(selected, |b| {
        b.bg(theme.primary)
            .text_color(theme.primary_foreground)
            .border_color(theme.primary)
    })
}

/// An address only this machine can reach (`http://127.0.0.1:7778`, `[::1]`, `localhost`).
fn loopback(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// What the checks found, as Settings shows them opened: each check, what it says and the
/// commands that fix it, with Copy. `key` is This Mac's boat key field: under the key's check, or
/// in a row of its own when the checks didn't look at the key (offline).
pub(crate) fn check_rows(
    doctor: &Doctor,
    key: Option<&dyn Fn() -> AnyElement>,
    theme: &Theme,
) -> Vec<AnyElement> {
    let mut rows = vec![];
    for (i, c) in doctor.checks.iter().enumerate() {
        let (icon, color) = match c.health {
            Health::Ok => (IconName::CircleCheck, theme.success),
            Health::Warn => (IconName::TriangleAlert, theme.warning),
            Health::Fail => (IconName::CircleX, theme.danger),
            Health::Off => (IconName::Circle, theme.muted_foreground),
        };
        let says = match c.health {
            Health::Fail => theme.danger,
            Health::Warn => theme.warning,
            _ => theme.muted_foreground,
        };
        let mut row = v_flex().gap_1().child(
            h_flex()
                .gap_2()
                .items_start()
                .child(
                    div()
                        .flex_none()
                        .pt(px(2.))
                        .child(Icon::new(icon).small().text_color(color)),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_sm().font_medium().child(c.check.label()))
                        .child(div().text_xs().text_color(says).child(c.says.clone())),
                ),
        );
        for (j, fix) in c.fix.iter().enumerate() {
            let copy = fix.clone();
            row = row.child(
                h_flex()
                    .pl_6()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(theme.muted)
                            .text_xs()
                            .font_family(theme.mono_font_family.clone())
                            .child(fix.clone()),
                    )
                    .child(
                        Button::new(SharedString::from(format!("copy-fix-{i}-{j}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Copy)
                            .tooltip("Copy")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                            }),
                    ),
            );
        }
        if c.check == CheckOf::BoatKey
            && let Some(key) = key
        {
            row = row.child(key());
        }
        rows.push(row.into_any_element());
    }
    if let Some(key) = key
        && !doctor.checks.iter().any(|c| c.check == CheckOf::BoatKey)
    {
        rows.push(key_row(key(), theme));
    }
    rows
}

/// The boat key's field with no check of the key to sit under: its own row, named.
fn key_row(field: AnyElement, theme: &Theme) -> AnyElement {
    v_flex()
        .gap_1()
        .child(
            h_flex()
                .gap_2()
                .child(
                    div().flex_none().child(
                        Icon::new(IconName::Circle)
                            .small()
                            .text_color(theme.muted_foreground),
                    ),
                )
                .child(
                    div()
                        .text_sm()
                        .font_medium()
                        .child(CheckOf::BoatKey.label()),
                ),
        )
        .child(field)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_this_machine_s_own_addresses_are_loopback() {
        use super::loopback;
        assert!(loopback("http://127.0.0.1:7778"));
        assert!(loopback("http://[::1]:7778/"));
        assert!(loopback("http://localhost:7778"));
        assert!(!loopback("https://laptop.example-tailnet.ts.net"));
        assert!(!loopback("http://100.64.0.7:7778"));
    }
}
