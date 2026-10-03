# Notification preferences API

The primary notification server exposes `GetNotificationPreferences() -> string` and `SetNotificationPreferences(string) -> ()` on the existing session-bus interface `dev.breadway.Bar`, at `/dev/breadway/Bar`, destination `org.freedesktop.Notifications`.

The JSON response includes `api_version: 1`, `effective_dnd`, and `sound_available`. Stored preferences are `dnd`, `schedule`, `start`, `end`, `lock_screen`, `sound` and `apps`. App policies are `all`, `banners`, `history` and `off`. Unknown keys are preserved. Preferences live in the XDG configuration directory at `breadbar/notifications.json`; writes retain a backup.

Manual DND and scheduled DND suppress ordinary banners; critical alerts still pass unless an app policy disables banners or the session is locked with lock-screen notifications disabled. Quiet hours support overnight ranges. History-only notifications refresh visible history without creating a popup. Off disables new history entries as well as banners. Lock detection uses the breadlock process and logind's LockedHint. Secure lock surfaces still control whether a compositor can display notification windows. Notification sounds require canberra-gtk-play, and respect the suppress-sound hint.

Stage DND controls update the same persisted preferences. Turning DND off in the stage disables the schedule, providing an explicit override. The primary bar polls preferences every two seconds, updating all shared stage indicators.

The bar also consumes BOS Settings' clock format from `breadbar/clock.json`, and validated text/font preferences from `hypr/bos-settings-v2.json`. A later edit of `breadbar/style.css` restores the existing bar-specific text overrides.

## Verification

Clippy passes with warnings denied. The headless notification tests cover policies, sound eligibility, manual and scheduled DND, critical notifications, lock-screen eligibility, notification history and synchronous replacement. Local builds use the documented dev dependency overrides; the committed Cargo.lock retains tagged dependencies.

No app launch, desktop control or live configuration writes were used for verification. The BOS Settings frontend still needs a running bar built from this branch to exercise these controls.
