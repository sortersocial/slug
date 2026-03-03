//! Shared time-ago formatting (no external deps). Used by server (HTML) and CLI (thread XML).

/// Render "time ago" with " ago" suffix, e.g. "4d3h40m20s ago".
/// Uses days/hours/minutes/seconds (no months/years).
pub fn timeago(now_ms: i64, ts_ms: i64) -> String {
    let mut delta_ms = now_ms.saturating_sub(ts_ms);
    if delta_ms < 0 {
        delta_ms = 0;
    }
    let mut secs = (delta_ms / 1000) as i64;

    let days = secs / 86_400;
    secs %= 86_400;
    let hours = secs / 3_600;
    secs %= 3_600;
    let mins = secs / 60;
    secs %= 60;
    let s = secs;

    let mut out = String::new();
    if days > 0 {
        out.push_str(&format!("{days}d"));
    }
    if hours > 0 || !out.is_empty() {
        if hours > 0 {
            out.push_str(&format!("{hours}h"));
        }
    }
    if mins > 0 || !out.is_empty() {
        if mins > 0 {
            out.push_str(&format!("{mins}m"));
        }
    }
    out.push_str(&format!("{s}s ago"));
    out
}

/// Compact form for attributes (no " ago"), e.g. "1d3h20s".
pub fn timeago_compact(now_ms: i64, ts_ms: i64) -> String {
    let mut delta_ms = now_ms.saturating_sub(ts_ms);
    if delta_ms < 0 {
        delta_ms = 0;
    }
    let secs = (delta_ms / 1000) as u64;
    let d = secs / 86_400;
    let h = (secs % 86_400) / 3_600;
    let m = (secs % 3_600) / 60;
    let s = secs % 60;

    let mut out = String::new();
    if d > 0 {
        out.push_str(&format!("{d}d"));
    }
    if h > 0 {
        out.push_str(&format!("{h}h"));
    }
    if m > 0 {
        out.push_str(&format!("{m}m"));
    }
    if s > 0 || out.is_empty() {
        out.push_str(&format!("{s}s"));
    }
    out
}
