use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Render a hyper-concise "time ago" string like:
/// - "0s ago"
/// - "3m5s ago"
/// - "4d3h40m20s ago"
///
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
    // Always show seconds.
    out.push_str(&format!("{s}s ago"));
    out
}

/// Format a millisecond Unix timestamp as RFC3339 (UTC) for hover/tooltips.
pub fn rfc3339_utc(ts_ms: i64) -> String {
    let nanos = (ts_ms as i128).saturating_mul(1_000_000);
    let dt = OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    dt.to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

