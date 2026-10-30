//! Providers: leaf data dependencies implementing capability shapes.
//!
//! Design (AFFiNE: "mntr: Providers & Extensions Design"): providers publish
//! normalized snapshots on their own refresh cadence (default 1h); extensions
//! declare a shape + cardinality and consume merged snapshots. Shapes are
//! typed interfaces, not vendors — a screen never knows if CalDAV or Google
//! answered.
//!
//! Shape implemented today: `CalendarSource` -> sorted normalized events.
//! Provider instance = one configured connection (URL + creds). Failed
//! refresh keeps the last good snapshot; stale timestamp is surfaced.

use chrono::{Timelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::SqlitePool;
use std::str::FromStr;

pub const DEFAULT_REFRESH_SECS: i64 = 3600;

/// One normalized calendar event — the CalendarSource shape's row type.
#[derive(Debug, Clone, Serialize)]
pub struct CalendarEvent {
    pub uid: String,
    /// ISO UTC datetime, or `YYYY-MM-DD` when all_day.
    pub start: String,
    pub all_day: bool,
    pub title: String,
    pub location: String,
    pub description: String,
}

pub const MIGRATE: &str = r#"
CREATE TABLE IF NOT EXISTS providers (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  config TEXT NOT NULL DEFAULT '{}',
  refresh_secs INTEGER NOT NULL DEFAULT 3600,
  last_ok_at TEXT,
  last_error TEXT
);
CREATE TABLE IF NOT EXISTS provider_events (
  provider_id INTEGER NOT NULL,
  uid TEXT NOT NULL,
  start TEXT NOT NULL,
  all_day INTEGER NOT NULL DEFAULT 0,
  title TEXT NOT NULL,
  location TEXT NOT NULL DEFAULT '',
  description TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS extensions (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,
  name TEXT NOT NULL,
  settings TEXT NOT NULL DEFAULT '{}',
  screen_id INTEGER
);
"#;

// ---------------------------------------------------------------------------
// CalDAV provider
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CalDavConfig {
    /// Collection URL serving merged text/calendar on GET (Radicale does).
    pub url: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub pass: Option<String>,
}

pub fn la() -> chrono_tz::Tz {
    "America/Los_Angeles".parse().expect("static tz")
}

/// Fetch a CalDAV collection and return normalized events for the next
/// `window_days` days (LA wall clock). Recurrences are expanded client-side.
pub async fn fetch_caldav(cfg: &CalDavConfig, window_days: i64) -> anyhow::Result<Vec<CalendarEvent>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let send_get = |url: String| {
        let mut req = client.get(&url)
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
    let host_end = base[scheme_end..].find('/').map(|i| scheme_end + i).unwrap_or(base.len());
    format!("{}{}", &base[..host_end], href)
}

/// Whether a DTSTART falls inside the LA wall-clock window (recurrence
/// expansion already windows itself; single instances need the same filter).
fn in_window(dt: &icalendar::DatePerhapsTime, win_start: chrono::DateTime<icalendar::Tz>, win_end: chrono::DateTime<icalendar::Tz>) -> bool {
    use icalendar::{CalendarDateTime, DatePerhapsTime};
    let tz = la();
    let start: chrono::DateTime<Utc> = match dt {
        DatePerhapsTime::Date(d) => tz.from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap())
            .single().map(|l| l.to_utc()).unwrap_or_else(|| Utc.from_utc_datetime(&d.and_hms_opt(0,0,0).unwrap())),
        DatePerhapsTime::DateTime(CalendarDateTime::Utc(utc)) => *utc,
        DatePerhapsTime::DateTime(CalendarDateTime::WithTimezone { date_time, tzid }) => {
            let t: chrono_tz::Tz = match tzid.parse() { Ok(t) => t, Err(_) => tz };
            t.from_local_datetime(date_time).single()
                .map(|l| l.to_utc())
                .unwrap_or_else(|| Utc.from_utc_datetime(date_time))
        }
        DatePerhapsTime::DateTime(CalendarDateTime::Floating(naive)) => {
            tz.from_local_datetime(naive).single()
                .map(|l| l.to_utc())
                .unwrap_or_else(|| Utc.from_utc_datetime(naive))
        }
    };
    let (a, b) = (win_start.to_utc(), win_end.to_utc());
    start >= a && start < b
}

/// Parse an ICS blob into normalized events within [today, today+window)
/// in LA wall-clock terms. Public so tests can feed fixtures.
pub fn parse_ics_events(ics: &str, window_days: i64) -> anyhow::Result<Vec<CalendarEvent>> {
    use icalendar::{Component, DatePerhapsTime};
    let cal = icalendar::Calendar::from_str(ics).map_err(|e| anyhow::anyhow!("ics parse: {e}"))?;
    let tz = la();
    let today = Utc::now().with_timezone(&tz).date_naive();
    let win_end_date = today.checked_add_days(chrono::Days::new(window_days.max(1) as u64))
        .unwrap_or(today);
    // rrule bounds in LA-local terms (rrule::Tz converts from chrono_tz::Tz).
    let rtz: icalendar::Tz = tz.into();
    let win_start = rtz.from_local_datetime(&today.and_hms_opt(0,0,0).unwrap())
        .single().unwrap_or_else(|| rtz.from_utc_datetime(&today.and_hms_opt(0,0,0).unwrap()));
    let win_end = rtz.from_local_datetime(&win_end_date.and_hms_opt(0,0,0).unwrap())
        .single().unwrap_or_else(|| rtz.from_utc_datetime(&win_end_date.and_hms_opt(0,0,0).unwrap()));

    let mut events = Vec::new();
    for event in cal.calendar_events() {
        let text = |name: &str| event
            .property_value(name)
            .unwrap_or_default()
            .to_string();
        // All occurrence start times: recurrence-expanded when RRULE is
        // present, else the single DTSTART.
        let starts: Vec<DatePerhapsTime> = match event.property_value("RRULE").and_then(|_| event.get_recurrence().ok()) {
            Some(rrset) => {
                let res = rrset.after(win_start).before(win_end).all(500);
                if res.dates.is_empty() {
                    event.get_start().into_iter()
                        .filter(|s| in_window(s, win_start, win_end))
                        .collect()
                } else {
                    // Recurrence dates come back in the rule's own tz; map
                    // each to its LA-local date/time.
                    res.dates.iter().map(|d| {
                        let local = d.with_timezone(&tz).naive_local();
                        DatePerhapsTime::DateTime(icalendar::CalendarDateTime::from(local))
                    }).collect()
                }
            }
            None => event.get_start()
                .into_iter()
                .filter(|s| in_window(s, win_start, win_end))
                .collect(),
        };
        for dt in starts {
            let (start_str, all_day) = match dt {
                DatePerhapsTime::Date(d) => (d.format("%Y-%m-%d").to_string(), true),
                DatePerhapsTime::DateTime(icalendar::CalendarDateTime::Utc(utc)) =>
                    (utc.to_rfc3339(), false),
                DatePerhapsTime::DateTime(icalendar::CalendarDateTime::WithTimezone { date_time, tzid }) => {
                    let utc = tzid.parse::<chrono_tz::Tz>()
                        .map(|t| t.from_local_datetime(&date_time).single()
                            .map(|l| l.to_utc())
                            .unwrap_or_else(|| Utc.from_utc_datetime(&date_time)))
                        .unwrap_or_else(|_| tz.from_local_datetime(&date_time)
                            .single().map(|l| l.to_utc())
                            .unwrap_or_else(|| Utc.from_utc_datetime(&date_time)));
                    (utc.to_rfc3339(), false)
                }
                DatePerhapsTime::DateTime(icalendar::CalendarDateTime::Floating(naive)) => {
                    // Floating time: treat as LA wall clock.
                    let utc = tz.from_local_datetime(&naive).single()
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

// ---------------------------------------------------------------------------
// Snapshot store + refresh loop
// ---------------------------------------------------------------------------

/// Replace one provider's snapshot atomically-ish (delete+insert).
pub async fn store_snapshot(pool: &SqlitePool, provider_id: i64, events: &[CalendarEvent]) -> anyhow::Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("DELETE FROM provider_events WHERE provider_id = ?").bind(provider_id)
        .execute(&mut *tx).await?;
    for e in events {
        sqlx::query("INSERT INTO provider_events (provider_id, uid, start, all_day, title, location, description) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(provider_id).bind(&e.uid).bind(&e.start).bind(e.all_day as i64)
            .bind(&e.title).bind(&e.location).bind(&e.description)
            .execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Refresh one provider by id: fetch, store snapshot, record outcome.
/// Failure keeps the last good snapshot (design: no backoff machinery).
pub async fn refresh_provider(pool: &SqlitePool, id: i64) -> anyhow::Result<usize> {
    let row = sqlx::query("SELECT kind, config FROM providers WHERE id = ?").bind(id)
        .fetch_optional(pool).await?
        .ok_or_else(|| anyhow::anyhow!("provider {id} not found"))?;
    let kind: String = row.get(0);
    let config: String = row.get(1);
    let result: anyhow::Result<usize> = match kind.as_str() {
        "caldav" => {
            let cfg: CalDavConfig = serde_json::from_str(&config)?;
            let events = fetch_caldav(&cfg, 8).await?;
            let n = events.len();
            store_snapshot(pool, id, &events).await?;
            Ok(n)
        }
        other => Err(anyhow::anyhow!("unknown provider kind {other}")),
    };
    match result {
        Ok(n) => {
            sqlx::query("UPDATE providers SET last_ok_at = datetime('now'), last_error = NULL WHERE id = ?")
                .bind(id).execute(pool).await?;
            tracing::info!(provider = id, events = n, "provider refreshed");
            Ok(n)
        }
        Err(e) => {
            sqlx::query("UPDATE providers SET last_error = ? WHERE id = ?")
                .bind(e.to_string()).bind(id).execute(pool).await?;
            Err(e)
        }
    }
}

/// Background provider loop: every `tick`, refresh providers whose interval
/// elapsed, then rebuild extension screens if anything changed.
pub fn spawn_provider_loop(pool: SqlitePool, cache: crate::cache::BmpCache, tick: std::time::Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut t = tokio::time::interval(tick);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            t.tick().await;
            let rows = sqlx::query("SELECT id, refresh_secs, last_ok_at FROM providers").fetch_all(&pool).await;
            let Ok(rows) = rows else { continue };
            let mut changed = false;
            for row in rows {
                let id: i64 = row.get(0);
                let secs: i64 = row.get(1);
                let last: Option<String> = row.get(2);
                let due = match last {
                    None => true,
                    Some(t) => chrono::NaiveDateTime::parse_from_str(&t, "%Y-%m-%d %H:%M:%S")
                        .map(|t| (Utc::now().naive_utc() - t).num_seconds() >= secs)
                        .unwrap_or(true),
                };
                if due {
                    match refresh_provider(&pool, id).await {
                        Ok(_) => changed = true,
                        Err(e) => tracing::warn!(%e, provider = id, "provider refresh failed"),
                    }
                }
            }
            if changed {
                if let Err(e) = crate::extension::rebuild_extensions(&pool, &cache).await {
                    tracing::warn!(%e, "extension rebuild failed");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ICS: &str = r#"BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//test//EN
BEGIN:VEVENT
UID:plain-1
DTSTART:20261029T170000Z
DTEND:20261029T180000Z
SUMMARY:Timed event
LOCATION:Office
END:VEVENT
BEGIN:VEVENT
UID:allday-1
DTSTART;VALUE=DATE:20261029
DTEND;VALUE=DATE:20261030
SUMMARY:All day thing
END:VEVENT
BEGIN:VEVENT
UID:recur-1
DTSTART;TZID=America/Los_Angeles:20261026T100000
RRULE:FREQ=WEEKLY;BYDAY=MO
SUMMARY:Monday standup
END:VEVENT
BEGIN:VEVENT
UID:old-1
DTSTART:20200101T170000Z
SUMMARY:Ancient event
END:VEVENT
END:VCALENDAR
"#;

    #[test]
    fn parses_and_windows_events() {
        // Window anchored on "today" must include the recurring Mondays from
        // late Oct 2026 forward but drop the 2020 event. Use a fixed parser
        // call: window large enough to cover the fixture regardless of run date.
        let evs = parse_ics_events(ICS, 365 * 6).unwrap();
        let uids: Vec<&str> = evs.iter().map(|e| e.uid.as_str()).collect();
        assert!(uids.contains(&"plain-1"), "timed event missing: {uids:?}");
        assert!(uids.contains(&"allday-1"), "all-day event missing");
        assert!(uids.contains(&"recur-1"), "recurring event missing");
        assert!(!uids.contains(&"old-1"), "out-of-window event leaked");
        let all_day = evs.iter().find(|e| e.uid == "allday-1").unwrap();
        assert!(all_day.all_day);
        assert_eq!(all_day.start, "2026-10-29");
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
        let evs = parse_ics_events(ICS, 365 * 6).unwrap();
        let recur: Vec<_> = evs.iter().filter(|e| e.uid == "recur-1").collect();
        assert!(recur.len() >= 4, "expected several Monday occurrences, got {}", recur.len());
        // Each occurrence is 10:00 LA = 17:00Z (PDT) or 18:00Z (PST).
        for e in &recur {
            assert!(e.start.ends_with("+00:00"), "stored as UTC rfc3339: {}", e.start);
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
        assert_eq!(parse_propfind_hrefs(xml), vec!["/caldav-service/family/1.ics", "/caldav-service/family/2.ics"]);
    }

    #[test]
    fn join_url_absolute_paths() {
        assert_eq!(join_url("http://h:5232/caldav-service/family/", "/x/y.ics"), "http://h:5232/x/y.ics");
        assert_eq!(join_url("http://h:5232/a/b/", "http://o/z"), "http://o/z");
    }
}
