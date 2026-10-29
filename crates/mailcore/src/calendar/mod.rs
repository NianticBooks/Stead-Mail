//! The local calendar: calendars, events, recurrence, iCalendar import/export.
//!
//! Every event keeps the time zone its wall clock belongs to, so "every Thursday at 9 AM" stays
//! 9 AM across Daylight Saving changes. Instants are stored in UTC. The schema already carries what
//! two-way sync needs (remote id and etag, dirty flag, tombstones), so the sync phase adds no
//! migration to events.

pub mod api;
pub mod db;
pub mod ical;
pub mod model;
pub mod recur;

pub use model::{Calendar, Event, EventInput, Occurrence};
