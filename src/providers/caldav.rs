// SPDX-License-Identifier: AGPL-3.0-only
//! CalDAV provider: one configured calendar collection (Radicale, Nextcloud,
//! Fastmail published feeds, …). Implements the CalendarSource shape.
//!
//! Fetch strategy: plain GET with Depth:1 — Radicale answers with merged
//! text/calendar. Servers that answer PROPFIND-style XML instead get the
//! fallback: enumerate item hrefs and GET each.

use super::{CalendarEvent, Provider, Snapshot};
use chrono::{TimeZone, Utc};
use serde::Deserialize;
use std::str::FromStr;

pub struct CalDav;

#[derive(Debug, Deserialize)]
pub struct CalDavConfig {
    /// Collection URL serving merged text/calendar on GET (Radicale does).
    pub url: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub pass: Option<String>,
}

/// Snapshot window: 8 days covers any extension's `days` setting with margin.
const WINDOW_DAYS: i64 = 8;

impl Provider for CalDav {
    fn kind(&self) -> &'static str {
        "caldav"
    }

    fn shape(&self) -> &'static str {
        "calendar"
    }

    fn validate(&self, config: &serde_json::Value) -> anyhow::Result<()> {
        let cfg: CalDavConfig = serde_json::from_value(config.clone())
            .map_err(|e| anyhow::anyhow!("invalid caldav config: {e}"))?;
        if !cfg.url.starts_with("http://") && !cfg.url.starts_with("https://") {
            anyhow::bail!("config.url must be an http(s) URL");
        }
        Ok(())
    }

    fn fetch(
        &self,
        config: serde_json::Value,
    ) -> Pin<Box<dyn std::future::Future<Output = anyhow::Result<Snapshot>> + Send>> {
        Box::pin(async move {
            let cfg: CalDavConfig = serde_json::from_value(config)?;
            Ok(Snapshot::Calendar(fetch(&cfg, WINDOW_DAYS).await?))
        })
    }
}

use std::pin::Pin;

/// Fetch a CalDAV collection and return normalized events for the next
/// `window_days` days (LA wall clock). Recurrences are expanded client-side.
pub async fn fetch(cfg: &CalDavConfig, window_days: i64) -> anyhow::Result<Vec<CalendarEvent>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let send_get = |url: String| {
        let mut req = client
            .get(&url)
            .header("Accept", "text/calendar")
            .header("Depth", "1");
        if let (Some(u), Some(p)) = (&cfg.user, &cfg.pass) {
            req = req.basic_auth(u, Some(p));
        }
        req.send()
    };
    let resp = send_get(cfg.url.clone()).await?.error_for_status()?;
    let body = resp.text().await?;
    // Radicale answers GET-with-Depth:1 with merged ICS. Servers that answer
    // XML instead get the fallback: enumerate item hrefs, GET each.
    let ics = if body.trim_start().starts_with("BEGIN:VCALENDAR") {
        body
    } else {
        let mut joined = String::from("BEGIN:VCALENDAR\r\n");
        for href in parse_propfind_hrefs(&body) {
            let url = join_url(&cfg.url, &href);
            let item = send_get(url).await?.error_for_status()?.text().await?;
            joined.push_str(&item);
            joined.push_str("\r\n");
        }
        joined.push_str("END:VCALENDAR\r\n");
        joined
    };
    parse_ics_events(&ics, window_days)
}

/// Pull `<D:href>` values out of a PROPFIND response (case-insensitive,
/// no XML dep — hrefs are plain text nodes).
fn parse_propfind_hrefs(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let lower = xml.to_ascii_lowercase();
    let mut from = 0;
    // Opening tag may carry attributes (`<d:href xmlns=...>`), so match the
    // prefix and skip to the closing '>'.
    while let Some(i) = lower[from..].find("<d:href") {
        let tag_end = match lower[i + from..].find('>') {
            Some(e) => i + from + e + 1,
            None => break,
        };
        if let Some(j) = lower[tag_end..].find("</d:href>") {
            out.push(xml[tag_end..tag_end + j].to_string());
            from = tag_end + j;
        } else {
            break;
        }
    }
    out
}

fn join_url(base: &str, href: &str) -> String {
    if href.starts_with("http") {
        return href.to_string();
    }
    // href is server-absolute (/base/coll/file.ics); base is a full URL.
    let scheme_end = base.find("://").map(|i| i + 3).unwrap_or(0);
    let host_end = base[scheme_end..]
        .find('/')
        .map(|i| scheme_end + i)
        .unwrap_or(base.len());
    format!("{}{}", &base[..host_end], href)
}

/// Whether a DTSTART falls inside the LA wall-clock window (recurrence
/// expansion already windows itself; single instances need the same filter).
fn in_window(
    dt: &icalendar::DatePerhapsTime,
    win_start: chrono::DateTime<icalendar::Tz>,
    win_end: chrono::DateTime<icalendar::Tz>,
) -> bool {
    use icalendar::{CalendarDateTime, DatePerhapsTime};
    let tz = super::la();
    let start: chrono::DateTime<Utc> = match dt {
        DatePerhapsTime::Date(d) => tz
            .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
            .single()
            .map(|l| l.to_utc())
            .unwrap_or_else(|| Utc.from_utc_datetime(&d.and_hms_opt(0, 0, 0).unwrap())),
        DatePerhapsTime::DateTime(CalendarDateTime::Utc(utc)) => *utc,
        DatePerhapsTime::DateTime(CalendarDateTime::WithTimezone { date_time, tzid }) => {
            let t: chrono_tz::Tz = match tzid.parse() {
                Ok(t) => t,
                Err(_) => tz,
            };
            t.from_local_datetime(date_time)
                .single()
                .map(|l| l.to_utc())
                .unwrap_or_else(|| Utc.from_utc_datetime(date_time))
        }
        DatePerhapsTime::DateTime(CalendarDateTime::Floating(naive)) => tz
            .from_local_datetime(naive)
            .single()
            .map(|l| l.to_utc())
            .unwrap_or_else(|| Utc.from_utc_datetime(naive)),
    };
    let (a, b) = (win_start.to_utc(), win_end.to_utc());
    start >= a && start < b
}

/// Parse an ICS blob into normalized events within [today, today+window)
/// in LA wall-clock terms. Public so tests can feed fixtures.
pub fn parse_ics_events(ics: &str, window_days: i64) -> anyhow::Result<Vec<CalendarEvent>> {
    use icalendar::{Component, DatePerhapsTime};
    let cal = icalendar::Calendar::from_str(ics).map_err(|e| anyhow::anyhow!("ics parse: {e}"))?;
    let tz = super::la();
    let today = Utc::now().with_timezone(&tz).date_naive();
    let win_end_date = today
        .checked_add_days(chrono::Days::new(window_days.max(1) as u64))
        .unwrap_or(today);
    // rrule bounds in LA-local terms (rrule::Tz converts from chrono_tz::Tz).
    let rtz: icalendar::Tz = tz.into();
    let win_start = rtz
        .from_local_datetime(&today.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .unwrap_or_else(|| rtz.from_utc_datetime(&today.and_hms_opt(0, 0, 0).unwrap()));
    let win_end = rtz
        .from_local_datetime(&win_end_date.and_hms_opt(0, 0, 0).unwrap())
        .single()
        .unwrap_or_else(|| rtz.from_utc_datetime(&win_end_date.and_hms_opt(0, 0, 0).unwrap()));

    let mut events = Vec::new();
    for event in cal.calendar_events() {
        let text = |name: &str| event.property_value(name).unwrap_or_default().to_string();
        // All occurrence start times: recurrence-expanded when RRULE is
        // present, else the single DTSTART.
        let starts: Vec<DatePerhapsTime> = match event
            .property_value("RRULE")
            .and_then(|_| event.get_recurrence().ok())
        {
            Some(rrset) => {
                let res = rrset.after(win_start).before(win_end).all(500);
                if res.dates.is_empty() {
                    event
                        .get_start()
                        .into_iter()
                        .filter(|s| in_window(s, win_start, win_end))
                        .collect()
                } else {
                    // Recurrence dates come back in the rule's own tz; map
                    // each to its LA-local date/time.
                    res.dates
                        .iter()
                        .map(|d| {
                            let local = d.with_timezone(&tz).naive_local();
                            DatePerhapsTime::DateTime(icalendar::CalendarDateTime::from(local))
                        })
                        .collect()
                }
            }
            None => event
                .get_start()
                .into_iter()
                .filter(|s| in_window(s, win_start, win_end))
                .collect(),
        };
        for dt in starts {
            let (start_str, all_day) = match dt {
                DatePerhapsTime::Date(d) => (d.format("%Y-%m-%d").to_string(), true),
                DatePerhapsTime::DateTime(icalendar::CalendarDateTime::Utc(utc)) => {
                    (utc.to_rfc3339(), false)
                }
                DatePerhapsTime::DateTime(icalendar::CalendarDateTime::WithTimezone {
                    date_time,
                    tzid,
                }) => {
                    let utc = tzid
                        .parse::<chrono_tz::Tz>()
                        .map(|t| {
                            t.from_local_datetime(&date_time)
                                .single()
                                .map(|l| l.to_utc())
                                .unwrap_or_else(|| Utc.from_utc_datetime(&date_time))
                        })
                        .unwrap_or_else(|_| {
                            tz.from_local_datetime(&date_time)
                                .single()
                                .map(|l| l.to_utc())
                                .unwrap_or_else(|| Utc.from_utc_datetime(&date_time))
                        });
                    (utc.to_rfc3339(), false)
                }
                DatePerhapsTime::DateTime(icalendar::CalendarDateTime::Floating(naive)) => {
                    // Floating time: treat as LA wall clock.
                    let utc = tz
                        .from_local_datetime(&naive)
                        .single()
                        .map(|l| l.to_utc())
                        .unwrap_or_else(|| Utc.from_utc_datetime(&naive));
                    (utc.to_rfc3339(), false)
                }
            };
            events.push(CalendarEvent {
                uid: text("UID"),
                start: start_str,
                all_day,
                title: text("SUMMARY"),
                location: text("LOCATION"),
                description: text("DESCRIPTION"),
            });
        }
    }
    events.sort_by(|a, b| (&a.start, &a.title).cmp(&(&b.start, &b.title)));
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture dates are relative to LA "today": the window starts at today,
    /// so hardcoded dates would silently age out (they did, on 2026-10-30).
    fn fixture_ics() -> (String, String) {
        let today = chrono::Utc::now()
            .with_timezone(&super::super::la())
            .date_naive();
        let day = |d: chrono::NaiveDate| d.format("%Y%m%d").to_string();
        let ics = format!(
            r#"BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//test//EN
BEGIN:VEVENT
UID:plain-1
DTSTART;TZID=America/Los_Angeles:{t}T100000
DTEND;TZID=America/Los_Angeles:{t}T110000
SUMMARY:Timed event
LOCATION:Office
END:VEVENT
BEGIN:VEVENT
UID:allday-1
DTSTART;VALUE=DATE:{t}
DTEND;VALUE=DATE:{n}
SUMMARY:All day thing
END:VEVENT
BEGIN:VEVENT
UID:recur-1
DTSTART;TZID=America/Los_Angeles:20260105T100000
RRULE:FREQ=WEEKLY;BYDAY=MO
SUMMARY:Monday standup
END:VEVENT
BEGIN:VEVENT
UID:old-1
DTSTART:20200101T170000Z
SUMMARY:Ancient event
END:VEVENT
END:VCALENDAR
"#,
            t = day(today),
            n = day(today + chrono::Duration::days(1))
        );
        (ics, today.format("%Y-%m-%d").to_string())
    }

    #[test]
    fn parses_and_windows_events() {
        // Window anchored on "today": in-window fixtures must survive, the
        // 2020 event must not. Window large enough to cover recurrences.
        let (ics, today) = fixture_ics();
        let evs = parse_ics_events(&ics, 365 * 6).unwrap();
        let uids: Vec<&str> = evs.iter().map(|e| e.uid.as_str()).collect();
        assert!(uids.contains(&"plain-1"), "timed event missing: {uids:?}");
        assert!(uids.contains(&"allday-1"), "all-day event missing");
        assert!(uids.contains(&"recur-1"), "recurring event missing");
        assert!(!uids.contains(&"old-1"), "out-of-window event leaked");
        let all_day = evs.iter().find(|e| e.uid == "allday-1").unwrap();
        assert!(all_day.all_day);
        assert_eq!(all_day.start, today);
        let timed = evs.iter().find(|e| e.uid == "plain-1").unwrap();
        assert!(!timed.all_day);
        assert_eq!(timed.title, "Timed event");
        assert_eq!(timed.location, "Office");
        // Sorted by start.
        let starts: Vec<&str> = evs.iter().map(|e| e.start.as_str()).collect();
        let mut sorted = starts.clone();
        sorted.sort();
        assert_eq!(starts, sorted);
    }

    #[test]
    fn recurring_expands_multiple_occurrences() {
        let (ics, _) = fixture_ics();
        let evs = parse_ics_events(&ics, 365 * 6).unwrap();
        let recur: Vec<_> = evs.iter().filter(|e| e.uid == "recur-1").collect();
        assert!(
            recur.len() >= 4,
            "expected several Monday occurrences, got {}",
            recur.len()
        );
        // Each occurrence is 10:00 LA = 17:00Z (PDT) or 18:00Z (PST).
        for e in &recur {
            assert!(
                e.start.ends_with("+00:00"),
                "stored as UTC rfc3339: {}",
                e.start
            );
            let hour = e.start.split('T').nth(1).unwrap()[..2].to_string();
            assert!(hour == "17" || hour == "18", "unexpected UTC hour {hour}");
        }
    }

    #[test]
    fn propfind_hrefs_parsed() {
        let xml = r#"<?xml version="1.0"?><D:multistatus xmlns:D="DAV:">
<D:response><D:href>/caldav-service/family/1.ics</D:href></D:response>
<D:response><d:href xmlns:d="DAV:">/caldav-service/family/2.ics</d:href></D:response>
</D:multistatus>"#;
        assert_eq!(
            parse_propfind_hrefs(xml),
            vec![
                "/caldav-service/family/1.ics",
                "/caldav-service/family/2.ics"
            ]
        );
    }

    #[test]
    fn join_url_absolute_paths() {
        assert_eq!(
            join_url("http://h:5232/caldav-service/family/", "/x/y.ics"),
            "http://h:5232/x/y.ics"
        );
        assert_eq!(join_url("http://h:5232/a/b/", "http://o/z"), "http://o/z");
    }

    #[test]
    fn validate_rejects_bad_config() {
        assert!(
            CalDav
                .validate(&serde_json::json!({"url": "ftp://x"}))
                .is_err()
        );
        assert!(
            CalDav
                .validate(&serde_json::json!({"url": "http://h/"}))
                .is_ok()
        );
        assert!(CalDav.validate(&serde_json::json!({})).is_err());
    }
}
