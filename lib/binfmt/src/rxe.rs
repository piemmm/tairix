//! Inspection view of the TAIRiX `rxe` load image.
//!
//! The view decodes *through* the `lib/abi` wire types —
//! [`LoadImage::parse_for_inspection`] checks every structural load-time
//! invariant, with the CFI tag reported rather than compared, since an
//! inspection tool has no kernel interface hash. There is no second copy of
//! the wire format here, so the view a user reads and the image the kernel
//! loads can never disagree.

use tairix_abi::{LoadHeader, LoadImage, Segment, LOAD_FLAG_PIE};

pub use tairix_abi::RxeError;

/// A decoded, structurally validated `rxe` load image plus its raw header.
///
/// Holding an `RxeView` is proof the image satisfies every structural
/// load-time invariant (W^X, page alignment, sorted non-overlapping
/// segments, entry inside an executable segment, PIE). The CFI tag is
/// *reported* through [`RxeView::header`], never compared — admitting a
/// binary to execution is the kernel loader's job, not this view's.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RxeView {
    header: LoadHeader,
    image: LoadImage,
}

impl RxeView {
    /// Decode `bytes` as an `rxe` load image for inspection.
    ///
    /// # Errors
    ///
    /// The [`RxeError`] the shared `lib/abi` validator reports for the
    /// first violated invariant; the input is rejected whole.
    pub fn parse(bytes: &[u8]) -> Result<Self, RxeError> {
        let image = LoadImage::parse_for_inspection(bytes)?;
        let header = LoadHeader::from_bytes(bytes)?;
        Ok(Self { header, image })
    }

    /// The raw load header, including the ABI version, flag bits, and the
    /// CFI interface-hash tag the binary was linked against.
    #[must_use]
    pub fn header(&self) -> &LoadHeader {
        &self.header
    }

    /// Image-relative entry-point virtual address.
    #[must_use]
    pub fn entry(&self) -> u64 {
        self.image.entry()
    }

    /// The validated segment table, sorted by ascending virtual address.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        self.image.segments()
    }

    /// The shared-library references the image declares, in declaration
    /// order.
    pub fn needed_libraries(&self) -> impl Iterator<Item = &str> {
        self.image.needed_libraries()
    }

    /// Whether the image is position-independent.
    ///
    /// Always `true` for a successfully parsed view (a fixed-address image
    /// is refused), surfaced so a renderer states the property from the
    /// decoded header rather than assuming it.
    #[must_use]
    pub fn is_pie(&self) -> bool {
        self.header.flags & LOAD_FLAG_PIE != 0
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{RxeError, RxeView};
    use alloc::vec::Vec;
    use tairix_abi::{
        CapabilityId, LoadHeader, ManifestHeader, NeededLibrary, Segment, LOAD_FLAG_PIE,
        LOAD_MAGIC, MANIFEST_MAGIC, RXE_PAGE_SIZE,
    };

    /// Encode a minimal valid load image: one RX code segment holding the
    /// entry point, one RW data segment, one needed library.
    pub(crate) fn valid_image() -> Vec<u8> {
        let code = Segment {
            vaddr: RXE_PAGE_SIZE,
            file_offset: 0,
            file_size: 64,
            mem_size: RXE_PAGE_SIZE,
            permission: tairix_abi::RxePermission::ReadExecute,
        };
        let data = Segment {
            vaddr: RXE_PAGE_SIZE * 2,
            file_offset: 64,
            file_size: 32,
            mem_size: RXE_PAGE_SIZE,
            permission: tairix_abi::RxePermission::ReadWrite,
        };
        let needed = NeededLibrary::from_reference("/System/Libraries/libexample.so")
            .expect("valid reference");
        let header = LoadHeader {
            magic: LOAD_MAGIC,
            abi_version: tairix_abi::ABI_VERSION_CURRENT,
            flags: LOAD_FLAG_PIE,
            segment_count: 2,
            needed_count: 1,
            entry: RXE_PAGE_SIZE + 16,
            cfi_tag: [0xA5; 32],
        };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&header.to_le_bytes());
        bytes.extend_from_slice(&code.to_le_bytes());
        bytes.extend_from_slice(&data.to_le_bytes());
        bytes.extend_from_slice(&needed.to_le_bytes());
        bytes
    }

    /// Encode a manifest with the given capability ids in the body.
    pub(crate) fn manifest_with(ids: &[CapabilityId]) -> Vec<u8> {
        let count = u16::try_from(ids.len()).expect("test counts fit");
        let header = ManifestHeader {
            magic: MANIFEST_MAGIC,
            abi_version: tairix_abi::ABI_VERSION_CURRENT,
            flags: 0,
            capability_count: count,
            reserved0: 0,
            syscall_table_hash: [0x11; 32],
            signer_pubkey: [0x22; 32],
            signature: [0x33; 64],
        };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&header.to_le_bytes());
        for id in ids {
            bytes.extend_from_slice(&id.as_u16().to_le_bytes());
        }
        bytes
    }

    #[test]
    fn parses_a_valid_image_and_reports_its_properties() {
        let bytes = valid_image();
        let view = RxeView::parse(&bytes).expect("valid image");
        assert!(view.is_pie());
        assert_eq!(view.entry(), RXE_PAGE_SIZE + 16);
        assert_eq!(view.header().cfi_tag, [0xA5; 32]);
        assert_eq!(view.segments().len(), 2);
        assert!(view.segments()[0].permission.is_executable());
        assert!(view.segments()[1].permission.is_writable());
        let needed: Vec<&str> = view.needed_libraries().collect();
        assert_eq!(needed, ["/System/Libraries/libexample.so"]);
    }

    #[test]
    fn every_truncation_of_a_valid_image_fails_closed() {
        let bytes = valid_image();
        for len in 0..bytes.len() {
            assert!(
                RxeView::parse(&bytes[..len]).is_err(),
                "truncation to {len} bytes must be refused"
            );
        }
    }

    #[test]
    fn header_mutations_are_refused_with_named_errors() {
        let good = valid_image();

        let mut bad_magic = good.clone();
        bad_magic[0] ^= 0xFF;
        assert_eq!(RxeView::parse(&bad_magic), Err(RxeError::BadMagic));

        let mut bad_version = good.clone();
        bad_version[4] ^= 0xFF;
        assert_eq!(RxeView::parse(&bad_version), Err(RxeError::BadAbiVersion));

        // Clearing the PIE bit (byte 8 carries the low flag bits).
        let mut non_pie = good.clone();
        non_pie[8] &= !0x01;
        assert_eq!(
            RxeView::parse(&non_pie),
            Err(RxeError::NotPositionIndependent)
        );

        // Entry pointed outside every executable segment (bytes 16..24).
        let mut wild_entry = good;
        wild_entry[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(RxeView::parse(&wild_entry), Err(RxeError::BadEntryPoint));
    }

    #[test]
    fn single_byte_flips_never_panic() {
        let good = valid_image();
        for i in 0..good.len() {
            let mut mutated = good.clone();
            mutated[i] ^= 0x40;
            // Any outcome is fine; the decode must simply not panic and
            // a success must yield a self-consistent view.
            if let Ok(view) = RxeView::parse(&mutated) {
                assert!(!view.segments().is_empty());
            }
        }
    }
}
