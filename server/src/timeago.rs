//! Re-export shared timeago and server-only RFC3339 formatting.

pub use slug_types::timeago::{timeago, timeago_compact};

use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// Format a millisecond Unix timestamp as RFC3339 (UTC) for hover/tooltips.
pub fn rfc3339_utc(ts_ms: i64) -> String {
    let nanos = (ts_ms as i128).saturating_mul(1_000_000);
    let dt = OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    dt.to_offset(time::UtcOffset::UTC)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}
