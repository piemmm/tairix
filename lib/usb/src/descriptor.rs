//! The descriptors a configuration descriptor stream concatenates (USB 2.0
//! §9.5), walked once for every reader of one, and the standard descriptor
//! vocabulary every reader shares.

/// `bDescriptorType` of a configuration descriptor (USB 2.0 §9.4 Table 9-5).
pub const DESC_TYPE_CONFIGURATION: u8 = 0x02;

/// `bDescriptorType` of an interface descriptor.
pub const DESC_TYPE_INTERFACE: u8 = 0x04;

/// `bDescriptorType` of an endpoint descriptor.
pub const DESC_TYPE_ENDPOINT: u8 = 0x05;

/// `bDescriptorType` of an interface association descriptor (USB 2.0
/// Interface Association Descriptor ECN).
pub const DESC_TYPE_INTERFACE_ASSOCIATION: u8 = 0x0B;

/// `bDescriptorType` of the `SuperSpeed` endpoint companion descriptor that
/// follows each endpoint descriptor of a `SuperSpeed` device (USB 3.2
/// §9.6.7).
pub const DESC_TYPE_SS_ENDPOINT_COMPANION: u8 = 0x30;

/// Byte length of an interface descriptor (USB 2.0 §9.6.5).
pub const INTERFACE_DESCRIPTOR_LEN: usize = 9;

/// Byte length of an endpoint descriptor (USB 2.0 §9.6.6).
pub const ENDPOINT_DESCRIPTOR_LEN: usize = 7;

/// Byte length of a `SuperSpeed` endpoint companion descriptor.
pub const SS_ENDPOINT_COMPANION_LEN: usize = 6;

/// Byte length of a configuration descriptor's own header (USB 2.0 §9.6.3).
pub const CONFIGURATION_HEADER_LEN: usize = 9;

/// What the header opening a configuration descriptor stream states.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConfigurationHeader {
    /// `bLength`: where the descriptors after the header begin.
    pub length: usize,
    /// `wTotalLength`: the bytes of the whole stream, header included.
    pub total: usize,
    /// `bConfigurationValue`.
    pub value: u8,
}

impl ConfigurationHeader {
    /// Decode the header opening `bytes`.
    ///
    /// # Errors
    ///
    /// [`Malformed`] for fewer bytes than the header, a descriptor type other
    /// than a configuration's, a `bLength` shorter than the header, or a
    /// `wTotalLength` shorter than the header it includes.
    pub fn decode(bytes: &[u8]) -> Result<Self, Malformed> {
        let header = bytes.get(..CONFIGURATION_HEADER_LEN).ok_or(Malformed)?;
        let length = usize::from(header[0]);
        let total = usize::from(u16::from_le_bytes([header[2], header[3]]));
        if header[1] != DESC_TYPE_CONFIGURATION
            || length < CONFIGURATION_HEADER_LEN
            || total < length
        {
            return Err(Malformed);
        }
        Ok(Self {
            length,
            total,
            value: header[5],
        })
    }
}

/// `bmAttributes` transfer-type mask (USB 2.0 §9.6.6 Table 9-13).
pub const ENDPOINT_ATTR_TYPE_MASK: u8 = 0x03;

/// `bmAttributes` transfer type: isochronous.
pub const ENDPOINT_ATTR_ISOCHRONOUS: u8 = 0x01;

/// `bmAttributes` transfer type: bulk.
pub const ENDPOINT_ATTR_BULK: u8 = 0x02;

/// `bmAttributes` transfer type: interrupt.
pub const ENDPOINT_ATTR_INTERRUPT: u8 = 0x03;

/// `bEndpointAddress` direction bit: set for an IN endpoint.
pub const ENDPOINT_ADDR_DIR_IN: u8 = 0x80;

/// `bEndpointAddress` endpoint-number mask.
pub const ENDPOINT_ADDR_NUMBER_MASK: u8 = 0x0F;

/// `wMaxPacketSize` packet-size mask (bits 0:10).
pub const ENDPOINT_MAX_PACKET_MASK: u16 = 0x07FF;

/// `wMaxPacketSize` bits 11:12, bits 3:4 of its high byte: a high-speed
/// periodic endpoint's additional transactions per microframe.
pub const ENDPOINT_TRANSACTIONS_SHIFT: u8 = 3;

/// A descriptor its stream cannot hold: shorter than its own two-byte header,
/// or running past the stream's end.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Malformed;

/// Every descriptor of `bytes`, each its `bLength` bytes. Trailing bytes too
/// few to begin a descriptor end the walk; a malformed descriptor ends it
/// with [`Malformed`].
#[must_use]
pub const fn descriptors(bytes: &[u8]) -> Descriptors<'_> {
    Descriptors { rest: bytes }
}

/// The walk [`descriptors`] returns.
#[derive(Clone, Debug)]
pub struct Descriptors<'a> {
    rest: &'a [u8],
}

impl<'a> Iterator for Descriptors<'a> {
    type Item = Result<&'a [u8], Malformed>;

    fn next(&mut self) -> Option<Self::Item> {
        let rest = core::mem::take(&mut self.rest);
        let length = usize::from(*rest.first()?);
        if rest.len() < 2 {
            return None;
        }
        if length < 2 || length > rest.len() {
            return Some(Err(Malformed));
        }
        let (descriptor, after) = rest.split_at(length);
        self.rest = after;
        Some(Ok(descriptor))
    }
}

impl core::iter::FusedIterator for Descriptors<'_> {}

#[cfg(test)]
mod tests {
    use super::{descriptors, ConfigurationHeader, Malformed, DESC_TYPE_CONFIGURATION};

    #[test]
    fn a_configuration_header_states_its_length_total_and_value() {
        let header = [9, DESC_TYPE_CONFIGURATION, 0x2A, 0x01, 2, 3, 0, 0x80, 50];
        assert_eq!(
            ConfigurationHeader::decode(&header),
            Ok(ConfigurationHeader {
                length: 9,
                total: 0x12A,
                value: 3,
            })
        );
    }

    #[test]
    fn a_short_mistyped_or_self_contradicting_header_is_malformed() {
        let good = [9, DESC_TYPE_CONFIGURATION, 9, 0, 1, 1, 0, 0x80, 50];
        assert_eq!(ConfigurationHeader::decode(&good[..8]), Err(Malformed));
        let mut mistyped = good;
        mistyped[1] = 0x04;
        assert_eq!(ConfigurationHeader::decode(&mistyped), Err(Malformed));
        let mut short_length = good;
        short_length[0] = 8;
        assert_eq!(ConfigurationHeader::decode(&short_length), Err(Malformed));
        let mut short_total = good;
        short_total[2] = 4;
        assert_eq!(
            ConfigurationHeader::decode(&short_total),
            Err(Malformed),
            "a stream cannot be shorter than its own header"
        );
    }

    #[test]
    fn each_descriptor_is_its_stated_length_and_a_fragment_ends_the_walk() {
        let stream = [3, 0x24, 7, 2, 0x05, 9];
        let walked: [Result<&[u8], Malformed>; 2] = [Ok(&stream[..3]), Ok(&stream[3..5])];
        assert!(descriptors(&stream).eq(walked));
    }

    #[test]
    fn a_descriptor_too_short_or_too_long_ends_the_walk_malformed() {
        assert!(descriptors(&[0, 0x05, 1, 2]).eq([Err(Malformed)]));
        assert!(descriptors(&[1, 0x05]).eq([Err(Malformed)]));
        assert!(descriptors(&[2, 0x04, 9, 0x05]).eq([Ok(&[2u8, 0x04][..]), Err(Malformed)]));
        assert_eq!(descriptors(&[]).next(), None);
    }
}
