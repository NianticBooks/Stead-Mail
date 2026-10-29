//! The local calendar: calendars, events, recurrence, iCalendar import/export.
//!
//! Every event keeps the time zone its wall clock belongs to, so "every Thursday at 9 AM" stays
//! 9 AM across Daylight Saving changes. Instants are stored in UTC. The schema already carries what
//! two-way sync needs (remote id and etag, dirty flag, tombstones), so the sync phase adds no
//! migration to events.

pub mod api;
pub mod dav;
pub mod db;
pub mod gcal;
pub mod ical;
pub mod model;
pub mod recur;
pub mod sync;
pub mod syncdb;
#[cfg(test)]
pub(crate) mod testserver;

pub use model::{Calendar, Event, EventInput, Occurrence};
