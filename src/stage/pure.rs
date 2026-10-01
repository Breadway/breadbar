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

// ── Matching ────────────────────────────────────────────────────────────────

/// How well a query matched one piece of text: lower `rank` sorts first
/// (0 a prefix, 1 a substring, 2 a scattered subsequence), `spread` breaks
/// ties between subsequences (the tighter the better), and `positions` are
/// the matched characters' indices in the text, for highlighting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub rank: u32,
    pub spread: usize,
    pub positions: Vec<usize>,
}

impl Match {
    fn key(&self) -> (u32, usize) {
        (self.rank, self.spread)
    }
}

/// Case-insensitive match of `query` against `text`: a substring when there
/// is one (so "fire" still ranks Firefox exactly as before), otherwise the
/// query's characters in order anywhere in the text ("kis" finds kitty,
/// "btset" finds Bluetooth settings). Whitespace in the query is ignored for
/// the subsequence pass. `None` when the characters are not all there.
pub fn match_text(query: &str, text: &str) -> Option<Match> {
    let q: Vec<char> = query.trim().to_lowercase().chars().collect();
    if q.is_empty() {
        return Some(Match {
            rank: 0,
            spread: 0,
            positions: Vec::new(),
        });
    }
    let t: Vec<char> = text.to_lowercase().chars().collect();
    if let Some(at) = t.windows(q.len()).position(|w| w == q.as_slice()) {
        return Some(Match {
            rank: if at == 0 { 0 } else { 1 },
            spread: 0,
            positions: (at..at + q.len()).collect(),
        });
    }
    let wanted: Vec<char> = q.into_iter().filter(|c| !c.is_whitespace()).collect();
    if wanted.is_empty() {
        return None;
    }
    // A scattered match must start at a word start and stay compact, or a
    // short query matches almost any long name letter by letter ("fire"
    // across "Wi-Fi & internet").
    let max_spread = 2 * wanted.len() + 2;
    let mut best: Option<Vec<usize>> = None;
    for start in 0..t.len() {
        let word_start = start == 0 || !t[start - 1].is_alphanumeric();
        if !word_start || t[start] != wanted[0] {
            continue;
        }
        let mut positions = vec![start];
        for (i, c) in t.iter().enumerate().skip(start + 1) {
            if positions.len() == wanted.len() {
                break;
            }
            if *c == wanted[positions.len()] {
                positions.push(i);
            }
        }
        if positions.len() < wanted.len() {
            continue;
        }
        let spread = positions[positions.len() - 1] - start;
        let better = best.as_ref().is_none_or(|b| spread < b[b.len() - 1] - b[0]);
        if spread <= max_spread && better {
            best = Some(positions);
        }
    }
    best.map(|positions| Match {
        rank: 2,
        spread: positions[positions.len() - 1] - positions[0],
        positions,
    })
}

/// Sorts `(match, item)` pairs best first, keeping the input order among
/// equal matches (the launcher's own ranking for apps).
fn best_first<T>(mut hits: Vec<(Match, T)>) -> Vec<(Match, T)> {
    hits.sort_by_key(|(m, _)| m.key());
    hits
}

/// Matches `windows` against `query`: on the window's title, else on its
/// app's display name (ranked after every title match), best first.
pub fn window_hits<'a>(
    query: &str,
    windows: &'a [OpenWindow],
    apps: &[bread_launcher::DesktopEntry],
) -> Vec<(Match, &'a OpenWindow)> {
    let hits = windows
        .iter()
        .filter_map(|w| {
            if let Some(m) = match_text(query, &w.title) {
                return Some((m, w));
            }
            match_text(query, &window_app_name(&w.app, apps)).map(|m| {
                (
                    Match {
                        rank: m.rank + 3,
                        spread: m.spread,
                        positions: Vec::new(),
                    },
                    w,
                )
            })
        })
        .collect();
    best_first(hits)
}

// ── Result rows/groups (phase 3) ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResultKind {
    Window,
    App,
    Command,
    /// A bos-settings page.
    Setting,
}

impl ResultKind {
    /// The mockup's own `.row .act` badge text ("open ↵" / "switch ↵" /
    /// "run ↵"; `mark()`'s CSS shows it only on the selected row).
    pub fn action_label(self) -> &'static str {
        match self {
            ResultKind::Window => "switch",
            ResultKind::App | ResultKind::Setting => "open",
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
    /// address, an app's desktop-file id, a command's id, a settings page
    /// id. Display names are not unique (two terminals can share a title),
    /// so activation never looks a row back up by `name`.
    pub target: String,
    /// Indices of the characters of `name` the query matched, highlighted
    /// in the row.
    pub marks: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultGroup {
    pub title: &'static str,
    pub rows: Vec<ResultRow>,
}

/// The stage's full results view for one query, mirroring the mockup's own
/// `render()`: calc mode shows one answer line, command mode shows one
/// group, plain-text mode shows "Open windows", "Apps" then "Settings"
/// (empty groups dropped), and no rows at all (in any mode) shows the
/// empty-state hint.
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

fn window_row(
    w: &OpenWindow,
    apps: &[bread_launcher::DesktopEntry],
    marks: Vec<usize>,
) -> ResultRow {
    let app = window_app_name(&w.app, apps);
    ResultRow {
        kind: ResultKind::Window,
        glyph: first_char_upper(&app),
        name: w.title.clone(),
        sub: format!("{app} · ws {}", w.workspace),
        target: w.address.clone(),
        marks,
    }
}

fn app_row(entry: &bread_launcher::DesktopEntry, marks: Vec<usize>) -> ResultRow {
    ResultRow {
        kind: ResultKind::App,
        glyph: first_char_upper(&entry.name),
        name: entry.name.clone(),
        sub: app_kind(&entry.categories),
        target: entry.id.clone(),
        marks,
    }
}

fn command_row(cmd: &StageCommand, marks: Vec<usize>) -> ResultRow {
    ResultRow {
        kind: ResultKind::Command,
        glyph: "\u{203a}".to_string(),
        name: cmd.name.to_string(),
        sub: cmd.sub.to_string(),
        target: cmd.id.to_string(),
        marks,
    }
}

fn setting_row(page: &SettingsPage, marks: Vec<usize>) -> ResultRow {
    ResultRow {
        kind: ResultKind::Setting,
        glyph: first_char_upper(page.name),
        name: page.name.to_string(),
        sub: "settings".to_string(),
        target: page.id.to_string(),
        marks,
    }
}

fn first_char_upper(s: &str) -> String {
    s.chars()
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().collect::<String>())
}

/// Most rows a result group shows. Every row is a real widget, and a single
/// letter matches most installed apps (99 rows for "o" on a 129-app machine),
/// which stalled each keystroke by up to half a second in layout. The stage
/// also keeps exactly this many pooled row widgets per group.
pub const MAX_GROUP_ROWS: usize = 8;

/// The Settings group is a supplement to apps and windows, so it stays short.
pub const MAX_SETTINGS_ROWS: usize = 4;

/// Most result groups at once (open windows + apps + settings); the pool's
/// size.
pub const MAX_GROUPS: usize = 3;

/// One bos-settings page the search can open (`bos-settings --page <id>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsPage {
    pub id: &'static str,
    pub name: &'static str,
    /// Other words people use for it ("wifi", "battery"), matched too.
    pub keywords: &'static str,
}

/// The pages people reach for from a search, named as bos-settings' own
/// sidebar names them.
pub const SETTINGS_PAGES: &[SettingsPage] = &[
    SettingsPage {
        id: "network",
        name: "Wi-Fi & internet",
        keywords: "wifi wireless network internet",
    },
    SettingsPage {
        id: "breadcrumbs",
        name: "Saved networks",
        keywords: "wifi profiles breadcrumbs",
    },
    SettingsPage {
        id: "bluetooth",
        name: "Bluetooth",
        keywords: "bluetooth devices headphones",
    },
    SettingsPage {
        id: "sound",
        name: "Sound",
        keywords: "audio volume speakers microphone",
    },
    SettingsPage {
        id: "displays",
        name: "Displays",
        keywords: "monitor screen resolution scale",
    },
    SettingsPage {
        id: "appearance",
        name: "Appearance",
        keywords: "theme colours wallpaper dark",
    },
    SettingsPage {
        id: "shelltheme",
        name: "Shell theme",
        keywords: "bar theme spotlight",
    },
    SettingsPage {
        id: "nightlight",
        name: "Night light",
        keywords: "night blue light warm",
    },
    SettingsPage {
        id: "power",
        name: "Power & battery",
        keywords: "battery power suspend",
    },
    SettingsPage {
        id: "input",
        name: "Keyboard & mouse",
        keywords: "keyboard mouse touchpad input",
    },
    SettingsPage {
        id: "keybinds",
        name: "Shortcuts",
        keywords: "keybinds hotkeys shortcuts",
    },
    SettingsPage {
        id: "breadbar",
        name: "Bar",
        keywords: "breadbar pill notifications",
    },
    SettingsPage {
        id: "datetime",
        name: "Date & time",
        keywords: "clock timezone date time",
    },
    SettingsPage {
        id: "apps",
        name: "Default apps",
        keywords: "default applications browser",
    },
    SettingsPage {
        id: "privacy",
        name: "Privacy & users",
        keywords: "privacy users accounts",
    },
    SettingsPage {
        id: "updates",
        name: "Updates",
        keywords: "updates upgrade packages",
    },
];

/// Matches the settings pages against `query`: on the page name, else on
/// its keywords (ranked after every name match, nothing highlighted).
pub fn filter_settings(query: &str) -> Vec<(Match, &'static SettingsPage)> {
    let hits = SETTINGS_PAGES
        .iter()
        .filter_map(|p| {
            if let Some(m) = match_text(query, p.name) {
                return Some((m, p));
            }
            // Keywords only by substring: a scattered subsequence across a
            // bag of words matches far too much.
            let q = query.trim().to_lowercase();
            (!q.is_empty() && p.keywords.contains(&q)).then(|| {
                (
                    Match {
                        rank: 3,
                        spread: 0,
                        positions: Vec::new(),
                    },
                    p,
                )
            })
        })
        .collect();
    best_first(hits)
}

/// Builds the grouped results for one query, given the current data sources.
/// `apps` should already be `bread_launcher::load_sorted_entries`'s output:
/// among equally good matches the launcher's ranking is kept.
pub fn build_results(
    parsed: &bread_launcher::ParsedQuery,
    windows: &[OpenWindow],
    apps: &[bread_launcher::DesktopEntry],
) -> StageResults {
    use bread_launcher::QueryKind;
    match parsed.kind {
        QueryKind::Calc => StageResults::Calc {
            expr: parsed.value.clone(),
            answer: bread_launcher::eval_calc(&parsed.value),
        },
        QueryKind::Cmd => {
            let rows: Vec<ResultRow> = filter_stage_commands(&parsed.value)
                .into_iter()
                .take(MAX_GROUP_ROWS)
                .map(|(m, c)| command_row(c, m.positions))
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
            let q = &parsed.value;
            let win_rows: Vec<ResultRow> = window_hits(q, windows, apps)
                .into_iter()
                .take(MAX_GROUP_ROWS)
                .map(|(m, w)| window_row(w, apps, m.positions))
                .collect();
            let app_hits = apps
                .iter()
                .filter_map(|e| {
                    if let Some(m) = match_text(q, &e.name) {
                        return Some((m, e));
                    }
                    // The app's kind ("browser") by substring, as before.
                    let ql = q.trim().to_lowercase();
                    app_kind(&e.categories).contains(&ql).then(|| {
                        (
                            Match {
                                rank: 3,
                                spread: 0,
                                positions: Vec::new(),
                            },
                            e,
                        )
                    })
                })
                .collect();
            let app_rows: Vec<ResultRow> = best_first(app_hits)
                .into_iter()
                .take(MAX_GROUP_ROWS)
                .map(|(m, e)| app_row(e, m.positions))
                .collect();
            let setting_rows: Vec<ResultRow> = filter_settings(q)
                .into_iter()
                .take(MAX_SETTINGS_ROWS)
                .map(|(m, p)| setting_row(p, m.positions))
                .collect();
            let groups: Vec<ResultGroup> = [
                ("Open windows", win_rows),
                ("Apps", app_rows),
                ("Settings", setting_rows),
            ]
            .into_iter()
            .filter(|(_, rows)| !rows.is_empty())
            .map(|(title, rows)| ResultGroup { title, rows })
            .collect();
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

/// What the calculator shows for `eval_calc`'s answer. `bread_launcher`
/// reports a half-typed sum ("=2*") as "err" and characters it cannot
/// evaluate at all ("=2^10") as "bad expr"; neither is a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalcView {
    /// Nothing to show yet: "…" and "keep typing".
    Pending,
    /// The expression uses something the calculator cannot do.
    Unsupported,
    Answer(String),
}

pub fn calc_view(answer: Option<&str>) -> CalcView {
    match answer {
        None | Some("err") => CalcView::Pending,
        Some("bad expr") => CalcView::Unsupported,
        Some(a) => CalcView::Answer(a.to_string()),
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

/// One history entry as the notifications zone needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteIn<T: Clone> {
    pub id: u32,
    pub app: String,
    pub title: String,
    pub body: String,
    pub time: T,
    pub critical: bool,
    /// `(key, label)` pairs the sender offered and can still receive.
    pub actions: Vec<(String, String)>,
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
    /// Any stacked entry was critical.
    pub critical: bool,
    /// The newest entry's actions, as buttons.
    pub actions: Vec<(String, String)>,
}

/// Stacks `entries` (newest first) into rows. Only entries whose app, title
/// AND body all match stack, so two different messages that share a title
/// stay separate. Critical rows are then pinned above the rest, each group
/// keeping its newest-first order.
pub fn stack_notes<T: Clone>(entries: &[NoteIn<T>]) -> Vec<NoteRow<T>> {
    let mut rows: Vec<NoteRow<T>> = Vec::new();
    for e in entries {
        match rows.last_mut() {
            Some(last) if last.app == e.app && last.msg == e.title && last.body == e.body => {
                last.count += 1;
                last.ids.push(e.id);
                last.critical |= e.critical;
            }
            _ => rows.push(NoteRow {
                app: e.app.clone(),
                msg: e.title.clone(),
                body: e.body.clone(),
                time: e.time.clone(),
                count: 1,
                ids: vec![e.id],
                critical: e.critical,
                actions: e.actions.clone(),
            }),
        }
    }
    let (mut pinned, rest): (Vec<_>, Vec<_>) = rows.into_iter().partition(|r| r.critical);
    pinned.extend(rest);
    pinned
}

/// How many notification rows show before "Show all".
pub const NOTES_COLLAPSED: usize = 5;

/// The buttons a notification row shows for `actions`: its own actions in
/// the sender's order, with the spec's `default` action named "Open". The
/// inline-reply action needs a text field the stage does not have, so it is
/// left to the sender's own window.
pub fn note_buttons(actions: &[(String, String)], inline_reply_key: &str) -> Vec<(String, String)> {
    actions
        .iter()
        .filter(|(key, _)| key != inline_reply_key)
        .map(|(key, label)| {
            let label = if key == "default" && (label.is_empty() || label == "default") {
                "Open".to_string()
            } else {
                label.clone()
            };
            (key.clone(), label)
        })
        .take(3)
        .collect()
}

/// A network rate for the System card: "84 KB/s", "1.2 MB/s".
pub fn format_rate(kbs: f32) -> String {
    if kbs >= 1000.0 {
        format!("{:.1} MB/s", kbs / 1024.0)
    } else {
        format!("{:.0} KB/s", kbs.max(0.0))
    }
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
    /// Flip the Wi-Fi radio.
    ToggleWifi,
    /// Flip the Bluetooth adapter's power.
    ToggleBluetooth,
    /// Empty the notification history.
    ClearNotifications,
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

/// The mockup's `CMDS`, in order, each wired to the real tool it names, then
/// BAR-38's radio and notification commands. Kept here rather than in
/// `bread_launcher::builtin_commands` so breadbox's own command palette is
/// unaffected.
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
    StageCommand {
        id: "wifi",
        name: "Toggle Wi-Fi",
        sub: "network",
        action: CommandAction::ToggleWifi,
    },
    StageCommand {
        id: "bluetooth",
        name: "Toggle Bluetooth",
        sub: "bluetooth",
        action: CommandAction::ToggleBluetooth,
    },
    StageCommand {
        id: "clear-notifications",
        name: "Clear notifications",
        sub: "notifications",
        action: CommandAction::ClearNotifications,
    },
];

/// Filters the command list: a match on the name, else a substring of the
/// owning tool, best first; an empty query lists everything in order.
pub fn filter_stage_commands(query: &str) -> Vec<(Match, &'static StageCommand)> {
    let q = query.trim().to_lowercase();
    let hits = STAGE_COMMANDS
        .iter()
        .filter_map(|c| {
            if let Some(m) = match_text(query, c.name) {
                return Some((m, c));
            }
            c.sub.contains(&q).then(|| {
                (
                    Match {
                        rank: 3,
                        spread: 0,
                        positions: Vec::new(),
                    },
                    c,
                )
            })
        })
        .collect();
    best_first(hits)
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

/// Which keys the footer describes: they depend on what Enter would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintMode {
    Idle,
    Search,
    Calc,
    Command,
}

impl HintMode {
    pub fn from_query(kind: bread_launcher::QueryKind, query_empty: bool) -> Self {
        use bread_launcher::QueryKind;
        match kind {
            _ if query_empty => HintMode::Idle,
            QueryKind::Calc => HintMode::Calc,
            QueryKind::Cmd => HintMode::Command,
            QueryKind::Apps | QueryKind::Url => HintMode::Search,
        }
    }
}

/// The key-hint footer (`.foot`): each hint's key and its description.
pub fn footer_hints(mode: HintMode) -> &'static [(&'static str, &'static str)] {
    match mode {
        HintMode::Idle => &[
            ("\u{2190}\u{2192}\u{2191}\u{2193}", "move"),
            ("enter", "open"),
            ("=", "calculate"),
            (">", "command"),
            ("esc", "close"),
        ],
        HintMode::Search => &[
            ("\u{2191}\u{2193}", "move"),
            ("enter", "open or switch"),
            ("esc", "clear"),
        ],
        HintMode::Calc => &[("enter", "copy answer"), ("esc", "clear")],
        HintMode::Command => &[
            ("\u{2191}\u{2193}", "move"),
            ("enter", "run"),
            ("esc", "clear"),
        ],
    }
}

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

    // ---- window_hits --------------------------------------------------

    #[test]
    fn filter_windows_matches_app_or_title() {
        let windows = vec![
            window("firefox", "breadboard: ECO board", 1),
            window("kitty", "~/Projects/breadbar", 1),
        ];
        let hits = window_hits("bread", &windows, &[]);
        assert_eq!(hits.len(), 2);
        let hits = window_hits("firefox", &windows, &[]);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1.app, "firefox");
    }

    #[test]
    fn filter_windows_empty_query_matches_all() {
        let windows = vec![window("firefox", "a", 1), window("kitty", "b", 2)];
        assert_eq!(window_hits("", &windows, &[]).len(), 2);
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
                for g in &groups {
                    let cap = if g.title == "Settings" {
                        MAX_SETTINGS_ROWS
                    } else {
                        MAX_GROUP_ROWS
                    };
                    assert!(g.rows.len() <= cap, "{}", g.title);
                }
                assert!(groups
                    .iter()
                    .filter(|g| g.title != "Settings")
                    .all(|g| g.rows.len() == MAX_GROUP_ROWS));
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
                rows: vec![window_row(&window("firefox", "a", 1), &[], vec![])],
            },
            ResultGroup {
                title: "Apps",
                rows: vec![app_row(&entry("Firefox"), vec![])],
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
        assert_eq!(ResultKind::Setting.action_label(), "open");
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
        let e = |id: u32, app: &str, msg: &str, t: u32| note(id, app, msg, "", t, false);
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
        let e = |id: u32, body: &str| note(id, "chat", "bread", body, id, false);
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
                "Toggle do not disturb",
                "Toggle Wi-Fi",
                "Toggle Bluetooth",
                "Clear notifications",
            ]
        );
        let first = |q: &str| filter_stage_commands(q)[0].1;
        assert_eq!(filter_stage_commands("").len(), STAGE_COMMANDS.len());
        assert_eq!(first("breadshot").id, "screenshot");
        assert_eq!(first("THEME").id, "switch-theme");
        assert!(filter_stage_commands("zzz").is_empty());
        assert_eq!(first("disturb").action, CommandAction::ToggleDnd);
        assert_eq!(first("wifi").action, CommandAction::ToggleWifi);
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
        let row = window_row(&window("firefox", "Arch Wiki", 1), &apps, vec![]);
        assert_eq!(row.sub, "Firefox · ws 1");
        assert_eq!(row.glyph, "F");
    }

    #[test]
    fn apps_match_by_substring_of_name_and_kind() {
        let mut ff = entry("Firefox");
        ff.categories = vec!["WebBrowser".into()];
        let r = build_results(&parsed("browser"), &[], &[ff]);
        let apps: Vec<_> = r
            .rows()
            .into_iter()
            .filter(|r| r.kind == ResultKind::App)
            .collect();
        assert_eq!(apps.len(), 1);
        // "browser" is also a keyword of the Default apps settings page.
        assert!(r.rows().iter().any(|r| r.target == "apps"));
    }

    // ---- BAR-38 --------------------------------------------------------------

    fn note(id: u32, app: &str, title: &str, body: &str, t: u32, critical: bool) -> NoteIn<u32> {
        NoteIn {
            id,
            app: app.into(),
            title: title.into(),
            body: body.into(),
            time: t,
            critical,
            actions: Vec::new(),
        }
    }

    #[test]
    fn match_prefers_prefix_then_substring_then_subsequence() {
        let prefix = match_text("fire", "Firefox").unwrap();
        assert_eq!(
            (prefix.rank, prefix.positions.clone()),
            (0, vec![0, 1, 2, 3])
        );
        let sub = match_text("fox", "Firefox").unwrap();
        assert_eq!((sub.rank, sub.positions.clone()), (1, vec![4, 5, 6]));
        let scattered = match_text("kty", "Kitty").unwrap();
        assert_eq!(
            (scattered.rank, scattered.positions.clone()),
            (2, vec![0, 2, 4])
        );
        let words = match_text("wsh", "web shell").unwrap();
        assert_eq!(words.positions, vec![0, 4, 5]);
        assert!(match_text("zq", "Firefox").is_none());
        // Too spread out, or not starting a word: no match.
        assert!(match_text("fire", "Wi-Fi & internet").is_none());
        assert_eq!(match_text("tty", "Kitty").map(|m| m.rank), Some(1));
        assert!(match_text("tty", "Kit ty").is_none());
        assert!(match_text("", "anything").is_some());
    }

    #[test]
    fn loose_matches_rank_after_exact_ones_and_carry_highlights() {
        let apps = vec![entry("Files"), entry("Kitty")];
        let r = build_results(&parsed("kty"), &[], &apps);
        let rows = r.rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Kitty");
        assert_eq!(rows[0].marks, vec![0, 2, 4]);
        // A substring still outranks a scattered match, whatever the order.
        let apps = vec![entry("Firefox"), entry("Shell for fox")];
        let r = build_results(&parsed("fox"), &[], &apps);
        assert_eq!(r.rows()[0].name, "Firefox");
    }

    #[test]
    fn settings_pages_match_by_name_or_keyword() {
        let r = build_results(&parsed("wifi"), &[], &[]);
        let StageResults::Groups(groups) = r else {
            panic!("expected groups");
        };
        assert_eq!(groups[0].title, "Settings");
        assert_eq!(groups[0].rows[0].target, "network");
        assert_eq!(groups[0].rows[0].kind, ResultKind::Setting);
        let r = build_results(&parsed("blue"), &[], &[]);
        assert_eq!(r.rows()[0].target, "bluetooth");
        assert_eq!(r.rows()[0].marks, vec![0, 1, 2, 3]);
    }

    #[test]
    fn calc_view_hides_errors_while_typing() {
        assert_eq!(calc_view(None), CalcView::Pending);
        assert_eq!(calc_view(Some("err")), CalcView::Pending);
        assert_eq!(calc_view(Some("bad expr")), CalcView::Unsupported);
        assert_eq!(calc_view(Some("87")), CalcView::Answer("87".into()));
        let half = build_results(&parsed("=2*"), &[], &[]);
        let StageResults::Calc { answer, .. } = half else {
            panic!("expected calc");
        };
        assert_eq!(calc_view(answer.as_deref()), CalcView::Pending);
    }

    #[test]
    fn footer_hints_follow_the_mode() {
        use bread_launcher::QueryKind;
        assert_eq!(HintMode::from_query(QueryKind::Calc, true), HintMode::Idle);
        assert_eq!(HintMode::from_query(QueryKind::Calc, false), HintMode::Calc);
        assert_eq!(
            HintMode::from_query(QueryKind::Cmd, false),
            HintMode::Command
        );
        let enter = |m| {
            footer_hints(m)
                .iter()
                .find(|(k, _)| *k == "enter")
                .map(|(_, w)| *w)
        };
        assert_eq!(enter(HintMode::Idle), Some("open"));
        assert_eq!(enter(HintMode::Calc), Some("copy answer"));
        assert_eq!(enter(HintMode::Command), Some("run"));
    }

    #[test]
    fn critical_notes_are_pinned_first() {
        let rows = stack_notes(&[
            note(3, "chat", "hi", "", 30, false),
            note(2, "power", "Battery low", "", 20, true),
            note(1, "mail", "new", "", 10, false),
        ]);
        let order: Vec<_> = rows.iter().map(|r| r.ids[0]).collect();
        assert_eq!(order, vec![2, 3, 1]);
        assert!(rows[0].critical && !rows[1].critical);
    }

    #[test]
    fn note_buttons_name_default_and_skip_inline_reply() {
        let a = |k: &str, l: &str| (k.to_string(), l.to_string());
        let buttons = note_buttons(
            &[
                a("default", ""),
                a("inline-reply", "Reply"),
                a("mark-read", "Mark read"),
            ],
            "inline-reply",
        );
        assert_eq!(
            buttons,
            vec![a("default", "Open"), a("mark-read", "Mark read")]
        );
    }

    #[test]
    fn rates_read_as_kb_then_mb() {
        assert_eq!(format_rate(84.2), "84 KB/s");
        assert_eq!(format_rate(1228.8), "1.2 MB/s");
        assert_eq!(format_rate(-3.0), "0 KB/s");
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
