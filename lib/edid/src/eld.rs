//! An ELD: the EDID-like data an HD Audio codec keeps for the display on its
//! pin, in the CTA-861 audio encoding an EDID carries.

use tairix_inline::ArrayString;

use crate::audio::{Audio, Speakers, SAD_LEN};

/// The most bytes an ELD holds: an HD Audio codec indexes it by one byte.
pub const ELD_MAX_LEN: usize = 256;

/// The most characters an ELD's monitor name holds.
pub const ELD_NAME_LEN: usize = 16;

/// The ELD version for CTA-861-D and later.
const VERSION_2: u8 = 2;

/// Where the baseline block starts, after the header.
const BASELINE: usize = 4;

/// Where the monitor name starts within the ELD.
const NAME: usize = 20;

/// Why an ELD was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EldError {
    /// A version other than 2.
    Version,
    /// A baseline block, name or descriptor list that runs past the bytes
    /// read, or past the baseline it belongs to.
    Length,
    /// A connection type no link has.
    Connection,
}

/// The link a display's audio reaches it over.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Connection {
    /// HDMI.
    Hdmi,
    /// `DisplayPort`.
    DisplayPort,
}

/// What an ELD states about the display on a pin.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Eld {
    /// The link the display is on.
    pub connection: Connection,
    /// Its monitor name, when it has one.
    pub name: Option<ArrayString<ELD_NAME_LEN>>,
    /// The audio it plays.
    pub audio: Audio,
}

impl Eld {
    /// Read the ELD `bytes` hold.
    ///
    /// # Errors
    ///
    /// * [`EldError::Version`] — not an ELD of version 2.
    /// * [`EldError::Length`] — a part that runs past what was read or past
    ///   the baseline block.
    /// * [`EldError::Connection`] — a connection type no link has.
    pub fn parse(bytes: &[u8]) -> Result<Self, EldError> {
        let header = bytes.get(..BASELINE).ok_or(EldError::Length)?;
        if header[0] >> 3 != VERSION_2 {
            return Err(EldError::Version);
        }
        let baseline_end = BASELINE + usize::from(header[2]) * 4;
        let baseline = bytes.get(..baseline_end).ok_or(EldError::Length)?;
        let fixed = baseline.get(..NAME).ok_or(EldError::Length)?;
        let name_len = usize::from(fixed[4] & 0x1F);
        let descriptors = usize::from(fixed[5] >> 4);
        let connection = match (fixed[5] >> 2) & 0x3 {
            0 => Connection::Hdmi,
            1 => Connection::DisplayPort,
            _ => return Err(EldError::Connection),
        };
        if name_len > ELD_NAME_LEN {
            return Err(EldError::Length);
        }
        let name = baseline
            .get(NAME..NAME + name_len)
            .ok_or(EldError::Length)?;
        let first_descriptor = NAME + name_len;
        let sads = baseline
            .get(first_descriptor..first_descriptor + descriptors * SAD_LEN)
            .ok_or(EldError::Length)?;
        let mut audio = Audio {
            speakers: Some(Speakers::decode([fixed[7], 0, 0])),
            ..Audio::default()
        };
        audio.add_descriptors(sads);
        Ok(Self {
            connection,
            name: monitor_name(name),
            audio,
        })
    }
}

/// A monitor name: printable ASCII, trimmed; anything else is no name.
fn monitor_name(bytes: &[u8]) -> Option<ArrayString<ELD_NAME_LEN>> {
    if !bytes.iter().all(|byte| (0x20..0x7F).contains(byte)) {
        return None;
    }
    let text = core::str::from_utf8(bytes).ok()?.trim();
    (!text.is_empty()).then(|| ArrayString::from_str_truncating(text))
}
