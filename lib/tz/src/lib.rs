//! Civil time: the compiled zone store and everything a surface needs to show
//! a human the local time.
//!
//! Stored and transmitted time stays UTC [`Time64`](tairix_abi::time::Time64);
//! a zone is presentation, applied when a person reads a time. The store,
//! `/System/Zoneinfo/zones.tzx`, is compiled from IANA's rules by
//! `tools/tzcompile` and read here: [`ZoneInfo::parse`] validates it whole or
//! refuses it, and everything after reads it with no allocation and no
//! failure. A machine with no store reads UTC with ISO 8601 dates.
//!
//! The format is specified in `docs/src/lib/tz.md`; the design is
//! `plans/TIMEZONES.md`.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(any(test, feature = "write", feature = "rt", feature = "listing"))]
extern crate alloc;

mod clock;
pub mod layout;
#[cfg(any(test, feature = "listing"))]
pub mod listing;
#[cfg(feature = "rt")]
mod load;
mod pattern;
pub mod rule;
mod store;
#[cfg(any(test, feature = "test-support"))]
pub mod testkit;
mod week;
#[cfg(any(test, feature = "write"))]
pub mod write;
mod zone;

/// Where the store is planted.
pub const STORE_PATH: &str = "/System/Zoneinfo/zones.tzx";

pub use clock::{resolve_region, resolve_zone, CivilClock, Decided, HourCycle};
#[cfg(feature = "rt")]
pub use load::{load, LoadError};
pub use pattern::{Pattern, WIDEST};
pub use store::{is_region_code, is_zone_name, Place, Region, StoreError, ZoneInfo};
pub use week::Weekday;
pub use zone::{Disambiguation, LocalError, LocalTime, OffsetChange, Zone, ZoneOffset};
