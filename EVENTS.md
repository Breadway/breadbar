# bar: bread event integration

breadbar is a standalone status bar: it works exactly the same with or
without `breadd` running. When breadd *is* present, breadbar publishes
widget clicks and successful launcher launches into the shared bread
automation fabric, and uses the daemon's runtime widget state to render its
bar. See the parent `bread` repo's `Documentation.md`, specifically its
"Namespaces" and "Integrating a bread\* app" sections, for the general
convention this follows.

App id: **`bar`**. Transport: `bread-utils`'s `bread_client` module
(feature `bread-client`); breadbar links it directly. Each `emit` is its own
short-lived connection. The widget subscription and the embedded-launcher
subscription each hold a reconnecting background subscription thread.

breadbar also consumes `bread.widget.**` (a re-fetch signal for the complete
widget list), `bread.box.open_requested`, and `bread.command.box.open` (the
last two only under an embedded launcher theme). Those are inbound signals
owned by the daemon and by app id `box`; they are not breadbar events.

## Events published (`bread.bar.*`)

| Event | Data | When |
|-------|------|------|
| `bread.bar.widget_clicked` | `{ "widget_id": "<id>", "action": <opaque value> }` | A rendered widget node with an `on_click` value is clicked. `widget_id` identifies the registered widget; `action` is passed through unchanged from the Lua module's `on_click`. Not emitted for nodes without `on_click`, or when the click is handled only locally by the bar. |
| `bread.bar.launched` | `{ "id": "<desktop id or exec>", "name": "<display name>" }` | A launcher entry is activated from breadbar's own launcher surface (stage or embedded capsule) and the spawn succeeds. `id` is the desktop-file id, falling back to the stripped `Exec=` line when empty. Not emitted when the spawn fails. |

Widget click actions are opaque to the daemon: breadbar does not interpret
them, and the event does not include the widget's module, label, or rendered
content.

## Commands honored (`bread.command.bar.*`)

None. breadbar has no `bread.command.bar.*` subscription. Under an embedded
launcher theme it subscribes to `bread.command.box.open` and
`bread.box.open_requested` as a handler for app id `box`'s launcher surface,
but that does not make breadbar the owner of those commands or events.

## Not implemented

- No `bread.command.bar.*` verb exists. The bar is rendered output, not a
  command target; there is no remote action to stub.
- No `bread.bar.*` event reports widget registration, widget clearing, bar
  reload, tray changes, or launcher open/close. `bread.widget.*` belongs to
  the daemon, and the rest are local UI state.
- No event carries rendered text, module source, tray contents, or a
  screenshot of the bar.

## Fail-safe behavior

- If breadd is not installed or not running, `emit` is a silent no-op
  (`BreadClient::emit` never blocks or errors the caller) and the
  subscriptions never receive anything. breadbar still starts, renders its
  configured local widgets, and runs its keybinds.
- If breadd restarts, both subscription threads reconnect with their own
  backoff loops. The 30-second widget poll re-fetches the full list and
  repairs any state missed during the reconnect window.
- If a widget fetch fails or returns a malformed entry, breadbar keeps the
  widgets it already has and drops only the malformed spec. A missing daemon
  never blanks the bar or crashes it.
