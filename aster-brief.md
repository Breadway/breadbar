# aster — Hyprland Bar & Notification Daemon

Minimal Rust status bar and notification daemon for Hyprland. Replaces colorshell with a lower system footprint. No system tray, no launcher, no wallpaper logic.

---

## Stack

| Crate | Purpose |
|---|---|
| `gtk4` | UI toolkit |
| `gtk4-layer-shell` | Wayland layer surface anchoring |
| `relm4` | Component architecture for GTK4 |
| `hyprland` | Hyprland IPC (workspace events) |
| `zbus` | D-Bus (notifications daemon) |
| `serde` + `serde_json` | Parse pywal colors.json |
| `tokio` | Async polling runtime |

---

## Component 1: Top Bar

Full-width bar anchored to the top of the screen via `gtk4-layer-shell`. Single horizontal row, three sections.

### Left — Workspaces

- Numbered workspace buttons sourced from Hyprland IPC
- Subscribe to live workspace change events via `/tmp/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock`
- Active workspace highlighted with accent colour

### Centre — Clock

- Format: `HH:MM`
- Updated every second

### Right — System Stats

Displayed left to right: CPU% → RAM% → Power draw (W) → WiFi SSID

| Stat | Source |
|---|---|
| CPU usage | `/proc/stat` delta between polls |
| RAM usage | `/proc/meminfo` |
| Power draw | `/sys/class/power_supply/*/power_now` or `current_now * voltage_now` |
| WiFi SSID | Parse `iw dev` output — no NetworkManager dependency, no system tray |

Polled every 2 seconds.

---

## Component 2: Notification Daemon

Implements the `org.freedesktop.Notifications` D-Bus interface via `zbus`.

- On `Notify` call: spawn a GTK4 layer-shell popup anchored **top-right**, 20px from edge
- Shows: app name, summary, body
- Auto-dismisses after the hint timeout (default 5s if unset by sender)
- Multiple notifications stack vertically downward
- Supports `CloseNotification`

---

## Theming

Read `~/.cache/wal/colors.json` on startup. Reload on `SIGHUP` for live wallpaper-change integration.

| pywal key | Role |
|---|---|
| `special.background` | Bar background (with slight CSS transparency) |
| `colors.color0` | Widget background / secondary surfaces |
| `colors.color15` | Foreground text |
| `colors.color1` | Accent — active workspace, highlights |

Apply as a GTK CSS provider at the `Display` level so the bar and all notification popups share the same palette automatically.

---

## Explicit Non-Goals

- No system tray (`StatusNotifierItem` / `XEmbed`)
- No launcher or search (external tool handles this)
- No wallpaper engine (external tool handles this)
- No Bluetooth, volume, or media controls

---

## Suggested File Structure

```
aster/
├── Cargo.toml
└── src/
    ├── main.rs                # Init GTK app, spawn bar + notification daemon
    ├── bar/
    │   ├── mod.rs
    │   ├── workspaces.rs      # Hyprland IPC workspace subscription
    │   ├── clock.rs           # Second-tick clock widget
    │   └── stats.rs           # CPU, RAM, power draw, WiFi SSID
    ├── notifications/
    │   ├── mod.rs             # zbus D-Bus service
    │   └── popup.rs           # GTK4 layer-shell popup window + stack
    └── theme.rs               # pywal reader + GTK CSS provider injection
```

---

## Build Order for Claude Code

Follow this sequence so each step is independently testable:

1. **Scaffold** — `Cargo.toml` with all dependencies, module stubs, empty `main.rs`
2. **Layer shell anchor** — blank GTK4 window pinned to top of screen; confirm it appears correctly on your setup before adding any widgets
3. **Theme loader** — read `colors.json`, build CSS string, inject as `CssProvider` at display level
4. **Bar layout** — three-section `Box`, no data yet, just structure with placeholder labels
5. **Workspaces** — Hyprland IPC socket reader, live workspace buttons
6. **Stats poller** — tokio task polling CPU/RAM/power/WiFi, sending results to bar via channels
7. **Clock** — second-tick timer widget
8. **Notification daemon** — zbus service on session bus claiming `org.freedesktop.Notifications`
9. **Notification popup** — layer-shell window anchored top-right, vertical stack, auto-dismiss timer
10. **Wire SIGHUP** — reload `colors.json` and re-inject CSS on signal

---

## First Prompt for Claude Code

> Create a new Rust project called `aster`. It is a minimal Hyprland status bar and notification daemon. Start with `Cargo.toml` — I need gtk4, relm4, gtk4-layer-shell, hyprland, zbus, tokio, serde, and serde_json as dependencies. Then scaffold the module structure from the file tree in the brief and get a blank GTK4 window anchored full-width to the top of the screen with gtk4-layer-shell before adding any widgets. The layer-shell anchor is the most environment-sensitive step so I want to confirm it works on my setup first.
