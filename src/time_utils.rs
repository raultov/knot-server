use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Returns the current time as a simple ISO 8601 string (e.g. "2023-10-25T14:30:00Z").
pub fn chrono_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format_iso8601(now.as_secs())
}

/// Formats seconds since the UNIX epoch as an ISO 8601 string.
pub fn format_iso8601(secs_since_epoch: u64) -> String {
    let days = secs_since_epoch / 86400;
    let remaining = secs_since_epoch % 86400;
    let hours = remaining / 3600;
    let remaining = remaining % 3600;
    let minutes = remaining / 60;
    let seconds = remaining % 60;

    let mut year = 1970i64;
    let mut days_left = days as i64;
    loop {
        let days_in_year = if is_leap(year) { 366 } else { 365 };
        if days_left < days_in_year {
            break;
        }
        days_left -= days_in_year;
        year += 1;
    }

    let month_days = get_month_days(year);

    let mut month = 1u32;
    for &md in &month_days {
        if days_left < md as i64 {
            break;
        }
        days_left -= md as i64;
        month += 1;
    }
    let day = days_left as u32 + 1;

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year, month, day, hours, minutes, seconds
    )
}

/// Calculates the duration elapsed since a given ISO 8601 timestamp string.
pub fn elapsed_since_iso8601(ts: &str) -> anyhow::Result<Duration> {
    let ts = ts.trim();
    let (date_part, time_part) = if let Some(pos) = ts.find('T') {
        (&ts[..pos], &ts[pos + 1..])
    } else {
        anyhow::bail!("Invalid ISO 8601 timestamp: missing 'T'");
    };

    let time_part = time_part.trim_end_matches('Z');

    let date_parts: Vec<&str> = date_part.split('-').collect();
    let time_parts: Vec<&str> = time_part.split(':').collect();

    if date_parts.len() != 3 || time_parts.len() < 2 {
        anyhow::bail!("Invalid ISO 8601 timestamp format");
    }

    let year: i64 = date_parts[0].parse()?;
    let month: u32 = date_parts[1].parse()?;
    let day: u32 = date_parts[2].parse()?;
    let hour: u64 = time_parts[0].parse()?;
    let min: u64 = time_parts[1].parse()?;
    let sec: u64 = if time_parts.len() > 2 {
        time_parts[2].parse()?
    } else {
        0
    };

    let days = days_since_epoch(year, month, day);
    let total_secs = days as u64 * 86400 + hour * 3600 + min * 60 + sec;

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if total_secs > now_secs {
        // Timestamp is in the future
        return Ok(Duration::ZERO);
    }

    Ok(Duration::from_secs(now_secs - total_secs))
}

fn days_since_epoch(year: i64, month: u32, day: u32) -> i64 {
    let mut days = 0i64;
    for y in 1970..year {
        days += if is_leap(y) { 366 } else { 365 };
    }
    let month_days = get_month_days(year);
    for m in 1..month as usize {
        days += month_days[m - 1] as i64;
    }
    days += day as i64 - 1;
    days
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
}

fn get_month_days(year: i64) -> [u32; 12] {
    if is_leap(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    }
}
