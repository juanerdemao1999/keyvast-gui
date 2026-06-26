//! Per-frame panic isolation for the GUI event loop (DA34).
//!
//! eframe drives [`KvApp::update`] once per frame and that frame drives the
//! live acquisition/recording pipeline. Without a guard, a single panic in the
//! render path (an out-of-range notch index, a non-power-of-two FFT, a stray
//! `unwrap`, …) unwinds straight out of the event loop and aborts the process.
//! That kills the recorder thread mid-write, so the in-progress `.kvraw` never
//! gets its footer flushed and an experiment's data is lost.
//!
//! [`guard_frame`] wraps the frame body in [`std::panic::catch_unwind`] so the
//! app can latch the panic, finalize the active recording, and keep the process
//! alive long enough to surface an error screen instead of vanishing.

use std::panic::{self, AssertUnwindSafe};

/// Run one frame body, catching any panic and returning its message.
///
/// On success returns `Ok(())`. On panic returns `Err(message)` with a
/// best-effort human-readable description extracted from the panic payload.
pub fn guard_frame<F: FnOnce()>(frame: F) -> Result<(), String> {
    // The frame closure captures `&mut KvApp`, which is not `UnwindSafe`; that
    // is fine here because a caught panic puts the app into a terminal
    // recovery state rather than continuing to mutate shared invariants.
    match panic::catch_unwind(AssertUnwindSafe(frame)) {
        Ok(()) => Ok(()),
        Err(payload) => Err(payload_message(payload.as_ref())),
    }
}

/// Extract a readable message from a caught panic payload.
///
/// `panic!` payloads are usually `&'static str` or `String`; anything else
/// falls back to a generic label so the caller always has something to show.
pub fn payload_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn runs_body_and_reports_success() {
        let counter = AtomicU32::new(0);
        let result = guard_frame(|| {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        assert!(result.is_ok());
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn catches_str_panic_and_extracts_message() {
        let result = guard_frame(|| panic!("notch index out of range"));
        assert_eq!(result, Err("notch index out of range".to_string()));
    }

    #[test]
    fn catches_string_panic_and_extracts_message() {
        let result = guard_frame(|| panic!("fft size {} not a power of two", 384));
        assert_eq!(result, Err("fft size 384 not a power of two".to_string()));
    }

    #[test]
    fn payload_message_handles_non_string_payload() {
        let result = guard_frame(|| std::panic::panic_any(42u8));
        assert_eq!(result, Err("unknown panic payload".to_string()));
    }
}
