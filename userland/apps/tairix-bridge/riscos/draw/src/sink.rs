//! Where a processed path goes, but the screen: a path buffer, a count of
//! the bytes one would need, or its bounding box.

use crate::geometry::Point;
use crate::path::Element;
use crate::stage::{Guest, Refusal, Sink, Stop};

/// The bytes a path's end element takes.
pub(crate) const END_BYTES: u32 = 8;

/// A path written to a buffer that begins as an end element: its second
/// word the room after it. Each element is written if it fits, the end
/// element after the last, its second word the room left; the client bits
/// are not copied.
pub(crate) struct Output {
    buffer: u32,
    room: u32,
    used: u32,
}

impl Output {
    /// The buffer at `buffer`, its room read now.
    pub(crate) fn new(guest: &mut dyn Guest, buffer: u32) -> Result<Self, Stop> {
        Ok(Self {
            buffer,
            room: guest.read_word(buffer.wrapping_add(4))?,
            used: 0,
        })
    }

    /// Where the end element is, once written.
    pub(crate) const fn end(&self) -> u32 {
        self.buffer.wrapping_add(self.used)
    }
}

/// `element`'s words written from `at`.
pub(crate) fn write_element(
    guest: &mut dyn Guest,
    at: u32,
    word: u32,
    element: Element,
) -> Result<(), Stop> {
    guest.write_word(at, word)?;
    let mut next = at.wrapping_add(4);
    for point in element.points().into_iter().flatten() {
        guest.write_word(next, point.x.cast_unsigned())?;
        guest.write_word(next.wrapping_add(4), point.y.cast_unsigned())?;
        next = next.wrapping_add(8);
    }
    Ok(())
}

impl Sink for Output {
    fn element(&mut self, guest: &mut dyn Guest, element: Element) -> Result<(), Stop> {
        let bytes = element.bytes();
        if u64::from(self.used) + u64::from(bytes) > u64::from(self.room) {
            return Err(Refusal::OutputFull.into());
        }
        write_element(guest, self.end(), element.code(), element)?;
        self.used += bytes;
        Ok(())
    }

    fn finish(&mut self, guest: &mut dyn Guest) -> Result<(), Stop> {
        let end = self.end();
        guest.write_word(end, 0)?;
        guest.write_word(end.wrapping_add(4), self.room.wrapping_sub(self.used))?;
        Ok(())
    }
}

/// The bytes the path would take, its end element included.
#[derive(Default)]
pub(crate) struct Count {
    pub(crate) bytes: u32,
}

impl Sink for Count {
    fn element(&mut self, _: &mut dyn Guest, element: Element) -> Result<(), Stop> {
        self.bytes = self.bytes.wrapping_add(element.bytes());
        Ok(())
    }

    fn finish(&mut self, _: &mut dyn Guest) -> Result<(), Stop> {
        self.bytes = self.bytes.wrapping_add(END_BYTES);
        Ok(())
    }
}

/// The box about every point, a curve's handles too, written as its lowest
/// x and y then its highest; an empty path's runs from the greatest word to
/// the least. RISC OS leaves R0 the x of the last point it met: a curve's
/// second handle, and a close's subpath start.
pub(crate) struct Bounds {
    block: u32,
    low: Point,
    high: Point,
    start: Point,
    pub(crate) last_x: i32,
}

impl Bounds {
    pub(crate) const fn new(block: u32) -> Self {
        Self {
            block,
            low: Point::new(i32::MAX, i32::MAX),
            high: Point::new(i32::MIN, i32::MIN),
            start: Point::new(0, 0),
            last_x: 0,
        }
    }
}

impl Sink for Bounds {
    fn element(&mut self, _: &mut dyn Guest, element: Element) -> Result<(), Stop> {
        for point in element.points().into_iter().flatten() {
            self.low = Point::new(self.low.x.min(point.x), self.low.y.min(point.y));
            self.high = Point::new(self.high.x.max(point.x), self.high.y.max(point.y));
        }
        self.last_x = match element {
            Element::Move { to, .. } => {
                self.start = to;
                to.x
            }
            Element::Curve { c2, .. } => c2.x,
            Element::Line { to } | Element::Gap { to } => to.x,
            Element::Close { .. } => self.start.x,
        };
        Ok(())
    }

    fn finish(&mut self, guest: &mut dyn Guest) -> Result<(), Stop> {
        let words = [self.low.x, self.low.y, self.high.x, self.high.y];
        for (i, word) in (0u32..).zip(words) {
            guest.write_word(self.block.wrapping_add(4 * i), word.cast_unsigned())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::testkit::{feed, parse, Memory};

    fn written(room: u32) -> (Memory, Result<(), Stop>) {
        let mut memory = Memory::with_words(0x100, &[0x77, room]);
        let ran = Output::new(&mut memory, 0x100)
            .and_then(|mut output| feed(&mut output, &mut memory, &parse("|2 0 0 |8 9000 0")));
        (memory, ran)
    }

    fn words(memory: &Memory, count: u32) -> alloc::vec::Vec<Option<u32>> {
        (0..count)
            .map(|i| memory.words.get(&(0x100 + 4 * i)).copied())
            .collect()
    }

    #[test]
    fn an_element_is_written_if_it_fits_and_the_end_after_the_last_as_risc_os_did() {
        let (memory, ran) = written(24);
        assert_eq!(ran, Ok(()));
        assert_eq!(
            words(&memory, 8),
            [
                Some(2),
                Some(0),
                Some(0),
                Some(8),
                Some(9000),
                Some(0),
                Some(0),
                Some(0)
            ]
        );
        let (memory, ran) = written(31);
        assert_eq!(ran, Ok(()));
        assert_eq!(words(&memory, 8)[6..], [Some(0), Some(7)]);
        let (memory, ran) = written(23);
        assert_eq!(ran, Err(Stop::Refused(Refusal::OutputFull)));
        assert_eq!(
            words(&memory, 4),
            [Some(2), Some(0), Some(0), None],
            "no end element"
        );
        let (memory, ran) = written(11);
        assert_eq!(ran, Err(Stop::Refused(Refusal::OutputFull)));
        assert_eq!(words(&memory, 2), [Some(0x77), Some(11)]);
    }

    #[test]
    fn the_room_is_unsigned() {
        let (memory, ran) = written((-8i32).cast_unsigned());
        assert_eq!(ran, Ok(()));
        assert_eq!(words(&memory, 8)[7], Some(0xFFFF_FFE0));
    }

    #[test]
    fn a_count_takes_the_end_element_in() {
        let mut count = Count::default();
        feed(
            &mut count,
            &mut Memory::default(),
            &parse("|2 0 0 |8 1 1 |6 1 2 3 4 5 6 |5"),
        )
        .expect("counted");
        assert_eq!(count.bytes, 12 + 12 + 28 + 4 + 8);
    }

    #[test]
    fn a_box_takes_handles_in_and_r0_is_as_risc_os_left_it() {
        let cases = [
            (
                "|2 100 200 |8 900 300 |8 400 700 |5",
                [100, 200, 900, 700],
                100,
            ),
            ("|2 100 200 |6 1 2 3 4 555 6", [1, 2, 555, 200], 3),
            ("|2 100 200 |8 900 300 |2 77 88", [77, 88, 900, 300], 77),
            ("|2 100 200 |7 900 300", [100, 200, 900, 300], 900),
            ("|2 100 200 |6 1 2 3 4 555 6 |8 9 8", [1, 2, 555, 200], 9),
            ("", [i32::MAX, i32::MAX, i32::MIN, i32::MIN], 0),
        ];
        for (path, expected, r0) in cases {
            let mut memory = Memory::default();
            let mut bounds = Bounds::new(0x200);
            feed(&mut bounds, &mut memory, &parse(path)).expect("bounded");
            let block: alloc::vec::Vec<i32> = (0..4)
                .map(|i| memory.words[&(0x200 + 4 * i)].cast_signed())
                .collect();
            assert_eq!(block, expected, "{path}");
            assert_eq!(bounds.last_x, r0, "{path}");
        }
    }
}
