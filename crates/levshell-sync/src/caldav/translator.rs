//! iCalendar → unified `Event` translator.
//!
//! Parses an `.ics` payload via the `icalendar` crate, walks every
//! `VEVENT`, and produces a [`TranslatedEvent`] carrying the fields
//! our `events` table needs. Adapter code then converts those into
//! `NewEvent` / `EventPatch`.
//!
//! ## Simplifications (v1)
//!
//! - **Recurrence is opaque.** `RRULE` is stored verbatim in
//!   `Event.recurrence` with no expansion. Downstream consumers that
//!   want occurrence lists can parse it themselves (e.g. the `rrule`
//!   crate). Matches spec §2.5 / §5.2 — the adapter is a sync plane,
//!   not a scheduler.
//! - **All-day events** (`DATE` form of DTSTART) are represented as
//!   start-of-day / end-of-day UTC. Time zones on all-day events
//!   aren't meaningful anyway.
//! - **Floating times** (RFC 5545 FORM #1) are assumed UTC. A
//!   correct implementation would apply the user's local TZ; we
//!   document the shortcut and move on.
//! - **Zoned times** (FORM #3 with `TZID`) currently take the naive
//!   local part as UTC — the `icalendar` crate doesn't resolve
//!   `VTIMEZONE` blocks. For the common case of servers that emit
//!   UTC (`20260416T120000Z`, FORM #2), this path is exact.
//! - **VTODO / VJOURNAL** components are dropped. The adapter
//!   writes events only.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use icalendar::{
    Calendar, CalendarDateTime, Component, DatePerhapsTime, Event as IcalEvent, EventLike,
};
use std::str::FromStr;
use thiserror::Error;

/// Fields the adapter needs per event. Timestamps are UTC.
#[derive(Debug, Clone, PartialEq)]
pub struct TranslatedEvent {
    pub uid: String,
    pub summary: String,
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub url: Option<String>,
    pub recurrence: Option<String>,
}

#[derive(Debug, Error)]
pub enum TranslateError {
    #[error("failed to parse iCalendar body: {0}")]
    Parse(String),
}

/// Parse an ICS payload and return every non-empty VEVENT we can
/// translate. VEVENTs without a UID or DTSTART are dropped (both are
/// RFC-5545 `MUST`s — anything missing them is malformed).
///
/// Errors from the parser itself surface as
/// [`TranslateError::Parse`]; per-event extraction failures are
/// logged and skipped so one malformed event can't hide its
/// siblings.
pub fn translate(ics: &str) -> Result<Vec<TranslatedEvent>, TranslateError> {
    let calendar = Calendar::from_str(ics).map_err(|e| TranslateError::Parse(e.to_string()))?;
    let mut out = Vec::new();
    for ical in calendar.events() {
        match extract_event(ical) {
            Some(ev) => out.push(ev),
            None => {
                tracing::warn!(
                    uid = ?ical.get_uid(),
                    "caldav: skipping VEVENT missing UID or DTSTART"
                );
            }
        }
    }
    Ok(out)
}

fn extract_event(ical: &IcalEvent) -> Option<TranslatedEvent> {
    let uid = ical.get_uid()?.to_string();
    let start_at = resolve_datetime(ical.get_start()?);
    let end_at = match ical.get_end() {
        Some(raw) => resolve_datetime(raw),
        // RFC 5545 §3.6.1: missing DTEND → treat as
        // DTSTART+0 for DATE-TIME, DTSTART+1d for DATE.
        None => fallback_end(&ical.get_start()?, start_at),
    };

    Some(TranslatedEvent {
        uid,
        summary: ical.get_summary().unwrap_or("").to_string(),
        start_at,
        end_at,
        location: ical.get_location().map(str::to_string),
        description: ical.get_description().map(str::to_string),
        url: ical.get_url().map(str::to_string),
        // RRULE isn't in the typed getter surface; grab it via the
        // generic property_value helper so we don't need the
        // `recurrence` feature flag.
        recurrence: ical.property_value("RRULE").map(str::to_string),
    })
}

fn resolve_datetime(raw: DatePerhapsTime) -> DateTime<Utc> {
    match raw {
        DatePerhapsTime::DateTime(CalendarDateTime::Utc(dt)) => dt,
        DatePerhapsTime::DateTime(CalendarDateTime::Floating(naive)) => {
            // Floating time: spec says "attendee's current TZ". v1
            // treats as UTC and documents the limitation. Matches
            // what most bar widgets care about (today/tomorrow).
            naive_to_utc(naive)
        }
        DatePerhapsTime::DateTime(CalendarDateTime::WithTimezone { date_time, tzid: _ }) => {
            // Zoned without VTIMEZONE resolution — treat naive part
            // as UTC. See module docs for the caveat.
            naive_to_utc(date_time)
        }
        DatePerhapsTime::Date(date) => date_to_utc(date),
    }
}

fn fallback_end(start_raw: &DatePerhapsTime, start_at: DateTime<Utc>) -> DateTime<Utc> {
    match start_raw {
        DatePerhapsTime::Date(_) => start_at + Duration::days(1),
        _ => start_at,
    }
}

fn naive_to_utc(naive: NaiveDateTime) -> DateTime<Utc> {
    Utc.from_utc_datetime(&naive)
}

fn date_to_utc(date: NaiveDate) -> DateTime<Utc> {
    let naive = date.and_hms_opt(0, 0, 0).unwrap_or_default();
    Utc.from_utc_datetime(&naive)
}

#[cfg(test)]
mod tests {
    use super::*;

    const UTC_EVENT: &str = "BEGIN:VCALENDAR\r
VERSION:2.0\r
PRODID:-//test//EN\r
BEGIN:VEVENT\r
UID:abc-123@example\r
DTSTAMP:20260416T120000Z\r
DTSTART:20260420T090000Z\r
DTEND:20260420T100000Z\r
SUMMARY:NeurIPS check-in\r
LOCATION:Zoom\r
DESCRIPTION:Joint session.\r
URL:https://example/meet\r
RRULE:FREQ=WEEKLY;BYDAY=MO\r
END:VEVENT\r
END:VCALENDAR\r
";

    #[test]
    fn parses_realistic_utc_event() {
        let events = translate(UTC_EVENT).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.uid, "abc-123@example");
        assert_eq!(e.summary, "NeurIPS check-in");
        assert_eq!(e.location.as_deref(), Some("Zoom"));
        assert_eq!(e.description.as_deref(), Some("Joint session."));
        assert_eq!(e.url.as_deref(), Some("https://example/meet"));
        assert_eq!(e.recurrence.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO"));
        assert_eq!(
            e.start_at,
            Utc.with_ymd_and_hms(2026, 4, 20, 9, 0, 0).unwrap()
        );
        assert_eq!(
            e.end_at,
            Utc.with_ymd_and_hms(2026, 4, 20, 10, 0, 0).unwrap()
        );
    }

    #[test]
    fn all_day_event_starts_at_midnight_spans_one_day() {
        let ics = "BEGIN:VCALENDAR\r
VERSION:2.0\r
BEGIN:VEVENT\r
UID:alldaty@example\r
DTSTAMP:20260416T120000Z\r
DTSTART;VALUE=DATE:20260501\r
DTEND;VALUE=DATE:20260502\r
SUMMARY:Vacation\r
END:VEVENT\r
END:VCALENDAR\r
";
        let events = translate(ics).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(
            e.start_at,
            Utc.with_ymd_and_hms(2026, 5, 1, 0, 0, 0).unwrap()
        );
        assert_eq!(
            e.end_at,
            Utc.with_ymd_and_hms(2026, 5, 2, 0, 0, 0).unwrap()
        );
    }

    #[test]
    fn missing_dtend_for_date_time_falls_back_to_start() {
        let ics = "BEGIN:VCALENDAR\r
VERSION:2.0\r
BEGIN:VEVENT\r
UID:dtend-missing@example\r
DTSTAMP:20260416T120000Z\r
DTSTART:20260420T090000Z\r
SUMMARY:Instant\r
END:VEVENT\r
END:VCALENDAR\r
";
        let events = translate(ics).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].start_at, events[0].end_at);
    }

    #[test]
    fn missing_dtend_for_date_falls_back_to_start_plus_1d() {
        let ics = "BEGIN:VCALENDAR\r
VERSION:2.0\r
BEGIN:VEVENT\r
UID:dtend-missing-allday@example\r
DTSTAMP:20260416T120000Z\r
DTSTART;VALUE=DATE:20260501\r
SUMMARY:Holiday\r
END:VEVENT\r
END:VCALENDAR\r
";
        let events = translate(ics).unwrap();
        assert_eq!(events.len(), 1);
        let e = &events[0];
        assert_eq!(e.end_at - e.start_at, Duration::days(1));
    }

    #[test]
    fn event_without_uid_is_dropped() {
        let ics = "BEGIN:VCALENDAR\r
VERSION:2.0\r
BEGIN:VEVENT\r
DTSTAMP:20260416T120000Z\r
DTSTART:20260420T090000Z\r
SUMMARY:Orphan\r
END:VEVENT\r
END:VCALENDAR\r
";
        let events = translate(ics).unwrap();
        assert!(events.is_empty(), "VEVENT without UID dropped per RFC");
    }

    #[test]
    fn multiple_events_in_one_calendar_all_translate() {
        let ics = "BEGIN:VCALENDAR\r
VERSION:2.0\r
BEGIN:VEVENT\r
UID:a@x\r
DTSTAMP:20260416T120000Z\r
DTSTART:20260420T090000Z\r
DTEND:20260420T100000Z\r
SUMMARY:First\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:b@x\r
DTSTAMP:20260416T120000Z\r
DTSTART:20260420T110000Z\r
DTEND:20260420T120000Z\r
SUMMARY:Second\r
END:VEVENT\r
END:VCALENDAR\r
";
        let events = translate(ics).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].uid, "a@x");
        assert_eq!(events[1].uid, "b@x");
    }

    #[test]
    fn vtodo_is_ignored() {
        let ics = "BEGIN:VCALENDAR\r
VERSION:2.0\r
BEGIN:VTODO\r
UID:todo-1\r
DTSTAMP:20260416T120000Z\r
SUMMARY:Write report\r
END:VTODO\r
BEGIN:VEVENT\r
UID:real@x\r
DTSTAMP:20260416T120000Z\r
DTSTART:20260420T090000Z\r
DTEND:20260420T100000Z\r
SUMMARY:Meeting\r
END:VEVENT\r
END:VCALENDAR\r
";
        let events = translate(ics).unwrap();
        assert_eq!(events.len(), 1, "VTODO filtered out");
        assert_eq!(events[0].uid, "real@x");
    }

    #[test]
    fn malformed_body_surfaces_parse_error() {
        let err = translate("not a calendar").unwrap_err();
        assert!(matches!(err, TranslateError::Parse(_)));
    }
}
