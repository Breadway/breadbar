//! Keep long-lived subscription threads alive when one callback misbehaves.
//!
//! `BreadClient::subscribe` runs its read loop on a background thread. A panic
//! in the caller's callback unwinds out of that loop and kills the thread; the
//! `Subscription` handle is leaked by design, so the socket stays open with
//! nobody reading it. That is the failure mode observed in BAR-59: one
//! subscription had 81 KB queued with no thread and no poll registration.
//!
//! Callbacks now send through a fallible channel and are wrapped here, so a
//! panic in one event handler is logged instead of silently ending the
//! subscription. This does not make the main loop healthy again, but it keeps
//! the remaining subscriptions connected and makes the panic visible.

use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Run `callback`, logging instead of propagating a panic.
pub fn guarded(label: &str, callback: impl FnOnce()) {
    let Err(payload) = catch_unwind(AssertUnwindSafe(callback)) else {
        return;
    };
    let message = panic_message(payload.as_ref());
    eprintln!(
        "breadbar: {label} subscription callback panicked: {message}; keeping the subscription alive"
    );
    record_panic(label, &message);
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        (*text).to_string()
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

/// Append the panic to `BREADBAR_PANIC_LOG` when that variable names a file.
///
/// The bar is normally launched by the compositor with no captured stderr, so
/// a bare eprintln is invisible. This is the opt-in durable path.
fn record_panic(label: &str, message: &str) {
    let Some(path) = std::env::var_os("BREADBAR_PANIC_LOG") else {
        return;
    };
    append_panic_line(std::path::Path::new(&path), label, message);
}

/// Append one complete log line in a single write on an `O_APPEND` handle.
///
/// The line is formatted first so a concurrent writer cannot interleave
/// fragments of it: every append is one `write_all` at the end of the file.
fn append_panic_line(path: &std::path::Path, label: &str, message: &str) {
    let line = format!("breadbar: {label} subscription callback panicked: {message}\n");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::{append_panic_line, guarded};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A panic-log path unique to this invocation. The opt-in variable is
    /// process-wide, so tests that panic concurrently may append to whatever it
    /// names while another test holds it; a shared path made those writes look
    /// like corruption.
    fn unique_log_path(tag: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "breadbar-panic-test-{}-{sequence}-{tag}.log",
            std::process::id()
        ))
    }

    fn nonce() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    }

    #[test]
    fn runs_a_normal_callback() {
        let mut ran = false;
        guarded("test", || ran = true);
        assert!(ran);
    }

    #[test]
    fn swallows_a_panicking_callback() {
        // The panic hook prints to stderr; the test only asserts that control
        // returns instead of unwinding into the subscription loop.
        guarded("test", || panic!("synthetic callback failure"));
    }

    #[test]
    fn a_later_callback_still_runs_after_a_panic() {
        guarded("test", || panic!("synthetic callback failure"));
        let mut ran = false;
        guarded("test", || ran = true);
        assert!(ran);
    }

    #[test]
    fn an_opt_in_log_records_the_panic() {
        // Unique path and label: other tests may panic through `guarded` while
        // this one holds the process-wide variable, but each append is a whole
        // line, so the exact line this test causes is always present intact.
        let stamp = nonce();
        let path = unique_log_path("opt-in");
        let _ = std::fs::remove_file(&path);
        std::env::set_var("BREADBAR_PANIC_LOG", &path);
        guarded(&format!("widgets-{stamp}"), || {
            panic!("synthetic logged failure {stamp}")
        });
        std::env::remove_var("BREADBAR_PANIC_LOG");
        let written = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        let expected = format!(
            "breadbar: widgets-{stamp} subscription callback panicked: synthetic logged failure {stamp}"
        );
        assert!(
            written.lines().any(|line| line == expected),
            "log did not contain the complete expected line; got: {written:?}"
        );
    }

    #[test]
    fn concurrent_appends_never_interleave_lines() {
        // The flake this guards against: two threads writing one line each in
        // separate fragments produced lines like "...widgetstesttest...".
        let path = unique_log_path("concurrent");
        let _ = std::fs::remove_file(&path);
        let writers = 8;
        let per_writer = 25;
        std::thread::scope(|scope| {
            for writer in 0..writers {
                let path = path.clone();
                scope.spawn(move || {
                    for line in 0..per_writer {
                        append_panic_line(
                            &path,
                            &format!("widgets-{writer}"),
                            &format!("failure {line}"),
                        );
                    }
                });
            }
        });
        let written = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        let lines: Vec<&str> = written.lines().collect();
        assert_eq!(lines.len(), writers * per_writer);
        for line in lines {
            let Some((head, tail)) = line.split_once(" subscription callback panicked: ") else {
                panic!("line lost its separator: {line:?}");
            };
            let Some(writer) = head.strip_prefix("breadbar: widgets-") else {
                panic!("line head was interleaved or malformed: {line:?}");
            };
            assert!(
                writer.parse::<usize>().is_ok(),
                "line head was interleaved or malformed: {line:?}"
            );
            let Some(failure) = tail.strip_prefix("failure ") else {
                panic!("line tail was interleaved or malformed: {line:?}");
            };
            assert!(
                failure.parse::<usize>().is_ok(),
                "line tail was interleaved or malformed: {line:?}"
            );
        }
    }
}
