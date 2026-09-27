# AGENTS.md: Repo hygiene

Scope: this file covers *repo hygiene* (branching, remotes, CI) plus a
short map of the binary. It is not user-facing project documentation.

This repo follows the branch/release workflow documented in `CONTRIBUTING.md`:
read and follow it for any git, branch, or release work here (the
single-trunk model, `feature/x`/`fix/x` branch naming, how RC tags work,
etc). Don't improvise a different workflow. The short version: there is one
long-lived branch, `main`; no `dev` or `beta` branch exists. `main`
auto-publishes a dev-track build on every push. "Beta" and "stable" are both
just tags, not branches: push a `vX.Y.Z-rc.N` tag to publish a beta-track
build, push a plain `vX.Y.Z` tag to cut the signed stable release.
"Freezing" for stabilization means pausing pushes to `main`, not moving a
branch. This replaced an earlier three-branch (`dev`/`beta`/`main`) model
after `main` was found to have silently rotted out of sync with `dev`/`beta`
across most repos in this ecosystem.

When starting work on a new feature, create branch `feature/<feature-name>`.
When working on a bug or issue, create branch `fix/<issue you are fixing>`.

## Remotes
- `origin`: Forgejo (`git.breadway.dev`, SSH), authoritative.
- `github`: GitHub mirror. Push `origin` only; GitHub auto-mirrors.

## CI
- `check.yml` triggers on `push: branches: ['feature/**', 'fix/**']`.
- `dev-release.yml` triggers on `push: branches: ['main']`.
- `rc-release.yml` triggers on `vX.Y.Z-rc.N` tag pushes (beta track).
- `release.yml` triggers on any other `v*` tag push (stable).
  None of these run on plain commits or PRs beyond what's listed.
- `check.yml` and `dev-release.yml` both build in "dev mode": every
  `git.breadway.dev` dependency (bread-theme, bread-utils,
  bread-screenshots, bread-launcher, bread-shared) is patched to its own
  repo's `main` at build time by bread-ecosystem's `scripts/dev-deps.sh`,
  and the build runs without `--locked`. `rc-release.yml`/`release.yml`
  never patch anything: tagged Cargo.toml pins, `--locked`, same as
  before. Cargo.toml itself always stays tag-pinned; don't hand-edit it to
  a `rev`/`branch`/`path` to test a cross-repo change; `scripts/lint-pins.sh`
  (also from bread-ecosystem) fails `check.yml` if you do. See
  bread-ecosystem's `docs/release-channels.md` ("Cross-repo dependency
  resolution") for the full model. Part of ECO-38.

## Architecture

One GTK4/`relm4` binary, four surfaces:

| Area | Path | Role |
|---|---|---|
| Bar | `src/bar/` | Layer-shell top bar: workspaces, clock, media, stats, wifi, bluetooth, control panel + SNI tray |
| Notifications | `src/notifications/` | `org.freedesktop.Notifications` daemon + stacked popups + in-memory history (`breadbar --history`) |
| OSD | `src/osd.rs` | Volume/brightness overlay |
| Widgets | `src/widgets/` | Live Lua widgets from breadd via `BreadClient` / `WidgetSpec` |

`--screenshot` (`src/screenshot.rs`) captures those views through
`bread-screenshots`; do not rewrite it just to retarget the crate pin.

`application_id` drift vs Hyprland layer-rules/tour docs is known; leave it
unless every mention is updated in the same change.

## Don't
- Don't embed credentials in remote URLs: SSH or a credential helper only.
- Don't rewrite the widget system or `screenshot.rs` as part of pin/docs work.

## Persistent memory
Read `~/.knowledge.md` at the start of a task: it points to a shared memory index at `~/.claude/memory/MEMORY.md` (also used by Claude Code). Selectively read the linked entries relevant to what you're working on.
