//! The per-LUN block-service endpoint id derivation.
//!
//! The `Run` binary receives each request on the LUN's call endpoint
//! (`call_recv`), hands it to the one shared block-service request engine
//! ([`tairix_abi::blkio::serve_request_recovering`]) with the LUN's shared data
//! window, and replies with the framed bytes (`call_reply`). The request
//! surface — validation, the fail-closed refusals, the success paths, and the
//! recovery grace window — lives in that single shared engine so every block
//! driver reuses one definition; only the usb_msd-specific endpoint-id
//! derivation lives here.

use crate::scsi::MAX_LUNS;

/// Reserved id range the per-LUN block-service endpoints are bound in
/// (`b"MSD\0"`-tagged, mirroring the HCD's URB endpoint range shape).
/// Each driver process derives one contiguous block of [`MAX_LUNS`] ids
/// from its URB endpoint grant ([`blk_block_for`]).
pub const BLK_ENDPOINT_BASE: u64 = 0x004D_5344_0000_0000;

/// Derive a driver process's block of [`MAX_LUNS`] contiguous
/// block-service endpoint ids from its URB endpoint grant: block base
/// `BLK_ENDPOINT_BASE | (grant counter × MAX_LUNS)`, LUN `n` at
/// `base + n`.
///
/// The kernel refuses to mint a second live endpoint with the grant's
/// id, so the counter in the grant's low half is unique among
/// concurrently served interfaces and the derived blocks are disjoint by
/// construction — a multi-drive enclosure's concurrently spawned driver
/// processes each create their endpoints first try, with no probing and
/// no rejected-create noise in the kernel log. Returns `None` when the
/// counter cannot be encoded inside the block id space (fail closed,
/// never a guessed or truncated id).
#[must_use]
pub fn blk_block_for(urb_endpoint: u64) -> Option<u64> {
    let stride = u64::try_from(MAX_LUNS).ok()?;
    let counter = urb_endpoint & 0xFFFF_FFFF;
    let offset = counter.checked_mul(stride)?;
    // The block (and its last LUN id) must stay inside the low 32-bit id
    // space beneath the `b"MSD\0"` tag.
    if offset + (stride - 1) > u64::from(u32::MAX) {
        return None;
    }
    Some(BLK_ENDPOINT_BASE | offset)
}

/// Publish one unit per number in `0..count` into `units`, slot `n` holding
/// unit `n`, answering how many were published.
///
/// `publish` answers a unit, `None` for one skipped, or the failure that ends
/// publication. On a failure every unit already published is retracted
/// through `retract` before it is answered, so a driver that gives up leaves
/// no node of a device it no longer serves. A number past the last slot is
/// never published.
///
/// # Errors
///
/// `publish`'s failure.
pub fn publish_units<U, E>(
    units: &mut [Option<U>],
    count: u8,
    mut publish: impl FnMut(u8) -> Result<Option<U>, E>,
    mut retract: impl FnMut(&U),
) -> Result<usize, E> {
    let mut published = 0;
    for number in 0..count {
        let slot = usize::from(number);
        if slot >= units.len() {
            break;
        }
        match publish(number) {
            Ok(Some(unit)) => {
                units[slot] = Some(unit);
                published += 1;
            }
            Ok(None) => {}
            Err(err) => {
                units.iter().flatten().for_each(&mut retract);
                return Err(err);
            }
        }
    }
    Ok(published)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_publication_retracts_every_unit_already_published() {
        // Regression: a refused endpoint, window or node partway through the
        // units returned at once, leaving the earlier units' nodes in the tree
        // for a driver that had exited.
        let mut units = [None; MAX_LUNS];
        let mut retracted = alloc::vec::Vec::new();
        let outcome = publish_units(
            &mut units,
            4,
            |number| match number {
                1 => Ok(None),
                3 => Err("no window"),
                _ => Ok(Some(number)),
            },
            |unit| retracted.push(*unit),
        );
        assert_eq!(outcome, Err("no window"));
        assert_eq!(retracted, [0, 2]);
    }

    #[test]
    fn every_unit_publishes_into_its_own_slot_and_none_past_the_last() {
        let mut units = [None; 2];
        let outcome = publish_units(
            &mut units,
            3,
            |number| Ok::<_, ()>(Some(number)),
            |_| panic!("nothing to retract"),
        );
        assert_eq!(outcome, Ok(2));
        assert_eq!(units, [Some(0), Some(1)]);
    }

    #[test]
    fn blk_blocks_derived_from_distinct_grants_never_overlap() {
        // The Pi 4 metal defect: ten concurrently spawned driver
        // processes (a multi-drive enclosure binds one per bridge) probed
        // a shared id range for a free block, and every taken probe
        // logged a kernel-side rejected `call_create`. The block is now
        // derived from the URB endpoint grant, whose counter the kernel
        // guarantees unique among live interfaces: distinct grants yield
        // disjoint MAX_LUNS-sized blocks, so every create succeeds first
        // try.
        let tag = 0x0055_5242_0000_0000u64;
        let mut previous_end = 0u64;
        for counter in [0u64, 1, 2, 9, 10, 0x1000, 0xFFF_FFFE] {
            let base = blk_block_for(tag | counter).expect("encodable counter derives");
            assert_eq!(base & 0xFFFF_FFFF_0000_0000, BLK_ENDPOINT_BASE);
            assert!(
                counter == 0 || base >= previous_end,
                "blocks of increasing counters never overlap"
            );
            previous_end = base + u64::try_from(MAX_LUNS).expect("small");
        }
    }

    #[test]
    fn an_unencodable_grant_counter_fails_the_derivation_closed() {
        // A counter whose block would spill past the 32-bit id space
        // under the b"MSD\0" tag is refused, never wrapped or truncated
        // into a colliding id.
        assert_eq!(blk_block_for(0x0055_5242_FFFF_FFFF), None);
        assert_eq!(blk_block_for(0x0055_5242_1000_0000), None);
    }
}
