//! Process-level CPU and memory metrics.
//!
//! On Windows, uses `GetProcessTimes` and `GetProcessMemoryInfo` via
//! `windows-sys`.  On other platforms the collector returns `None`.

/// Snapshot of process resource usage.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessMetrics {
    /// Average CPU usage as a percentage (0.0 – 100.0+, multi-core can exceed 100).
    pub cpu_percent_avg: f64,
    /// Peak working set in megabytes.
    pub memory_mb_max: f64,
}

/// Opaque handle that records a starting point for CPU time measurement.
/// Call [`ProcessMetricsCollector::start`] before the workload, then
/// [`ProcessMetricsCollector::finish`] after.
pub struct ProcessMetricsCollector {
    #[cfg(windows)]
    inner: Option<WindowsCollector>,
    #[cfg(not(windows))]
    _unused: (),
}

impl ProcessMetricsCollector {
    /// Begin collecting.  On non-Windows this is a no-op.
    pub fn start() -> Self {
        #[cfg(windows)]
        {
            Self {
                inner: WindowsCollector::start(),
            }
        }
        #[cfg(not(windows))]
        {
            Self { _unused: () }
        }
    }

    /// End collection and return metrics.  Returns `None` on non-Windows or
    /// if any API call failed.
    pub fn finish(self, wall_clock_seconds: f64) -> Option<ProcessMetrics> {
        #[cfg(windows)]
        {
            self.inner?.finish(wall_clock_seconds)
        }
        #[cfg(not(windows))]
        {
            let _ = wall_clock_seconds;
            None
        }
    }
}

// ── Windows implementation ──────────────────────────────────────────

#[cfg(windows)]
struct WindowsCollector {
    start_kernel: u64,
    start_user: u64,
}

#[cfg(windows)]
impl WindowsCollector {
    fn start() -> Option<Self> {
        let (kernel, user) = process_cpu_times()?;
        Some(Self {
            start_kernel: kernel,
            start_user: user,
        })
    }

    fn finish(self, wall_clock_seconds: f64) -> Option<ProcessMetrics> {
        let (end_kernel, end_user) = process_cpu_times()?;
        let memory_mb_max = peak_working_set_mb()?;

        let cpu_delta_100ns =
            end_kernel.saturating_sub(self.start_kernel) + end_user.saturating_sub(self.start_user);
        // FILETIME units are 100-ns intervals → divide by 10_000_000 for seconds.
        let cpu_seconds = cpu_delta_100ns as f64 / 10_000_000.0;
        let cpu_percent_avg = if wall_clock_seconds > 0.0 {
            (cpu_seconds / wall_clock_seconds) * 100.0
        } else {
            0.0
        };

        Some(ProcessMetrics {
            cpu_percent_avg,
            memory_mb_max,
        })
    }
}

#[cfg(windows)]
fn process_cpu_times() -> Option<(u64, u64)> {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    let mut creation = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut exit = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut kernel = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    let mut user = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };

    // SAFETY: GetCurrentProcess() returns a pseudo-handle that is always valid
    // for the current process. All four FILETIME pointers are valid stack
    // allocations initialized to zero; the API writes into them.
    let ok = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &mut creation,
            &mut exit,
            &mut kernel,
            &mut user,
        )
    };

    if ok == 0 {
        return None;
    }

    Some((filetime_to_u64(&kernel), filetime_to_u64(&user)))
}

#[cfg(windows)]
fn peak_working_set_mb() -> Option<f64> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: zeroed() is valid for PROCESS_MEMORY_COUNTERS (all-zero is a
    // valid bit pattern for this POD struct).
    let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    counters.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;

    // SAFETY: GetCurrentProcess() returns a valid pseudo-handle. `counters`
    // is a valid stack-allocated struct with `cb` set to its size; the API
    // writes at most `cb` bytes into it.
    let ok = unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters,
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
    };

    if ok == 0 {
        return None;
    }

    Some(counters.PeakWorkingSetSize as f64 / (1024.0 * 1024.0))
}

#[cfg(windows)]
fn filetime_to_u64(ft: &windows_sys::Win32::Foundation::FILETIME) -> u64 {
    (ft.dwHighDateTime as u64) << 32 | ft.dwLowDateTime as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collector_round_trip() {
        let collector = ProcessMetricsCollector::start();
        // Do a tiny bit of work so CPU time isn't zero.
        let mut sum = 0u64;
        for i in 0..100_000 {
            sum = sum.wrapping_add(i);
        }
        let _ = std::hint::black_box(sum);

        let metrics = collector.finish(0.01);
        if cfg!(windows) {
            let m = metrics.expect("metrics should be available on Windows");
            assert!(
                m.cpu_percent_avg >= 0.0,
                "cpu_percent_avg={}",
                m.cpu_percent_avg
            );
            assert!(m.memory_mb_max > 0.0, "memory_mb_max={}", m.memory_mb_max);
        } else {
            assert!(metrics.is_none(), "non-Windows should return None");
        }
    }
}
