# Notification preferences API

The primary notification server exposes `GetNotificationPreferences() -> string` and `SetNotificationPreferences(string) -> ()` on the existing session-bus interface `dev.breadway.Bar`, at `/dev/breadway/Bar`, destination `org.freedesktop.Notifications`.

The JSON response includes `api_version: 1`, `effective_dnd`, `sound_available`, and `known_apps` (apps seen in history, so Settings can offer them). Stored preferences are `dnd`, `schedule`, `start`, `end`, `lock_screen`, `sound` and `apps`. App policies are `all`, `banners`, `history` and `off`. Unknown keys are preserved. Preferences live in the XDG configuration directory at `breadbar/notifications.json`; writes retain a backup. A read never absorbs history apps into `apps`, so a later Set cannot persist policies the user never chose.

Manual DND and scheduled DND suppress ordinary banners only; critical alerts keep history plus banner even on a locked session, unless an app policy of `history` or `off` applies (per-app policy still wins over urgency). Quiet hours support overnight ranges; `start == end` means off. A stored schedule whose times no longer parse falls back to 22:00-07:00 with a log line instead of silently disabling the schedule. History-only notifications refresh visible history without creating a popup. Off disables new history entries as well as banners. Lock detection uses the breadlock process and logind's LockedHint. Secure lock surfaces still control whether a compositor can display notification windows. Notification sounds require canberra-gtk-play, and respect the suppress-sound hint.

Stage DND controls update the same persisted preferences. Turning DND off in the stage disables the schedule, providing an explicit override. The primary bar polls preferences every two seconds, repainting every live stage and updating all shared stage indicators together.

The bar also consumes BOS Settings' clock format from `breadbar/clock.json`, layered over the theme clock format (only the hour fields change, so the theme keeps its date half), and validated text/font preferences from `hypr/bos-settings-v2.json`. A later edit of `breadbar/style.css` restores the existing bar-specific text overrides.

## Verification

Clippy passes with warnings denied. The headless notification tests cover policies, sound eligibility and command construction, manual and scheduled DND (including the invalid-schedule fallback), critical notifications under DND and lock (predicate and live path pinned to agree), lock-screen eligibility, notification history, synchronous replacement, the provider Set/Get round trip with backup and unknown-key preservation, the stage DND round trip, and the clock-format layering. Local builds use the documented dev dependency overrides; the committed Cargo.lock retains tagged dependencies.

No app launch, desktop control or live configuration writes were used for verification. The BOS Settings frontend still needs a running bar built from this branch to exercise these controls.
