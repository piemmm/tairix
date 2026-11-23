//! The Venus check's verdict, which its program records and the vertical's
//! guest keys on.

#![no_std]
#![deny(missing_docs)]

use tairix_log::EventId;

/// Contexts, blobs and their mappings were made against the device and let
/// go, a blob's second mapping read what its first wrote, and the driver's
/// Venus backend agreed with the software device.
pub const PASSED: EventId = EventId(4590);

/// The check failed at the step its record names.
pub const FAILED: EventId = EventId(4591);
