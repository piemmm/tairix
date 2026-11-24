//! The Venus check's verdict, which its program records and the vertical's
//! guest keys on. Whether the GPU may be served is the platform's to decide
//! and the driver's never to learn, so the check records what became of it
//! and each vertical judges that against the board it booted.

#![no_std]
#![deny(missing_docs)]

use tairix_log::EventId;

/// Contexts, blobs and their mappings were made against the device and let
/// go, a blob's second mapping read what its first wrote, and the driver's
/// Venus backend agreed with the software device in all of `gpucheck`'s
/// work, its image work, its render work and its four-sample pass included.
pub const PASSED: EventId = EventId(4590);

/// The message [`PASSED`] is recorded with.
pub const PASSED_MESSAGE: &str = "venus check: contexts, blobs and mappings made and let go; \
                                  the GPU agreed with the software device, images and draws too";

// A longer message would be cut short in the log.
const _: () = assert!(PASSED_MESSAGE.len() <= tairix_log::CALLER_MESSAGE_MAX);

/// The check failed at the step its record names.
pub const FAILED: EventId = EventId(4591);

/// The device's 3D work ran, and its GPU was not served: the record names
/// the step refused and the refusal.
pub const UNSERVED: EventId = EventId(4592);
