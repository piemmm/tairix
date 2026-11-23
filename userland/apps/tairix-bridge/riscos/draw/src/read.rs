//! A path read from guest memory an element at a time, continuations
//! followed and each element's order checked as it is read, so an error
//! stops processing where RISC OS's would.

use crate::geometry::Point;
use tairix_drawing::riscos::format::{CONTINUATION, END, TAG};

use crate::path::{Element, Order};
use crate::stage::{Guest, Refusal, Stop};

/// An element as the path holds it: where, and its first word, client bits
/// and all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Read {
    pub(crate) at: u32,
    pub(crate) word: u32,
    pub(crate) element: Element,
}

/// Reads a path's elements in turn.
pub(crate) struct Reader {
    at: u32,
    order: Order,
    hops: Hops,
}

impl Reader {
    /// A reader of the path at `at`.
    pub(crate) fn new(at: u32) -> Self {
        Self {
            at,
            order: Order::default(),
            hops: Hops::default(),
        }
    }

    /// The next element; `None` at the path's end.
    ///
    /// # Errors
    ///
    /// The abort a read takes; a code no element has; an element out of
    /// order; continuations that lead back on themselves.
    pub(crate) fn next(&mut self, guest: &mut dyn Guest) -> Result<Option<Read>, Stop> {
        loop {
            guest.yield_point()?;
            let word = guest.read_word(self.at)?;
            match word & TAG {
                END => return Ok(None),
                CONTINUATION => {
                    let target = guest.read_word(self.at.wrapping_add(4))?;
                    if self.hops.looped(target) {
                        return Err(Refusal::Looped.into());
                    }
                    self.at = target;
                }
                kind => {
                    let mut point_at = self.at.wrapping_add(4);
                    let element = Element::decode(kind, || -> Result<Point, Stop> {
                        let x = guest.read_word(point_at)?;
                        let y = guest.read_word(point_at.wrapping_add(4))?;
                        point_at = point_at.wrapping_add(8);
                        Ok(Point::new(x.cast_signed(), y.cast_signed()))
                    })?
                    .ok_or(Refusal::BadElement)?;
                    if !self.order.admit(element) {
                        return Err(Refusal::Order.into());
                    }
                    let read = Read {
                        at: self.at,
                        word,
                        element,
                    };
                    self.at = self.at.wrapping_add(element.bytes());
                    return Ok(Some(read));
                }
            }
        }
    }
}

/// The continuations followed, watched for a cycle as Brent finds one: a
/// target seen again leads round for ever, with no memory kept but one
/// target.
#[derive(Debug, Default)]
struct Hops {
    saved: Option<u32>,
    power: u64,
    taken: u64,
}

impl Hops {
    fn looped(&mut self, target: u32) -> bool {
        if self.saved == Some(target) {
            return true;
        }
        self.taken += 1;
        if self.taken >= self.power {
            self.saved = Some(target);
            self.power = (self.power * 2).max(1);
            self.taken = 0;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;
    use crate::stage::testkit::Memory;

    fn all(memory: &mut Memory, at: u32) -> Result<Vec<Read>, Stop> {
        let mut reader = Reader::new(at);
        let mut read = Vec::new();
        while let Some(element) = reader.next(memory)? {
            read.push(element);
        }
        Ok(read)
    }

    #[test]
    fn continuations_are_followed_and_client_bits_kept() {
        let mut memory = Memory::with_words(0x100, &[0xAB02, 1, 2, 1, 0x200]);
        memory.put(0x200, &[0x8, 3, 4, 0, 0]);
        let read = all(&mut memory, 0x100).expect("read");
        assert_eq!(read.len(), 2);
        assert_eq!(
            read[0],
            Read {
                at: 0x100,
                word: 0xAB02,
                element: Element::Move {
                    to: Point::new(1, 2),
                    winds: true
                }
            }
        );
        assert_eq!(read[1].at, 0x200);
        assert_eq!(
            read[1].element,
            Element::Line {
                to: Point::new(3, 4)
            }
        );
    }

    #[test]
    fn a_bad_code_or_order_stops_after_the_elements_before_it() {
        let mut memory = Memory::with_words(0x100, &[2, 0, 0, 9]);
        let mut reader = Reader::new(0x100);
        assert!(reader.next(&mut memory).expect("the move").is_some());
        assert_eq!(
            reader.next(&mut memory),
            Err(Stop::Refused(Refusal::BadElement))
        );
        let mut memory = Memory::with_words(0x100, &[8, 0, 0]);
        assert_eq!(all(&mut memory, 0x100), Err(Stop::Refused(Refusal::Order)));
    }

    #[test]
    fn continuations_leading_round_are_refused_however_long_the_round() {
        for round in 1..40u32 {
            let mut memory = Memory::default();
            for i in 0..round {
                memory.put(0x1000 + 8 * i, &[1, 0x1000 + 8 * ((i + 1) % round)]);
            }
            assert_eq!(
                all(&mut memory, 0x1000),
                Err(Stop::Refused(Refusal::Looped)),
                "{round}"
            );
        }
        let mut memory = Memory::with_words(0x100, &[2, 0, 0, 1, 0x100]);
        assert_eq!(all(&mut memory, 0x100), Err(Stop::Refused(Refusal::Looped)));
    }

    #[test]
    fn a_long_chain_of_continuations_that_ends_is_read() {
        let mut memory = Memory::default();
        for i in 0..1000u32 {
            memory.put(0x1000 + 8 * i, &[1, 0x1000 + 8 * (i + 1)]);
        }
        memory.put(0x1000 + 8 * 1000, &[0, 0]);
        assert_eq!(all(&mut memory, 0x1000), Ok(Vec::new()));
    }
}
