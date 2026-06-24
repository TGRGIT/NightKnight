//! # nightknight-core
//!
//! The runtime-agnostic domain model for NightKnight: glucose units, CGM/treatment
//! records, validation, trend classification, and glucose analytics. It performs no
//! I/O and depends on no runtime, so it compiles unchanged for the Cloudflare Worker
//! (wasm32) and the native container, and is exhaustively unit- and property-tested.
//!
//! Modules:
//! * [`units`] — mg/dL + mmol/L model; lets the two units mix freely in one stream.
//! * [`trend`] — the glucose trend arrows and rate-of-change classification.
//! * [`analytics`] — Time-in-Range, GMI, eA1c, variability.
//! * [`documents`] — Nightscout `entries`/`treatments`/`devicestatus`/`profile`
//!   record types and their clinical validation.

pub mod analytics;
pub mod documents;
pub mod timeutil;
pub mod trend;
pub mod units;

pub use analytics::{GlucoseReading, GlucoseSummary, TimeInRange, TirThresholds};
pub use documents::{DeviceStatus, DocumentError, Entry, Profile, Treatment};
pub use trend::Direction;
pub use units::{GlucoseUnit, GlucoseValue, UnitsError, MGDL_PER_MMOL};
