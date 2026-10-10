//! Codec verbs, parameters, and the capability words a codec answers with
//! (Intel High Definition Audio Specification 1.0a, section 7).

use tairix_abi::DriverError;

/// One command: the node it addresses and the verb with its payload.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Verb {
    nid: u8,
    body: u32,
}

impl Verb {
    /// A verb with a twelve-bit identifier and an eight-bit payload.
    #[must_use]
    pub const fn short(nid: u8, verb: u16, payload: u8) -> Self {
        Self {
            nid,
            body: ((verb as u32 & 0xFFF) << 8) | payload as u32,
        }
    }

    /// A verb with a four-bit identifier and a sixteen-bit payload.
    #[must_use]
    pub const fn long(nid: u8, verb: u8, payload: u16) -> Self {
        Self {
            nid,
            body: ((verb as u32 & 0xF) << 16) | payload as u32,
        }
    }

    /// `GET_PARAMETER`.
    #[must_use]
    pub const fn parameter(nid: u8, parameter: u8) -> Self {
        Self::short(nid, GET_PARAMETER, parameter)
    }

    /// The node addressed.
    #[cfg(test)]
    #[must_use]
    pub const fn nid(self) -> u8 {
        self.nid
    }

    /// The command word for the codec at `address`, which must be below 16.
    #[must_use]
    pub const fn command(self, address: u8) -> u32 {
        ((address as u32 & 0xF) << 28) | ((self.nid as u32) << 20) | self.body
    }

    /// The codec address, node and verb of a command word.
    #[cfg(test)]
    #[must_use]
    pub const fn from_command(command: u32) -> (u8, u8, Self) {
        let address = (command >> 28) as u8;
        let nid = ((command >> 20) & 0xFF) as u8;
        (
            address,
            nid,
            Self {
                nid,
                body: command & 0xF_FFFF,
            },
        )
    }

    /// The verb and payload bits.
    #[cfg(test)]
    #[must_use]
    pub const fn body(self) -> u32 {
        self.body
    }
}

pub const GET_PARAMETER: u16 = 0xF00;
pub const SET_CONNECTION_SELECT: u16 = 0x701;
pub const GET_CONNECTION_LIST: u16 = 0xF02;
pub const SET_POWER_STATE: u16 = 0x705;
pub const SET_STREAM_CHANNEL: u16 = 0x706;
pub const SET_PIN_CONTROL: u16 = 0x707;
pub const SET_UNSOLICITED: u16 = 0x708;
pub const GET_PIN_SENSE: u16 = 0xF09;
pub const EXECUTE_PIN_SENSE: u16 = 0x709;
pub const SET_EAPD: u16 = 0x70C;
pub const SET_DIGITAL_CONTROL: u16 = 0x70D;
pub const GET_CONFIG_DEFAULT: u16 = 0xF1C;
pub const SET_CHANNEL_COUNT: u16 = 0x72D;
pub const GET_DIP_SIZE: u16 = 0xF2E;
pub const GET_ELD_DATA: u16 = 0xF2F;
pub const SET_DIP_INDEX: u16 = 0x730;
pub const SET_DIP_DATA: u16 = 0x731;
pub const SET_DIP_TRANSMIT: u16 = 0x732;
/// Four-bit verbs: the converter format, and amplifier gain and mute.
pub const SET_FORMAT: u8 = 0x2;
pub const SET_AMP: u8 = 0x3;

/// Parameters, read with [`GET_PARAMETER`].
pub mod param {
    pub const VENDOR_ID: u8 = 0x00;
    pub const NODE_COUNT: u8 = 0x04;
    pub const FUNCTION_GROUP: u8 = 0x05;
    pub const WIDGET_CAPS: u8 = 0x09;
    pub const PCM: u8 = 0x0A;
    pub const STREAM_FORMATS: u8 = 0x0B;
    pub const PIN_CAPS: u8 = 0x0C;
    pub const INPUT_AMP: u8 = 0x0D;
    pub const CONNECTION_LENGTH: u8 = 0x0E;
    pub const OUTPUT_AMP: u8 = 0x12;
}

/// The function group type of an audio function.
pub const AUDIO_FUNCTION_GROUP: u32 = 0x01;

/// Power state D0, fully on.
pub const POWER_D0: u8 = 0;

/// `NODE_COUNT`: the first subordinate node and how many there are.
#[must_use]
pub const fn subordinates(answer: u32) -> (u8, u8) {
    (((answer >> 16) & 0xFF) as u8, (answer & 0xFF) as u8)
}

/// What a widget is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WidgetKind {
    /// A digital-to-analogue converter: playback samples enter here.
    Output,
    /// An analogue-to-digital converter: capture samples leave here.
    Input,
    /// Sums its inputs.
    Mixer,
    /// Chooses one of its inputs.
    Selector,
    /// A connector, or a fixed device such as a speaker.
    Pin,
    /// Anything else, which no path passes through.
    Other,
}

/// `WIDGET_CAPS`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct WidgetCaps(pub u32);

impl WidgetCaps {
    /// What the widget is.
    #[must_use]
    pub const fn kind(self) -> WidgetKind {
        match (self.0 >> 20) & 0xF {
            0 => WidgetKind::Output,
            1 => WidgetKind::Input,
            2 => WidgetKind::Mixer,
            3 => WidgetKind::Selector,
            4 => WidgetKind::Pin,
            _ => WidgetKind::Other,
        }
    }

    /// Channels a converter carries, one to sixteen.
    #[must_use]
    pub const fn channels(self) -> u8 {
        let extra = (((self.0 >> 13) & 0x7) << 1 | (self.0 & 1)) & 0xF;
        extra as u8 + 1
    }

    /// The widget carries digital samples.
    #[must_use]
    pub const fn digital(self) -> bool {
        self.0 & (1 << 9) != 0
    }

    /// The widget has a connection list.
    #[must_use]
    pub const fn connections(self) -> bool {
        self.0 & (1 << 8) != 0
    }

    /// The widget may send unsolicited responses.
    #[must_use]
    pub const fn unsolicited(self) -> bool {
        self.0 & (1 << 7) != 0
    }

    /// The widget states its own format support rather than the function's.
    #[must_use]
    pub const fn format_override(self) -> bool {
        self.0 & (1 << 4) != 0
    }

    /// The widget states its own amplifier capabilities.
    #[must_use]
    pub const fn amp_override(self) -> bool {
        self.0 & (1 << 3) != 0
    }

    /// The widget has an output amplifier.
    #[must_use]
    pub const fn output_amp(self) -> bool {
        self.0 & (1 << 2) != 0
    }

    /// The widget has input amplifiers.
    #[must_use]
    pub const fn input_amp(self) -> bool {
        self.0 & (1 << 1) != 0
    }

    /// The widget has power control.
    #[must_use]
    pub const fn power_control(self) -> bool {
        self.0 & (1 << 10) != 0
    }
}

/// `PIN_CAPS`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PinCaps(pub u32);

impl PinCaps {
    /// Presence detection must be triggered before it is read.
    #[must_use]
    pub const fn trigger_required(self) -> bool {
        self.0 & (1 << 1) != 0
    }

    /// The pin can tell whether its connector is occupied.
    #[must_use]
    pub const fn presence_detect(self) -> bool {
        self.0 & (1 << 2) != 0
    }

    /// The pin can drive headphones.
    #[must_use]
    pub const fn headphone_drive(self) -> bool {
        self.0 & (1 << 3) != 0
    }

    /// The pin can output.
    #[must_use]
    pub const fn output(self) -> bool {
        self.0 & (1 << 4) != 0
    }

    /// The pin can input.
    #[must_use]
    pub const fn input(self) -> bool {
        self.0 & (1 << 5) != 0
    }

    /// The pin is an HDMI connector.
    #[must_use]
    pub const fn hdmi(self) -> bool {
        self.0 & (1 << 7) != 0
    }

    /// The pin supplies an 80% bias voltage, as an electret microphone wants.
    #[must_use]
    pub const fn vref_80(self) -> bool {
        self.0 & (1 << 10) != 0
    }

    /// The pin has an external amplifier enable.
    #[must_use]
    pub const fn eapd(self) -> bool {
        self.0 & (1 << 16) != 0
    }

    /// The pin is a `DisplayPort` connector.
    #[must_use]
    pub const fn display_port(self) -> bool {
        self.0 & (1 << 24) != 0
    }
}

/// `SET_PIN_CONTROL`'s bits.
pub mod pin_control {
    pub const HEADPHONE: u8 = 1 << 7;
    pub const OUT: u8 = 1 << 6;
    pub const IN: u8 = 1 << 5;
    pub const VREF_80: u8 = 0b100;
}

/// `GET_PIN_SENSE`: the connector is occupied; a display's ELD is valid.
pub mod pin_sense {
    pub const PRESENT: u32 = 1 << 31;
    pub const ELD_VALID: u32 = 1 << 30;
}

/// An amplifier's capabilities.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct AmpCaps(pub u32);

impl AmpCaps {
    /// The amplifier can mute.
    #[must_use]
    pub const fn mute(self) -> bool {
        self.0 & (1 << 31) != 0
    }

    /// Hundredths of a decibel per step.
    #[must_use]
    pub const fn step_millibel(self) -> u32 {
        (((self.0 >> 16) & 0x7F) + 1) * 25
    }

    /// The highest step, so there are one more steps than this.
    #[must_use]
    pub const fn top(self) -> u8 {
        ((self.0 >> 8) & 0x7F) as u8
    }

    /// The step that is 0 dB.
    #[must_use]
    pub const fn offset(self) -> u8 {
        (self.0 & 0x7F) as u8
    }

    /// The amplifier's gain can be set.
    #[must_use]
    pub const fn adjustable(self) -> bool {
        self.top() > 0
    }

    /// Hundredths of a decibel at `step`.
    #[must_use]
    pub const fn millibel_at(self, step: u8) -> i32 {
        (step as i32 - self.offset() as i32) * self.step_millibel().cast_signed()
    }

    /// The step nearest 0 dB without amplifying.
    #[must_use]
    pub const fn unity(self) -> u8 {
        if self.offset() > self.top() {
            self.top()
        } else {
            self.offset()
        }
    }

    /// The lowest step at or above `millibel`, within the range.
    #[must_use]
    pub fn step_at_or_above(self, millibel: i32) -> u8 {
        let step = self.step_millibel().cast_signed();
        let above = millibel.saturating_sub(self.millibel_at(0));
        if above <= 0 {
            return 0;
        }
        let steps = (above + step - 1) / step;
        u8::try_from(steps).map_or(self.top(), |steps| steps.min(self.top()))
    }
}

/// `SET_AMP`'s payload.
pub mod amp {
    pub const OUTPUT: u16 = 1 << 15;
    pub const INPUT: u16 = 1 << 14;
    pub const LEFT: u16 = 1 << 13;
    pub const RIGHT: u16 = 1 << 12;
    pub const INDEX_SHIFT: u16 = 8;
    pub const MUTE: u16 = 1 << 7;
}

/// The pin configuration default a codec publishes for each pin
/// (section 7.3.3.31).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct PinConfig(pub u32);

/// What a pin's connector is for.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Device {
    LineOut,
    Speaker,
    Headphones,
    Cd,
    SpdifOut,
    DigitalOut,
    LineIn,
    Aux,
    Microphone,
    SpdifIn,
    DigitalIn,
    Other,
}

impl PinConfig {
    /// Nothing is connected to the pin.
    #[must_use]
    pub const fn unconnected(self) -> bool {
        self.0 >> 30 == 1
    }

    /// The pin is a fixed device rather than a connector.
    #[must_use]
    pub const fn fixed(self) -> bool {
        self.0 >> 30 == 2
    }

    /// The connector is inside the chassis.
    #[must_use]
    pub const fn internal(self) -> bool {
        (self.0 >> 28) & 0x3 == 1
    }

    /// Where on the chassis the connector is: front, rear, or neither.
    #[must_use]
    pub const fn place(self) -> Option<&'static str> {
        match (self.0 >> 24) & 0xF {
            1 => Some("Rear"),
            2 => Some("Front"),
            _ => None,
        }
    }

    /// What the connector is for.
    #[must_use]
    pub const fn device(self) -> Device {
        match (self.0 >> 20) & 0xF {
            0x0 => Device::LineOut,
            0x1 => Device::Speaker,
            0x2 => Device::Headphones,
            0x3 => Device::Cd,
            0x4 => Device::SpdifOut,
            0x5 => Device::DigitalOut,
            0x8 => Device::LineIn,
            0x9 => Device::Aux,
            0xA => Device::Microphone,
            0xC => Device::SpdifIn,
            0xD => Device::DigitalIn,
            _ => Device::Other,
        }
    }

    /// The connector's colour, where it states one.
    #[must_use]
    pub const fn colour(self) -> Option<&'static str> {
        match (self.0 >> 12) & 0xF {
            1 => Some("Black"),
            2 => Some("Grey"),
            3 => Some("Blue"),
            4 => Some("Green"),
            5 => Some("Red"),
            6 => Some("Orange"),
            7 => Some("Yellow"),
            8 => Some("Purple"),
            9 => Some("Pink"),
            0xE => Some("White"),
            _ => None,
        }
    }

    /// The board says presence detection on this pin is not wired.
    #[must_use]
    pub const fn no_presence_detect(self) -> bool {
        self.0 & (1 << 8) != 0
    }

    /// The pins of one association make one multichannel output.
    #[must_use]
    pub const fn association(self) -> u8 {
        ((self.0 >> 4) & 0xF) as u8
    }

    /// The pin's place within its association.
    #[must_use]
    pub const fn sequence(self) -> u8 {
        (self.0 & 0xF) as u8
    }
}

/// `CONNECTION_LENGTH`: entries, and whether each is sixteen bits wide.
#[must_use]
pub const fn connection_length(answer: u32) -> (u8, bool) {
    ((answer & 0x7F) as u8, answer & 0x80 != 0)
}

/// The entries one `GET_CONNECTION_LIST` answer carries: four short entries
/// or two long ones, each with its range bit.
pub fn connection_entries(answer: u32, long: bool) -> impl Iterator<Item = (u16, bool)> {
    let (count, width, range_bit) = if long {
        (2, 16, 1u16 << 15)
    } else {
        (4, 8, 1u16 << 7)
    };
    (0..count).map(move |slot| {
        let entry = ((answer >> (slot * width)) & 0xFFFF) as u16;
        let entry = if long { entry } else { entry & 0xFF };
        (entry & !range_bit, entry & range_bit != 0)
    })
}

/// An unsolicited response's tag, from the response word.
#[must_use]
pub const fn unsolicited_tag(response: u32) -> u8 {
    (response >> 26) as u8
}

/// The extended response word: the codec it came from, and whether it was
/// unsolicited.
#[must_use]
pub const fn response_source(extended: u32) -> (u8, bool) {
    ((extended & 0xF) as u8, extended & (1 << 4) != 0)
}

/// The codec answered a node it does not have, or a verb it does not know,
/// with all ones — the spec's "no such parameter".
///
/// # Errors
///
/// [`DriverError::DeviceFault`] for that answer.
pub const fn answered(response: u32) -> Result<u32, DriverError> {
    if response == u32::MAX {
        Err(DriverError::DeviceFault)
    } else {
        Ok(response)
    }
}
