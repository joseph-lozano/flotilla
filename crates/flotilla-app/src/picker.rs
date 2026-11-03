//! The agent / model / effort / fast picker, shared by the new-thread page and the defaults
//! in Settings. One searchable list holds every model, grouped by agent.

use crate::assets::provider_logo;
use crate::fmt;
use flotilla_proto::{Catalog, Model, ThreadSettings};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::menu::{DropdownMenu, PopupMenuItem};
use gpui_kit::component::searchable_list::{
    SearchableGroup, SearchableListDelegate, SearchableListItem, SearchableVec,
};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::{ActiveTheme, Icon, IndexPath, Selectable, Sizable, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::cell::RefCell;
use std::rc::Rc;

/// One model in a list: what it's called and which it is. The picker's rows and button are
/// drawn from what's picked when they're drawn, so they follow it (and the theme) as it
/// changes, with the menu open or not.
#[derive(Clone)]
pub struct ModelChoice {
    label: SharedString,
    /// (provider key, model id)
    value: (String, String),
    /// The model picker's choice (none in the summary model's list): its button shows that.
    picked: Option<Rc<RefCell<Picked>>>,
}

/// What a model picker has picked: the models in the order they were, the first the chosen
/// one (the select's, which the effort and fast buttons are for) and the rest ⇧-clicked
/// beside it.
#[derive(Default)]
pub struct Picked {
    models: Vec<(String, String)>,
    /// The button's names: the first model's, then the rest.
    title: SharedString,
}

impl Picked {
    fn has(&self, value: &(String, String)) -> bool {
        self.models.contains(value)
    }
}

/// ⇧-click on `value`: add it after the rest, or take it away while another is left (the
/// first too: the next is first then). Whether the first one changed.
fn toggled(models: &mut Vec<(String, String)>, value: (String, String)) -> bool {
    match models.iter().position(|m| *m == value) {
        Some(_) if models.len() == 1 => false,
        Some(at) => {
            models.remove(at);
            at == 0
        }
        None => {
            models.push(value);
            models.len() == 1
        }
    }
}

/// Every model picked, each with an effort and fast that suit it, as near `effort` and `fast`
/// (the first one's) as it allows.
fn fitted(
    catalog: &Catalog,
    models: &[(String, String)],
    effort: Option<&str>,
    fast: bool,
) -> Vec<ThreadSettings> {
    models
        .iter()
        .filter_map(|(p, m)| catalog.fit(p, m, effort, fast).ok())
        .collect()
}

/// A logo and a name, the logo picked for the theme showing when it's drawn.
#[derive(IntoElement)]
struct Named {
    provider: String,
    label: SharedString,
}

impl RenderOnce for Named {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let logo = provider_logo(&self.provider, cx.theme().mode.is_dark());
        h_flex()
            .gap_2()
            .children(logo.map(|p| img(p).size(px(14.))))
            .child(self.label)
    }
}

impl SearchableListItem for ModelChoice {
    type Value = (String, String);

    fn title(&self) -> SharedString {
        match &self.picked {
            Some(p) => p.borrow().title.clone(),
            None => self.label.clone(),
        }
    }

    fn display_title(&self) -> Option<AnyElement> {
        let (provider, label) = match self.picked.as_ref().map(|p| p.borrow()) {
            Some(p) => (
                p.models.first().map(|c| c.0.clone()).unwrap_or_default(),
                p.title.clone(),
            ),
            None => (self.value.0.clone(), self.label.clone()),
        };
        Some(Named { provider, label }.into_any_element())
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        choice_row(self, cx)
    }

    fn value(&self) -> &(String, String) {
        &self.value
    }

    fn matches(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        [
            self.label.as_ref(),
            self.value.1.as_str(),
            fmt::provider_name(&self.value.0),
        ]
        .iter()
        .any(|s| s.to_lowercase().contains(&q))
    }
}

/// A row's logo (its room kept without one, so every name starts in line), name and id.
fn choice_row(item: &ModelChoice, cx: &App) -> Div {
    let theme = cx.theme();
    let id = &item.value.1;
    let logo = provider_logo(&item.value.0, theme.mode.is_dark());
    let with_id = !id.is_empty() && item.label.as_ref() != id;
    h_flex()
        .flex_1()
        .min_w_0()
        .gap_2()
        .child(
            div()
                .flex_none()
                .size(px(14.))
                .children(logo.map(|p| img(p).size(px(14.)))),
        )
        .child(
            div()
                .min_w_0()
                .when(with_id, |el| el.flex_shrink_0())
                .truncate()
                .child(item.label.clone()),
        )
        .when(with_id, |el| {
            el.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(id.clone()),
            )
        })
}

/// Adds a model beside the chosen one, or takes it away, with the menu left open.
type Toggle = Rc<dyn Fn((String, String), &mut Window, &mut App)>;

/// Every model, grouped by agent, with the ones ⇧-clicked beside the chosen one checked too.
pub struct ModelList {
    groups: SearchableVec<SearchableGroup<ModelChoice>>,
    picked: Rc<RefCell<Picked>>,
    /// New thread: ⇧-click, or a row's + or −, adds or takes away a model.
    toggle: Option<Toggle>,
}

/// The group the rows' hover marks answer to.
const ROW: &str = "model-row";

impl SearchableListDelegate for ModelList {
    type Item = ModelChoice;

    fn sections_count(&self, cx: &App) -> usize {
        self.groups.sections_count(cx)
    }

    #[allow(deprecated)]
    fn section(&self, section: usize) -> Option<AnyElement> {
        self.groups.section(section)
    }

    fn items_count(&self, section: usize) -> usize {
        self.groups.items_count(section)
    }

    fn item(&self, ix: IndexPath) -> Option<&ModelChoice> {
        self.groups.item(ix)
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        ModelChoice: SearchableListItem<Value = V>,
        V: PartialEq,
    {
        self.groups.position(value)
    }

    fn perform_search(&mut self, query: &str, window: &mut Window, cx: &mut App) -> Task<()> {
        self.groups.perform_search(query, window, cx)
    }

    fn is_item_checked(
        &self,
        _: IndexPath,
        item: &ModelChoice,
        _: &[(IndexPath, ModelChoice)],
        _: &App,
    ) -> bool {
        self.picked.borrow().has(&item.value)
    }

    /// The row, with a check on what's picked. In New thread a + shows on the row under the
    /// pointer (a − on a picked one, while another is picked too): it, or ⇧-click, adds the
    /// model beside the chosen one, or takes it away, and the menu stays open where it was.
    fn render_item(
        &self,
        _: IndexPath,
        item: &ModelChoice,
        checked: bool,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        let picked = self.picked.borrow();
        let several = picked.models.len() > 1;
        let icon = |name: IconName| Icon::new(name).xsmall();
        let mark = div().flex_none().relative().size(px(14.));
        let mark = match &self.toggle {
            None => mark.when(checked, |el| el.child(icon(IconName::Check).text_color(fg))),
            Some(toggle) => {
                let (toggle, value) = (toggle.clone(), item.value.clone());
                // The hover mark: + to add; − to take away, the check showing otherwise.
                let (rest, hover) = match (checked, several) {
                    (true, true) => (
                        Some(icon(IconName::Check).text_color(fg)),
                        Some(IconName::Minus),
                    ),
                    (true, false) => (Some(icon(IconName::Check).text_color(fg)), None),
                    (false, _) => (None, Some(IconName::Plus)),
                };
                mark.children(rest.map(|i| {
                    div()
                        .absolute()
                        .inset_0()
                        .when(hover.is_some(), |el| el.group_hover(ROW, |s| s.opacity(0.)))
                        .child(i)
                }))
                .children(hover.map(|h| {
                    div()
                        .absolute()
                        .inset_0()
                        .opacity(0.)
                        .group_hover(ROW, |s| s.opacity(1.))
                        .child(icon(h).text_color(muted))
                }))
                .when(hover.is_some(), |el| {
                    el.cursor_pointer()
                        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                            cx.stop_propagation();
                            toggle(value.clone(), window, cx);
                        })
                })
            }
        };
        let row = h_flex()
            .group(ROW)
            .w_full()
            .min_w_0()
            .gap_2()
            .child(choice_row(item, cx))
            .child(mark);
        let row = match &self.toggle {
            // ⇧-click anywhere on the row: as its mark.
            Some(toggle) => {
                let (toggle, value) = (toggle.clone(), item.value.clone());
                row.on_mouse_down(MouseButton::Left, move |e, window, cx| {
                    if e.modifiers.shift {
                        cx.stop_propagation();
                        toggle(value.clone(), window, cx);
                    }
                })
            }
            None => row,
        };
        Some(row.into_any_element())
    }
}

pub enum PickerEvent {
    Changed,
}

pub struct ThreadSettingsPicker {
    catalog: Catalog,
    model: Entity<SelectState<ModelList>>,
    /// The models picked: the chosen one (the select's), then others ⇧-clicked beside it (New
    /// thread only: `several`); shared with the list's rows and button.
    picked: Rc<RefCell<Picked>>,
    /// The chosen model changed with the menu open: the select's own choice follows when it
    /// closes (moving it now would scroll the open list).
    resync: bool,
    effort: Option<String>,
    fast: bool,
    /// Where the model chip is in the window, as last laid out: how tall its menu may be.
    chip: Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PickerEvent> for ThreadSettingsPicker {}

/// The model menu's list at most: the select's own default (20rem).
const MENU_LIST: f32 = 320.;
/// What the menu takes besides its list (the search field), what the window keeps clear at
/// its edge, and a few points to spare.
const MENU_CHROME: f32 = 26. + 8. + 6.;
/// The list it's capped to at the least, with the room it leaves: its smallest menu (the
/// search field, a group's heading and one model, 87 points; with nothing found, 100) never
/// fits in less room than this leaves below the chip, so it stays above.
const MENU_LIST_MIN: f32 = 40.;

/// How tall the model menu's list may be, with `room` points below its chip: short enough
/// that the whole menu fits below, so it stays there as a search narrows it. (It goes below
/// when it fits, else above: a menu that opened above, too tall to fit below, moved below as
/// soon as what you typed made it short enough.) With less room below than even its smallest
/// menu needs, as in a thread's composer at the window's foot, it's above whatever its
/// length: `None`, the select's own height.
fn menu_list_height(room: f32) -> Option<f32> {
    let fits = room - MENU_CHROME;
    (fits >= MENU_LIST_MIN).then_some(fits.min(MENU_LIST))
}

impl ThreadSettingsPicker {
    /// Starts on `initial` when the catalog still offers it, else on Claude Code's default.
    /// Only the agents whose subagents Flotilla can show are offered. `several`: ⇧-click
    /// adds more models (New thread starts one thread for each).
    pub fn new(
        catalog: Catalog,
        initial: Option<ThreadSettings>,
        several: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let start = initial
            .filter(|s| offered(&s.provider))
            .and_then(|s| {
                catalog
                    .validate(&s.provider, &s.model, s.effort.as_deref(), s.fast)
                    .ok()
            })
            .or_else(|| default_settings(&catalog));
        let chosen = start
            .as_ref()
            .map(|s| (s.provider.clone(), s.model.clone()));
        let picked = Rc::new(RefCell::new(Picked::default()));
        let weak = cx.weak_entity();
        let toggle: Option<Toggle> = several.then(|| {
            Rc::new(
                move |value: (String, String), window: &mut Window, cx: &mut App| {
                    let _ = weak.update(cx, |p, cx| {
                        p.toggle(value, window, cx);
                        cx.emit(PickerEvent::Changed);
                    });
                },
            ) as Toggle
        });
        let list = model_list(&catalog, picked.clone(), toggle);
        let index = chosen.as_ref().and_then(|v| list.groups.position(v));
        let model = cx.new(|cx| SelectState::new(list, index, window, cx).searchable(true));
        let sub = cx.subscribe_in(
            &model,
            window,
            |this, _, event: &SelectEvent<ModelList>, _, cx| {
                // A plain pick (a ⇧-click never gets here: the row takes it).
                if let SelectEvent::Confirm(Some(value)) = event {
                    this.choose(value.clone(), cx);
                    cx.emit(PickerEvent::Changed);
                }
            },
        );
        // Closed: the select's choice catches up with the chosen model.
        let closed = cx.subscribe_in(
            &model,
            window,
            |this, model, _: &DismissEvent, window, cx| {
                if std::mem::take(&mut this.resync) {
                    let chosen = this.picked.borrow().models.first().cloned();
                    let model = model.clone();
                    window.defer(cx, move |window, cx| {
                        if let Some(c) = chosen {
                            model.update(cx, |m, cx| m.set_selected_value(&c, window, cx));
                        }
                    });
                }
            },
        );
        let mut this = ThreadSettingsPicker {
            effort: start.as_ref().and_then(|s| s.effort.clone()),
            fast: start.as_ref().is_some_and(|s| s.fast),
            catalog,
            model,
            picked,
            resync: false,
            chip: Rc::default(),
            _subscriptions: vec![sub, closed],
        };
        this.set_picked(chosen.into_iter().collect());
        this
    }

    /// What's picked now, with the button's names for it.
    fn set_picked(&mut self, models: Vec<(String, String)>) {
        let title = models
            .iter()
            .map(|(p, m)| label(&self.catalog, p, m))
            .collect::<Vec<_>>()
            .join(" + ");
        *self.picked.borrow_mut() = Picked {
            models,
            title: title.into(),
        };
    }

    /// A plain pick: that model alone.
    fn choose(&mut self, value: (String, String), cx: &mut Context<Self>) {
        self.set_picked(vec![value]);
        self.fit_options_to_model(cx);
    }

    /// ⇧-click: add `value` beside the chosen model, or take it away (the chosen one too, while
    /// another is left). The button shows the first, with the rest after it.
    fn toggle(&mut self, value: (String, String), _window: &mut Window, cx: &mut Context<Self>) {
        let mut models = self.picked.borrow().models.clone();
        let first_changed = toggled(&mut models, value);
        self.set_picked(models);
        if first_changed {
            self.resync = true;
            self.fit_options_to_model(cx);
        }
        // The open list's rows and the button redraw from what's picked.
        self.model.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Dev: choose `s`, as picking it would (when the catalog offers it).
    #[cfg(feature = "snapshot")]
    pub fn snapshot_choose(
        &mut self,
        s: ThreadSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Ok(s) = self
            .catalog
            .validate(&s.provider, &s.model, s.effort.as_deref(), s.fast)
        else {
            return;
        };
        let value = (s.provider.clone(), s.model.clone());
        self.set_picked(vec![value.clone()]);
        let model = self.model.clone();
        window.defer(cx, move |window, cx| {
            model.update(cx, |m, cx| m.set_selected_value(&value, window, cx))
        });
        (self.effort, self.fast) = (s.effort, s.fast);
        cx.emit(PickerEvent::Changed);
        cx.notify();
    }

    /// Dev: ⇧-click `provider`/`model`, as the list would.
    #[cfg(feature = "snapshot")]
    pub fn snapshot_also(
        &mut self,
        provider: &str,
        model: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.catalog.validate(provider, model, None, false).is_ok() {
            self.toggle((provider.to_string(), model.to_string()), window, cx);
            cx.emit(PickerEvent::Changed);
        }
    }

    fn selected_model(&self, _cx: &App) -> Option<(String, Model)> {
        let (provider, model) = self.picked.borrow().models.first().cloned()?;
        let m = self
            .catalog
            .provider(&provider)?
            .models
            .iter()
            .find(|m| m.id == model)?
            .clone();
        Some((provider, m))
    }

    /// Keep the effort if the new model supports it, else its default; drop fast if unsupported.
    fn fit_options_to_model(&mut self, cx: &mut Context<Self>) {
        let model = self.selected_model(cx).map(|(_, m)| m);
        let opts = model.as_ref().and_then(|m| m.effort.clone());
        self.effort = match opts {
            Some(o)
                if self
                    .effort
                    .as_ref()
                    .is_some_and(|e| o.supported.contains(e)) =>
            {
                self.effort.clone()
            }
            Some(o) => o.default.clone().or_else(|| o.supported.first().cloned()),
            None => None,
        };
        if !model.is_some_and(|m| m.fast_mode) {
            self.fast = false;
        }
        cx.notify();
    }

    /// The chosen model (the first), validated against the catalog: Settings' defaults.
    pub fn settings(&self, _cx: &App) -> Option<ThreadSettings> {
        let (provider, model) = self.picked.borrow().models.first().cloned()?;
        self.catalog
            .validate(&provider, &model, self.effort.as_deref(), self.fast)
            .ok()
    }

    /// Every model chosen, first the one the effort and fast buttons are for, each with its
    /// name and with an effort and fast that suit it (as near those as it allows).
    pub fn chosen(&self, _cx: &App) -> Vec<(ThreadSettings, String)> {
        fitted(
            &self.catalog,
            &self.picked.borrow().models,
            self.effort.as_deref(),
            self.fast,
        )
        .into_iter()
        .map(|s| (s.clone(), label(&self.catalog, &s.provider, &s.model)))
        .collect()
    }
}

/// A model's name as the catalog gives it (its id when it has none).
fn label(catalog: &Catalog, provider: &str, model: &str) -> String {
    catalog
        .provider(provider)
        .and_then(|p| p.models.iter().find(|m| m.id == model))
        .and_then(|m| m.label.clone())
        .unwrap_or_else(|| model.to_string())
}

/// The picker's list: every offered model by agent.
fn model_list(catalog: &Catalog, picked: Rc<RefCell<Picked>>, toggle: Option<Toggle>) -> ModelList {
    let groups: Vec<SearchableGroup<ModelChoice>> = catalog
        .providers
        .iter()
        .filter(|p| offered(&p.key))
        .map(|p| SearchableGroup {
            title: fmt::provider_name(&p.key).to_string().into(),
            items: p
                .models
                .iter()
                .map(|m| ModelChoice {
                    label: m.label.clone().unwrap_or_else(|| m.id.clone()).into(),
                    value: (p.key.clone(), m.id.clone()),
                    picked: Some(picked.clone()),
                })
                .collect(),
        })
        .collect();
    ModelList {
        groups: SearchableVec::new(groups),
        picked,
        toggle,
    }
}

pub type SummaryModelList = SearchableVec<ModelChoice>;

/// Choices for the summary model: "Off", then the Claude models in boat's catalog by the full
/// id the local `claude` CLI takes (boat's own ids include aliases and Bedrock models). A
/// stored model the catalog doesn't list keeps its own entry, so it still shows.
pub fn summary_model_choices(
    catalog: Option<&Catalog>,
    current: &str,
) -> (SummaryModelList, Option<IndexPath>) {
    let choice = |label: String, id: &str| ModelChoice {
        label: label.into(),
        value: ("claude-code".into(), id.to_string()),
        picked: None,
    };
    let mut items = vec![ModelChoice {
        label: "Off".into(),
        value: (String::new(), String::new()),
        picked: None,
    }];
    let models = catalog
        .and_then(|c| c.provider("claude-code"))
        .map(|p| p.models.as_slice())
        .unwrap_or_default();
    for m in models {
        let Some(id) = m.sdk_id.as_deref().filter(|id| id.starts_with("claude-")) else {
            continue;
        };
        if !items.iter().any(|i| i.value.1 == id) {
            items.push(choice(
                m.label.clone().unwrap_or_else(|| id.to_string()),
                id,
            ));
        }
    }
    let current = current.trim();
    if !items.iter().any(|i| i.value.1 == current) {
        items.insert(1, choice(current.to_string(), current));
    }
    let index = items
        .iter()
        .position(|i| i.value.1 == current)
        .map(IndexPath::new);
    (SearchableVec::new(items), index)
}

/// The agents offered for new threads: those whose subagents Flotilla can show (Claude Code,
/// Codex, OpenCode). Threads on others still open as they are.
fn offered(provider: &str) -> bool {
    flotilla_model::lineage::reads(provider)
}

/// Claude Code's default model, else the first offered agent's.
pub fn default_settings(catalog: &Catalog) -> Option<ThreadSettings> {
    let p = catalog
        .provider("claude-code")
        .or_else(|| catalog.providers.iter().find(|p| offered(&p.key)))?;
    let model = p
        .default_model
        .clone()
        .or_else(|| p.models.first().map(|m| m.id.clone()))?;
    catalog.validate(&p.key, &model, None, false).ok()
}

impl Render for ThreadSettingsPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.selected_model(cx).map(|(_, m)| m);
        let weak = cx.weak_entity();
        let effort: Option<AnyElement> =
            model.as_ref().and_then(|m| m.effort.clone()).map(|opts| {
                let current = self.effort.clone();
                let label = current.clone().unwrap_or_else(|| "effort".into());
                Button::new("effort")
                    .ghost()
                    .small()
                    .icon(IconName::Gauge)
                    .label(capitalize(&label))
                    .dropdown_caret(true)
                    .tooltip("Reasoning effort")
                    .dropdown_menu(move |menu, _, _| {
                        opts.supported.iter().fold(menu, |menu, level| {
                            let weak = weak.clone();
                            let level = level.clone();
                            menu.item(
                                PopupMenuItem::new(capitalize(&level))
                                    .checked(current.as_deref() == Some(level.as_str()))
                                    .on_click(move |_, _, cx| {
                                        let level = level.clone();
                                        let _ = weak.update(cx, |p, cx| {
                                            p.effort = Some(level);
                                            cx.emit(PickerEvent::Changed);
                                            cx.notify();
                                        });
                                    }),
                            )
                        })
                    })
                    .into_any_element()
            });
        let fast = model.as_ref().filter(|m| m.fast_mode).map(|_| {
            Button::new("fast")
                .ghost()
                .small()
                .icon(IconName::Zap)
                .label("Fast")
                .selected(self.fast)
                .tooltip(if self.fast {
                    "Fast mode on"
                } else {
                    "Fast mode off"
                })
                .on_click(cx.listener(|p, _, _, cx| {
                    p.fast = !p.fast;
                    cx.emit(PickerEvent::Changed);
                    cx.notify();
                }))
        });
        let room = self
            .chip
            .get()
            .map(|b| f32::from(window.viewport_size().height - b.bottom()));
        let chip = self.chip.clone();
        h_flex()
            .gap_1()
            .child(
                div()
                    .relative()
                    .child(
                        Select::new(&self.model)
                            .appearance(false)
                            .small()
                            .menu_width(px(320.))
                            .when_some(room.and_then(menu_list_height), |s, h| s.menu_max_h(px(h)))
                            .search_placeholder("Search models"),
                    )
                    .child(
                        canvas(move |bounds, _, _| chip.set(Some(bounds)), |_, _, _, _| {})
                            .absolute()
                            .size_full(),
                    ),
            )
            .children(effort)
            .children(fast)
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{MENU_LIST, fitted, menu_list_height, toggled};

    /// With room below its chip (New thread), the model menu is short enough to fit there
    /// whole, so narrowing it never moves it; with little (a thread's composer), it's left to
    /// go above, as tall as the select makes it.
    #[test]
    fn the_model_menu_fits_below_its_chip_where_there_is_room() {
        // New thread in a 780-point window: the chip's foot at 438.
        let h = menu_list_height(780. - 438.).unwrap();
        assert!(
            h < MENU_LIST && h + super::MENU_CHROME <= 780. - 438.,
            "{h}"
        );
        assert_eq!(menu_list_height(2000.), Some(MENU_LIST));
        // A little room: capped short, so it can't open above and then fit below once a search
        // narrows it to one model.
        assert_eq!(menu_list_height(120.), Some(80.));
        // Less than its smallest menu needs below (87 points, and the 8 kept clear: 79 leaves
        // 71): above, as tall as the select makes it.
        assert_eq!(menu_list_height(79.), None);
        assert_eq!(menu_list_height(36.), None);
    }
    use flotilla_proto::{Catalog, EffortOptions, Model, Provider};

    fn m(p: &str, id: &str) -> (String, String) {
        (p.to_string(), id.to_string())
    }

    #[test]
    fn shift_click_adds_a_model_or_takes_one_away_while_another_is_left() {
        let mut models = vec![m("claude-code", "haiku")];
        // Added after the rest; the first stays first.
        assert!(!toggled(&mut models, m("codex", "sol")));
        assert!(!toggled(&mut models, m("codex", "luna")));
        assert_eq!(
            models,
            vec![
                m("claude-code", "haiku"),
                m("codex", "sol"),
                m("codex", "luna")
            ]
        );
        // One after the first taken away.
        assert!(!toggled(&mut models, m("codex", "luna")));
        assert_eq!(models, vec![m("claude-code", "haiku"), m("codex", "sol")]);
        // The first taken away: the next is first.
        assert!(toggled(&mut models, m("claude-code", "haiku")));
        assert_eq!(models, vec![m("codex", "sol")]);
        // The last one stays.
        assert!(!toggled(&mut models, m("codex", "sol")));
        assert_eq!(models, vec![m("codex", "sol")]);
        // Nothing picked (no catalog default): the first one.
        let mut none = Vec::new();
        assert!(toggled(&mut none, m("codex", "sol")));
        assert_eq!(none, vec![m("codex", "sol")]);
    }

    #[test]
    fn every_model_gets_an_effort_and_fast_that_suit_it() {
        let model = |id: &str, fast_mode: bool, levels: Option<(&[&str], &str)>| Model {
            id: id.into(),
            label: None,
            sdk_id: None,
            fast_mode,
            effort: levels.map(|(s, d)| EffortOptions {
                supported: s.iter().map(|l| l.to_string()).collect(),
                default: Some(d.into()),
            }),
        };
        let provider = |key: &str, models| Provider {
            key: key.into(),
            cli_name: None,
            aliases: vec![],
            default_model: None,
            models,
            order: 0,
        };
        let catalog = Catalog {
            providers: vec![
                provider(
                    "claude-code",
                    vec![
                        model("opus", true, Some((&["low", "high", "max"], "high"))),
                        model("haiku", false, Some((&["low", "medium", "high"], "medium"))),
                    ],
                ),
                provider("codex", vec![model("plain", false, None)]),
            ],
        };
        let got = fitted(
            &catalog,
            &[
                m("claude-code", "opus"),
                m("claude-code", "haiku"),
                m("codex", "plain"),
                m("codex", "gone"),
            ],
            Some("max"),
            true,
        );
        let got: Vec<_> = got
            .iter()
            .map(|s| (s.model.as_str(), s.effort.as_deref(), s.fast))
            .collect();
        // The first as asked; the rest as near as each allows; a model the catalog no longer has
        // left out.
        assert_eq!(
            got,
            vec![
                ("opus", Some("max"), true),
                ("haiku", Some("high"), false),
                ("plain", None, false)
            ]
        );
    }
}
