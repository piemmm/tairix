//! Which firmware property requests the mailbox service forwards, and for
//! whom.
//!
//! The property interface reaches the whole `VideoCore`: beside the tags the
//! drivers use, it runs code on the GPU, locks GPU memory, sets voltages and
//! writes one-time-programmable bits, and the GPU masters the bus. So the
//! service forwards a request only when it carries one tag, word for word in
//! the shape a driver sends it, from the driver the kernel attests is loaded
//! for the device that tag concerns. Anything else is refused.

use tairix_abi::{Errno, HwMatchKey, HwNode};

use crate::{
    FirmwareClock, Malformed, RtcRegister, Tags, CODE_REQUEST, DISPLAY_ENGINE_KEY,
    FIRMWARE_CLOCKS_KEY, HEADER_WORDS, LINKED_CLOCKS, PROPERTY_WORDS, RPI_RTC_KEY,
    SKIP_SETTING_TURBO, TAG_GET_CLOCK_RATE, TAG_GET_MAX_CLOCK_RATE, TAG_GET_MIN_CLOCK_RATE,
    TAG_GET_PHYSICAL_WH, TAG_GET_RTC_REG, TAG_NOTIFY_DISPLAY_DONE, TAG_NOTIFY_XHCI_RESET,
    TAG_SET_CLOCK_RATE, TAG_SET_RTC_REG, VL805_FIRMWARE_DEV_ADDR, VL805_KEY,
};

/// Why the service refused a request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Refusal {
    /// The tag the request names, once its framing is sound enough to read
    /// one.
    pub tag: Option<u32>,
    /// The answer the caller is given.
    pub errno: Errno,
}

/// What one value word of an admitted tag may carry.
enum Word {
    /// Any value: the datum the request exists to carry.
    Any,
    /// One of these values.
    OneOf(&'static [u32]),
}

impl Word {
    fn admits(&self, value: u32) -> bool {
        match self {
            Self::Any => true,
            Self::OneOf(values) => values.contains(&value),
        }
    }
}

/// A request the service forwards: one tag, from one device's driver.
struct Admitted {
    tag: u32,
    /// The device whose driver may send it.
    device: HwMatchKey,
    /// Its value buffer, word for word.
    values: &'static [Word],
}

impl Admitted {
    fn admits(&self, values: &[u32], caller: &HwNode) -> bool {
        values.len() == self.values.len()
            && self
                .values
                .iter()
                .zip(values)
                .all(|(word, &value)| word.admits(value))
            && caller
                .match_keys()
                .iter()
                .any(|key| self.device.matches(key))
    }
}

/// A slot the firmware answers into, which every driver sends zeroed.
const SLOT: Word = Word::OneOf(&[0]);

/// The clocks the firmware-clocks node's driver drives: the ARM clock and the
/// clocks it serves to the nodes linking to them. Every other clock feeds a
/// device another driver owns.
const DRIVEN_CLOCKS: [u32; 1 + LINKED_CLOCKS.len()] = {
    let mut driven = [FirmwareClock::Arm.as_u32(); 1 + LINKED_CLOCKS.len()];
    let mut at = 0;
    while at < LINKED_CLOCKS.len() {
        driven[1 + at] = LINKED_CLOCKS[at].as_u32();
        at += 1;
    }
    driven
};

/// A clock the firmware-clocks node's driver drives.
const DRIVEN_CLOCK: Word = Word::OneOf(&DRIVEN_CLOCKS);

/// Every request the service forwards: each driver's working set and no more.
const ADMITTED: [Admitted; 9] = [
    Admitted {
        tag: TAG_NOTIFY_DISPLAY_DONE,
        device: DISPLAY_ENGINE_KEY,
        values: &[],
    },
    // The size of the screen the firmware lit, which the engine hosts the
    // console at as it takes the screen over.
    Admitted {
        tag: TAG_GET_PHYSICAL_WH,
        device: DISPLAY_ENGINE_KEY,
        values: &[SLOT, SLOT],
    },
    Admitted {
        tag: TAG_NOTIFY_XHCI_RESET,
        device: VL805_KEY,
        values: &[Word::OneOf(&[VL805_FIRMWARE_DEV_ADDR])],
    },
    Admitted {
        tag: TAG_GET_RTC_REG,
        device: RPI_RTC_KEY,
        values: &[
            Word::OneOf(&[
                RtcRegister::Time.as_u32(),
                RtcRegister::BackupVolts.as_u32(),
            ]),
            SLOT,
        ],
    },
    // Only the counter: the backup cell's charging registers can damage a
    // cell that is not rechargeable.
    Admitted {
        tag: TAG_SET_RTC_REG,
        device: RPI_RTC_KEY,
        values: &[Word::OneOf(&[RtcRegister::Time.as_u32()]), Word::Any],
    },
    Admitted {
        tag: TAG_GET_CLOCK_RATE,
        device: FIRMWARE_CLOCKS_KEY,
        values: &[DRIVEN_CLOCK, SLOT],
    },
    Admitted {
        tag: TAG_GET_MAX_CLOCK_RATE,
        device: FIRMWARE_CLOCKS_KEY,
        values: &[DRIVEN_CLOCK, SLOT],
    },
    Admitted {
        tag: TAG_GET_MIN_CLOCK_RATE,
        device: FIRMWARE_CLOCKS_KEY,
        values: &[DRIVEN_CLOCK, SLOT],
    },
    // Never the turbo transition: it moves the part to its turbo point
    // whatever rate was asked for.
    Admitted {
        tag: TAG_SET_CLOCK_RATE,
        device: FIRMWARE_CLOCKS_KEY,
        values: &[DRIVEN_CLOCK, Word::Any, Word::OneOf(&[SKIP_SETTING_TURBO])],
    },
];

/// Admit `message` from the driver the kernel attests is loaded for `caller`,
/// or refuse it. A caller loaded for no node is `None`, and is refused.
///
/// # Errors
///
/// A [`Refusal`] answering:
///
/// - [`Errno::LengthOutOfRange`] when the buffer is not one well-formed
///   request: a length outside the buffer or not whole words, a tag that
///   overruns it, or an end tag anywhere but its last word.
/// - [`Errno::OutOfRange`] when the request's or its tag's code is not a
///   request's.
/// - [`Errno::PermissionDenied`] when the request carries other than one tag,
///   that tag is not admitted or not in the shape its driver sends, or the
///   caller is not the driver of the device it concerns.
pub fn admit_request(
    message: &[u32; PROPERTY_WORDS],
    caller: Option<&HwNode>,
) -> Result<(), Refusal> {
    let refused = |tag, errno| Refusal { tag, errno };
    let end = declared_words(message).map_err(|errno| refused(None, errno))?;
    if message[1] != CODE_REQUEST {
        return Err(refused(None, Errno::OutOfRange));
    }
    let mut tags = Tags::new(&message[..end]);
    let tag = tags
        .next()
        .map_err(|Malformed| refused(None, Errno::LengthOutOfRange))?
        .ok_or_else(|| refused(None, Errno::PermissionDenied))?;
    let named = Some(tag.id);
    if tags
        .next()
        .map_err(|Malformed| refused(named, Errno::LengthOutOfRange))?
        .is_some()
    {
        return Err(refused(named, Errno::PermissionDenied));
    }
    if tags.at() + 1 != end {
        return Err(refused(named, Errno::LengthOutOfRange));
    }
    if tag.code != CODE_REQUEST {
        return Err(refused(named, Errno::OutOfRange));
    }
    let admitted = ADMITTED
        .iter()
        .find(|rule| rule.tag == tag.id)
        .zip(caller)
        .is_some_and(|(rule, caller)| rule.admits(tag.values, caller));
    if admitted {
        Ok(())
    } else {
        Err(refused(named, Errno::PermissionDenied))
    }
}

/// The request's declared length in words, if it is a whole number of words
/// that holds the header and an end tag and fits the buffer.
fn declared_words(message: &[u32; PROPERTY_WORDS]) -> Result<usize, Errno> {
    let bytes = usize::try_from(message[0]).map_err(|_| Errno::LengthOutOfRange)?;
    let words = bytes / 4;
    if !bytes.is_multiple_of(4) || words <= HEADER_WORDS || words > PROPERTY_WORDS {
        return Err(Errno::LengthOutOfRange);
    }
    Ok(words)
}

#[cfg(test)]
#[path = "admit_tests.rs"]
mod tests;
