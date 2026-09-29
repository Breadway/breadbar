//! BAR-30 phases 2-4: GTK-free logic for the spotlight stage, kept separate
//! from `stage/mod.rs` (which owns the widget tree and every real I/O
//! source) so the parts that matter for correctness are unit-testable
//! without a display, the same split `pill_autohide::State` already
//! established for phase 1.
//!
//! Covers: the Esc-key state machine, the power row's confirm-armed state
//! machine, and phase 3's result grouping/filtering (open windows, apps,
//! commands, calc) on top of `bread_launcher`'s own pure query/match/calc
//! primitives.

use std::time::{Duration, Instant};

// ── Esc: clears the query first, then closes ───────────────────────────────

/// What pressing Esc should do while the stage is open, mirroring the
/// mockup's own `if(e.key==="Escape"&&isOpen()){if(q.value){clear}else{close}}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscAction {
    /// The query was non-empty: clear it (and reselect the first recent
    /// tile); the stage stays open.
    ClearQuery,
    /// The query was already empty: close the stage.
    Close,
}

pub fn esc_action(query_is_empty: bool) -> EscAction {
    if query_is_empty {
        EscAction::Close
    } else {
        EscAction::ClearQuery
    }
}

// ── Power row: confirm-armed state machine ─────────────────────────────────

/// One power-row press outcome: `Run` fires the action now (no confirmation
/// needed, or this is a second press on the still-armed same action within
/// the confirm window); `Arm` requires a following press instead and should
/// show the red armed hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerPress {
    Run,
    Arm,
    /// A confirming press that came too soon after the previous press of
    /// the same armed action (see [`MIN_REPRESS_GAP`]): swallowed, the arm
    /// stays as it is.
    Ignore,
}

/// The shortest gap between two presses of the same armed action that
/// still counts as a deliberate confirmation. Anything faster is a
/// double-click or a held key's autorepeat (a focused GTK button activates
/// on every repeat), neither of which should be able to reboot or power off
/// the machine. Measured from the previous press, ignored ones included, so
/// a held key never produces a qualifying gap.
pub const MIN_REPRESS_GAP: Duration = Duration::from_millis(400);

/// Confirm-armed state for the stage's power row (Restart/Power off need a
/// second press within a window; Lock/Sleep never do). A plain struct
/// (rather than the raw closures `main.rs`'s existing control-panel power
/// row uses today) so the decision itself is unit-testable without GTK
/// buttons or a real timer; `stage/mod.rs` drives a real
/// `glib::timeout_add_local_once` off the same `window` duration purely to
/// reset the button's own label/CSS, since [`press`] already treats a stale
/// arm as unarmed on its own.
#[derive(Debug, Default)]
pub struct PowerConfirm {
    /// `(id, armed_at, last_press)`.
    armed: Option<(&'static str, Instant, Instant)>,
    /// Bumped on every arm, so a stale expiry timer from an earlier arm
    /// cannot cut a newer one short.
    generation: u32,
}

impl PowerConfirm {
    pub fn new() -> Self {
        Self {
            armed: None,
            generation: 0,
        }
    }

    /// The current arm's generation (see the field doc).
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Press `id` (a power action's stable id, e.g. "restart") at `now`,
    /// needing `confirm`. Returns [`PowerPress::Run`] if `confirm` is false,
    /// or this is a second press of the SAME id while its previous arm is
    /// still within `window`; otherwise arms (or re-arms) `id` and returns
    /// [`PowerPress::Arm`]. A press that runs, or a press of a different id,
    /// clears any other pending arm, mirroring the mockup's own single
    /// `armed` slot (`disarm()` called unconditionally before either arming
    /// or running).
    pub fn press(
        &mut self,
        id: &'static str,
        confirm: bool,
        now: Instant,
        window: Duration,
    ) -> PowerPress {
        if !confirm {
            self.armed = None;
            return PowerPress::Run;
        }
        match self.armed {
            Some((armed_id, at, last)) if armed_id == id && now < at + window => {
                if now < last + MIN_REPRESS_GAP {
                    self.armed = Some((armed_id, at, now));
                    PowerPress::Ignore
                } else {
                    self.armed = None;
                    PowerPress::Run
                }
            }
            _ => {
                self.armed = Some((id, now, now));
                self.generation = self.generation.wrapping_add(1);
                PowerPress::Arm
            }
        }
    }

    /// Clears any pending arm (closing the stage, or the confirm window's
    /// own timer firing).
    pub fn disarm(&mut self) {
        self.armed = None;
    }

    /// The currently-armed action's id, if any (used to know which button's
    /// CSS/label to reset on disarm).
    pub fn armed_id(&self) -> Option<&'static str> {
        self.armed.map(|(id, _, _)| id)
    }
}

// ── Open windows (Hyprland clients) ────────────────────────────────────────

/// One open window, as listed by `hyprctl clients -j` (fetched in
/// `stage/mod.rs`; this struct is the pure projection everything below
/// filters/renders).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWindow {
    pub address: String,
    pub title: String,
    /// The window's class (`initialClass`/`class` in `hyprctl`'s JSON):
    /// the mockup's own "app" column for a window row.
    pub app: String,
    pub workspace: i32,
}

/// Filters `windows` by `query` the mockup's way: a case-insensitive
/// substring of "`app` `title`" (`hit(a+" "+title,t)`), where `app` is the
/// window's display name.
pub fn filter_windows(
    query: &str,
    windows: &[OpenWindow],
    apps: &[bread_launcher::DesktopEntry],
) -> Vec<OpenWindow> {
    let q = query.trim().to_lowercase();
    windows
        .iter()
        .filter(|w| {
            format!("{} {}", window_app_name(&w.app, apps), w.title)
                .to_lowercase()
                .contains(&q)
        })
        .cloned()
        .collect()
}

// ── Result rows/groups (phase 3) ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultKind {
    Window,
    App,
    Command,
}

impl ResultKind {
    /// The mockup's own `.row .act` badge text ("open ↵" / "switch ↵" /
    /// "run ↵"; `mark()`'s CSS shows it only on the selected row).
    pub fn action_label(self) -> &'static str {
        match self {
            ResultKind::Window => "switch",
            ResultKind::App => "open",
            ResultKind::Command => "run",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultRow {
    pub kind: ResultKind,
    /// The row's glyph-tile letter (`it.g` in the mockup: a window's app
    /// initial, an app's own initial).
    pub glyph: String,
    pub name: String,
    pub sub: String,
    /// What activating the row acts on, unambiguously: a window's Hyprland
    /// address, an app's desktop-file id, a command's id. Display names
    /// are not unique (two terminals can share a title), so activation
    /// never looks a row back up by `name`.
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultGroup {
    pub title: &'static str,
    pub rows: Vec<ResultRow>,
}

/// The stage's full results view for one query, mirroring the mockup's own
/// `render()`: calc mode shows one answer line, command mode shows one
/// group, plain-text mode shows "Open windows" then "Apps" (empty groups
/// dropped), and no rows at all (in any mode) shows the empty-state hint.
#[derive(Debug, Clone, PartialEq)]
pub enum StageResults {
    Calc {
        expr: String,
        answer: Option<String>,
    },
    Groups(Vec<ResultGroup>),
    /// "Nothing matches "<query>". Try = for maths or > for commands."
    Empty {
        query: String,
    },
}

impl StageResults {
    /// Every row across every group, in display order: the flat list
    /// Up/Down/Enter index against (mirrors the mockup's own
    /// `list=groups.flatMap(g=>g[1])`). Empty for `Calc`/`Empty`.
    pub fn rows(&self) -> Vec<&ResultRow> {
        match self {
            StageResults::Groups(groups) => groups.iter().flat_map(|g| &g.rows).collect(),
            _ => Vec::new(),
        }
    }
}

fn window_row(w: &OpenWindow, apps: &[bread_launcher::DesktopEntry]) -> ResultRow {
    let app = window_app_name(&w.app, apps);
    ResultRow {
        kind: ResultKind::Window,
        glyph: first_char_upper(&app),
        name: w.title.clone(),
        sub: format!("{app} · ws {}", w.workspace),
        target: w.address.clone(),
    }
}

fn app_row(entry: &bread_launcher::DesktopEntry) -> ResultRow {
    ResultRow {
        kind: ResultKind::App,
        glyph: first_char_upper(&entry.name),
        name: entry.name.clone(),
        sub: app_kind(&entry.categories),
        target: entry.id.clone(),
    }
}

fn command_row(cmd: &StageCommand) -> ResultRow {
    ResultRow {
        kind: ResultKind::Command,
        glyph: "\u{203a}".to_string(),
        name: cmd.name.to_string(),
        sub: cmd.sub.to_string(),
        target: cmd.id.to_string(),
    }
}

fn first_char_upper(s: &str) -> String {
    s.chars()
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().collect::<String>())
}

/// Builds phase 3's grouped results for one query, given the current data
/// sources. `apps` should already be `bread_launcher::load_sorted_entries`'s
/// output (ranking is inherited; this only filters and groups). Matching is
/// the mockup's: a case-insensitive substring, of "`name` `kind`" for apps.
/// Most rows a result group shows. Every row is a real widget, and a single
/// letter matches most installed apps (99 rows for "o" on a 129-app machine),
/// which stalled each keystroke by up to half a second in layout. The stage
/// also keeps exactly this many pooled row widgets per group.
pub const MAX_GROUP_ROWS: usize = 8;

/// Most result groups at once (open windows + apps); the pool's size.
pub const MAX_GROUPS: usize = 2;

pub fn build_results(
    parsed: &bread_launcher::ParsedQuery,
    windows: &[OpenWindow],
    apps: &[bread_launcher::DesktopEntry],
) -> StageResults {
    use bread_launcher::QueryKind;
    let q = parsed.value.trim().to_lowercase();
    match parsed.kind {
        QueryKind::Calc => StageResults::Calc {
            expr: parsed.value.clone(),
            answer: bread_launcher::eval_calc(&parsed.value),
        },
        QueryKind::Cmd => {
            let rows: Vec<ResultRow> = filter_stage_commands(&parsed.value)
                .iter()
                .take(MAX_GROUP_ROWS)
                .map(command_row)
                .collect();
            if rows.is_empty() {
                StageResults::Empty {
                    query: parsed.value.clone(),
                }
            } else {
                StageResults::Groups(vec![ResultGroup {
                    title: "Commands",
                    rows,
                }])
            }
        }
        // `Url` mode (a leading `.`) has no grouped-results equivalent in
        // the mockup; treated as a plain apps query so a `.`-prefixed query
        // still shows something rather than silently doing nothing.
        QueryKind::Apps | QueryKind::Url => {
            let win_rows: Vec<ResultRow> = filter_windows(&parsed.value, windows, apps)
                .iter()
                .take(MAX_GROUP_ROWS)
                .map(|w| window_row(w, apps))
                .collect();
            let app_rows: Vec<ResultRow> = apps
                .iter()
                .filter(|e| {
                    format!("{} {}", e.name, app_kind(&e.categories))
                        .to_lowercase()
                        .contains(&q)
                })
                .take(MAX_GROUP_ROWS)
                .map(app_row)
                .collect();
            let mut groups = Vec::new();
            if !win_rows.is_empty() {
                groups.push(ResultGroup {
                    title: "Open windows",
                    rows: win_rows,
                });
            }
            if !app_rows.is_empty() {
                groups.push(ResultGroup {
                    title: "Apps",
                    rows: app_rows,
                });
            }
            if groups.is_empty() {
                StageResults::Empty {
                    query: parsed.value.clone(),
                }
            } else {
                StageResults::Groups(groups)
            }
        }
    }
}

/// Clamps a selection index into `[0, len)`, or `None` for an empty list.
/// Shared by the results list and the recent-apps row's arrow-key handling.
pub fn clamp_selection(len: usize, index: i32) -> Option<usize> {
    if len == 0 {
        None
    } else {
        Some(index.clamp(0, len as i32 - 1) as usize)
    }
}

// ── BAR-31: layout, copy and commands from the mockup ──────────────────────

/// The column's inner width on an output `output_width` px wide:
/// `.column{width:min(1000px,100%);padding:0 16px}`.
pub fn column_inner_width(output_width: i32) -> i32 {
    (output_width.min(1000) - 32).max(0)
}

/// The floor's three zone widths for a column `inner` px wide:
/// `.floor{grid-template-columns:1fr 1.15fr 1fr;gap:18px}`. The rounding
/// remainder goes to the centre so the three always sum to the track space.
pub fn zone_widths(inner: i32) -> [i32; 3] {
    let tracks = (inner - 2 * 18).max(0) as f64;
    let side = (tracks / 3.15).floor() as i32;
    let centre = tracks as i32 - 2 * side;
    [side, centre, side]
}

/// "5h left", "2h 40m left", "40m left": the status sentence's battery
/// clause (the mockup's `83% battery, 5h left`).
pub fn format_time_left(minutes: u32) -> String {
    let (h, m) = (minutes / 60, minutes % 60);
    match (h, m) {
        (0, m) => format!("{m}m left"),
        (h, 0) => format!("{h}h left"),
        (h, m) => format!("{h}h {m}m left"),
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The centre zone's status sentence as Pango markup, mirroring the
/// mockup's `sentence()`: `"{day}. <b>{pct}%</b> battery{tail}. On
/// <b>{network}</b>."`, where the tail is ", plug in soon" when low, else
/// ", {time left}" when known. The label's own colour is the opaque light
/// tone; running text is dropped to the ash tone's 52% and each `<b>` (the
/// mockup's `.sentence b`: weight 500 in the light colour) restores full
/// alpha. Colouring by alpha keeps it working whatever the palette is.
pub fn status_sentence(
    day: &str,
    battery: Option<u8>,
    low: bool,
    minutes_left: Option<u32>,
    network: &str,
) -> String {
    let em = |t: &str| format!("<span weight=\"500\" fgalpha=\"100%\">{}</span>", escape(t));
    let mut out = format!("<span fgalpha=\"52%\">{}.", escape(day));
    match battery {
        Some(pct) => {
            out.push_str(&format!(" {} battery", em(&format!("{pct}%"))));
            if low {
                out.push_str(", plug in soon");
            } else if let Some(m) = minutes_left {
                out.push_str(&format!(", {}", format_time_left(m)));
            }
            out.push('.');
        }
        None => out.push_str(" On AC power."),
    }
    if !network.is_empty() && network != "\u{2014}" {
        out.push_str(&format!(" On {}.", em(network)));
    }
    out.push_str("</span>");
    out
}

/// Replaces the text spaces (never those inside a markup tag) whose
/// 1-based ordinals are in `after`, counting from the start of the text, with
/// line breaks: `after = [4]` breaks at the 4th space.
pub fn break_markup_at_spaces(markup: &str, after: &[usize]) -> String {
    let mut out = String::with_capacity(markup.len());
    let mut in_tag = false;
    let mut spaces = 0;
    for c in markup.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            ' ' if !in_tag => {
                spaces += 1;
                if after.contains(&spaces) {
                    out.push('\n');
                    continue;
                }
            }
            _ => {}
        }
        out.push(c);
    }
    out
}

/// One notification row after stacking: consecutive entries with the same
/// app and message collapse into one row with a count and the newest time,
/// the mockup's `top.n++;top.t=now()` (`×3`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteRow<T: Clone> {
    pub app: String,
    /// The notification's title (its `summary`).
    pub msg: String,
    /// Its body as one plain line; empty when there is none.
    pub body: String,
    /// The newest entry's timestamp (whatever the caller uses).
    pub time: T,
    pub count: usize,
    /// Every stacked entry's id, so dismissing the row removes them all.
    pub ids: Vec<u32>,
}

/// Stacks `entries` (newest first: `(id, app, title, body, time)`) into rows.
/// Only entries whose app, title AND body all match stack, so two different
/// messages that share a title stay separate.
pub fn stack_notes<T: Clone>(entries: &[(u32, String, String, String, T)]) -> Vec<NoteRow<T>> {
    let mut rows: Vec<NoteRow<T>> = Vec::new();
    for (id, app, msg, body, time) in entries {
        match rows.last_mut() {
            Some(last) if &last.app == app && &last.msg == msg && &last.body == body => {
                last.count += 1;
                last.ids.push(*id);
            }
            _ => rows.push(NoteRow {
                app: app.clone(),
                msg: msg.clone(),
                body: body.clone(),
                time: time.clone(),
                count: 1,
                ids: vec![*id],
            }),
        }
    }
    rows
}

/// A short, lower-case kind for an app row's sub label (the mockup's
/// "browser", "terminal", "file manager"...), from the desktop entry's
/// freedesktop categories; the first recognised category wins.
pub fn app_kind(categories: &[String]) -> String {
    const KINDS: &[(&str, &str)] = &[
        ("WebBrowser", "browser"),
        ("TerminalEmulator", "terminal"),
        ("FileManager", "file manager"),
        ("TextEditor", "editor"),
        ("IDE", "editor"),
        ("Email", "mail"),
        ("InstantMessaging", "chat"),
        ("Chat", "chat"),
        ("Music", "music"),
        ("Player", "media"),
        ("Video", "video"),
        ("3DGraphics", "3d"),
        ("RasterGraphics", "images"),
        ("Photography", "images"),
        ("Graphics", "graphics"),
        ("Game", "games"),
        ("Settings", "settings"),
        ("Office", "office"),
        ("Development", "development"),
        ("Network", "network"),
        ("Utility", "utility"),
        ("System", "system"),
    ];
    for (cat, kind) in KINDS {
        if categories.iter().any(|c| c == cat) {
            return (*kind).to_string();
        }
    }
    categories
        .first()
        .map(|c| c.to_lowercase())
        .unwrap_or_default()
}

/// What a spotlight command does when run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAction {
    /// A shell command line.
    Shell(&'static str),
    /// Flip do-not-disturb (handled in-process).
    ToggleDnd,
}

/// One entry in the stage's `>` command list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StageCommand {
    pub id: &'static str,
    pub name: &'static str,
    /// The owning tool, shown as the row's sub label.
    pub sub: &'static str,
    pub action: CommandAction,
}

/// The mockup's `CMDS`, in order, each wired to the real tool it names.
/// Kept here rather than in `bread_launcher::builtin_commands` so breadbox's
/// own command palette is unaffected.
pub const STAGE_COMMANDS: &[StageCommand] = &[
    StageCommand {
        id: "lock",
        name: "Lock screen",
        sub: "breadlock",
        action: CommandAction::Shell("breadlock"),
    },
    StageCommand {
        id: "reload-bar",
        name: "Reload bar",
        sub: "breadbar",
        action: CommandAction::Shell("bread-theme reload"),
    },
    StageCommand {
        id: "switch-theme",
        name: "Switch theme",
        sub: "bread-theme",
        action: CommandAction::Shell("bos-settings --page appearance"),
    },
    StageCommand {
        id: "screenshot",
        name: "Take screenshot",
        sub: "breadshot",
        action: CommandAction::Shell("breadshot region"),
    },
    StageCommand {
        id: "dnd",
        name: "Toggle do not disturb",
        sub: "notifications",
        action: CommandAction::ToggleDnd,
    },
];

/// The mockup's command filter: a case-insensitive substring of
/// "`name` `sub`" (`hit(n+" "+s,t)`); an empty query lists everything.
pub fn filter_stage_commands(query: &str) -> Vec<StageCommand> {
    let q = query.trim().to_lowercase();
    STAGE_COMMANDS
        .iter()
        .filter(|c| format!("{} {}", c.name, c.sub).to_lowercase().contains(&q))
        .copied()
        .collect()
}

/// The display name for a window's app: the matching desktop entry's name
/// (by `StartupWMClass`, then by id or name), else the class itself, so a
/// window row reads "Firefox · ws 1" like the mockup rather than the raw
/// lower-case class.
pub fn window_app_name(class: &str, apps: &[bread_launcher::DesktopEntry]) -> String {
    let lc = class.to_lowercase();
    apps.iter()
        .find(|e| {
            e.wm_class
                .as_deref()
                .is_some_and(|w| w.eq_ignore_ascii_case(class))
        })
        .or_else(|| {
            apps.iter().find(|e| {
                e.id.trim_end_matches(".desktop").to_lowercase() == lc
                    || e.id
                        .trim_end_matches(".desktop")
                        .to_lowercase()
                        .ends_with(&format!(".{lc}"))
                    || e.name.to_lowercase() == lc
            })
        })
        .map(|e| e.name.clone())
        .unwrap_or_else(|| class.to_string())
}

/// A centre-zone widget card (`.card`) derived from one pill widget: the
/// widget API has a single node tree, so the card reuses it. The spec's
/// tooltip supplies the text: its first line is the card title, any further
/// lines the mono sub line. Without a tooltip the title is the module name.
/// The first progress node becomes the card's brass rail; the value row
/// shows the rest of the tree.
#[derive(Debug, Clone)]
pub struct CardParts {
    pub title: String,
    pub tag: String,
    pub sub: Option<String>,
    pub rail: Option<f64>,
    pub value: Option<bread_shared::widget::WidgetNode>,
}

fn first_progress(node: &bread_shared::widget::WidgetNode) -> Option<f64> {
    use bread_shared::widget::WidgetNode;
    match node {
        WidgetNode::Progress { value, .. } => Some(value.clamp(0.0, 1.0)),
        WidgetNode::Box { children, .. } => children.iter().find_map(first_progress),
        _ => None,
    }
}

fn without_progress(
    node: &bread_shared::widget::WidgetNode,
) -> Option<bread_shared::widget::WidgetNode> {
    use bread_shared::widget::WidgetNode;
    match node {
        WidgetNode::Progress { .. } => None,
        WidgetNode::Box {
            orientation,
            spacing,
            class,
            style,
            on_click,
            children,
        } => Some(WidgetNode::Box {
            orientation: *orientation,
            spacing: *spacing,
            class: class.clone(),
            style: *style,
            on_click: on_click.clone(),
            children: children.iter().filter_map(without_progress).collect(),
        }),
        other => Some(other.clone()),
    }
}

pub fn card_parts(spec: &bread_shared::widget::WidgetSpec) -> CardParts {
    let mut lines = spec
        .tooltip
        .as_deref()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty());
    let title = lines
        .next()
        .map(str::to_string)
        .unwrap_or_else(|| spec.module.clone());
    let rest: Vec<&str> = lines.collect();
    CardParts {
        title,
        tag: spec.module.clone(),
        sub: (!rest.is_empty()).then(|| rest.join(" ")),
        rail: first_progress(&spec.root),
        value: without_progress(&spec.root),
    }
}

/// The key-hint footer (`.foot`): each hint's key and its description.
pub const FOOTER_HINTS: &[(&str, &str)] = &[
    ("\u{2190}\u{2192}\u{2191}\u{2193}", "move"),
    ("enter", "open"),
    ("=", "calculate"),
    (">", "command"),
    ("esc", "close"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> bread_launcher::DesktopEntry {
        bread_launcher::DesktopEntry {
            id: format!("{name}.desktop"),
            name: name.to_string(),
            exec: "true".to_string(),
            icon_name: String::new(),
            icon_path: None,
            categories: Vec::new(),
            wm_class: None,
            terminal: false,
        }
    }

    fn window(app: &str, title: &str, ws: i32) -> OpenWindow {
        OpenWindow {
            address: "0x1".into(),
            title: title.to_string(),
            app: app.to_string(),
            workspace: ws,
        }
    }

    // ---- esc_action --------------------------------------------------

    #[test]
    fn esc_clears_before_closing() {
        assert_eq!(esc_action(false), EscAction::ClearQuery);
        assert_eq!(esc_action(true), EscAction::Close);
    }

    // ---- PowerConfirm --------------------------------------------------

    #[test]
    fn no_confirm_action_always_runs() {
        let mut pc = PowerConfirm::new();
        let now = Instant::now();
        assert_eq!(
            pc.press("lock", false, now, Duration::from_secs(3)),
            PowerPress::Run
        );
        assert!(pc.armed_id().is_none());
    }

    #[test]
    fn confirm_action_arms_on_first_press() {
        let mut pc = PowerConfirm::new();
        let now = Instant::now();
        assert_eq!(
            pc.press("restart", true, now, Duration::from_secs(3)),
            PowerPress::Arm
        );
        assert_eq!(pc.armed_id(), Some("restart"));
    }

    #[test]
    fn second_press_within_window_runs() {
        let mut pc = PowerConfirm::new();
        let t0 = Instant::now();
        assert_eq!(
            pc.press("restart", true, t0, Duration::from_secs(3)),
            PowerPress::Arm
        );
        let t1 = t0 + Duration::from_millis(500);
        assert_eq!(
            pc.press("restart", true, t1, Duration::from_secs(3)),
            PowerPress::Run
        );
        assert!(pc.armed_id().is_none());
    }

    #[test]
    fn second_press_after_window_rearms_instead_of_running() {
        let mut pc = PowerConfirm::new();
        let t0 = Instant::now();
        pc.press("restart", true, t0, Duration::from_secs(3));
        let t1 = t0 + Duration::from_secs(4);
        assert_eq!(
            pc.press("restart", true, t1, Duration::from_secs(3)),
            PowerPress::Arm
        );
        assert_eq!(pc.armed_id(), Some("restart"));
    }

    #[test]
    fn pressing_a_different_confirm_action_disarms_the_first() {
        let mut pc = PowerConfirm::new();
        let t0 = Instant::now();
        pc.press("restart", true, t0, Duration::from_secs(3));
        assert_eq!(
            pc.press("off", true, t0, Duration::from_secs(3)),
            PowerPress::Arm
        );
        assert_eq!(pc.armed_id(), Some("off"));
    }

    #[test]
    fn double_click_does_not_confirm() {
        let mut pc = PowerConfirm::new();
        let t0 = Instant::now();
        pc.press("restart", true, t0, Duration::from_secs(3));
        assert_eq!(
            pc.press(
                "restart",
                true,
                t0 + Duration::from_millis(150),
                Duration::from_secs(3)
            ),
            PowerPress::Ignore
        );
        assert_eq!(pc.armed_id(), Some("restart"));
        // A deliberate press after a real pause still confirms.
        assert_eq!(
            pc.press(
                "restart",
                true,
                t0 + Duration::from_millis(800),
                Duration::from_secs(3)
            ),
            PowerPress::Run
        );
    }

    #[test]
    fn held_key_autorepeat_never_confirms() {
        let mut pc = PowerConfirm::new();
        let t0 = Instant::now();
        let window = Duration::from_secs(3);
        assert_eq!(pc.press("off", true, t0, window), PowerPress::Arm);
        // 30 Hz repeats for 6 s: past the confirm window and beyond.
        for i in 1..=180u64 {
            let r = pc.press("off", true, t0 + Duration::from_millis(33 * i), window);
            assert_ne!(r, PowerPress::Run, "repeat {i} ran the action");
        }
    }

    #[test]
    fn rearming_bumps_generation() {
        let mut pc = PowerConfirm::new();
        let t0 = Instant::now();
        pc.press("restart", true, t0, Duration::from_secs(3));
        let g1 = pc.generation();
        pc.press("off", true, t0, Duration::from_secs(3));
        assert_ne!(pc.generation(), g1);
    }

    #[test]
    fn disarm_clears_pending_arm() {
        let mut pc = PowerConfirm::new();
        pc.press("restart", true, Instant::now(), Duration::from_secs(3));
        pc.disarm();
        assert!(pc.armed_id().is_none());
    }

    // ---- filter_windows --------------------------------------------------

    #[test]
    fn filter_windows_matches_app_or_title() {
        let windows = vec![
            window("firefox", "breadboard: ECO board", 1),
            window("kitty", "~/Projects/breadbar", 1),
        ];
        let hits = filter_windows("bread", &windows, &[]);
        assert_eq!(hits.len(), 2);
        let hits = filter_windows("firefox", &windows, &[]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].app, "firefox");
    }

    #[test]
    fn filter_windows_empty_query_matches_all() {
        let windows = vec![window("firefox", "a", 1), window("kitty", "b", 2)];
        assert_eq!(filter_windows("", &windows, &[]).len(), 2);
    }

    // ---- build_results --------------------------------------------------

    fn parsed(raw: &str) -> bread_launcher::ParsedQuery {
        bread_launcher::parse_query(raw)
    }

    #[test]
    fn calc_mode_returns_answer() {
        let r = build_results(&parsed("=2+2"), &[], &[]);
        assert_eq!(
            r,
            StageResults::Calc {
                expr: "2+2".into(),
                answer: Some("4".into()),
            }
        );
    }

    #[test]
    fn command_mode_groups_matches() {
        let r = build_results(&parsed(">lock"), &[], &[]);
        match r {
            StageResults::Groups(groups) => {
                assert_eq!(groups.len(), 1);
                assert_eq!(groups[0].title, "Commands");
                assert!(groups[0].rows.iter().any(|row| row.name == "Lock screen"));
            }
            other => panic!("expected Groups, got {other:?}"),
        }
    }

    #[test]
    fn command_mode_no_match_is_empty() {
        let r = build_results(&parsed(">zzzznope"), &[], &[]);
        assert_eq!(
            r,
            StageResults::Empty {
                query: "zzzznope".into()
            }
        );
    }

    #[test]
    fn apps_mode_groups_windows_then_apps() {
        let windows = vec![window("firefox", "breadboard", 1)];
        let apps = vec![entry("Firefox"), entry("Files")];
        let r = build_results(&parsed("fire"), &windows, &apps);
        match r {
            StageResults::Groups(groups) => {
                assert_eq!(groups.len(), 2);
                assert_eq!(groups[0].title, "Open windows");
                assert_eq!(groups[1].title, "Apps");
                assert_eq!(groups[1].rows.len(), 1);
                assert_eq!(groups[1].rows[0].name, "Firefox");
            }
            other => panic!("expected Groups, got {other:?}"),
        }
    }

    #[test]
    fn every_group_is_capped() {
        let windows: Vec<OpenWindow> = (0..20)
            .map(|i| window("kitty", &format!("shell {i}"), 1))
            .collect();
        let apps: Vec<bread_launcher::DesktopEntry> =
            (0..40).map(|i| entry(&format!("Shell tool {i}"))).collect();
        match build_results(&parsed("shell"), &windows, &apps) {
            StageResults::Groups(groups) => {
                assert!(groups.len() <= MAX_GROUPS);
                assert!(groups.iter().all(|g| g.rows.len() == MAX_GROUP_ROWS));
            }
            other => panic!("expected Groups, got {other:?}"),
        }
    }

    #[test]
    fn apps_mode_no_match_anywhere_is_empty() {
        let r = build_results(&parsed("zzzznope"), &[], &[entry("Firefox")]);
        assert_eq!(
            r,
            StageResults::Empty {
                query: "zzzznope".into()
            }
        );
    }

    #[test]
    fn apps_mode_drops_empty_groups_but_keeps_populated_ones() {
        let apps = vec![entry("Firefox")];
        let r = build_results(&parsed("fire"), &[], &apps);
        match r {
            StageResults::Groups(groups) => {
                assert_eq!(groups.len(), 1);
                assert_eq!(groups[0].title, "Apps");
            }
            other => panic!("expected Groups, got {other:?}"),
        }
    }

    #[test]
    fn rows_flattens_groups_in_order() {
        let r = StageResults::Groups(vec![
            ResultGroup {
                title: "Open windows",
                rows: vec![window_row(&window("firefox", "a", 1), &[])],
            },
            ResultGroup {
                title: "Apps",
                rows: vec![app_row(&entry("Firefox"))],
            },
        ]);
        assert_eq!(r.rows().len(), 2);
        assert_eq!(r.rows()[0].kind, ResultKind::Window);
        assert_eq!(r.rows()[1].kind, ResultKind::App);
    }

    #[test]
    fn action_labels_match_the_mockup() {
        assert_eq!(ResultKind::Window.action_label(), "switch");
        assert_eq!(ResultKind::App.action_label(), "open");
        assert_eq!(ResultKind::Command.action_label(), "run");
    }

    // ---- clamp_selection --------------------------------------------------

    #[test]
    fn clamp_selection_empty_list_is_none() {
        assert_eq!(clamp_selection(0, 0), None);
    }

    #[test]
    fn clamp_selection_clamps_both_ends() {
        assert_eq!(clamp_selection(3, -1), Some(0));
        assert_eq!(clamp_selection(3, 5), Some(2));
        assert_eq!(clamp_selection(3, 1), Some(1));
    }

    // ---- BAR-31 ------------------------------------------------------------

    #[test]
    fn column_and_zones_match_the_mockup_grid() {
        assert_eq!(column_inner_width(1920), 968);
        assert_eq!(column_inner_width(800), 768);
        let z = zone_widths(968);
        assert_eq!(z.iter().sum::<i32>(), 968 - 36);
        // 1fr : 1.15fr : 1fr
        assert_eq!(z[0], z[2]);
        assert!(
            (f64::from(z[1]) / f64::from(z[0]) - 1.15).abs() < 0.01,
            "{z:?}"
        );
        assert_eq!(zone_widths(0), [0, 0, 0]);
    }

    #[test]
    fn time_left_reads_like_the_mockup() {
        assert_eq!(format_time_left(300), "5h left");
        assert_eq!(format_time_left(160), "2h 40m left");
        assert_eq!(format_time_left(40), "40m left");
    }

    #[test]
    fn status_sentence_matches_the_mockup_copy() {
        let s = status_sentence(
            "Sunday, September 27",
            Some(83),
            false,
            Some(300),
            "Totally NCC",
        );
        let em = |t: &str| format!("<span weight=\"500\" fgalpha=\"100%\">{t}</span>");
        assert_eq!(
            s,
            format!(
                "<span fgalpha=\"52%\">Sunday, September 27. {} battery, 5h left. On {}.</span>",
                em("83%"),
                em("Totally NCC")
            )
        );
        let low = status_sentence("D", Some(12), true, Some(20), "");
        assert!(low.ends_with("battery, plug in soon.</span>"), "{low}");
        let ac = status_sentence("D", None, false, None, "Net & Co");
        assert!(
            ac.contains("On AC power.") && ac.contains("Net &amp; Co"),
            "{ac}"
        );
    }

    #[test]
    fn identical_consecutive_notes_stack() {
        let e = |id: u32, app: &str, msg: &str, t: u32| {
            (id, app.to_string(), msg.to_string(), String::new(), t)
        };
        let rows = stack_notes(&[
            e(5, "kitty", "waiting", 50),
            e(4, "kitty", "waiting", 40),
            e(3, "kitty", "waiting", 30),
            e(2, "breadshot", "saved", 20),
            e(1, "kitty", "waiting", 10),
        ]);
        assert_eq!(rows.len(), 3);
        assert_eq!((rows[0].count, rows[0].time), (3, 50));
        assert_eq!(rows[0].ids, vec![5, 4, 3]);
        assert_eq!(rows[1].count, 1);
        assert_eq!(rows[2].ids, vec![1]);
    }

    #[test]
    fn same_title_with_a_different_body_does_not_stack() {
        let e = |id: u32, body: &str| {
            (
                id,
                "chat".to_string(),
                "bread".to_string(),
                body.to_string(),
                id,
            )
        };
        let rows = stack_notes(&[e(3, "hello"), e(2, "hello"), e(1, "different")]);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].count, rows[0].body.as_str()), (2, "hello"));
        assert_eq!(rows[1].body, "different");
    }

    #[test]
    fn app_kind_prefers_specific_categories() {
        let c = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(app_kind(&c(&["Network", "WebBrowser"])), "browser");
        assert_eq!(app_kind(&c(&["System", "TerminalEmulator"])), "terminal");
        assert_eq!(app_kind(&c(&["Weird"])), "weird");
        assert_eq!(app_kind(&[]), "");
    }

    #[test]
    fn stage_commands_are_the_mockups_and_filter_by_name_or_tool() {
        let names: Vec<_> = STAGE_COMMANDS.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            [
                "Lock screen",
                "Reload bar",
                "Switch theme",
                "Take screenshot",
                "Toggle do not disturb"
            ]
        );
        assert_eq!(filter_stage_commands("").len(), 5);
        assert_eq!(filter_stage_commands("breadshot")[0].id, "screenshot");
        assert_eq!(filter_stage_commands("THEME")[0].id, "switch-theme");
        assert!(filter_stage_commands("zzz").is_empty());
        assert_eq!(
            filter_stage_commands("disturb")[0].action,
            CommandAction::ToggleDnd
        );
    }

    #[test]
    fn window_rows_use_the_apps_display_name() {
        let mut ff = entry("Firefox");
        ff.id = "firefox.desktop".into();
        let mut kitty = entry("Kitty");
        kitty.wm_class = Some("kitty".into());
        let apps = vec![ff, kitty];
        assert_eq!(window_app_name("firefox", &apps), "Firefox");
        assert_eq!(window_app_name("kitty", &apps), "Kitty");
        assert_eq!(window_app_name("mystery", &apps), "mystery");
        let row = window_row(&window("firefox", "Arch Wiki", 1), &apps);
        assert_eq!(row.sub, "Firefox · ws 1");
        assert_eq!(row.glyph, "F");
    }

    #[test]
    fn apps_match_by_substring_of_name_and_kind() {
        let mut ff = entry("Firefox");
        ff.categories = vec!["WebBrowser".into()];
        let r = build_results(&parsed("browser"), &[], &[ff]);
        assert_eq!(r.rows().len(), 1);
        // Subsequence-only matches are not the mockup's behaviour.
        let r = build_results(&parsed("ffx"), &[], &[entry("Firefox")]);
        assert!(matches!(r, StageResults::Empty { .. }));
    }

    #[test]
    fn card_parts_split_tooltip_and_progress() {
        use bread_shared::widget::{Orientation, WidgetNode, WidgetPlacement, WidgetSpec};
        let label = |t: &str| WidgetNode::Label {
            text: t.into(),
            class: None,
            style: None,
            on_click: None,
        };
        let spec = WidgetSpec {
            id: "claude.usage".into(),
            module: "claude".into(),
            placement: WidgetPlacement::RightOfClock,
            order: 0,
            visible: true,
            tooltip: Some("Claude usage\nResets 12:20".into()),
            root: WidgetNode::Box {
                orientation: Orientation::Horizontal,
                spacing: None,
                class: None,
                style: None,
                on_click: None,
                children: vec![
                    label("81%"),
                    WidgetNode::Progress {
                        value: 0.81,
                        class: None,
                        style: None,
                        on_click: None,
                    },
                ],
            },
            updated_at: 0,
        };
        let c = card_parts(&spec);
        assert_eq!(c.title, "Claude usage");
        assert_eq!(c.tag, "claude");
        assert_eq!(c.sub.as_deref(), Some("Resets 12:20"));
        assert_eq!(c.rail, Some(0.81));
        match c.value {
            Some(WidgetNode::Box { children, .. }) => assert_eq!(children.len(), 1),
            other => panic!("{other:?}"),
        }
        let bare = WidgetSpec {
            tooltip: None,
            root: label("x"),
            ..spec
        };
        let c = card_parts(&bare);
        assert_eq!((c.title.as_str(), c.sub, c.rail), ("claude", None, None));
    }

    #[test]
    fn breaks_land_on_text_spaces_only() {
        let m = "<span a=\"1\">one two</span> three four";
        assert_eq!(
            break_markup_at_spaces(m, &[2]),
            "<span a=\"1\">one two</span>\nthree four"
        );
        assert_eq!(break_markup_at_spaces(m, &[]), m);
    }
}
