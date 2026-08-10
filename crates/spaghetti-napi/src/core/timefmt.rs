//! Shared epoch-millisecond → ISO 8601 formatter.
//!
//! Every source that derives a session `created`/`modified` timestamp from a
//! file mtime needs the exact string JS `new Date(ms).toISOString()` produces
//! — a UTC instant with a 3-digit millisecond fraction and a trailing `Z`
//! (e.g. `2026-04-17T14:36:40.342Z`). The `time` crate's `Rfc3339` formatter
//! trims trailing fractional zeros (`.340Z` → `.34Z`, `.000Z` → `Z`), which
//! diverges from JS; this helper pins the fraction to exactly 3 digits.

/// Format an epoch-millisecond timestamp as an ISO 8601 UTC string matching
/// JS `new Date(ms).toISOString()` (always exactly 3 fractional digits + `Z`).
///
/// Values outside the representable range fall back to the Unix epoch, matching
/// how JS coerces `NaN`/`Invalid Date` at the boundaries.
pub fn epoch_ms_to_iso8601(ms: f64) -> String {
    const EPOCH: &str = "1970-01-01T00:00:00.000Z";

    // Two float hazards live here, and both cost a whole millisecond. Neither
    // is visible on a fixture — they need a real mtime to land on the wrong
    // side of a representation boundary.
    //
    // 1. Scaling to nanoseconds in f64. `ms * 1e6` overflows f64's exact-integer
    //    range (2^53 ≈ 9.0e15): a 2026 mtime is ~1.79e12 ms, so the product is
    //    ~1.79e18 and the nearest f64 can land tens of nanoseconds low —
    //    1785895422465.0 ms becomes 1785895422464999936 ns, which truncates to
    //    `.464Z` where JS renders `.465Z`. Fixed by truncating to integer
    //    milliseconds FIRST (they sit far below 2^53) and never scaling in f64.
    //
    // 2. Letting `time`'s `Iso8601` config render the subsecond. Asked for 3
    //    decimal digits it renders 38 of every 1000 millisecond values one
    //    millisecond low — precisely those ≡ 4 or 7 (mod 8), the fractions that
    //    are not exactly representable in binary. So the fraction is formatted
    //    here from the integer remainder, and `time` is used only for the
    //    calendar breakdown, which is integer-exact.
    //
    // `trunc()` matches JS `new Date(x)`, which applies ToInteger (toward zero);
    // NaN and out-of-range floats saturate to 0 under `as` and land on EPOCH.
    let total_ms = ms.trunc() as i128;

    // Euclidean split so pre-1970 instants borrow instead of yielding a
    // negative fraction: -1 ms must be 1969-12-31T23:59:59.999Z, not `.-001`.
    let secs = total_ms.div_euclid(1_000);
    let millis = total_ms.rem_euclid(1_000);

    let Ok(secs) = i64::try_from(secs) else {
        return EPOCH.to_string();
    };
    let Ok(dt) = time::OffsetDateTime::from_unix_timestamp(secs) else {
        return EPOCH.to_string();
    };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        dt.year(),
        dt.month() as u8,
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        millis,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_js_to_iso_string_with_three_digit_ms() {
        // JS: new Date(1700000000342).toISOString() === "2023-11-14T22:13:20.342Z"
        assert_eq!(
            epoch_ms_to_iso8601(1_700_000_000_342.0),
            "2023-11-14T22:13:20.342Z"
        );
    }

    #[test]
    fn keeps_trailing_zero_fraction() {
        // JS: new Date(1700000000340).toISOString() === "2023-11-14T22:13:20.340Z"
        // (Rfc3339 would trim this to ".34Z" — the bug this helper fixes.)
        assert_eq!(
            epoch_ms_to_iso8601(1_700_000_000_340.0),
            "2023-11-14T22:13:20.340Z"
        );
    }

    #[test]
    fn whole_second_keeps_three_zeros() {
        // JS: new Date(1700000000000).toISOString() === "2023-11-14T22:13:20.000Z"
        assert_eq!(
            epoch_ms_to_iso8601(1_700_000_000_000.0),
            "2023-11-14T22:13:20.000Z"
        );
    }

    #[test]
    fn epoch_zero() {
        assert_eq!(epoch_ms_to_iso8601(0.0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn does_not_lose_a_millisecond_to_f64_nanosecond_overflow() {
        // The real mtime that exposed this: a session file stamped exactly
        // 1785895422.465 s. Scaling in f64 gives 1785895422464999936 ns rather
        // than …465000000 (the product is ~1.79e18, well past 2^53), and
        // truncating that drops a whole millisecond — TS rendered `.465Z` and
        // Rust `.464Z` on the same file.
        //
        // JS: new Date(1785895422465).toISOString() === "2026-08-05T02:03:42.465Z"
        assert_eq!(
            epoch_ms_to_iso8601(1_785_895_422_465.0),
            "2026-08-05T02:03:42.465Z"
        );
    }

    #[test]
    fn fractional_milliseconds_truncate_toward_zero_like_js_date() {
        // Node's `stat().mtimeMs` is a float and can carry a sub-ms fraction.
        // JS `new Date(x)` applies ToInteger, i.e. truncation toward zero:
        // new Date(1785895422465.9).toISOString() === "2026-08-05T02:03:42.465Z"
        assert_eq!(
            epoch_ms_to_iso8601(1_785_895_422_465.9),
            "2026-08-05T02:03:42.465Z"
        );
    }

    #[test]
    fn nan_falls_back_to_epoch() {
        assert_eq!(epoch_ms_to_iso8601(f64::NAN), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn every_millisecond_fraction_renders_exactly() {
        // The bug this pins was invisible to the single-value tests above:
        // `time`'s Iso8601 config with 3 decimal digits rendered 38 of these
        // 1000 one millisecond low — exactly the values ≡ 4 or 7 (mod 8).
        // Spot-checking a handful of fractions passes straight through it, so
        // the assertion has to sweep the whole range.
        let base: i64 = 1_776_039_664_000;
        let mismatches: Vec<(i64, String)> = (0..1000i64)
            .filter_map(|f| {
                let iso = epoch_ms_to_iso8601((base + f) as f64);
                (iso[20..23].parse::<i64>().ok() != Some(f)).then_some((f, iso))
            })
            .collect();
        assert!(
            mismatches.is_empty(),
            "{} fractions rendered wrong, first few: {:?}",
            mismatches.len(),
            &mismatches[..mismatches.len().min(5)]
        );
    }

    #[test]
    fn pre_epoch_instants_borrow_instead_of_going_negative() {
        // JS: new Date(-1).toISOString() === "1969-12-31T23:59:59.999Z".
        // A truncating (rather than Euclidean) split would render ".-001".
        assert_eq!(epoch_ms_to_iso8601(-1.0), "1969-12-31T23:59:59.999Z");
        assert_eq!(epoch_ms_to_iso8601(-1000.0), "1969-12-31T23:59:59.000Z");
    }
}
