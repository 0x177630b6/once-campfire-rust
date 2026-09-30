//! Shift boundaries (phase 2.6): the handover's shift ends (`"07:00"`, `"15:00"`, `"23:00"` by
//! default) in the settings' time zone.
//!
//! Time zones: an IANA name (`Europe/Paris`) from the system's time zone database when it has one;
//! Campfire's image is a slim Debian without `tzdata`, so a table of common zones as POSIX rules
//! ([`builtin`], today's rules, daylight saving included) takes over; a POSIX TZ string
//! (`CET-1CEST,M3.5.0,M10.5.0/3`) is accepted as is. Anything else doesn't validate.

use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

pub const DEFAULT_TIME_ZONE: &str = "Europe/Paris";
pub const DEFAULT_SHIFT_ENDS: [&str; 3] = ["07:00", "15:00", "23:00"];
pub const MAX_SHIFTS: usize = 6;

/// The time zone called `name`, if it can be resolved.
pub fn time_zone(name: &str) -> Option<TimeZone> {
    let name = name.trim();
    if name.is_empty() || name.len() > 64 {
        return None;
    }
    if let Ok(zone) = TimeZone::get(name) {
        return Some(zone);
    }
    if let Some(rule) = builtin(name) {
        return TimeZone::posix(rule).ok();
    }
    if name.bytes().any(|b| b.is_ascii_digit()) {
        return TimeZone::posix(name).ok();
    }
    None
}

/// Today's rules of common zones, for a system without a time zone database.
pub fn builtin(name: &str) -> Option<&'static str> {
    const CET: &str = "CET-1CEST,M3.5.0,M10.5.0/3";
    const EET: &str = "EET-2EEST,M3.5.0/3,M10.5.0/4";
    const TABLE: &[(&str, &str)] = &[
        ("UTC", "UTC0"),
        ("Etc/UTC", "UTC0"),
        ("GMT", "GMT0"),
        ("Europe/London", "GMT0BST,M3.5.0/1,M10.5.0"),
        ("Europe/Dublin", "IST-1GMT0,M10.5.0,M3.5.0/1"),
        ("Europe/Lisbon", "WET0WEST,M3.5.0/1,M10.5.0"),
        ("Europe/Paris", CET),
        ("Europe/Brussels", CET),
        ("Europe/Luxembourg", CET),
        ("Europe/Monaco", CET),
        ("Europe/Amsterdam", CET),
        ("Europe/Berlin", CET),
        ("Europe/Zurich", CET),
        ("Europe/Vienna", CET),
        ("Europe/Madrid", CET),
        ("Europe/Rome", CET),
        ("Europe/Copenhagen", CET),
        ("Europe/Oslo", CET),
        ("Europe/Stockholm", CET),
        ("Europe/Prague", CET),
        ("Europe/Warsaw", CET),
        ("Europe/Budapest", CET),
        ("Europe/Athens", EET),
        ("Europe/Helsinki", EET),
        ("Europe/Bucharest", EET),
        ("Europe/Sofia", EET),
        ("Europe/Kyiv", EET),
        ("America/New_York", "EST5EDT,M3.2.0,M11.1.0"),
        ("America/Toronto", "EST5EDT,M3.2.0,M11.1.0"),
        ("America/Montreal", "EST5EDT,M3.2.0,M11.1.0"),
        ("America/Chicago", "CST6CDT,M3.2.0,M11.1.0"),
        ("America/Denver", "MST7MDT,M3.2.0,M11.1.0"),
        ("America/Los_Angeles", "PST8PDT,M3.2.0,M11.1.0"),
        ("America/Martinique", "AST4"),
        ("America/Guadeloupe", "AST4"),
        ("America/Cayenne", "<-03>3"),
        ("Indian/Reunion", "<+04>-4"),
        ("Indian/Mayotte", "EAT-3"),
        ("Pacific/Noumea", "<+11>-11"),
        ("Pacific/Tahiti", "<-10>10"),
        ("Asia/Dubai", "<+04>-4"),
    ];
    TABLE.iter().find(|(known, _)| known.eq_ignore_ascii_case(name)).map(|(_, rule)| *rule)
}

/// `"07:00"` → `(7, 0)`.
pub fn parse_time(value: &str) -> Option<(i8, i8)> {
    let (hours, minutes) = value.trim().split_once(':')?;
    let (hours, minutes): (i8, i8) = (hours.parse().ok()?, minutes.parse().ok()?);
    ((0..24).contains(&hours) && (0..60).contains(&minutes) && value.trim().len() == 5).then_some((hours, minutes))
}

/// Every shift end from two days before `now` to the day after, oldest first.
pub fn boundaries(now: Timestamp, ends: &[(i8, i8)], zone: &TimeZone) -> Vec<Timestamp> {
    let today: Date = now.to_zoned(zone.clone()).date();
    let mut all = Vec::new();
    for offset in -2i64..=1 {
        let Ok(day) = today.checked_add(jiff::Span::new().days(offset)) else { continue };
        for (hours, minutes) in ends {
            let local = day.at(*hours, *minutes, 0, 0);
            if let Ok(zoned) = zone.to_ambiguous_zoned(local).compatible() {
                all.push(zoned.timestamp());
            }
        }
    }
    all.sort();
    all.dedup();
    all
}

/// The latest shift end at or before `now`.
pub fn last_end(now: Timestamp, ends: &[(i8, i8)], zone: &TimeZone) -> Option<Timestamp> {
    boundaries(now, ends, zone).into_iter().rfind(|end| *end <= now)
}

/// The shift being handed over at `now`: the one whose end is nearest (before or after, the coming
/// one on a tie), from the end before it. `(start, end)`.
pub fn handed_over(now: Timestamp, ends: &[(i8, i8)], zone: &TimeZone) -> Option<(Timestamp, Timestamp)> {
    let all = boundaries(now, ends, zone);
    let nearest = all.iter().enumerate().filter(|(at, _)| *at > 0).min_by_key(|(_, end)| {
        let distance = now.duration_since(**end).abs();
        (distance, **end < now)
    })?;
    Some((all[nearest.0 - 1], *nearest.1))
}

/// `"15:00"` in `zone`.
pub fn local_time(at: Timestamp, zone: &TimeZone) -> String {
    let zoned = at.to_zoned(zone.clone());
    format!("{:02}:{:02}", zoned.hour(), zoned.minute())
}

/// `"Tue 30 Sep 15:00"` in `zone`.
pub fn local_label(at: Timestamp, zone: &TimeZone) -> String {
    at.to_zoned(zone.clone()).strftime("%a %-d %b %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ends() -> Vec<(i8, i8)> {
        DEFAULT_SHIFT_ENDS.iter().map(|end| parse_time(end).unwrap()).collect()
    }

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    fn paris() -> TimeZone {
        // The table's rule, whether or not this system has a time zone database.
        TimeZone::posix(builtin("Europe/Paris").unwrap()).unwrap()
    }

    #[test]
    fn times_and_zones_are_validated() {
        assert_eq!(parse_time("07:00"), Some((7, 0)));
        assert_eq!(parse_time("23:59"), Some((23, 59)));
        for bad in ["24:00", "7:00", "07:60", "0700", "", "ab:cd"] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
        assert!(time_zone("Europe/Paris").is_some());
        assert!(time_zone("europe/paris").is_some(), "the table ignores case");
        assert!(time_zone("UTC").is_some());
        assert!(time_zone("CET-1CEST,M3.5.0,M10.5.0/3").is_some());
        assert!(time_zone("Mars/Olympus").is_none());
        assert!(time_zone("").is_none());
    }

    #[test]
    fn shift_ends_follow_the_time_zone_and_daylight_saving() {
        let zone = paris();
        // Summer (UTC+2): 07:00 Paris = 05:00 UTC.
        assert_eq!(last_end(at("2026-09-30T12:30:00Z"), &ends(), &zone), Some(at("2026-09-30T05:00:00Z")));
        assert_eq!(last_end(at("2026-09-30T13:00:00Z"), &ends(), &zone), Some(at("2026-09-30T13:00:00Z")), "15:00 Paris");
        // Winter (UTC+1).
        assert_eq!(last_end(at("2026-12-01T06:30:00Z"), &ends(), &zone), Some(at("2026-12-01T06:00:00Z")));
        // Across midnight: 01:00 Paris is after the day before's 23:00.
        assert_eq!(last_end(at("2026-09-30T23:30:00Z"), &ends(), &zone), Some(at("2026-09-30T21:00:00Z")));
        // The night the clocks go back (25 Oct 2026): still one 07:00.
        let night = boundaries(at("2026-10-25T12:00:00Z"), &ends(), &zone);
        assert!(night.contains(&at("2026-10-25T06:00:00Z")) && night.contains(&at("2026-10-24T21:00:00Z")));
        assert_eq!(local_time(at("2026-09-30T13:00:00Z"), &zone), "15:00");
        let utc = TimeZone::UTC;
        assert_eq!(last_end(at("2026-09-30T13:30:00Z"), &ends(), &utc), Some(at("2026-09-30T07:00:00Z")));
    }

    #[test]
    fn the_shift_handed_over_is_the_nearest_end() {
        let zone = paris();
        // 14:50 and 15:10 Paris: the 07:00-15:00 shift.
        let shift = (at("2026-09-30T05:00:00Z"), at("2026-09-30T13:00:00Z"));
        assert_eq!(handed_over(at("2026-09-30T12:50:00Z"), &ends(), &zone), Some(shift));
        assert_eq!(handed_over(at("2026-09-30T13:10:00Z"), &ends(), &zone), Some(shift));
        // 11:00 Paris, four hours from both ends: the coming one.
        assert_eq!(handed_over(at("2026-09-30T09:00:00Z"), &ends(), &zone), Some(shift));
        // 00:30 Paris: the 15:00-23:00 shift of the day before.
        assert_eq!(handed_over(at("2026-09-30T22:30:00Z"), &ends(), &zone), Some((at("2026-09-30T13:00:00Z"), at("2026-09-30T21:00:00Z"))));
        assert_eq!(local_label(at("2026-09-30T13:00:00Z"), &zone), "Wed 30 Sep 15:00");
    }
}
