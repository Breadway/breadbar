//! BAR-30 phases 2-4, BAR-31: the spotlight stage, a full-screen
//! Overlay-layer surface opened from the pill (clicking the time,
//! `breadbar --stage`, or Super+Space through breadbox's redirect). Not the
//! bar window itself: this is its own layer-shell surface, built once in
//! `main.rs::init` and shown/hidden for the App's whole lifetime, the same
//! long-lived-window pattern `panel::PanelSet` already uses.
//!
//! Split from `stage::pure` (GTK-free result grouping/filtering, layout
//! maths, copy, the power confirm state machine, the Esc state machine) so
//! that logic is unit-testable without a display; this module owns every
//! real widget and every real I/O source.
//!
//! Every value here is the mockup's
//! (`bos-ui-demos/proposed/spotlight-stage.html`); the look lives in
//! bread-theme's base.css under the `.stage-*` classes. Motion: the whole
//! stage fades over .28s (a CSS transition on `.stage-root`), the column
//! slides its last 10px on the spring curve over .45s (a frame-clock margin
//! animation, since GTK CSS cannot transition position), and the light cone
//! fades in over .5s after a .05s delay.
//!
//! `Stage` is built as an `Rc<Stage>` (see [`build`]): interactive widgets
//! call straight back into a `Stage` method from their own GTK signal
//! closure. Dynamically-built rows recover an `Rc<Stage>` via `self_rc()`
//! (a `Weak` self-reference set right after construction). Only what
//! touches `App`-level state goes through `AppInput`, always via
//! `input_sender().send` so a torn-down satellite drops the message instead
//! of panicking (see `main.rs::post`).

pub mod pure;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use gtk4::prelude::*;
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};
use relm4::ComponentSender;

use crate::bar::bluetooth::BtPopoverData;
use crate::bar::control::{AudioSink, ControlPanelData};
use crate::bar::media::MediaState;
use crate::motion;
use crate::notifications::history;
use crate::{App, AppInput};
use pure::{OpenWindow, PowerConfirm, StageResults};

/// How many recent-app tiles the idle row shows (the mockup's `RECENT`).
const MAX_RECENT_TILES: usize = 6;
/// At most this many notification rows (`NOTES.slice(0,5)`).
const MAX_NOTES: usize = 5;
/// `.column{padding-top:64px}` and its hidden `translateY(-10px)`.
const COLUMN_TOP: i32 = 64;
const COLUMN_SLIDE: i32 = 10;
/// `.stage{transition:opacity .28s}`.
const FADE_MS: u64 = 280;
/// `.search`, `.recent` and `.results` are all `min(640px,100%)` wide.
const CONTENT_WIDTH: i32 = 640;

const STAGE_APP_ID: &str = "bar";
const STAGE_LAUNCHED_EVENT: &str = "bread.bar.launched";

/// Below this output width (logical px) the floor stacks: the mockup's
/// `@media (max-width:860px)`.
const NARROW_BREAKPOINT_PX: i32 = 860;

/// State every bar instance shares, so a satellite monitor's stage shows the
/// primary's notification history, flips the same do-not-disturb flag, and
/// only one stage is ever open at a time.
#[derive(Clone)]
pub struct Shared {
    pub store: history::Store,
    pub dnd: Rc<Cell<bool>>,
    open: Rc<RefCell<Weak<Stage>>>,
    stages: Rc<RefCell<Vec<Weak<Stage>>>>,
    /// Every pill's unread dot, so opening a stage on any monitor clears
    /// them all and a new notification lights them all.
    unread_dots: Rc<RefCell<Vec<gtk4::Widget>>>,
    /// The primary's surtitle (the only one that ever shows): opening a
    /// stage on any monitor clears it.
    pub surtitle: Rc<RefCell<Option<crate::surtitle::Surtitle>>>,
}

impl Shared {
    pub fn new(store: history::Store) -> Self {
        Self {
            store,
            dnd: Rc::new(Cell::new(false)),
            open: Rc::new(RefCell::new(Weak::new())),
            stages: Rc::new(RefCell::new(Vec::new())),
            unread_dots: Rc::new(RefCell::new(Vec::new())),
            surtitle: Rc::new(RefCell::new(None)),
        }
    }

    /// The stage currently open on any monitor.
    pub fn open_stage(&self) -> Option<Rc<Stage>> {
        self.open.borrow().upgrade().filter(|s| s.is_open())
    }

    fn live_stages(&self) -> Vec<Rc<Stage>> {
        let mut stages = self.stages.borrow_mut();
        stages.retain(|w| w.strong_count() > 0);
        stages.iter().filter_map(Weak::upgrade).collect()
    }

    pub fn register_unread_dot(&self, dot: &gtk4::Widget) {
        self.unread_dots.borrow_mut().push(dot.clone());
    }

    pub fn unregister_unread_dot(&self, dot: &gtk4::Widget) {
        self.unread_dots.borrow_mut().retain(|d| d != dot);
    }

    pub fn set_unread(&self, on: bool) {
        for dot in self.unread_dots.borrow().iter() {
            dot.set_visible(on);
        }
    }

    pub fn set_dnd(&self, on: bool) {
        self.dnd.set(on);
        for stage in self.live_stages() {
            set_chip_on(&stage.dnd_chip, on);
        }
    }

    /// Re-renders the open stage's notifications (a notification arrived,
    /// or was dismissed elsewhere).
    pub fn refresh_open_notifications(&self) {
        if let Some(stage) = self.open_stage() {
            stage.refresh_notifications();
        }
    }
}

/// One power-row button: its underlying `PowerAction` plus the widgets
/// [`Stage::reset_power_buttons`] touches.
struct PowerButton {
    action: crate::PowerAction,
    btn: gtk4::Button,
    label: gtk4::Label,
}

/// The inputs of the last status sentence, kept so a theme change can
/// re-render it in the new colours.
#[derive(Clone, Default)]
struct Status {
    battery: Option<u8>,
    low: bool,
    minutes_left: Option<u32>,
    network: String,
}

/// One pooled result row: its widgets are built once and re-filled on every
/// keystroke (see [`ResultPool`]).
struct RowSlot {
    btn: gtk4::Button,
    glyph: gtk4::Label,
    name: gtk4::Label,
    sub: gtk4::Label,
    act: gtk4::Label,
    /// Flat result index this slot currently shows, read by its click handler.
    index: Rc<Cell<i32>>,
}

/// Every widget the results list can show, built once on first use. Creating
/// and tearing down a row per match on each keystroke was what stalled typing
/// (half a second for one letter on a 129-app machine); a keystroke now only
/// sets text and visibility. Sized by `pure::MAX_GROUPS`/`MAX_GROUP_ROWS`,
/// which `build_results` already caps to.
struct ResultPool {
    groups: Vec<(gtk4::Label, Vec<RowSlot>)>,
    answer: gtk4::Label,
    empty: gtk4::Label,
}

/// The stage's settings menu (a "Settings" button at the end of the power
/// row revealing one chip row): pill auto-hide, widgets, and which workspaces
/// the pill shows. Choices are written to `spotlight.json`
/// (`crate::spotlight_prefs`), which every bar instance watches.
struct SettingsPanel {
    toggle: gtk4::Button,
    revealer: gtk4::Revealer,
    autohide: gtk4::Button,
    widgets: gtk4::Button,
    /// None / Current / All, in that order.
    workspaces: [gtk4::Button; 3],
    prefs: Cell<crate::spotlight_prefs::Prefs>,
}

pub struct Stage {
    pub window: gtk4::Window,
    root: gtk4::Overlay,
    column: gtk4::Box,
    column_anim: RefCell<Option<gtk4::TickCallbackId>>,
    /// Bumped on every open/close so a finished close fade only unmaps a
    /// stage nothing has re-opened in the meantime.
    fade_token: Cell<u32>,
    search_row: gtk4::Box,
    entry: gtk4::Entry,
    mode_badge: gtk4::Label,
    idle_box: gtk4::Box,
    recent_box: gtk4::Box,
    floor: gtk4::Box,
    /// The three zones' width-holding wrappers (see [`zone_holder`]).
    controls_zone: gtk4::ScrolledWindow,
    centre_zone: gtk4::ScrolledWindow,
    notif_zone: gtk4::ScrolledWindow,
    results_box: gtk4::Box,
    result_pool: RefCell<Option<ResultPool>>,
    settings: SettingsPanel,
    recent_tiles: Vec<gtk4::Button>,

    // Controls zone
    vol_scale: gtk4::Scale,
    vol_pct_lbl: gtk4::Label,
    sinks_box: gtk4::FlowBox,
    art_picture: gtk4::Picture,
    media_track_lbl: gtk4::Label,
    media_artist_lbl: gtk4::Label,
    media_play_btn: gtk4::Button,
    bri_scale: gtk4::Scale,
    bri_pct_lbl: gtk4::Label,
    wifi_chip: gtk4::Button,
    bt_chip: gtk4::Button,
    dnd_chip: gtk4::Button,

    // Centre zone
    big_time_lbl: gtk4::Label,
    sentence_lbl: gtk4::Label,
    cards_box: gtk4::Box,
    /// A never-shown label styled `color: @accent`: the brass colour for
    /// Pango markup (the calc answer), read at render time.
    brass_probe: gtk4::Label,
    status: RefCell<Status>,

    // Notifications zone
    notes_box: gtk4::Box,
    clear_all_btn: gtk4::Button,

    // Power row
    power_buttons: Vec<PowerButton>,
    power_confirm: RefCell<PowerConfirm>,
    power_hint: gtk4::Label,

    // Shared / cross-cutting state
    open: Cell<bool>,
    query: RefCell<String>,
    shared: Shared,
    sel: Cell<i32>,
    rec_sel: Cell<i32>,
    results: RefCell<StageResults>,
    windows: RefCell<Vec<OpenWindow>>,
    /// Every known app (unfiltered): what a search filters against.
    all_apps: RefCell<Vec<bread_launcher::DesktopEntry>>,
    /// The idle row's recent tiles (most-launched first).
    recent: RefCell<Vec<bread_launcher::DesktopEntry>>,
    launcher_history: RefCell<bread_launcher::LaunchHistory>,
    /// Screenshot mode seeded sample apps: opening must not replace them
    /// with this machine's real app list.
    sample_apps: Cell<bool>,
    /// Set while a slider is being programmatically synced from a fresh
    /// `ControlPanelData` poll, so `connect_value_changed` doesn't
    /// re-issue the same value as a user edit.
    loading: Cell<bool>,
    self_weak: RefCell<Weak<Stage>>,
    sender: ComponentSender<App>,
}

impl Stage {
    fn self_rc(&self) -> Rc<Stage> {
        self.self_weak
            .borrow()
            .upgrade()
            .expect("Stage method called after its own Rc was dropped")
    }

    fn post(&self, msg: AppInput) {
        let _ = self.sender.input_sender().send(msg);
    }

    pub fn is_open(&self) -> bool {
        self.open.get()
    }

    /// Opens the stage: shows the window, takes keyboard focus, resets the
    /// query, selects the first recent tile, and refreshes its data (the
    /// mockup's `setOpen(true)`: `recSel=0; drawRecent(); q.focus();`).
    pub fn open(&self) {
        if self.open.get() {
            return;
        }
        self.open_common(true);
        crate::bar::control::spawn_load(self.sender.clone());
        self.refresh_windows();
    }

    /// Screenshot-mode open: everything `open()` does except the real
    /// `bar::control`/Hyprland-clients fetches (the caller seeds sample
    /// data instead) and output pinning (there is no Hyprland to ask).
    pub fn open_for_screenshot(&self) {
        if self.open.get() {
            return;
        }
        self.open_common(false);
    }

    fn open_common(&self, pin: bool) {
        self.open.set(true);
        *self.shared.open.borrow_mut() = self.self_weak.borrow().clone();
        // `setOpen(true)` clears the unread dot and the surtitle.
        self.shared.set_unread(false);
        if let Some(surtitle) = self.shared.surtitle.borrow().as_ref() {
            surtitle.hide();
        }
        self.entry.set_text("");
        *self.query.borrow_mut() = String::new();
        self.rec_sel.set(0);
        if !self.sample_apps.get() {
            let entries = bread_launcher::load_sorted_entries(
                &std::collections::HashMap::new(),
                &[],
                &self.launcher_history.borrow(),
            );
            self.set_apps(entries);
        } else {
            self.render_recent();
            self.render_results();
        }
        self.refresh_notifications();
        set_chip_on(&self.dnd_chip, self.shared.dnd.get());
        if pin {
            crate::theme::pin_focused_output(&self.window);
        }
        self.apply_layout(self.output_width());
        self.render_status();
        self.window.set_keyboard_mode(KeyboardMode::OnDemand);
        if let Some(surface) = self.window.surface() {
            surface.set_input_region(None);
        }
        self.window.set_visible(true);
        self.window.present();
        self.entry.grab_focus();

        // The fade and slide start from the hidden state on the next
        // main-loop turn, once the surface is mapped (a CSS transition needs
        // a start state to run from).
        let token = self.bump_fade();
        self.column.set_margin_top(COLUMN_TOP - COLUMN_SLIDE);
        self.root.remove_css_class("open");
        let s = self.self_rc();
        gtk4::glib::idle_add_local_once(move || {
            if s.fade_token.get() == token && s.open.get() {
                s.root.add_css_class("open");
                s.slide_column(COLUMN_TOP);
            }
        });
        self.post(AppInput::StageOpenChanged(true));
    }

    fn bump_fade(&self) -> u32 {
        let t = self.fade_token.get().wrapping_add(1);
        self.fade_token.set(t);
        t
    }

    /// `.column{transition:transform .45s var(--spring)}`.
    fn slide_column(&self, target: i32) {
        if let Some(id) = self.column_anim.borrow_mut().take() {
            id.remove();
        }
        let from = f64::from(self.column.margin_top());
        let column = self.column.clone();
        *self.column_anim.borrow_mut() =
            motion::animate(&self.column, 450.0, 0.0, motion::SPRING, move |p| {
                column.set_margin_top(motion::lerp(from, f64::from(target), p).round() as i32);
            });
    }

    /// The width of the output the stage is on (the first output when it
    /// is not bound to one yet, as in an isolated capture).
    fn output_width(&self) -> i32 {
        LayerShell::monitor(&self.window)
            .or_else(|| {
                gtk4::gdk::Display::default()
                    .and_then(|d| d.monitors().item(0))
                    .and_then(|m| m.downcast::<gtk4::gdk::Monitor>().ok())
            })
            .map(|m| m.geometry().width())
            .unwrap_or(1920)
    }

    /// Sizes the column, the 640px content blocks and the floor's
    /// `1fr 1.15fr 1fr` grid for an output `width` px wide, stacking the
    /// floor below the 860px breakpoint.
    fn apply_layout(&self, width: i32) {
        let inner = pure::column_inner_width(width);
        self.column.set_size_request(inner, -1);
        let content = CONTENT_WIDTH.min(inner);
        for w in [
            self.search_row.upcast_ref::<gtk4::Widget>(),
            self.recent_box.upcast_ref(),
            self.results_box.upcast_ref(),
        ] {
            w.set_size_request(content, -1);
        }
        self.force_narrow_with(width < NARROW_BREAKPOINT_PX, inner);
    }

    /// Screenshot-mode helper: types `text` into the search field, driving
    /// the same `on_query_changed` path a real keystroke would.
    pub fn force_query(&self, text: &str) {
        self.entry.set_text(text);
    }

    /// Screenshot-mode helper: opens the settings menu as its button would,
    /// without animating, for the "stage-settings" view.
    pub fn force_settings_open(&self) {
        self.settings.revealer.set_transition_duration(0);
        self.settings.revealer.set_reveal_child(true);
        set_chip_on(&self.settings.toggle, true);
    }

    /// Screenshot-mode helper: arms (or runs) a power-row action exactly as
    /// a real click would, for the "stage-power-armed" view.
    pub fn force_power_press(&self, id: &'static str) {
        self.press_power(id);
    }

    /// Screenshot-mode helper: puts the first notification row in its hover
    /// state (showing its dismiss button), as a pointer over it would.
    pub fn force_note_hover(&self) {
        if let Some(row) = self.notes_box.first_child() {
            row.set_state_flags(gtk4::StateFlags::PRELIGHT, false);
        }
    }

    /// Screenshot-mode helper: the mockup's `@media (max-width:860px)` at
    /// an 800px-wide column.
    pub fn force_narrow(&self, narrow: bool) {
        let width = if narrow { 800 } else { self.output_width() };
        self.apply_layout(width);
    }

    fn force_narrow_with(&self, narrow: bool, inner: i32) {
        if narrow {
            self.floor.set_orientation(gtk4::Orientation::Vertical);
            self.floor.add_css_class("narrow");
            self.floor
                .reorder_child_after(&self.centre_zone, None::<&gtk4::Widget>);
            for zone in [&self.controls_zone, &self.centre_zone, &self.notif_zone] {
                zone.set_size_request(inner, -1);
            }
        } else {
            self.floor.set_orientation(gtk4::Orientation::Horizontal);
            self.floor.remove_css_class("narrow");
            self.floor
                .reorder_child_after(&self.centre_zone, Some(&self.controls_zone));
            let [a, b, c] = pure::zone_widths(inner);
            self.controls_zone.set_size_request(a, -1);
            self.centre_zone.set_size_request(b, -1);
            self.notif_zone.set_size_request(c, -1);
        }
    }

    /// Closes the stage (the mockup's `setOpen(false)`: `q.value="";
    /// render(); disarm(); q.blur()`), fading out over .28s. Input passes
    /// through and the keyboard is released at once; the surface unmaps
    /// once the fade is done.
    pub fn close(&self) {
        if !self.open.get() {
            return;
        }
        self.open.set(false);
        self.entry.set_text("");
        *self.query.borrow_mut() = String::new();
        self.rec_sel.set(-1);
        self.power_confirm.borrow_mut().disarm();
        self.reset_power_buttons();
        self.window.set_keyboard_mode(KeyboardMode::None);
        if let Some(surface) = self.window.surface() {
            surface.set_input_region(Some(&gtk4::cairo::Region::create()));
        }
        self.root.remove_css_class("open");
        self.slide_column(COLUMN_TOP - COLUMN_SLIDE);
        let token = self.bump_fade();
        let s = self.self_rc();
        let fade = if motion::animations_enabled() {
            FADE_MS
        } else {
            0
        };
        gtk4::glib::timeout_add_local_once(Duration::from_millis(fade), move || {
            if s.fade_token.get() == token && !s.open.get() {
                s.window.set_visible(false);
            }
        });
        self.post(AppInput::StageOpenChanged(false));
    }

    pub fn toggle(&self) {
        if self.open.get() {
            self.close();
        } else {
            self.open();
        }
    }

    // ── Data feeds (called from `App::update`, forwarding the bar's own
    //    pollers rather than running a second copy of each) ─────────────

    pub fn set_control_data(&self, data: &ControlPanelData) {
        self.loading.set(true);
        self.vol_scale.set_value(data.volume);
        self.bri_scale.set_value(data.brightness);
        self.loading.set(false);
        self.sync_level_labels();
        self.render_sinks(&data.sinks);
    }

    /// `syncRange`: the Sound/Light values follow the sliders, drags
    /// included.
    fn sync_level_labels(&self) {
        self.vol_pct_lbl
            .set_label(&format!("{:.0}%", self.vol_scale.value() * 100.0));
        self.bri_pct_lbl
            .set_label(&format!("{:.0}%", self.bri_scale.value() * 100.0));
    }

    fn render_sinks(&self, sinks: &[AudioSink]) {
        self.sinks_box.remove_all();
        for sink in sinks {
            let chip = make_chip(&sink.description);
            if sink.is_default {
                chip.add_css_class("on");
            }
            let name = sink.name.clone();
            let sender = self.sender.clone();
            chip.connect_clicked(move |_| {
                crate::bar::control::spawn_set_sink(name.clone(), sender.clone());
            });
            self.sinks_box.append(&chip);
        }
    }

    pub fn set_media(&self, m: &MediaState) {
        if m.has_player {
            self.media_track_lbl.set_label(&m.title);
            let sub = match (m.artist.is_empty(), m.player.is_empty()) {
                (false, false) => format!("{} \u{00b7} {}", m.artist, m.player),
                (false, true) => m.artist.clone(),
                (true, false) => m.player.clone(),
                (true, true) => String::new(),
            };
            self.media_artist_lbl.set_label(&sub);
        } else {
            self.media_track_lbl.set_label("Nothing playing");
            self.media_artist_lbl.set_label("");
        }
        match m.art.as_ref().filter(|p| p.is_file()) {
            Some(path) => {
                self.art_picture.set_filename(Some(path));
                self.art_picture.set_visible(true);
            }
            None => {
                self.art_picture
                    .set_paintable(None::<&gtk4::gdk::Paintable>);
                self.art_picture.set_visible(false);
            }
        }
        self.media_play_btn.set_label(if m.playing {
            "\u{275a}\u{275a}"
        } else {
            "\u{25b6}"
        });
    }

    /// The Wi-Fi chip is the radio itself (on/off), labelled "Wi-Fi" as in
    /// the mockup; the network name belongs to the status sentence.
    pub fn set_wifi_radio(&self, on: bool) {
        set_chip_on(&self.wifi_chip, on);
    }

    pub fn set_bluetooth(&self, data: &BtPopoverData) {
        set_chip_on(&self.bt_chip, data.powered);
    }

    /// The centre zone's status sentence (the mockup's `sentence()`).
    pub fn set_status(
        &self,
        battery: Option<u8>,
        low: bool,
        minutes_left: Option<u32>,
        network: &str,
    ) {
        *self.status.borrow_mut() = Status {
            battery,
            low,
            minutes_left,
            network: network.to_string(),
        };
        self.render_status();
    }

    fn render_status(&self) {
        let st = self.status.borrow().clone();
        let day = crate::bar::clock::now()
            .format("%A, %B %-d")
            .map(|s| s.to_string())
            .unwrap_or_default();
        let markup = pure::status_sentence(&day, st.battery, st.low, st.minutes_left, &st.network);
        balance_wrap(&self.sentence_lbl, &markup, 30);
    }

    pub fn set_time(&self, time: &str) {
        self.big_time_lbl.set_label(time);
        self.render_status();
    }

    /// The centre zone's widget cards, hidden when there are none (the
    /// mockup's `.cards.off` with widgets off). `tray` are widgets registered
    /// at the `tray` placement, the cards the other themes show in the
    /// control panel: rendered whole inside the card frame. `derived` are
    /// pill widgets whose module has no tray card, summarised from their pill
    /// content ([`pure::card_parts`]).
    pub fn set_cards(
        &self,
        tray: &[&bread_shared::widget::WidgetSpec],
        derived: &[&bread_shared::widget::WidgetSpec],
    ) {
        while let Some(c) = self.cards_box.first_child() {
            self.cards_box.remove(&c);
        }
        for spec in tray {
            let card = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            card.add_css_class("stage-card");
            card.add_css_class("stage-card-tray");
            let node = crate::widgets::build_node(&spec.root, &spec.id);
            if let Some(tooltip) = &spec.tooltip {
                node.set_tooltip_text(Some(tooltip));
            }
            card.append(&node);
            self.cards_box.append(&card);
        }
        for spec in derived {
            self.cards_box
                .append(&make_card(&pure::card_parts(spec), &spec.id));
        }
        self.cards_box
            .set_visible(!tray.is_empty() || !derived.is_empty());
    }

    /// Shows the settings menu's current state. `workspaces` is the effective
    /// view (the saved choice, or the theme's default when none is saved).
    pub fn set_prefs(
        &self,
        prefs: &crate::spotlight_prefs::Prefs,
        workspaces: bread_theme::shell::WorkspaceDisplay,
    ) {
        use bread_theme::shell::WorkspaceDisplay;
        self.settings.prefs.set(*prefs);
        set_chip_on(&self.settings.autohide, prefs.autohide_on());
        set_chip_on(&self.settings.widgets, prefs.widgets_on());
        for (chip, d) in self.settings.workspaces.iter().zip([
            WorkspaceDisplay::None,
            WorkspaceDisplay::Current,
            WorkspaceDisplay::All,
        ]) {
            set_chip_on(chip, d == workspaces);
        }
    }

    /// Applies one settings-menu change: saved to `spotlight.json`, whose
    /// watchers (every bar instance, this one included) apply it. Also posted
    /// here directly so this monitor updates without waiting on the watcher.
    fn change_prefs(&self, edit: impl FnOnce(&mut crate::spotlight_prefs::Prefs)) {
        let mut prefs = self.settings.prefs.get();
        edit(&mut prefs);
        self.settings.prefs.set(prefs);
        if let Err(e) = crate::spotlight_prefs::save(&prefs) {
            tracing::warn!(target: "breadbar::stage", error = %e, "could not save spotlight settings");
        }
        let _ = self
            .sender
            .input_sender()
            .send(AppInput::PrefsChanged(prefs));
    }

    /// Screenshot-mode helper: the mockup's own sample notifications, in a
    /// capture-only store (never persisted; the real history is untouched).
    pub fn seed_sample_notifications(&self) {
        use std::time::SystemTime;
        let now = SystemTime::now();
        let at = |mins_ago: u64| now - Duration::from_secs(mins_ago * 60);
        let mut store = self.shared.store.lock().unwrap();
        store.clear();
        for (id, app, summary, body, received) in [
            (5u32, "kitty", "Claude is waiting for your input", "", at(6)),
            (4, "kitty", "Claude is waiting for your input", "", at(8)),
            (3, "kitty", "Claude is waiting for your input", "", at(9)),
            (
                2,
                "breadshot",
                "Screenshot",
                "Saved to ~/Pictures/Screenshots/2026-09-26.png",
                at(69),
            ),
            (1, "breadcrumbs", "Back online on Totally NCC", "", at(166)),
        ] {
            store.push_back(history::Entry {
                id,
                app_name: app.to_string(),
                summary: summary.to_string(),
                body: body.to_string(),
                urgency: crate::notifications::Urgency::Normal,
                received,
            });
        }
        drop(store);
        self.refresh_notifications();
    }

    /// Screenshot-mode helper: the mockup's sample apps and recent tiles in
    /// place of this machine's real ones.
    pub fn seed_sample_apps(&self) {
        let app = |name: &str, cat: &str| bread_launcher::DesktopEntry {
            id: format!("{}.desktop", name.to_lowercase()),
            name: name.to_string(),
            exec: "true".to_string(),
            icon_name: String::new(),
            icon_path: None,
            categories: vec![cat.to_string()],
            wm_class: Some(name.to_lowercase()),
            terminal: false,
        };
        let apps: Vec<_> = [
            ("Firefox", "WebBrowser"),
            ("Kitty", "TerminalEmulator"),
            ("Files", "FileManager"),
            ("Breadpad", "TextEditor"),
            ("Settings", "Settings"),
            ("Spotify", "Music"),
            ("Obsidian", "Office"),
            ("Steam", "Game"),
            ("Blender", "3DGraphics"),
            ("GIMP", "RasterGraphics"),
            ("Thunderbird", "Email"),
            ("Code", "IDE"),
            ("Breadclip", "Utility"),
        ]
        .iter()
        .map(|(n, c)| app(n, c))
        .collect();
        let recent = ["Kitty", "Firefox", "Code", "Spotify", "Files", "Obsidian"]
            .iter()
            .filter_map(|n| apps.iter().find(|a| a.name == *n).cloned())
            .collect();
        *self.all_apps.borrow_mut() = apps;
        *self.recent.borrow_mut() = recent;
        self.sample_apps.set(true);
        self.render_recent();
    }

    /// Rebuilds the notifications zone from the shared history store: the
    /// same store the surtitle path and the history window write to, on
    /// every monitor.
    pub fn refresh_notifications(&self) {
        while let Some(c) = self.notes_box.first_child() {
            self.notes_box.remove(&c);
        }
        let entries: Vec<(u32, String, String, String, std::time::SystemTime)> = self
            .shared
            .store
            .lock()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e.id,
                    e.app_name.clone(),
                    e.summary.clone(),
                    history::plain_body(&e.body),
                    e.received,
                )
            })
            .collect();
        let rows = pure::stack_notes(&entries);
        self.clear_all_btn.set_visible(!rows.is_empty());
        if rows.is_empty() {
            let empty = gtk4::Label::new(Some("You're all caught up."));
            empty.add_css_class("stage-note-empty");
            empty.set_xalign(0.0);
            self.notes_box.append(&empty);
            return;
        }
        for row in rows.iter().take(MAX_NOTES) {
            self.notes_box.append(&self.make_note_row(row));
        }
    }

    fn make_note_row(&self, note: &pure::NoteRow<std::time::SystemTime>) -> gtk4::Box {
        let row = gtk4::Box::new(gtk4::Orientation::Vertical, 2);
        row.add_css_class("stage-note");
        let top = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
        let app = gtk4::Label::new(Some(&note.app));
        app.add_css_class("stage-note-app");
        app.set_xalign(0.0);
        app.set_hexpand(true);
        app.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        top.append(&app);
        let t = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let time = gtk4::Label::new(Some(&clock_hhmm(note.time)));
        time.add_css_class("stage-note-time");
        t.append(&time);
        let dismiss = gtk4::Button::with_label("\u{2715}");
        dismiss.add_css_class("stage-note-dismiss");
        dismiss.set_tooltip_text(Some("Dismiss"));
        dismiss.set_valign(gtk4::Align::Center);
        let s = self.self_rc();
        let ids = note.ids.clone();
        dismiss.connect_clicked(move |_| {
            s.shared
                .store
                .lock()
                .unwrap()
                .retain(|e| !ids.contains(&e.id));
            history::persist(&s.shared.store);
            s.refresh_notifications();
        });
        t.append(&dismiss);
        top.append(&t);
        row.append(&top);
        let msg_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        let msg = gtk4::Label::new(Some(&note.msg));
        msg.add_css_class("stage-note-msg");
        msg.set_xalign(0.0);
        msg.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        msg.set_single_line_mode(true);
        msg_row.append(&msg);
        if note.count > 1 {
            let count = gtk4::Label::new(Some(&format!("\u{00d7}{}", note.count)));
            count.add_css_class("stage-note-count");
            count.set_valign(gtk4::Align::Baseline);
            msg.set_valign(gtk4::Align::Baseline);
            msg_row.append(&count);
        }
        row.append(&msg_row);
        // The notification's body on its own dimmer line, one line long.
        if !note.body.is_empty() {
            let body = gtk4::Label::new(Some(&note.body));
            body.add_css_class("stage-note-body");
            body.set_xalign(0.0);
            body.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            body.set_single_line_mode(true);
            row.append(&body);
        }
        row
    }

    fn refresh_windows(&self) {
        let s = self.self_rc();
        relm4::spawn_local(async move {
            let windows = fetch_open_windows().await;
            s.set_windows(windows);
        });
    }

    pub fn set_windows(&self, windows: Vec<OpenWindow>) {
        *self.windows.borrow_mut() = windows;
        self.render_results();
    }

    pub fn set_apps(&self, apps: Vec<bread_launcher::DesktopEntry>) {
        let (recent, _rest) =
            bread_launcher::split_sections(apps.clone(), &self.launcher_history.borrow());
        *self.recent.borrow_mut() = recent;
        *self.all_apps.borrow_mut() = apps;
        self.render_recent();
        self.render_results();
    }

    // ── Search / results ───────────────────────────────────────────────

    fn on_query_changed(&self, text: &str) {
        let start = std::time::Instant::now();
        *self.query.borrow_mut() = text.to_string();
        self.rec_sel.set(-1);
        self.render_recent();
        self.render_results();
        tracing::debug!(
            target: "breadbar::stage",
            query = text,
            total_ms = start.elapsed().as_secs_f64() * 1000.0,
            "query changed"
        );
    }

    fn render_recent(&self) {
        let recent = self.recent.borrow();
        let sel = self.rec_sel.get();
        for (i, tile) in self.recent_tiles.iter().enumerate() {
            let occupied = i < recent.len();
            tile.set_visible(occupied);
            if occupied {
                let entry = &recent[i];
                if let Some(inner) = tile.child().and_then(|c| c.downcast::<gtk4::Box>().ok()) {
                    if let (Some(glyph), Some(name)) = (
                        inner.first_child().and_downcast::<gtk4::Label>(),
                        inner.last_child().and_downcast::<gtk4::Label>(),
                    ) {
                        glyph.set_label(&first_char_upper(&entry.name));
                        name.set_label(&entry.name);
                    }
                }
                if i as i32 == sel {
                    tile.add_css_class("sel");
                } else {
                    tile.remove_css_class("sel");
                }
            }
        }
    }

    fn render_results(&self) {
        let query = self.query.borrow().clone();
        let has_query = !query.trim().is_empty();
        self.idle_box.set_visible(!has_query);
        self.results_box.set_visible(has_query);

        let parsed = bread_launcher::parse_query(&query);
        self.mode_badge.set_visible(matches!(
            parsed.kind,
            bread_launcher::QueryKind::Calc | bread_launcher::QueryKind::Cmd
        ));
        self.mode_badge.set_label(match parsed.kind {
            bread_launcher::QueryKind::Calc => "calc",
            bread_launcher::QueryKind::Cmd => "command",
            _ => "",
        });

        if !has_query {
            *self.results.borrow_mut() = StageResults::Groups(Vec::new());
            return;
        }

        let t_build = std::time::Instant::now();
        let results = {
            let windows = self.windows.borrow();
            let apps = self.all_apps.borrow();
            pure::build_results(&parsed, &windows, &apps)
        };
        let build_ms = t_build.elapsed().as_secs_f64() * 1000.0;
        self.sel.set(0);
        let t_draw = std::time::Instant::now();
        self.draw_results(&results);
        tracing::debug!(
            target: "breadbar::stage",
            rows = results.rows().len(),
            windows = self.windows.borrow().len(),
            apps = self.all_apps.borrow().len(),
            build_ms,
            draw_ms = t_draw.elapsed().as_secs_f64() * 1000.0,
            "results rendered"
        );
        *self.results.borrow_mut() = results;
    }

    fn draw_results(&self, results: &StageResults) {
        self.ensure_result_pool();
        {
            let pool_ref = self.result_pool.borrow();
            let pool = pool_ref.as_ref().expect("result pool built above");
            pool.answer.set_visible(false);
            pool.empty.set_visible(false);
            for (header, slots) in &pool.groups {
                header.set_visible(false);
                for slot in slots {
                    slot.btn.set_visible(false);
                }
            }
            match results {
                StageResults::Calc { expr, answer } => {
                    // `.answer`: "<expr> = <b>result</b>", the result in brass.
                    let expr = if expr.trim().is_empty() {
                        "\u{2026}".to_string()
                    } else {
                        expr.trim().to_string()
                    };
                    let brass = hex(&self.brass_probe.color());
                    pool.answer.set_markup(&format!(
                        "{} = <span weight=\"500\" foreground=\"{brass}\">{}</span>",
                        gtk4::glib::markup_escape_text(&expr),
                        gtk4::glib::markup_escape_text(answer.as_deref().unwrap_or("?")),
                    ));
                    pool.answer.set_visible(true);
                }
                StageResults::Empty { query } => {
                    pool.empty.set_label(&format!(
                        "Nothing matches \u{201c}{}\u{201d}. Try = for maths or > for commands.",
                        query.trim()
                    ));
                    pool.empty.set_visible(true);
                }
                StageResults::Groups(groups) => {
                    let mut i = 0i32;
                    for (group, (header, slots)) in groups.iter().zip(&pool.groups) {
                        header.set_label(group.title);
                        header.set_visible(true);
                        for (row, slot) in group.rows.iter().zip(slots) {
                            slot.glyph.set_label(&row.glyph);
                            slot.name.set_label(&row.name);
                            slot.sub.set_label(&row.sub);
                            slot.act
                                .set_label(&format!("{} \u{21b5}", row.kind.action_label()));
                            slot.index.set(i);
                            slot.btn.set_visible(true);
                            i += 1;
                        }
                    }
                }
            }
        }
        self.mark_selection();
    }

    /// Builds [`ResultPool`] into `results_box` the first time results show.
    fn ensure_result_pool(&self) {
        if self.result_pool.borrow().is_some() {
            return;
        }
        let answer = gtk4::Label::new(None);
        answer.add_css_class("stage-answer");
        answer.set_justify(gtk4::Justification::Center);
        answer.set_visible(false);
        self.results_box.append(&answer);

        let empty = gtk4::Label::new(None);
        empty.add_css_class("stage-empty");
        empty.set_wrap(true);
        empty.set_justify(gtk4::Justification::Center);
        empty.set_visible(false);
        self.results_box.append(&empty);

        let groups = (0..pure::MAX_GROUPS)
            .map(|_| {
                let header = gtk4::Label::new(None);
                header.add_css_class("stage-group");
                header.set_xalign(0.0);
                header.set_visible(false);
                self.results_box.append(&header);
                let slots = (0..pure::MAX_GROUP_ROWS)
                    .map(|_| {
                        let slot = self.make_row_slot();
                        slot.btn.set_visible(false);
                        self.results_box.append(&slot.btn);
                        slot
                    })
                    .collect();
                (header, slots)
            })
            .collect();
        *self.result_pool.borrow_mut() = Some(ResultPool {
            groups,
            answer,
            empty,
        });
    }

    fn make_row_slot(&self) -> RowSlot {
        let btn = gtk4::Button::new();
        btn.add_css_class("stage-row");
        let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 14);
        let glyph = gtk4::Label::new(None);
        glyph.add_css_class("stage-row-glyph");
        glyph.set_valign(gtk4::Align::Center);
        hbox.append(&glyph);
        let name = gtk4::Label::new(None);
        name.add_css_class("stage-row-name");
        name.set_xalign(0.0);
        name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        hbox.append(&name);
        // `.sub{margin-left:auto}` pushes itself right; with it hidden on
        // the selected row, the act badge follows the name directly.
        let sub = gtk4::Label::new(None);
        sub.add_css_class("stage-row-sub");
        sub.set_hexpand(true);
        sub.set_xalign(1.0);
        hbox.append(&sub);
        let act = gtk4::Label::new(None);
        act.add_css_class("stage-row-act");
        act.set_valign(gtk4::Align::Center);
        act.set_visible(false);
        hbox.append(&act);
        btn.set_child(Some(&hbox));
        let index = Rc::new(Cell::new(-1));
        let weak = self.self_weak.borrow().clone();
        let idx = index.clone();
        btn.connect_clicked(move |_| {
            if let Some(s) = weak.upgrade() {
                s.sel.set(idx.get());
                s.activate_result(idx.get());
            }
        });
        RowSlot {
            btn,
            glyph,
            name,
            sub,
            act,
            index,
        }
    }

    fn mark_selection(&self) {
        let sel = self.sel.get();
        let pool_ref = self.result_pool.borrow();
        let Some(pool) = pool_ref.as_ref() else {
            return;
        };
        for slot in pool.groups.iter().flat_map(|(_, slots)| slots) {
            if !slot.btn.is_visible() {
                continue;
            }
            // `.row.sel .act{display:block}` / `.row.sel .sub{display:none}`.
            let selected = slot.index.get() == sel;
            if selected {
                slot.btn.add_css_class("sel");
            } else {
                slot.btn.remove_css_class("sel");
            }
            slot.act.set_visible(selected);
            slot.sub.set_visible(!selected);
        }
    }

    fn move_selection(&self, delta: i32) {
        let len = self.results.borrow().rows().len();
        if let Some(new_sel) = pure::clamp_selection(len, self.sel.get() + delta) {
            self.sel.set(new_sel as i32);
            self.mark_selection();
        }
    }

    fn move_recent_selection(&self, delta: i32) {
        let len = self.recent.borrow().len();
        if let Some(new_sel) = pure::clamp_selection(len, self.rec_sel.get().max(0) + delta) {
            self.rec_sel.set(new_sel as i32);
            self.render_recent();
        }
    }

    /// Runs the selected result (or recent tile, if the query is empty),
    /// then closes the stage: the mockup's `launch()`.
    fn activate_selection(&self) {
        if self.query.borrow().trim().is_empty() {
            self.activate_recent_tile(self.rec_sel.get());
            return;
        }
        self.activate_result(self.sel.get());
    }

    fn activate_recent_tile(&self, index: i32) {
        if index < 0 {
            return;
        }
        if let Some(entry) = self.recent.borrow().get(index as usize).cloned() {
            self.launch_app(&entry);
        }
    }

    fn activate_result(&self, index: i32) {
        let rows_owned: Vec<pure::ResultRow> = {
            let results = self.results.borrow();
            let StageResults::Groups(groups) = &*results else {
                return;
            };
            groups.iter().flat_map(|g| g.rows.clone()).collect()
        };
        let Some(row) = rows_owned.get(index as usize) else {
            return;
        };
        match row.kind {
            pure::ResultKind::Window => {
                tracing::debug!(
                    target: "breadbar::stage",
                    index,
                    name = %row.name,
                    address = %row.target,
                    "activating window result"
                );
                focus_window(row.target.clone());
                self.close();
            }
            pure::ResultKind::App => {
                let entry = self
                    .all_apps
                    .borrow()
                    .iter()
                    .find(|e| e.id == row.target)
                    .cloned();
                if let Some(entry) = entry {
                    self.launch_app(&entry);
                }
            }
            pure::ResultKind::Command => {
                if let Some(cmd) = pure::STAGE_COMMANDS.iter().find(|c| c.id == row.target) {
                    match cmd.action {
                        pure::CommandAction::Shell(line) => run_shell(line),
                        pure::CommandAction::ToggleDnd => {
                            self.shared.set_dnd(!self.shared.dnd.get());
                        }
                    }
                }
                self.close();
            }
        }
    }

    fn launch_app(&self, entry: &bread_launcher::DesktopEntry) {
        bread_launcher::do_launch(entry, STAGE_APP_ID, STAGE_LAUNCHED_EVENT);
        self.launcher_history.borrow_mut().increment(&entry.name);
        self.launcher_history.borrow().save();
        self.close();
    }

    // ── Footer ─────────────────────────────────────────────────────────

    // ── Power row ──────────────────────────────────────────────────────

    fn press_power(&self, id: &'static str) {
        let Some(button) = self.power_buttons.iter().find(|b| b.action.label == id) else {
            return;
        };
        let outcome = self.power_confirm.borrow_mut().press(
            id,
            button.action.confirm,
            Instant::now(),
            Duration::from_millis(crate::POWER_CONFIRM_MS),
        );
        match outcome {
            pure::PowerPress::Arm => {
                self.reset_power_buttons();
                button.btn.add_css_class("armed");
                self.power_hint.set_label(&format!(
                    "press again to {}",
                    power_display_label(button.action.label).to_lowercase()
                ));
                let s = self.self_rc();
                let generation = self.power_confirm.borrow().generation();
                gtk4::glib::timeout_add_local_once(
                    Duration::from_millis(crate::POWER_CONFIRM_MS),
                    move || s.power_confirm_expired(generation),
                );
            }
            pure::PowerPress::Ignore => {}
            pure::PowerPress::Run => {
                self.reset_power_buttons();
                let cmd = button.action.cmd;
                self.close();
                relm4::spawn(async move {
                    let _ = tokio::process::Command::new(cmd[0]).args(&cmd[1..]).spawn();
                });
            }
        }
    }

    fn reset_power_buttons(&self) {
        for b in &self.power_buttons {
            b.btn.remove_css_class("armed");
            b.label.set_label(power_display_label(b.action.label));
        }
        self.power_hint.set_label("");
    }

    /// The confirm window's own timer fired: only disarm if this is still
    /// the same arm (nothing re-armed since).
    fn power_confirm_expired(&self, generation: u32) {
        let confirm = self.power_confirm.borrow();
        let still_this_arm = confirm.armed_id().is_some() && confirm.generation() == generation;
        drop(confirm);
        if still_this_arm {
            self.power_confirm.borrow_mut().disarm();
            self.reset_power_buttons();
        }
    }

    // ── Keyboard (Esc / arrows / Enter) ────────────────────────────────

    fn entry_focused(&self) -> bool {
        let entry: &gtk4::Widget = self.entry.upcast_ref();
        GtkWindowExt::focus(&self.window).is_some_and(|f| &f == entry || f.is_ancestor(entry))
    }

    /// Window-level, capture-phase key handling. Esc works wherever focus
    /// is; arrows and Enter only drive the stage while the search field
    /// has focus (the mockup's `if(e.target!==q)return;`).
    fn handle_key(&self, key: gtk4::gdk::Key) -> gtk4::glib::Propagation {
        use gtk4::gdk::Key;
        if !self.open.get() {
            return gtk4::glib::Propagation::Proceed;
        }
        if key != Key::Escape && !self.entry_focused() {
            return gtk4::glib::Propagation::Proceed;
        }
        let query_empty = self.query.borrow().is_empty();
        match key {
            Key::Escape => {
                match pure::esc_action(query_empty) {
                    pure::EscAction::ClearQuery => {
                        self.entry.set_text("");
                        self.rec_sel.set(0);
                        self.render_recent();
                    }
                    pure::EscAction::Close => self.close(),
                }
                if self.open.get() {
                    self.entry.grab_focus();
                }
                gtk4::glib::Propagation::Stop
            }
            Key::Down | Key::Right if query_empty => {
                self.move_recent_selection(1);
                gtk4::glib::Propagation::Stop
            }
            Key::Up | Key::Left if query_empty => {
                self.move_recent_selection(-1);
                gtk4::glib::Propagation::Stop
            }
            Key::Down => {
                self.move_selection(1);
                gtk4::glib::Propagation::Stop
            }
            Key::Up => {
                self.move_selection(-1);
                gtk4::glib::Propagation::Stop
            }
            Key::Return | Key::KP_Enter => {
                self.activate_selection();
                gtk4::glib::Propagation::Stop
            }
            _ => gtk4::glib::Propagation::Proceed,
        }
    }
}

/// The stage's own power-row wording, keyed by the shared `POWER_ACTIONS`
/// ids.
fn power_display_label(id: &str) -> &str {
    match id {
        "lock" => "Lock",
        "sleep" => "Sleep",
        "restart" => "Restart",
        "off" => "Power off",
        other => other,
    }
}

fn set_chip_on(btn: &gtk4::Button, on: bool) {
    if on {
        btn.add_css_class("on");
    } else {
        btn.remove_css_class("on");
    }
}

fn make_chip(label: &str) -> gtk4::Button {
    let chip = gtk4::Button::with_label(label);
    chip.add_css_class("stage-chip");
    chip
}

/// A `.chips` row: wraps like `flex-wrap:wrap` with the mockup's 6px gaps.
fn make_chip_row() -> gtk4::FlowBox {
    let row = gtk4::FlowBox::new();
    row.add_css_class("stage-chips");
    row.set_selection_mode(gtk4::SelectionMode::None);
    row.set_column_spacing(6);
    row.set_row_spacing(6);
    row.set_homogeneous(false);
    row.set_max_children_per_line(12);
    row.set_halign(gtk4::Align::Start);
    row
}

fn first_char_upper(s: &str) -> String {
    s.chars()
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().collect::<String>())
}

/// `#rrggbb` for Pango markup from a theme colour read at runtime (only
/// for text whose CSS colour is opaque: GTK applies a label's CSS alpha on
/// top of any markup colour).
fn hex(c: &gtk4::gdk::RGBA) -> String {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c.red()), b(c.green()), b(c.blue()))
}

/// "HH:MM" local time of a notification.
fn clock_hhmm(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    gtk4::glib::DateTime::from_unix_local(secs)
        .map(|dt| format!("{:02}:{:02}", dt.hour(), dt.minute()))
        .unwrap_or_default()
}

/// `text-wrap:balance` within `max-width:{max_chars}ch`, for a centred
/// label: keeps the line count greedy wrapping at that width would give,
/// narrows the wrap width as far as that allows so the lines come out even,
/// and sets the result with explicit line breaks (a GTK label allocated
/// more than its natural width would otherwise rewrap to fill it).
fn balance_wrap(label: &gtk4::Label, markup: &str, max_chars: i32) {
    let ctx = label.pango_context();
    // `ch` is the width of "0" in the label's face.
    let probe = gtk4::pango::Layout::new(&ctx);
    probe.set_text("0");
    let ch = probe.size().0.max(1);
    let layout = gtk4::pango::Layout::new(&ctx);
    layout.set_markup(markup);
    layout.set_wrap(gtk4::pango::WrapMode::Word);
    let max = ch * max_chars;
    layout.set_width(max);
    let lines = layout.line_count();
    let (mut lo, mut hi) = (ch, max);
    while hi - lo > gtk4::pango::SCALE {
        let mid = (lo + hi) / 2;
        layout.set_width(mid);
        if layout.line_count() > lines {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    layout.set_width(hi);
    let text = layout.text();
    let breaks: Vec<usize> = layout
        .lines_readonly()
        .iter()
        .skip(1)
        .map(|l| text[..l.start_index() as usize].matches(' ').count())
        .collect();
    label.set_markup(&pure::break_markup_at_spaces(markup, &breaks));
}

/// Builds one `.card` from a pill widget.
fn make_card(parts: &pure::CardParts, widget_id: &str) -> gtk4::Box {
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 7);
    card.add_css_class("stage-card");
    let head = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let title = gtk4::Label::new(Some(&parts.title));
    title.add_css_class("stage-card-title");
    title.set_xalign(0.0);
    title.set_hexpand(true);
    title.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    head.append(&title);
    let tag = gtk4::Label::new(Some(&parts.tag));
    tag.add_css_class("stage-card-tag");
    head.append(&tag);
    card.append(&head);
    if let Some(node) = &parts.value {
        let v = crate::widgets::build_node(node, widget_id);
        v.add_css_class("stage-card-v");
        v.set_halign(gtk4::Align::Start);
        card.append(&v);
    }
    if let Some(value) = parts.rail {
        let rail = gtk4::ProgressBar::new();
        rail.add_css_class("stage-card-rail");
        rail.set_fraction(value);
        card.append(&rail);
    }
    if let Some(sub) = &parts.sub {
        let s = gtk4::Label::new(Some(sub));
        s.add_css_class("stage-card-s");
        s.set_xalign(0.0);
        s.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        card.append(&s);
    }
    card
}

fn run_shell(cmd: &str) {
    if let Err(e) = std::process::Command::new("bash")
        .args(["-c", cmd])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        eprintln!("breadbar: stage failed to run command {cmd:?}: {e}");
    }
}

/// Focus the window at `address`. Stock Hyprland takes
/// `hyprctl dispatch focuswindow address:X`; Lua-config Hyprland (BOS)
/// rewrites that into `hl.dispatch(focuswindow address:X)`, a Lua syntax
/// error, so fall back to `hl.dsp.focus({window=...})` via `hyprctl eval`,
/// the same two-step `bar::workspaces::switch_workspace` uses.
fn focus_window(address: String) {
    relm4::spawn(async move {
        let selector = format!("address:{address}");
        tracing::debug!(target: "breadbar::stage", %selector, "focusing window");
        let stock = tokio::process::Command::new("hyprctl")
            .args(["dispatch", "focuswindow", &selector])
            .output()
            .await;
        if let Ok(o) = &stock {
            let out = String::from_utf8_lossy(&o.stdout);
            let err = String::from_utf8_lossy(&o.stderr);
            if o.status.success() && !out.contains("hl.dispatch") && !err.contains("hl.dispatch") {
                tracing::debug!(target: "breadbar::stage", %selector, "focused via dispatch");
                return;
            }
            tracing::debug!(
                target: "breadbar::stage",
                %selector,
                stdout = %out.trim(),
                stderr = %err.trim(),
                "dispatch focuswindow rejected, trying the Lua form"
            );
        }
        let expr = format!("hl.dispatch(hl.dsp.focus({{window={selector:?}}}))");
        match tokio::process::Command::new("hyprctl")
            .args(["eval", &expr])
            .output()
            .await
        {
            Ok(o) if o.status.success() => {
                tracing::debug!(target: "breadbar::stage", %selector, "focused via eval")
            }
            Ok(o) => tracing::warn!(
                target: "breadbar::stage",
                %selector,
                stderr = %String::from_utf8_lossy(&o.stderr).trim(),
                "could not focus window"
            ),
            Err(e) => {
                tracing::warn!(target: "breadbar::stage", %selector, error = %e, "could not run hyprctl")
            }
        }
    });
}

/// `hyprctl clients -j`: the stage's own "Open windows" data source.
async fn fetch_open_windows() -> Vec<OpenWindow> {
    let Some(stdout) =
        crate::bar::proc::stdout_ok("hyprctl", &["clients", "-j"], Duration::from_secs(2)).await
    else {
        return Vec::new();
    };
    let Ok(arr) = serde_json::from_slice::<Vec<serde_json::Value>>(&stdout) else {
        return Vec::new();
    };
    arr.into_iter()
        .filter_map(|v| {
            let address = v["address"].as_str()?.to_string();
            let title = v["title"].as_str().unwrap_or("").to_string();
            let app = v["class"].as_str().unwrap_or("").to_string();
            let workspace = v["workspace"]["id"].as_i64().unwrap_or(0) as i32;
            Some(OpenWindow {
                address,
                title,
                app,
                workspace,
            })
        })
        .collect()
}

/// Draws the mockup's `.cone`: a radial gradient
/// (`46% 70% at 50% 0%`: light at .15, .05 at 50%, clear at 76%) filling a
/// `min(1100px,160vw)` x 120% box hung 40px above the top edge, clipped to
/// the trapezoid `polygon(40% 0,60% 0,82% 100%,18% 100%)`. The colour is the
/// widget's own CSS `color` (`@fg`); CSS drives its fade.
/// Logs how long one cone repaint took when it goes out of scope.
struct ConeDrawTimer(std::time::Instant);

impl Drop for ConeDrawTimer {
    fn drop(&mut self) {
        tracing::debug!(
            target: "breadbar::stage",
            ms = self.0.elapsed().as_secs_f64() * 1000.0,
            "cone repaint"
        );
    }
}

fn draw_cone(area: &gtk4::DrawingArea, cr: &gtk4::cairo::Context, width: i32, height: i32) {
    let _t = ConeDrawTimer(std::time::Instant::now());
    let (w, h) = (f64::from(width), f64::from(height));
    let cw = 1100f64.min(1.6 * w);
    let ch = 1.2 * h;
    let left = (w - cw) / 2.0;
    let top = -40.0;
    let c = area.color();
    let (r, g, b) = (
        f64::from(c.red()),
        f64::from(c.green()),
        f64::from(c.blue()),
    );

    cr.move_to(left + 0.40 * cw, top);
    cr.line_to(left + 0.60 * cw, top);
    cr.line_to(left + 0.82 * cw, top + ch);
    cr.line_to(left + 0.18 * cw, top + ch);
    cr.close_path();
    cr.clip();

    // Paint in a unit-circle space scaled to the gradient's ellipse; the
    // clip above was fixed in device space, so it still applies.
    cr.translate(left + cw / 2.0, top);
    cr.scale(0.46 * cw, 0.70 * ch);
    let grad = gtk4::cairo::RadialGradient::new(0.0, 0.0, 0.0, 0.0, 0.0, 1.0);
    grad.add_color_stop_rgba(0.0, r, g, b, 0.15);
    grad.add_color_stop_rgba(0.5, r, g, b, 0.05);
    grad.add_color_stop_rgba(0.76, r, g, b, 0.0);
    grad.add_color_stop_rgba(1.0, r, g, b, 0.0);
    let _ = cr.set_source(&grad);
    let _ = cr.paint();
}

/// Holds a floor zone at the width the grid gives it. A plain box would grow
/// to its content's natural width (a long notification or track title),
/// while CSS grid tracks stay `1fr 1.15fr 1fr` and the text ellipsizes. A
/// non-scrolling `ScrolledWindow` reports its child's minimum width as its
/// natural width, which is exactly that behaviour.
fn zone_holder(zone: &gtk4::Box) -> gtk4::ScrolledWindow {
    let holder = gtk4::ScrolledWindow::new();
    holder.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Never);
    holder.set_propagate_natural_height(true);
    holder.set_valign(gtk4::Align::Start);
    holder.set_child(Some(zone));
    holder
}

/// A `.label` row: mono caps title on the left, an optional value right.
fn label_row(title: &str, value: Option<&gtk4::Label>) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let t = gtk4::Label::new(Some(title));
    t.add_css_class("stage-ctl-label");
    t.set_hexpand(true);
    t.set_xalign(0.0);
    row.append(&t);
    if let Some(v) = value {
        v.add_css_class("stage-ctl-value");
        row.append(v);
    }
    row
}

fn make_slider(max: f64) -> gtk4::Scale {
    let s = gtk4::Scale::with_range(gtk4::Orientation::Horizontal, 0.0, max, 0.01);
    s.set_draw_value(false);
    s.add_css_class("stage-slider");
    s
}

/// Builds the stage window and its whole widget tree, wraps it in an `Rc`,
/// and wires every closure that needs to call back into it. `shared` is
/// the state every bar instance shares (notification history, DND, which
/// stage is open).
pub fn build(sender: ComponentSender<App>, shared: Shared) -> Rc<Stage> {
    let window = gtk4::Window::new();
    window.add_css_class("breadbar-stage");
    window.init_layer_shell();
    window.set_namespace(Some("breadbar-stage"));
    window.set_layer(Layer::Overlay);
    for edge in [
        gtk4_layer_shell::Edge::Top,
        gtk4_layer_shell::Edge::Bottom,
        gtk4_layer_shell::Edge::Left,
        gtk4_layer_shell::Edge::Right,
    ] {
        window.set_anchor(edge, true);
    }
    window.set_exclusive_zone(-1);
    window.set_keyboard_mode(KeyboardMode::None);
    window.set_visible(false);

    let root = gtk4::Overlay::new();
    root.add_css_class("stage-root");
    let house = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    house.add_css_class("stage-house");
    house.set_hexpand(true);
    house.set_vexpand(true);
    root.set_child(Some(&house));

    let cone = gtk4::DrawingArea::new();
    cone.add_css_class("stage-cone");
    cone.set_can_target(false);
    cone.set_draw_func(draw_cone);
    root.add_overlay(&cone);
    root.set_measure_overlay(&cone, false);

    // The column stays targetable: GTK's `can-target = false` makes every
    // descendant unpickable (unlike CSS `pointer-events`). Scrim-vs-content
    // is decided by the overlay's click gesture below instead. Wrapped in a
    // vertical scroller so a stacked (narrow) floor taller than the output
    // still reaches the power row (`.stage{overflow-y:auto}`).
    let column = gtk4::Box::new(gtk4::Orientation::Vertical, 20);
    column.add_css_class("stage-column");
    column.set_halign(gtk4::Align::Center);
    column.set_valign(gtk4::Align::Start);
    column.set_margin_top(COLUMN_TOP - COLUMN_SLIDE);
    column.set_margin_bottom(32);
    let scroller = gtk4::ScrolledWindow::new();
    scroller.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
    scroller.set_child(Some(&column));
    root.add_overlay(&scroller);
    root.set_measure_overlay(&scroller, false);
    window.set_child(Some(&root));

    // ── Search field ───────────────────────────────────────────────────
    let search_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    search_row.add_css_class("stage-search");
    search_row.set_halign(gtk4::Align::Center);
    let mode_badge = gtk4::Label::new(None);
    mode_badge.add_css_class("stage-mode");
    mode_badge.set_valign(gtk4::Align::Center);
    mode_badge.set_visible(false);
    search_row.append(&mode_badge);
    let entry = gtk4::Entry::new();
    entry.set_has_frame(false);
    entry.add_css_class("stage-entry");
    entry.set_hexpand(true);
    entry.set_valign(gtk4::Align::Center);
    entry.set_placeholder_text(Some("Search apps and windows"));
    search_row.append(&entry);
    let esc_hint = gtk4::Label::new(Some("esc"));
    esc_hint.add_css_class("stage-kbd");
    esc_hint.set_valign(gtk4::Align::Center);
    search_row.append(&esc_hint);
    column.append(&search_row);

    // ── Idle: recent apps + floor + power row ──────────────────────────
    let idle_box = gtk4::Box::new(gtk4::Orientation::Vertical, 22);
    idle_box.add_css_class("stage-idle");
    column.append(&idle_box);

    let recent_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    recent_box.add_css_class("stage-recent");
    recent_box.set_halign(gtk4::Align::Center);
    let recent_inner = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    recent_inner.set_halign(gtk4::Align::Center);
    recent_inner.set_hexpand(true);
    recent_box.append(&recent_inner);
    let mut recent_tiles = Vec::new();
    for _ in 0..MAX_RECENT_TILES {
        let tile = gtk4::Button::new();
        tile.add_css_class("stage-tile");
        tile.set_visible(false);
        let inner = gtk4::Box::new(gtk4::Orientation::Vertical, 7);
        let glyph = gtk4::Label::new(None);
        glyph.add_css_class("stage-tile-glyph");
        glyph.set_halign(gtk4::Align::Center);
        inner.append(&glyph);
        let name = gtk4::Label::new(None);
        name.add_css_class("stage-tile-name");
        name.set_ellipsize(gtk4::pango::EllipsizeMode::End);
        name.set_max_width_chars(12);
        inner.append(&name);
        tile.set_child(Some(&inner));
        recent_inner.append(&tile);
        recent_tiles.push(tile);
    }
    idle_box.append(&recent_box);

    let floor = gtk4::Box::new(gtk4::Orientation::Horizontal, 18);
    floor.add_css_class("stage-floor");
    idle_box.append(&floor);

    // Left zone: controls.
    let controls_zone = gtk4::Box::new(gtk4::Orientation::Vertical, 14);
    controls_zone.add_css_class("stage-zone");
    controls_zone.set_valign(gtk4::Align::Start);

    let vol_pct_lbl = gtk4::Label::new(Some("0%"));
    let vol_scale = make_slider(1.5);
    let vol_ctl = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    vol_ctl.append(&label_row("Sound", Some(&vol_pct_lbl)));
    vol_ctl.append(&vol_scale);
    // The output chips sit inside the Sound `.ctl` (8px gap), not the zone.
    let sinks_box = make_chip_row();
    vol_ctl.append(&sinks_box);
    controls_zone.append(&vol_ctl);

    // `.play{grid-template-columns:auto 1fr auto;gap:10px}`.
    let play_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    play_row.add_css_class("stage-play");
    let art = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    art.add_css_class("stage-art");
    art.set_valign(gtk4::Align::Center);
    art.set_overflow(gtk4::Overflow::Hidden);
    let art_picture = gtk4::Picture::new();
    art_picture.set_content_fit(gtk4::ContentFit::Cover);
    art_picture.set_size_request(34, 34);
    art_picture.set_visible(false);
    art.append(&art_picture);
    play_row.append(&art);
    let track_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    track_box.set_hexpand(true);
    track_box.set_valign(gtk4::Align::Center);
    let media_track_lbl = gtk4::Label::new(Some("Nothing playing"));
    media_track_lbl.add_css_class("stage-track-title");
    media_track_lbl.set_xalign(0.0);
    media_track_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    media_track_lbl.set_max_width_chars(1);
    track_box.append(&media_track_lbl);
    let media_artist_lbl = gtk4::Label::new(None);
    media_artist_lbl.add_css_class("stage-track-artist");
    media_artist_lbl.set_xalign(0.0);
    media_artist_lbl.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    media_artist_lbl.set_max_width_chars(1);
    track_box.append(&media_artist_lbl);
    play_row.append(&track_box);
    let transport = gtk4::Box::new(gtk4::Orientation::Horizontal, 2);
    transport.add_css_class("stage-transport");
    transport.set_valign(gtk4::Align::Center);
    let prev_btn = gtk4::Button::with_label("\u{25c0}\u{25c0}");
    prev_btn.set_tooltip_text(Some("Previous"));
    prev_btn.connect_clicked(|_| crate::bar::media::spawn_cmd("previous"));
    transport.append(&prev_btn);
    let media_play_btn = gtk4::Button::with_label("\u{25b6}");
    media_play_btn.set_tooltip_text(Some("Play or pause"));
    media_play_btn.connect_clicked(|_| crate::bar::media::spawn_cmd("play-pause"));
    transport.append(&media_play_btn);
    let next_btn = gtk4::Button::with_label("\u{25b6}\u{25b6}");
    next_btn.set_tooltip_text(Some("Next"));
    next_btn.connect_clicked(|_| crate::bar::media::spawn_cmd("next"));
    transport.append(&next_btn);
    play_row.append(&transport);
    controls_zone.append(&play_row);

    let bri_pct_lbl = gtk4::Label::new(Some("0%"));
    let bri_scale = make_slider(1.0);
    let bri_ctl = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    bri_ctl.append(&label_row("Light", Some(&bri_pct_lbl)));
    bri_ctl.append(&bri_scale);
    controls_zone.append(&bri_ctl);

    let toggle_row = make_chip_row();
    let wifi_chip = make_chip("Wi-Fi");
    toggle_row.append(&wifi_chip);
    let bt_chip = make_chip("Bluetooth");
    toggle_row.append(&bt_chip);
    let dnd_chip = make_chip("Do not disturb");
    toggle_row.append(&dnd_chip);
    controls_zone.append(&toggle_row);
    let controls_zone = zone_holder(&controls_zone);
    floor.append(&controls_zone);

    // Centre zone: time, sentence, widget cards.
    let centre_zone = gtk4::Box::new(gtk4::Orientation::Vertical, 14);
    centre_zone.add_css_class("stage-zone");
    centre_zone.add_css_class("stage-zone-centre");
    centre_zone.set_valign(gtk4::Align::Start);
    let big_time_lbl = gtk4::Label::new(Some("--:--"));
    big_time_lbl.add_css_class("stage-big-time");
    centre_zone.append(&big_time_lbl);
    let sentence_lbl = gtk4::Label::new(None);
    sentence_lbl.add_css_class("stage-sentence");
    sentence_lbl.set_justify(gtk4::Justification::Center);
    sentence_lbl.set_halign(gtk4::Align::Center);
    centre_zone.append(&sentence_lbl);
    let cards_box = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    cards_box.add_css_class("stage-cards");
    cards_box.set_visible(false);
    centre_zone.append(&cards_box);
    let brass_probe = gtk4::Label::new(None);
    brass_probe.add_css_class("stage-brass-probe");
    brass_probe.set_visible(false);
    centre_zone.append(&brass_probe);
    let centre_zone = zone_holder(&centre_zone);
    floor.append(&centre_zone);

    // Right zone: notifications.
    let notif_zone = gtk4::Box::new(gtk4::Orientation::Vertical, 14);
    notif_zone.add_css_class("stage-zone");
    notif_zone.set_valign(gtk4::Align::Start);
    let notif_header = label_row("Notifications", None);
    let clear_all_btn = gtk4::Button::with_label("Clear all");
    clear_all_btn.add_css_class("stage-clear");
    clear_all_btn.set_valign(gtk4::Align::Center);
    notif_header.append(&clear_all_btn);
    notif_zone.append(&notif_header);
    let notes_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    notes_box.add_css_class("stage-notes");
    notif_zone.append(&notes_box);
    let notif_zone = zone_holder(&notif_zone);
    floor.append(&notif_zone);

    // ── Power row ──────────────────────────────────────────────────────
    let power_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    power_row.add_css_class("stage-power");
    power_row.set_halign(gtk4::Align::Center);
    let mut power_buttons = Vec::new();
    for action in crate::POWER_ACTIONS {
        let btn = gtk4::Button::new();
        btn.add_css_class("stage-power-btn");
        let label = gtk4::Label::new(Some(power_display_label(action.label)));
        btn.set_child(Some(&label));
        btn.set_tooltip_text(Some(action.tooltip));
        power_row.append(&btn);
        power_buttons.push(PowerButton {
            action: *action,
            btn,
            label,
        });
    }
    let power_hint = gtk4::Label::new(None);
    power_hint.add_css_class("stage-power-hint");
    power_row.append(&power_hint);
    let settings_toggle = gtk4::Button::new();
    settings_toggle.add_css_class("stage-power-btn");
    settings_toggle.add_css_class("stage-settings-toggle");
    settings_toggle.set_child(Some(&gtk4::Label::new(Some("Settings"))));
    settings_toggle.set_tooltip_text(Some("Pill and widget settings"));
    power_row.append(&settings_toggle);
    idle_box.append(&power_row);

    // ── Settings menu ──────────────────────────────────────────────────
    let settings_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 18);
    settings_row.add_css_class("stage-settings");
    settings_row.set_halign(gtk4::Align::Center);
    let group = |title: &str, chips: &[&gtk4::Button]| {
        let g = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
        let t = gtk4::Label::new(Some(title));
        t.add_css_class("stage-ctl-label");
        t.set_valign(gtk4::Align::Center);
        g.append(&t);
        for c in chips {
            g.append(*c);
        }
        g
    };
    let autohide_chip = make_chip("Auto-hide");
    let widgets_chip = make_chip("Widgets");
    let ws_chips = [make_chip("None"), make_chip("Current"), make_chip("All")];
    settings_row.append(&group("Pill", &[&autohide_chip, &widgets_chip]));
    settings_row.append(&group(
        "Workspaces",
        &[&ws_chips[0], &ws_chips[1], &ws_chips[2]],
    ));
    let settings_revealer = gtk4::Revealer::new();
    settings_revealer.set_transition_type(gtk4::RevealerTransitionType::SlideDown);
    settings_revealer.set_transition_duration(180);
    settings_revealer.set_child(Some(&settings_row));
    idle_box.append(&settings_revealer);

    // ── Results ────────────────────────────────────────────────────────
    let results_box = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    results_box.add_css_class("stage-results");
    results_box.set_halign(gtk4::Align::Center);
    results_box.set_visible(false);
    column.append(&results_box);

    // ── Key hints ──────────────────────────────────────────────────────
    let foot = gtk4::Box::new(gtk4::Orientation::Horizontal, 16);
    foot.add_css_class("stage-foot");
    foot.set_halign(gtk4::Align::Center);
    for (key, word) in pure::FOOTER_HINTS {
        // `<span><b>key</b> word</span>`: the key light at weight 500.
        let hint = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
        let k = gtk4::Label::new(Some(key));
        k.add_css_class("stage-foot-key");
        hint.append(&k);
        hint.append(&gtk4::Label::new(Some(&format!(" {word}"))));
        foot.append(&hint);
    }
    column.append(&foot);

    // Clicking the dimmed house closes; clicking content does not. Decided
    // by what the press lands on: the scrim itself, or one of the purely
    // layout containers around the content (the mockup's `.column`/`.idle`
    // are `pointer-events:none`, so their empty space falls through).
    let click = gtk4::GestureClick::new();
    click.set_button(0);
    root.add_controller(click.clone());
    let mut scrim_targets: Vec<gtk4::Widget> = vec![
        house.clone().upcast(),
        scroller.clone().upcast(),
        column.clone().upcast(),
        idle_box.clone().upcast(),
        floor.clone().upcast(),
        recent_box.clone().upcast(),
        recent_inner.clone().upcast(),
        power_row.clone().upcast(),
        settings_row.clone().upcast(),
        foot.clone().upcast(),
        results_box.clone().upcast(),
    ];
    if let Some(viewport) = scroller.child() {
        scrim_targets.push(viewport);
    }

    // Capture phase on the window: GtkEntry consumes Return itself, and Esc
    // must still close the stage after a click moved focus elsewhere.
    let keys = gtk4::EventControllerKey::new();
    keys.set_propagation_phase(gtk4::PropagationPhase::Capture);
    window.add_controller(keys.clone());

    let launcher_history = bread_launcher::LaunchHistory::load(bread_launcher::LAUNCHER_APP);

    let stage = Rc::new(Stage {
        window,
        root,
        column,
        column_anim: RefCell::new(None),
        fade_token: Cell::new(0),
        search_row,
        entry,
        mode_badge,
        idle_box,
        recent_box,
        floor,
        controls_zone,
        centre_zone,
        notif_zone,
        results_box,
        result_pool: RefCell::new(None),
        settings: SettingsPanel {
            toggle: settings_toggle,
            revealer: settings_revealer,
            autohide: autohide_chip,
            widgets: widgets_chip,
            workspaces: ws_chips,
            prefs: Cell::new(crate::spotlight_prefs::load()),
        },
        recent_tiles,
        vol_scale,
        vol_pct_lbl,
        sinks_box,
        art_picture,
        media_track_lbl,
        media_artist_lbl,
        media_play_btn,
        bri_scale,
        bri_pct_lbl,
        wifi_chip,
        bt_chip,
        dnd_chip,
        big_time_lbl,
        sentence_lbl,
        cards_box,
        brass_probe,
        status: RefCell::new(Status::default()),
        notes_box,
        clear_all_btn,
        power_buttons,
        power_confirm: RefCell::new(PowerConfirm::new()),
        power_hint,
        open: Cell::new(false),
        query: RefCell::new(String::new()),
        shared: shared.clone(),
        sel: Cell::new(0),
        rec_sel: Cell::new(-1),
        results: RefCell::new(StageResults::Groups(Vec::new())),
        windows: RefCell::new(Vec::new()),
        all_apps: RefCell::new(Vec::new()),
        recent: RefCell::new(Vec::new()),
        launcher_history: RefCell::new(launcher_history),
        sample_apps: Cell::new(false),
        loading: Cell::new(false),
        self_weak: RefCell::new(Weak::new()),
        sender,
    });
    *stage.self_weak.borrow_mut() = Rc::downgrade(&stage);
    shared.stages.borrow_mut().push(Rc::downgrade(&stage));

    // ── Wire every closure that calls back into `stage` ────────────────

    {
        let s = stage.clone();
        let root = stage.root.clone();
        click.connect_pressed(move |_, _, x, y| {
            let on_scrim = match root.pick(x, y, gtk4::PickFlags::DEFAULT) {
                None => true,
                Some(w) => scrim_targets.contains(&w),
            };
            if on_scrim {
                s.close();
            }
        });
    }
    {
        let s = stage.clone();
        keys.connect_key_pressed(move |_, key, _, _| s.handle_key(key));
    }
    {
        // Anything that unmaps the surface behind our back (the compositor
        // closing it when its output is unplugged) must still clear `open`.
        // `close()` is a no-op once `open` is false.
        let s = stage.clone();
        stage.window.connect_unmap(move |_| s.close());
    }
    {
        let s = stage.clone();
        stage
            .entry
            .connect_changed(move |e| s.on_query_changed(&e.text()));
    }
    {
        let s = stage.clone();
        stage.clear_all_btn.connect_clicked(move |_| {
            s.shared.store.lock().unwrap().clear();
            history::persist(&s.shared.store);
            s.refresh_notifications();
        });
    }
    for (i, tile) in stage.recent_tiles.iter().enumerate() {
        let s = stage.clone();
        tile.connect_clicked(move |_| {
            s.rec_sel.set(i as i32);
            s.activate_recent_tile(i as i32);
        });
    }
    {
        let s = stage.clone();
        stage.vol_scale.connect_value_changed(move |sc| {
            s.sync_level_labels();
            if s.loading.get() {
                return;
            }
            crate::bar::control::spawn_set_volume(sc.value());
        });
    }
    {
        let s = stage.clone();
        stage.bri_scale.connect_value_changed(move |sc| {
            s.sync_level_labels();
            if s.loading.get() {
                return;
            }
            crate::bar::control::spawn_set_brightness(sc.value());
        });
    }
    stage.wifi_chip.connect_clicked(|btn| {
        let on = !btn.has_css_class("on");
        crate::bar::wifi::spawn_set_enabled(on);
        set_chip_on(btn, on);
    });
    stage.bt_chip.connect_clicked(|btn| {
        let on = !btn.has_css_class("on");
        crate::bar::bluetooth::spawn_set_powered(on);
        set_chip_on(btn, on);
    });
    {
        let shared = shared.clone();
        stage.dnd_chip.connect_clicked(move |_| {
            shared.set_dnd(!shared.dnd.get());
        });
    }
    {
        let s = stage.clone();
        stage.settings.toggle.connect_clicked(move |_| {
            let open = !s.settings.revealer.reveals_child();
            s.settings.revealer.set_reveal_child(open);
            set_chip_on(&s.settings.toggle, open);
        });
        let s = stage.clone();
        stage.settings.autohide.connect_clicked(move |_| {
            s.change_prefs(|p| p.autohide = Some(!p.autohide_on()));
        });
        let s = stage.clone();
        stage.settings.widgets.connect_clicked(move |_| {
            s.change_prefs(|p| p.widgets = Some(!p.widgets_on()));
        });
        use crate::spotlight_prefs::WorkspacesPref;
        for (chip, pref) in stage.settings.workspaces.iter().zip([
            WorkspacesPref::None,
            WorkspacesPref::Current,
            WorkspacesPref::All,
        ]) {
            let s = stage.clone();
            chip.connect_clicked(move |_| s.change_prefs(|p| p.workspaces = Some(pref)));
        }
    }
    for button in &stage.power_buttons {
        let s = stage.clone();
        let id = button.action.label;
        button.btn.connect_clicked(move |_| s.press_power(id));
    }
    stage
}
