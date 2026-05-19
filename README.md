# breadbar

Minimal status bar and notification daemon for [Hyprland](https://hyprland.org/) on Wayland.

A single Rust binary that provides a full-width top bar and a standards-compliant D-Bus notification daemon, with no system tray, no launcher, and no wallpaper logic.

## Features

**Status bar** (anchored to the top of every monitor via `gtk4-layer-shell`):

- Left: live workspace buttons sourced from Hyprland IPC, active workspace highlighted
- Centre: clock (`HH:MM`, updates at the top of each minute)
- Right: CPU%, RAM, power draw (W), battery level + AC indicator, Bluetooth state, WiFi SSID with signal strength

**Notification daemon**:

- Implements `org.freedesktop.Notifications` (D-Bus) — works with any standard notification sender (`notify-send`, etc.)
- Popups appear top-right, stack vertically, auto-dismiss after the sender-specified timeout (default 5 s)
- Supports `CloseNotification`

**Theming**:

- Reads `~/.cache/wal/colors.json` (pywal) on startup for a palette that matches your wallpaper
- Falls back to a Catppuccin Mocha palette if pywal is not present
- User CSS override: `~/.config/breadbar/style.css`
- Send `SIGHUP` to reload the theme at runtime (integrates with wallpaper-change hooks)

## Dependencies

Runtime:

- GTK4 (≥ 4.12)
- `gtk4-layer-shell`
- `iw` — for WiFi SSID/signal (`iw dev <iface> link`)
- A running Hyprland compositor
- D-Bus session bus

Bluetooth status is read from `/sys/class/rfkill` and BlueZ D-Bus; it degrades gracefully if unavailable.

## Building

```sh
cargo build --release
```

The binary is at `target/release/breadbar`.

Requirements: Rust 1.77+ (uses `LazyLock`), a GTK4 development environment (`libgtk-4-dev` / `gtk4` package).

On Arch Linux:

```sh
sudo pacman -S gtk4 gtk4-layer-shell iw
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

breadbar reads `~/.cache/wal/colors.json` automatically. To reload after a wallpaper change:

```sh
pkill -HUP breadbar
```

Or hook it into your wallpaper script:

```sh
wal -i /path/to/wallpaper.jpg
pkill -HUP breadbar
```

### Custom CSS

Drop a `~/.config/breadbar/style.css` file and send `SIGHUP` to reload. This CSS is applied at a higher priority than the pywal palette so you can override anything.

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
| `src/notifications/mod.rs` | `org.freedesktop.Notifications` zbus service |
| `src/notifications/popup.rs` | Layer-shell popup window and card stack |
| `src/theme.rs` | pywal reader, GTK CSS provider injection |

Stats are polled every 2 seconds. Bluetooth and WiFi are sampled every 16 seconds and cached in between to avoid hammering D-Bus and `iw`.

## License

MIT
