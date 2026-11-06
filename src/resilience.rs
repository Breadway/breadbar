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
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            file,
            "breadbar: {label} subscription callback panicked: {message}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::guarded;

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
        let path =
            std::env::temp_dir().join(format!("breadbar-panic-test-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::env::set_var("BREADBAR_PANIC_LOG", &path);
        guarded("widgets", || panic!("synthetic logged failure"));
        std::env::remove_var("BREADBAR_PANIC_LOG");
        let written = std::fs::read_to_string(&path).unwrap_or_default();
        let _ = std::fs::remove_file(&path);
        assert!(written.contains("widgets subscription callback panicked"));
        assert!(written.contains("synthetic logged failure"));
    }
}
