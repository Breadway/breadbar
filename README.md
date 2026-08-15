# breadbar

Minimal status bar and notification daemon for [Hyprland](https://hyprland.org/) on Wayland.

A single Rust binary that provides a full-width top bar, a D-Bus notification daemon, a volume/brightness OSD, and an SNI system tray housed in a control panel popover.

## Features

**Status bar** (anchored to the top of every monitor via `gtk4-layer-shell`):

- Left: live workspace buttons sourced from Hyprland IPC, active workspace highlighted
- Centre: media widget (track/artist from `playerctl`, click to open prev/play-pause/next controls; hidden when no player is active, lingers up to 30 minutes after the last pause) + clock (`HH:MM`, updates at the top of each minute)
- Right: CPU%, RAM, power draw (W), battery level + AC indicator, Bluetooth icon (click to open `blueman-manager`), WiFi SSID with signal-strength icon (click for details popover), hamburger control panel button

**WiFi popover** (click the WiFi area):

- Shows current SSID, IP address, and internet/Tailscale connectivity status via `breadcrumbs status`
- Lists saved `breadcrumbs` profiles for one-click switching
- Shows nearby SSIDs from `breadcrumbs scan-list` (saved networks are clickable to join)
- Degrades gracefully if `breadcrumbs` is not installed

**Control panel** (hamburger button on the right):

- Volume slider (reads/writes via `wpctl`, up to 150%)
- Brightness slider (reads/writes via `brightnessctl`)
- Live CPU%, GPU%, and network throughput (download/upload)
- Audio output selector (lists PulseAudio sinks via `pactl`, switching takes effect immediately)
- System tray (SNI): apps that register with `org.kde.StatusNotifierWatcher` appear as icon buttons
- Power buttons: lock (`breadlock`), suspend, reboot, poweroff

**Notification daemon**:

- Implements `org.freedesktop.Notifications` (D-Bus) — works with any standard sender (`notify-send`, etc.)
- Popups appear top-right, stack vertically, auto-dismiss after the sender-specified timeout (default 5 s)
- Supports `CloseNotification` and `replaces_id`
- In-memory history of the last 50 notifications (app, summary, truncated body, time). Toggle with `breadbar --history` (Hyprland: `bind = SUPER, N, exec, breadbar --history`) or D-Bus `dev.breadway.Bar.ToggleHistory` on `org.freedesktop.Notifications` at `/dev/breadway/Bar`. Not persisted.

**Volume/brightness OSD**:

- Overlay window at the bottom of the screen, auto-dismisses after 2 s
- Appears automatically on any `pactl` sink-change event or `sysfs` backlight change

**Theming**:

- Uses `bread-theme` for palette loading; reads `~/.cache/wal/colors.json` (pywal) if present, falls back to a Catppuccin Mocha palette
- User CSS override: `~/.config/breadbar/style.css`
- Send `SIGHUP` to reload the theme at runtime (integrates with wallpaper-change hooks)

## Dependencies

Runtime (required):

- GTK4 (≥ 4.12)
- `gtk4-layer-shell`
- `iw` — for WiFi SSID/signal (`iw dev <iface> link`)
- `wpctl` (WirePlumber) — volume read/write
- `pactl` (PipeWire-Pulse) — audio sink listing and OSD volume events
- `brightnessctl` — brightness read/write
- A running Hyprland compositor
- D-Bus session bus

Runtime (optional, degrade gracefully if absent):

- `playerctl` — media widget; hidden if no player is found
- `breadcrumbs` — WiFi popover enrichment (profiles, internet/Tailscale status); basic SSID/signal still shown without it
- `blueman-manager` — opened when the Bluetooth icon is clicked; Bluetooth state still shown without it

Bluetooth state is read from `/sys/class/rfkill` and BlueZ D-Bus and degrades gracefully if unavailable.

## Building

```sh
cargo build --release
```

The binary is at `target/release/breadbar`.

Requirements: Rust 1.77+ (uses `LazyLock`), a GTK4 development environment (`libgtk-4-dev` / `gtk4` package).

On Arch Linux:

```sh
sudo pacman -S gtk4 gtk4-layer-shell wireplumber pipewire-pulse brightnessctl iw
cargo build --release
```

## Running

```sh
./target/release/breadbar
```

Typically launched from your Hyprland config:

```
exec-once = /path/to/breadbar
```

breadbar claims `org.freedesktop.Notifications` on the session D-Bus on startup. If another notification daemon is already running, startup will fail — stop the other daemon first.

## Theming

### pywal integration

breadbar reads `~/.cache/wal/colors.json` automatically (via `bread-theme`). To reload after a wallpaper change:

```sh
pkill -HUP breadbar
```

Or hook it into your wallpaper script:

```sh
wal -i /path/to/wallpaper.jpg
pkill -HUP breadbar
```

### Custom CSS

Drop a `~/.config/breadbar/style.css` file and send `SIGHUP` to reload. This CSS is applied at a higher priority than the generated palette so you can override anything.

Example — change the font size:

```css
* {
    font-size: 13px;
}
```

## Architecture

| Module | Responsibility |
|---|---|
| `src/main.rs` | GTK4 app entry point, widget tree, `relm4` component |
| `src/bar/workspaces.rs` | Hyprland IPC event stream, workspace buttons |
| `src/bar/clock.rs` | Minute-tick clock |
| `src/bar/stats.rs` | Polling loop: CPU, RAM, power, battery, Bluetooth, WiFi |
| `src/bar/media.rs` | `playerctl` polling, media widget and controls popover |
| `src/bar/wifi.rs` | WiFi details popover, `breadcrumbs` profile/scan integration |
| `src/bar/control.rs` | Control panel data: volume (`wpctl`), brightness (`brightnessctl`), sinks (`pactl`) |
| `src/bar/tray.rs` | `org.kde.StatusNotifierWatcher` D-Bus service, SNI item rendering |
| `src/notifications/mod.rs` | `org.freedesktop.Notifications` zbus service + `dev.breadway.Bar` history IPC |
| `src/notifications/popup.rs` | Layer-shell popup window and card stack |
| `src/notifications/history.rs` | Bounded in-memory history and layer-shell history window |
| `src/osd.rs` | Volume/brightness on-screen display |
| `src/widgets/` | Live Lua widgets from breadd (`BreadClient` + `WidgetSpec`) |
| `src/theme.rs` | `bread-theme` palette loading, GTK CSS provider injection |

Stats are polled every 2 seconds. Bluetooth and WiFi are sampled every 16 seconds and cached in between to avoid hammering D-Bus and `iw`.

## License

MIT
