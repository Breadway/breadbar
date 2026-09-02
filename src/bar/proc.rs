//! One place for "run a helper CLI, capture stdout, give up after N seconds".
//!
//! The bar polls `wpctl` / `playerctl` / `nmcli` / `breadcrumbs` on every
//! refresh. Each call is a `tokio::time::timeout` around
//! `tokio::process::Command::output()` — and when the timeout fires it
//! **drops** the pending `output()` future. Without `kill_on_drop(true)` the
//! spawned child (and anything *it* spawned — `breadcrumbs status` shells
//! out to `bwrap`) keeps running and is never reaped, so a slow subprocess
//! leaves a `<defunct>` entry behind every poll cycle. `kill_on_drop`
//! SIGKILLs the child on drop and hands it to tokio's orphan reaper.

use std::process::Output;
use std::time::Duration;

use tokio::process::Command;

/// Run `prog args…`, capturing stdout, killed if it outlives `timeout`.
///
/// `None` means the process could not be spawned or it hit the timeout —
/// callers that also care about the exit status check `Output::status` on
/// the `Some` themselves (some do, some deliberately use the partial output
/// of a non-zero exit).
pub async fn output(prog: &str, args: &[&str], timeout: Duration) -> Option<Output> {
    let fut = Command::new(prog).args(args).kill_on_drop(true).output();
    tokio::time::timeout(timeout, fut).await.ok()?.ok()
}

/// [`output`], narrowed to "stdout, but only if the command exited 0" — the
/// common case for the bar's status polls.
pub async fn stdout_ok(prog: &str, args: &[&str], timeout: Duration) -> Option<Vec<u8>> {
    let out = output(prog, args, timeout).await?;
    out.status.success().then_some(out.stdout)
}
