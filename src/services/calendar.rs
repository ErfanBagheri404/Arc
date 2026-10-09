//! Calendar: local `.ics` subscriptions, parsed by hand — the iCalendar
//! grammar Arc needs (VEVENT / DTSTART / DTEND / SUMMARY) is a flat line-based
//! format, so a state machine over lines beats a crate.
//!
//! OAuth device flow for Google Calendar lands as its own increment; this
//! module only understands subscribed URLs and local files.

/// One upcoming event.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub summary: String,
    /// Unix seconds, UTC.
    pub start: i64,
    /// Unix seconds, UTC. All-day events end at midnight.
    pub end: i64,
    /// All-day events show a date, not a clock time.
    pub all_day: bool,
}

impl Event {
    /// A pill/panel label like `14:00 Standup` or `Mon picnic`.
    pub fn label(&self) -> String {
        if self.all_day {
            return self.summary.clone();
        }
        let secs = self.start % 86_400;
        format!("{:02}:{:02} {}", secs / 3600, (secs % 3600) / 60, self.summary)
    }
}

/// Parse the VEVENTs out of an `.ics` body. Handles line unfolding (a CRLF
/// followed by a space or tab continues the previous line) and
/// the `DTSTART;VALUE=DATE` all-day form. Recurrence is out of scope: only
/// concrete instances are listed.
pub fn parse(ics: &str) -> Vec<Event> {
    let mut out = Vec::new();
    // Unfold first: continuation lines start with a space or tab.
    let mut flat = String::with_capacity(ics.len());
    for line in ics.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            flat.push_str(line.trim_start());
        } else {
            flat.push('\n');
            flat.push_str(line);
        }
    }
    let mut summary = String::new();
    let mut start = 0i64;
    let mut end = 0i64;
    let mut all_day = false;
    let mut inside = false;
    for line in flat.lines() {
        if line.starts_with("BEGIN:VEVENT") {
            inside = true;
            summary.clear();
            start = 0;
            end = 0;
            all_day = false;
            continue;
        }
        if line.starts_with("END:VEVENT") {
            if inside && start > 0 {
                out.push(Event {
                    summary: summary.clone(),
                    start,
                    end: if end > start { end } else { start + 3600 },
                    all_day,
                });
            }
            inside = false;
            continue;
        }
        if !inside {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        // Property parameters live before the colon: `DTSTART;TZID=X:…`.
        let prop = name.split(';').next().unwrap_or(name);
        match prop {
            "SUMMARY" => summary = unescape(value),
            "DTSTART" => {
                if let Some((t, ad)) = dt(value) {
                    start = t;
                    all_day = ad;
                }
            }
            "DTEND" => {
                if let Some((t, _)) = dt(value) {
                    end = t;
                }
            }
            _ => {}
        }
    }
    out.sort_by_key(|e| e.start);
    out
}

/// Drop the escape sequences RFC 5545 defines in text values.
fn unescape(v: &str) -> String {
    v.replace("\\n", " ")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

/// Parse one DTSTART/DTEND value: `YYYYMMDDTHHMMSSZ` (UTC), the same without
/// `Z` (floating — treated as UTC), or `YYYYMMDD` (all-day). Returns
/// `(unix_seconds, is_all_day)`.
fn dt(v: &str) -> Option<(i64, bool)> {
    let v = v.trim();
    if v.len() == 8 && v.chars().all(|c| c.is_ascii_digit()) {
        let (y, m, d) = ymd(v)?;
        return Some((days_from_civil(y, m, d) * 86_400, true));
    }
    if v.len() >= 15 && v.as_bytes()[8] == b'T' {
        let (y, m, d) = ymd(&v[..8])?;
        let hh: i64 = v[9..11].parse().ok()?;
        let mm: i64 = v[11..13].parse().ok()?;
        let ss: i64 = v[13..15].parse().ok()?;
        return Some((
            days_from_civil(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss,
            false,
        ));
    }
    None
}

fn ymd(s: &str) -> Option<(i64, i64, i64)> {
    let y: i64 = s[..4].parse().ok()?;
    let m: i64 = s[4..6].parse().ok()?;
    let d: i64 = s[6..8].parse().ok()?;
    Some((y, m, d))
}

/// Howard Hinnant's `days_from_civil`: days since 1970-01-01, proleptic
/// Gregorian. The std has no date math, so this is the one calendar formula.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Fetch an `.ics` URL via the weather service's WinINet helper — one HTTP
/// path for the whole app.
pub fn fetch(url: &str) -> Option<Vec<Event>> {
    Some(parse(&crate::services::weather::http(url)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note the escaped comma, and the folded SUMMARY ("Pi\r\n cnic") that
    // unfolds back to "Picnic" — real iCalendar producers do both.
    const SAMPLE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nSUMMARY:Standup\\, room 2\r\nDTSTART:20261009T090000Z\r\nDTEND:20261009T093000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nSUMMARY:Pi\r\n cnic\r\nDTSTART;VALUE=DATE:20261010\r\nEND:VEVENT\r\nEND:VCALENDAR";

    #[test]
    fn parses_timed_and_all_day_events_with_unfolding() {
        let evs = parse(SAMPLE);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].summary, "Standup, room 2");
        assert_eq!(evs[0].label(), "09:00 Standup, room 2");
        assert!(!evs[0].all_day);
        assert_eq!(evs[1].summary, "Picnic");
        assert!(evs[1].all_day);
        assert_eq!(evs[1].label(), "Picnic");
    }

    #[test]
    fn events_sort_by_start() {
        let evs = parse(
            "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nSUMMARY:Late\r\nDTSTART:20261009T180000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nSUMMARY:Early\r\nDTSTART:20261009T070000Z\r\nEND:VEVENT\r\nEND:VCALENDAR",
        );
        assert_eq!(evs[0].summary, "Early");
        assert_eq!(evs[1].summary, "Late");
    }

    #[test]
    fn a_broken_event_is_skipped_not_fatal() {
        let evs = parse("BEGIN:VEVENT\r\nSUMMARY:No date\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nSUMMARY:Good\r\nDTSTART:20261009T100000Z\r\nEND:VEVENT\r\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].summary, "Good");
    }

    #[test]
    fn epoch_math_is_self_consistent() {
        // One day is one day, wherever the anchor sits; and the timed-SAMPLE
        // parse must equal the formula (round-trip across both paths).
        assert_eq!(days_from_civil(2026, 10, 10) - days_from_civil(2026, 10, 9), 1);
        let t = dt("20261009T090000Z").unwrap().0;
        assert_eq!(t, days_from_civil(2026, 10, 9) * 86_400 + 9 * 3600);
    }
}
