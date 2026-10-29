//! Labels shared with the web client; see `apps/web/src/timestampFormat.ts`.

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

/// The compact age the sidebar shows: "just now", "5m", "3h", "2d".
pub fn relative_time(timestamp_ms: i64, now_ms: i64) -> String {
    let elapsed = (now_ms - timestamp_ms).max(0);
    if elapsed < MINUTE_MS {
        "just now".into()
    } else if elapsed < HOUR_MS {
        format!("{}m", elapsed / MINUTE_MS)
    } else if elapsed < DAY_MS {
        format!("{}h", elapsed / HOUR_MS)
    } else {
        format!("{}d", elapsed / DAY_MS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floors_to_the_largest_whole_unit() {
        assert_eq!(relative_time(0, 59_999), "just now");
        assert_eq!(relative_time(0, 119_999), "1m");
        assert_eq!(relative_time(0, HOUR_MS * 23 + 1), "23h");
        assert_eq!(relative_time(0, DAY_MS * 3), "3d");
        // Clock skew must not produce negative ages.
        assert_eq!(relative_time(10_000, 0), "just now");
    }
}
