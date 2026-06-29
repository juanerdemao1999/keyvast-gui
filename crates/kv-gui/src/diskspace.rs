//! Free-disk-space query for the recording headroom indicator (#13).
//!
//! Windows-only via `GetDiskFreeSpaceExW`; other platforms return `None` so
//! the UI simply omits the headroom line.

use std::path::PathBuf;

/// Bytes available to the caller on the volume that holds `path`.
///
/// `path` may be relative or point at a directory that doesn't exist yet (the
/// usual case before the first recording) — we resolve it to the nearest
/// existing ancestor so the query still reflects the right volume.
#[cfg(windows)]
pub fn free_bytes(path: &str) -> Option<u64> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let mut p = PathBuf::from(path);
    if p.is_relative()
        && let Ok(cwd) = std::env::current_dir()
    {
        p = cwd.join(p);
    }
    while !p.exists() {
        match p.parent() {
            Some(parent) => p = parent.to_path_buf(),
            None => break,
        }
    }

    let wide: Vec<u16> = OsStr::new(p.as_os_str())
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut free_to_caller: u64 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 Vec kept alive for the
    // duration of the call. `free_to_caller` is a valid stack-allocated u64
    // written by the callee. The two null pointers are permitted by the
    // Win32 API for unused output parameters (total bytes, total free bytes).
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_to_caller,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if ok != 0 { Some(free_to_caller) } else { None }
}

#[cfg(not(windows))]
pub fn free_bytes(_path: &str) -> Option<u64> {
    None
}

// ── Recording disk-space guard (DA18) ───────────────────────────────────────
//
// Free space was only *displayed*; nothing enforced it, so an unattended
// overnight/behavioral recording (~3.7 MB/s at 30 kHz × 64 ch ≈ 13 GB/h) would
// silently fill the volume and truncate the in-progress file as an opaque write
// error. These thresholds drive a pre-flight check at recording start and a
// periodic low-water check that warns and then cleanly auto-stops (normal
// finalize) before the disk is full. Values are decimal GB to match the
// headroom indicator in the recording panel.

/// Minimum free space required to *start* a recording. Below this, starting is
/// refused outright rather than beginning a recording that is already doomed.
pub const RECORDING_MIN_START_FREE_BYTES: u64 = 2_000_000_000;

/// Free space at which an in-progress recording starts warning the operator
/// that it will auto-stop soon.
pub const RECORDING_WARN_FREE_BYTES: u64 = 5_000_000_000;

/// Free space at which an in-progress recording is cleanly auto-stopped to keep
/// the file from being truncated by a full disk.
pub const RECORDING_STOP_FREE_BYTES: u64 = 1_000_000_000;

/// Outcome of the pre-flight free-space check performed when a recording is
/// about to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartDecision {
    /// Enough headroom — proceed.
    Allow,
    /// Proceed, but the volume likely cannot hold the whole planned session.
    Warn { free_bytes: u64 },
    /// Refuse to start: free space is below the safe minimum.
    Block { free_bytes: u64 },
}

/// Decide whether a recording may start. `free_bytes` is `None` when the volume
/// could not be queried (non-Windows, or a failed query) — we do not block in
/// that case, since refusing on an unknowable quantity is worse than letting
/// the existing write-error path handle a genuinely full disk.
///
/// `estimated_session_bytes` is the caller's best guess at the full session
/// size (when a duration is known); if it exceeds the available space we warn
/// but still allow, since recordings here are normally open-ended.
pub fn evaluate_start(
    free_bytes: Option<u64>,
    estimated_session_bytes: Option<u64>,
) -> StartDecision {
    let Some(free) = free_bytes else {
        return StartDecision::Allow;
    };
    if free < RECORDING_MIN_START_FREE_BYTES {
        return StartDecision::Block { free_bytes: free };
    }
    if estimated_session_bytes.is_some_and(|needed| needed > free) {
        return StartDecision::Warn { free_bytes: free };
    }
    StartDecision::Allow
}

/// Disk status sampled periodically while a recording is in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingDiskStatus {
    /// Healthy headroom.
    Ok,
    /// Getting low — warn the operator; recording continues.
    Low { free_bytes: u64 },
    /// Below the safe water line — stop and finalize now.
    Critical { free_bytes: u64 },
}

/// Classify the current free space for an in-progress recording. `None`
/// (unqueryable) is treated as `Ok` so an unmonitorable volume never forces a
/// spurious stop.
pub fn evaluate_recording(free_bytes: Option<u64>) -> RecordingDiskStatus {
    let Some(free) = free_bytes else {
        return RecordingDiskStatus::Ok;
    };
    if free <= RECORDING_STOP_FREE_BYTES {
        RecordingDiskStatus::Critical { free_bytes: free }
    } else if free <= RECORDING_WARN_FREE_BYTES {
        RecordingDiskStatus::Low { free_bytes: free }
    } else {
        RecordingDiskStatus::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_blocks_below_minimum() {
        assert_eq!(
            evaluate_start(Some(RECORDING_MIN_START_FREE_BYTES - 1), None),
            StartDecision::Block {
                free_bytes: RECORDING_MIN_START_FREE_BYTES - 1
            }
        );
    }

    #[test]
    fn start_allows_with_ample_space() {
        assert_eq!(
            evaluate_start(Some(500_000_000_000), None),
            StartDecision::Allow
        );
    }

    #[test]
    fn start_warns_when_estimate_exceeds_free() {
        // Above the hard minimum, but the planned session won't fit.
        let free = 10_000_000_000;
        assert_eq!(
            evaluate_start(Some(free), Some(free + 1)),
            StartDecision::Warn { free_bytes: free }
        );
    }

    #[test]
    fn start_does_not_block_when_unqueryable() {
        assert_eq!(evaluate_start(None, None), StartDecision::Allow);
        assert_eq!(evaluate_start(None, Some(u64::MAX)), StartDecision::Allow);
    }

    #[test]
    fn recording_status_thresholds() {
        assert_eq!(evaluate_recording(None), RecordingDiskStatus::Ok);
        assert_eq!(
            evaluate_recording(Some(RECORDING_WARN_FREE_BYTES + 1)),
            RecordingDiskStatus::Ok
        );
        assert_eq!(
            evaluate_recording(Some(RECORDING_WARN_FREE_BYTES)),
            RecordingDiskStatus::Low {
                free_bytes: RECORDING_WARN_FREE_BYTES
            }
        );
        assert_eq!(
            evaluate_recording(Some(RECORDING_STOP_FREE_BYTES)),
            RecordingDiskStatus::Critical {
                free_bytes: RECORDING_STOP_FREE_BYTES
            }
        );
        assert_eq!(
            evaluate_recording(Some(0)),
            RecordingDiskStatus::Critical { free_bytes: 0 }
        );
    }
}
