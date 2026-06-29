//! Display-string formatting helpers for the side panels.

pub(crate) fn format_time_window(secs: f64) -> String {
    if secs >= 1.0 {
        format!("{:.0} s", secs)
    } else {
        format!("{:.0} ms", secs * 1000.0)
    }
}

pub(crate) fn format_uv(uv: f64) -> String {
    if uv >= 1000.0 {
        format!("{:.0} mV/div", uv / 1000.0)
    } else {
        format!("{:.0} uV/div", uv)
    }
}

pub(crate) fn format_large_number(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
