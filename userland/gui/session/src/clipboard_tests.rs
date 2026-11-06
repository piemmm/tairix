//! Host tests for the session clipboard, over regions held in memory.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use tairix_abi::window_ipc::{ClipboardHeld, ClipboardKind, CLIPBOARD_MAX_BYTES};
use tairix_abi::{Errno, ProcId, PROC_ID_LEN};
use tairix_window::ClientRegion;

use super::{ClipboardService, PayloadRegion, SessionClipboard};

/// The client every region in these tests was granted by.
const CLIENT: ProcId = ProcId::from_raw([0xC1; PROC_ID_LEN]);

/// The client's region `handle`.
const fn region(handle: u64) -> ClientRegion {
    ClientRegion::of(CLIENT, handle)
}

/// Regions by handle, as the client granted them; another client's name for
/// one finds nothing, as the kernel's grantor binding answers.
#[derive(Default)]
struct Regions(BTreeMap<u64, Vec<u8>>);

impl Regions {
    fn of(&mut self, named: ClientRegion) -> Result<&mut Vec<u8>, Errno> {
        if named.grantor != CLIENT {
            return Err(Errno::NotFound);
        }
        self.0.get_mut(&named.handle).ok_or(Errno::NotFound)
    }
}

impl PayloadRegion for Regions {
    fn read(&mut self, named: ClientRegion, len: usize, into: &mut Vec<u8>) -> Result<(), Errno> {
        let from = self.of(named)?.get(..len).ok_or(Errno::LengthOutOfRange)?;
        into.extend_from_slice(from);
        Ok(())
    }

    fn write(&mut self, named: ClientRegion, from: &[u8]) -> Result<bool, Errno> {
        let region = self.of(named)?;
        let Some(to) = region.get_mut(..from.len()) else {
            return Ok(false);
        };
        to.copy_from_slice(from);
        Ok(true)
    }
}

fn clipboard(regions: &[(u64, &[u8])]) -> SessionClipboard<Regions> {
    SessionClipboard::new(Regions(
        regions
            .iter()
            .map(|(handle, bytes)| (*handle, bytes.to_vec()))
            .collect(),
    ))
}

#[test]
fn an_empty_clipboard_answers_nothing_held() {
    let mut clipboard = clipboard(&[(1, &[0; 8])]);
    assert_eq!(
        clipboard.get(region(1)),
        Ok(ClipboardHeld {
            kind: None,
            len: 0,
            copied: false
        })
    );
}

#[test]
fn a_payload_put_is_what_a_later_get_copies_out() {
    let mut clipboard = clipboard(&[(1, b"hello, world"), (2, &[0; 16])]);
    clipboard
        .put(region(1), 5, ClipboardKind::Text)
        .expect("text");
    let held = clipboard.get(region(2)).expect("copied");
    assert_eq!(
        (held.kind, held.len, held.copied),
        (Some(ClipboardKind::Text), 5, true)
    );
    assert_eq!(&clipboard.regions.0[&2][..5], b"hello");
}

#[test]
fn a_region_too_short_is_untouched_and_told_the_length() {
    let mut clipboard = clipboard(&[(1, b"a longer payload"), (2, &[7; 4])]);
    clipboard
        .put(region(1), 16, ClipboardKind::Octets)
        .expect("octets");
    let held = clipboard.get(region(2)).expect("answered");
    assert_eq!((held.len, held.copied), (16, false));
    assert_eq!(clipboard.regions.0[&2], [7; 4]);
}

#[test]
fn text_that_is_not_utf8_is_refused_and_the_old_payload_stays() {
    let mut clipboard = clipboard(&[(1, b"kept"), (2, b"\xff\xfe"), (3, &[0; 8])]);
    clipboard
        .put(region(1), 4, ClipboardKind::Text)
        .expect("text");
    assert_eq!(
        clipboard.put(region(2), 2, ClipboardKind::Text),
        Err(Errno::OutOfRange)
    );
    let held = clipboard.get(region(3)).expect("answered");
    assert_eq!((held.kind, held.len), (Some(ClipboardKind::Text), 4));
    assert_eq!(
        &clipboard.regions.0[&3][..4],
        b"kept",
        "the old payload stays"
    );
    assert_eq!(
        clipboard.put(region(2), 2, ClipboardKind::Octets),
        Ok(()),
        "the same bytes as octets are fine"
    );
    assert_eq!(
        clipboard.get(region(3)).map(|held| held.kind),
        Ok(Some(ClipboardKind::Octets))
    );
}

#[test]
fn a_payload_past_the_bound_or_past_its_region_is_refused() {
    let mut clipboard = clipboard(&[(1, b"abc")]);
    assert_eq!(
        clipboard.put(
            region(1),
            CLIPBOARD_MAX_BYTES as u64 + 1,
            ClipboardKind::Octets
        ),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        clipboard.put(region(1), 4, ClipboardKind::Octets),
        Err(Errno::LengthOutOfRange),
        "longer than the region"
    );
    assert_eq!(
        clipboard.put(region(9), 1, ClipboardKind::Octets),
        Err(Errno::NotFound)
    );
    assert_eq!(
        clipboard.put(
            ClientRegion::of(ProcId::from_raw([0x0E; PROC_ID_LEN]), 1),
            1,
            ClipboardKind::Octets
        ),
        Err(Errno::NotFound),
        "another client's name for the region"
    );
}

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

#[test]
fn a_still_the_desktop_holds_is_pasted_as_a_png() {
    let mut board = clipboard(&[(1, &[0; 32])]);
    board.hold_png(PNG.to_vec()).expect("held");
    assert_eq!(
        board.get(region(1)),
        Ok(ClipboardHeld {
            kind: Some(ClipboardKind::Png),
            len: u64::try_from(PNG.len()).expect("small"),
            copied: true,
        })
    );
}

#[test]
fn a_png_past_the_bound_or_no_png_at_all_is_not_held() {
    let mut board = clipboard(&[]);
    assert_eq!(
        board.hold_png(alloc::vec![0x89; CLIPBOARD_MAX_BYTES + 1]),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(board.hold_png(b"text".to_vec()), Err(Errno::OutOfRange));
}

#[test]
fn a_put_naming_a_png_that_is_none_is_refused() {
    let mut board = clipboard(&[(1, b"not a picture"), (2, PNG)]);
    assert_eq!(
        board.put(region(1), 13, ClipboardKind::Png),
        Err(Errno::OutOfRange)
    );
    let len = u64::try_from(PNG.len()).expect("small");
    assert_eq!(board.put(region(2), len, ClipboardKind::Png), Ok(()));
}
