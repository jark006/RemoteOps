//! Shared display-side epoch → RFC 3339 formatting.
//!
//! Raw values (Unix epoch seconds, bytes, `st_mode`, boot-relative ticks)
//! remain canonical on every tool for exact computation. These helpers render
//! the one derived field — a human- and model-readable wall-clock timestamp in
//! the Agent's local timezone — that the stat/process/system tools surface.

/// Format epoch seconds as RFC 3339 in the Agent's local timezone, e.g.
/// `2026-08-22T12:29:22+08:00`. The offset is derived from the local civil
/// time of the same instant, so the string round-trips to `secs` exactly.
#[cfg(unix)]
pub(crate) fn format_epoch_iso(secs: i64) -> String {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // musl's ABI is `long time_t`, so c_long matches every supported Unix
    // target without depending on libc's deprecated time_t alias.
    let time: std::os::raw::c_long = secs as std::os::raw::c_long;
    if unsafe { libc::localtime_r(&time as *const _, &mut tm) }.is_null() {
        return format_iso(1970, 1, 1, 0, 0, 0, 0);
    }
    let year = (tm.tm_year + 1900) as i64;
    let month = (tm.tm_mon + 1) as i64;
    let day = tm.tm_mday as i64;
    let hour = tm.tm_hour as i64;
    let minute = tm.tm_min as i64;
    let second = tm.tm_sec as i64;
    // The civil-to-UTC wall clock difference is the local UTC offset.
    let offset = civil_seconds_utc(year, month, day, hour, minute, second) - secs;
    format_iso(
        year as u32,
        month as u32,
        day as u32,
        hour as u32,
        minute as u32,
        second as u32,
        offset,
    )
}

#[cfg(windows)]
pub(crate) fn format_epoch_iso(secs: i64) -> String {
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct SystemTime {
        w_year: u16,
        w_month: u16,
        w_day_of_week: u16,
        w_day: u16,
        w_hour: u16,
        w_minute: u16,
        w_second: u16,
        w_milliseconds: u16,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct FileTime {
        dw_low: u32,
        dw_high: u32,
    }

    #[link(name = "Kernel32")]
    unsafe extern "system" {
        fn FileTimeToLocalFileTime(file_time: *const FileTime, local: *mut FileTime) -> i32;
        fn FileTimeToSystemTime(file_time: *const FileTime, system_time: *mut SystemTime) -> i32;
    }

    // 100 ns ticks of FILETIME (1601-01-01) per epoch second.
    const EPOCH_TICKS: i64 = 11_644_473_600 * 10_000_000;
    let ticks = secs.saturating_mul(10_000_000).saturating_add(EPOCH_TICKS);
    let utc = FileTime {
        dw_low: (ticks & 0xffff_ffff) as u32,
        dw_high: ((ticks >> 32) & 0xffff_ffff) as u32,
    };
    let mut local = FileTime::default();
    let mut civil = SystemTime::default();
    let ok = unsafe { FileTimeToLocalFileTime(&utc, &mut local) != 0 }
        && unsafe { FileTimeToSystemTime(&local, &mut civil) != 0 };
    if !ok {
        return format_iso(1970, 1, 1, 0, 0, 0, 0);
    }
    let year = civil.w_year as i64;
    let month = civil.w_month as i64;
    let day = civil.w_day as i64;
    let hour = civil.w_hour as i64;
    let minute = civil.w_minute as i64;
    let second = civil.w_second as i64;
    let offset = civil_seconds_utc(year, month, day, hour, minute, second) - secs;
    format_iso(
        year as u32,
        month as u32,
        day as u32,
        hour as u32,
        minute as u32,
        second as u32,
        offset,
    )
}

fn format_iso(
    year: u32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    offset_seconds: i64,
) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let magnitude = offset_seconds.unsigned_abs();
    format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}{sign}{:02}:{:02}",
        magnitude / 3600,
        (magnitude % 3600) / 60,
    )
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146097 + day_of_era - 719_468
}

/// Seconds since the epoch for a civil wall-clock time interpreted as UTC.
fn civil_seconds_utc(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> i64 {
    days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_iso_for_test(iso: &str) -> i64 {
        let bytes = iso.as_bytes();
        let num = |range: std::ops::Range<usize>| -> i64 {
            std::str::from_utf8(&bytes[range]).unwrap().parse().unwrap()
        };
        let year = num(0..4);
        let month = num(5..7);
        let day = num(8..10);
        let hour = num(11..13);
        let minute = num(14..16);
        let second = num(17..19);
        let offset_sign = if bytes[19] == b'-' { -1 } else { 1 };
        let offset = offset_sign * (num(20..22) * 3600 + num(23..25) * 60);
        civil_seconds_utc(year, month, day, hour, minute, second) - offset
    }

    #[test]
    fn format_epoch_iso_round_trips_to_epoch() {
        for secs in [0i64, 946_684_800, 1_710_000_000, 2_000_000_000] {
            let iso = format_epoch_iso(secs);
            assert_eq!(
                parse_iso_for_test(&iso),
                secs,
                "epoch {secs} did not round-trip through {iso}"
            );
        }
    }
}
