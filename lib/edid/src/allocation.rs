//! Where an HDMI sink plays each channel of a linear PCM stream: CTA-861's
//! speaker allocations (CTA-861-H §6.6.2, Table 34), as the audio `InfoFrame`
//! names them and as the stream's channels fill the link's eight slots.
//!
//! A layout is placed only where its channels fill the first slots with none
//! left empty between them — stereo, 2.1, 3.1 with or without a rear
//! centre, 5.1, 6.1 and 7.1 — so a stream is framed in place, a sample to a
//! slot.

use tairix_abi::driver::audio::{ChannelMap, ChannelPosition, MAX_CHANNELS};

use crate::Speakers;

/// The channel slots an HDMI audio sample carries.
pub const SLOTS: usize = 8;
const _: () = assert!(MAX_CHANNELS <= SLOTS, "every layout fits the slots");

/// A layout placed on a sink's speakers.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Allocation {
    /// The speaker allocation the audio `InfoFrame` states.
    pub code: u8,
    /// The slots the layout fills, the first ones.
    pub channels: u8,
    /// For each slot, the channel of the layout's interleave it carries.
    pub order: [u8; SLOTS],
}

/// The slot each CTA-861 placement takes in every allocation it is part of:
/// front left and right, then LFE, front centre, rear left and right, and
/// rear left and right of centre. A rear centre takes the rear pair's first
/// slot without that pair, and the slot behind it with one.
const LFE_SLOT: usize = 2;
const CENTRE_SLOT: usize = 3;
const REAR_SLOTS: [usize; 2] = [4, 5];
const REAR_CENTRE_SLOTS: [usize; 2] = [6, 7];

const LFE_BIT: u8 = 0x01;
const CENTRE_BIT: u8 = 0x02;
const LONE_REAR_CENTRE_BIT: u8 = 0x04;
const REAR_CODE: u8 = 0x08;
const REAR_CENTRE_CODE: u8 = 0x10;

impl Allocation {
    /// The speakers it plays on, as [`Speakers`] bits.
    const fn speakers(&self) -> u32 {
        let mut speakers = Speakers::FRONT;
        if self.code & LFE_BIT != 0 {
            speakers |= Speakers::LFE;
        }
        if self.code & CENTRE_BIT != 0 {
            speakers |= Speakers::FRONT_CENTRE;
        }
        if self.code & (REAR_CODE | REAR_CENTRE_CODE) != 0 {
            speakers |= Speakers::REAR;
        }
        if self.code & LONE_REAR_CENTRE_BIT != 0 {
            speakers |= Speakers::REAR_CENTRE;
        }
        if self.code & REAR_CENTRE_CODE != 0 {
            speakers |= Speakers::REAR_CENTRE_PAIR;
        }
        speakers
    }

    /// Whether a sink with `speakers` has every speaker it plays on. Every
    /// sink that plays audio has the front pair, stated or not.
    #[must_use]
    pub fn fits(&self, speakers: Option<Speakers>) -> bool {
        let present = Speakers::FRONT | speakers.map_or(0, Speakers::bits);
        present & self.speakers() == self.speakers()
    }
}

/// The allocation that places `map`, or [`None`] for a layout with no front
/// pair, one that would leave a slot empty below its last, or one CTA-861
/// has no allocation for.
///
/// A layout with both side and rear pairs is 7.1: the sides take the sink's
/// rear pair and the rears its rear-of-centre pair, as CTA-861 places a
/// back-surround layout. A layout with one of the two pairs puts it on the
/// rear pair, and a rear centre behind it.
#[must_use]
pub fn allocate(map: &ChannelMap) -> Option<Allocation> {
    use ChannelPosition::{
        FrontCentre, FrontLeft, FrontRight, LowFrequency, Mono, RearCentre, RearLeft, RearRight,
        SideLeft, SideRight,
    };
    let positions = map.positions();
    let at = |position| {
        positions
            .iter()
            .position(|&p| p == position)
            .and_then(|index| u8::try_from(index).ok())
    };
    let [front_left, front_right] = [at(FrontLeft)?, at(FrontRight)?];
    let sides = (at(SideLeft), at(SideRight));
    let rears = (at(RearLeft), at(RearRight));
    let pair = |pair: (Option<u8>, Option<u8>)| match pair {
        (Some(left), Some(right)) => Some(Some([left, right])),
        (None, None) => Some(None),
        _ => None,
    };
    let (sides, rears) = (pair(sides)?, pair(rears)?);
    let (rear, rear_centre) = match (sides, rears) {
        (Some(sides), Some(rears)) => (Some(sides), Some(rears)),
        (Some(pair), None) | (None, Some(pair)) => (Some(pair), None),
        (None, None) => (None, None),
    };
    let mut order = [0u8; SLOTS];
    let mut filled = [false; SLOTS];
    let mut place = |slot: usize, channel: u8| {
        order[slot] = channel;
        filled[slot] = true;
    };
    place(0, front_left);
    place(1, front_right);
    let mut code = 0;
    if let Some(channel) = at(LowFrequency) {
        place(LFE_SLOT, channel);
        code |= LFE_BIT;
    }
    if let Some(channel) = at(FrontCentre) {
        place(CENTRE_SLOT, channel);
        code |= CENTRE_BIT;
    }
    if let Some(pair) = rear {
        for (slot, channel) in REAR_SLOTS.into_iter().zip(pair) {
            place(slot, channel);
        }
        code |= REAR_CODE;
    }
    if let Some(pair) = rear_centre {
        for (slot, channel) in REAR_CENTRE_SLOTS.into_iter().zip(pair) {
            place(slot, channel);
        }
        code = code & !REAR_CODE | REAR_CENTRE_CODE;
    }
    if let Some(channel) = at(RearCentre) {
        if rear_centre.is_some() {
            return None;
        }
        place(
            if rear.is_some() {
                REAR_CENTRE_SLOTS[0]
            } else {
                REAR_SLOTS[0]
            },
            channel,
        );
        code |= LONE_REAR_CENTRE_BIT;
    }
    let channels = positions.len();
    let contiguous = filled.iter().take(channels).all(|&slot| slot)
        && filled.iter().skip(channels).all(|&slot| !slot);
    if positions.contains(&Mono) || !contiguous {
        return None;
    }
    Some(Allocation {
        code,
        channels: u8::try_from(channels).ok()?,
        order,
    })
}

/// The widest conventional layout of at most `channels` channels that
/// [`allocate`] places and that [`fits`](Allocation::fits) `speakers`: 7.1,
/// 5.1 or stereo.
#[must_use]
pub fn widest_layout(speakers: Option<Speakers>, channels: u8) -> ChannelMap {
    [8, 6]
        .into_iter()
        .filter(|&count| count <= channels)
        .filter_map(ChannelMap::conventional)
        .find(|layout| allocate(layout).is_some_and(|placed| placed.fits(speakers)))
        .unwrap_or(ChannelMap::STEREO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_abi::driver::audio::ChannelPosition::{
        FrontCentre, FrontLeft, FrontRight, LowFrequency, RearCentre, RearLeft, RearRight,
        SideLeft, SideRight,
    };

    fn map(positions: &[ChannelPosition]) -> ChannelMap {
        ChannelMap::new(positions).expect("a layout")
    }

    /// CTA-861-H Table 34: a rear centre in the rear pair's first slot
    /// without that pair, and behind it with one; never beside the pair of
    /// rear centres.
    #[test]
    fn a_rear_centre_takes_the_slot_its_allocation_gives_it() {
        let six_one = map(&[
            FrontLeft,
            FrontRight,
            FrontCentre,
            LowFrequency,
            RearCentre,
            SideLeft,
            SideRight,
        ]);
        let placed = allocate(&six_one).expect("6.1");
        assert_eq!((placed.code, placed.channels), (0x0F, 7));
        assert_eq!(placed.order[..7], [0, 1, 3, 2, 5, 6, 4]);
        let lone = allocate(&map(&[
            FrontLeft,
            FrontRight,
            LowFrequency,
            FrontCentre,
            RearCentre,
        ]))
        .expect("placed");
        assert_eq!((lone.code, lone.channels, lone.order[4]), (0x07, 5, 4));
        assert_eq!(
            allocate(&map(&[FrontLeft, FrontRight, FrontCentre, RearCentre])),
            None,
            "a slot left empty"
        );
        let beside = map(&[
            FrontLeft, FrontRight, SideLeft, SideRight, RearLeft, RearRight, RearCentre,
        ]);
        assert_eq!(allocate(&beside), None, "no allocation has both");
        let speakers = |bits: u32| {
            Some(Speakers::decode(
                bits.to_le_bytes()[..3].try_into().expect("three"),
            ))
        };
        let surround = Speakers::FRONT | Speakers::LFE | Speakers::FRONT_CENTRE | Speakers::REAR;
        assert!(!placed.fits(speakers(surround)));
        assert!(placed.fits(speakers(surround | Speakers::REAR_CENTRE)));
        assert!(!lone.fits(speakers(surround)));
    }

    #[test]
    fn the_conventional_layouts_take_their_codes_in_slot_order() {
        let stereo = allocate(&ChannelMap::STEREO).expect("placed");
        assert_eq!((stereo.code, stereo.channels), (0x00, 2));
        let five_one = allocate(&ChannelMap::conventional(6).expect("5.1")).expect("placed");
        assert_eq!((five_one.code, five_one.channels), (0x0B, 6));
        assert_eq!(five_one.order[..6], [0, 1, 3, 2, 4, 5], "LFE before centre");
        let seven_one = allocate(&ChannelMap::conventional(8).expect("7.1")).expect("placed");
        assert_eq!((seven_one.code, seven_one.channels), (0x13, 8));
        assert_eq!(
            seven_one.order,
            [0, 1, 3, 2, 6, 7, 4, 5],
            "the sides on the rear pair, the rears behind them"
        );
    }

    #[test]
    fn a_side_pair_alone_plays_on_the_rear_pair() {
        let side = map(&[
            FrontLeft,
            FrontRight,
            FrontCentre,
            LowFrequency,
            SideLeft,
            SideRight,
        ]);
        let placed = allocate(&side).expect("placed");
        assert_eq!((placed.code, &placed.order[4..6]), (0x0B, &[4, 5][..]));
    }

    #[test]
    fn a_layout_that_would_leave_a_slot_empty_is_not_placed() {
        assert_eq!(
            allocate(&map(&[FrontLeft, FrontRight, RearLeft, RearRight])),
            None,
            "4.0"
        );
        assert_eq!(
            allocate(&map(&[FrontLeft, FrontRight, FrontCentre])),
            None,
            "3.0"
        );
        assert_eq!(allocate(&ChannelMap::MONO), None);
        assert_eq!(
            allocate(&map(&[FrontLeft, FrontCentre])),
            None,
            "no front pair"
        );
        assert_eq!(
            allocate(&map(&[
                FrontLeft,
                FrontRight,
                LowFrequency,
                FrontCentre,
                SideLeft
            ])),
            None,
            "half a pair"
        );
        let two_one = allocate(&map(&[FrontLeft, FrontRight, LowFrequency])).expect("2.1");
        assert_eq!((two_one.code, two_one.channels), (0x01, 3));
    }

    #[test]
    fn a_sink_is_offered_the_widest_layout_its_speakers_and_channels_carry() {
        let surround = Speakers::FRONT | Speakers::LFE | Speakers::FRONT_CENTRE | Speakers::REAR;
        let speakers = |bits: u32| {
            Some(Speakers::decode(
                bits.to_le_bytes()[..3].try_into().expect("three"),
            ))
        };
        assert_eq!(widest_layout(speakers(surround), 8).channels(), 6);
        assert_eq!(
            widest_layout(speakers(surround | Speakers::REAR_CENTRE_PAIR), 8).channels(),
            8
        );
        assert_eq!(widest_layout(speakers(surround), 2), ChannelMap::STEREO);
        assert_eq!(widest_layout(None, 8), ChannelMap::STEREO);
        for channels in [6, 8] {
            assert!(allocate(&widest_layout(
                speakers(surround | Speakers::REAR_CENTRE_PAIR),
                channels
            ))
            .is_some());
        }
    }

    #[test]
    fn an_allocation_fits_a_sink_only_with_every_speaker_it_plays_on() {
        let speakers = |bits: u32| {
            Some(Speakers::decode(
                bits.to_le_bytes()[..3].try_into().expect("three"),
            ))
        };
        let placed = |channels| {
            allocate(&ChannelMap::conventional(channels).expect("a layout")).expect("placed")
        };
        let two_one = allocate(&map(&[FrontLeft, FrontRight, LowFrequency])).expect("2.1");
        assert!(
            allocate(&ChannelMap::STEREO).expect("placed").fits(None),
            "the front pair is implied"
        );
        assert!(!two_one.fits(None));
        assert!(two_one.fits(speakers(Speakers::FRONT | Speakers::LFE)));
        let surround = Speakers::FRONT | Speakers::LFE | Speakers::FRONT_CENTRE | Speakers::REAR;
        assert!(placed(6).fits(speakers(surround)));
        assert!(!placed(6).fits(speakers(surround & !Speakers::FRONT_CENTRE)));
        assert!(
            !placed(8).fits(speakers(surround)),
            "no rear pair of centre"
        );
        assert!(placed(8).fits(speakers(surround | Speakers::REAR_CENTRE_PAIR)));
        assert!(
            !placed(8).fits(speakers(Speakers::FRONT | Speakers::REAR_CENTRE_PAIR)),
            "nor rears"
        );
    }
}
