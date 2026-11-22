//! VDU save areas (`OS_SpriteOp` 60 to 62): the drivers' state for one
//! destination — its windows, cursors, colours, patterns and character
//! sizes, the VDU queue, the changed box, the spool handle and the output
//! streams — kept while output is elsewhere.
//!
//! The drivers' own save area holds the state as it is. A program's holds it
//! in guest memory in the drivers' own layout, and is read back only where
//! every field is one the drivers could have written; any other is taken as
//! never written, so nothing a program writes there reaches the drivers'
//! state. Restored where its windows do not fit, both windows become the
//! destination's whole extent and the text cursor goes home, as RISC OS 5
//! restores them; everything else is restored as it was.

use tairix_riscos_kernel::byte::var;
use tairix_riscos_kernel::os::{Fail, Os};

use crate::colour::{Colours, Gcol, TextColour};
use crate::mode::Format;
use crate::plot::Graphics;
use crate::state::Vdu;
use crate::text::{CursorShape, Text, Window};

/// The bytes a save area takes, as RISC OS 5 reports them.
pub(crate) const SAVE_AREA_BYTES: u32 = 0x180;
/// A save area's first word once the drivers have written it: any other but
/// nought is not their layout.
const WRITTEN: u32 = 0x5644_5553;
/// The words of the layout, `WRITTEN` among them.
const WORDS: usize = 86;
const _: () = assert!(WORDS * 4 <= SAVE_AREA_BYTES as usize);
/// The longest dot pattern, in dots.
const MOST_DOTS: u32 = 64;
/// The largest character size or spacing `VDU 23,17,7` can give.
const MOST_CHARACTER: u32 = 0x8000;
/// Past the furthest text cell any destination has, so a window's extent
/// stays within a word.
const MOST_CELLS: u32 = 0xFFFF;

/// The VDU queue: the code being gathered, its parameters so far, and the
/// count of those still awaited as the queue's byte variable holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Queue {
    code: u8,
    gathered: [u8; 9],
    waiting: u8,
}

/// The drivers' state for a destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Saved {
    text: Text,
    graphics: Graphics,
    colours: Colours,
    queue: Queue,
    changed: [u32; 5],
    spool: u8,
    streams: u8,
}

impl Saved {
    /// The drivers' state as it is.
    pub(crate) fn take(vdu: &Vdu, os: &mut Os<'_>) -> Result<Self, Fail> {
        Ok(Self {
            text: vdu.text.clone(),
            graphics: vdu.graphics.clone(),
            colours: vdu.colours.clone(),
            queue: Queue {
                code: vdu.stream.code,
                gathered: vdu.stream.gathered,
                waiting: os.byte_var(var::VDU_QUEUE),
            },
            changed: crate::changed::words(os)?,
            spool: os.byte_var(var::SPOOL_HANDLE),
            streams: os.byte_var(var::WRCH_DEST),
        })
    }

    /// The state a destination of `format` starts with, as a mode change
    /// leaves it but for the text flags, which stay `flags`.
    pub(crate) fn fresh(format: &Format, flags: u8) -> Self {
        let (text, graphics, colours) = crate::select::fresh_state(format, flags);
        Self {
            text,
            graphics,
            colours,
            queue: Queue {
                code: 0,
                gathered: [0; 9],
                waiting: 0,
            },
            changed: crate::changed::RESET,
            spool: 0,
            streams: 0,
        }
    }

    /// Makes it the drivers' state, drawn on the destination as it now is.
    pub(crate) fn restore(mut self, vdu: &mut Vdu, os: &mut Os<'_>) -> Result<(), Fail> {
        let format = *vdu.format();
        self.fit(&format);
        vdu.text = self.text;
        vdu.graphics = self.graphics;
        vdu.colours = self.colours;
        vdu.stream.code = self.queue.code;
        vdu.stream.gathered = self.queue.gathered;
        os.set_byte_var(var::VDU_QUEUE, self.queue.waiting);
        crate::changed::set_words(os, self.changed)?;
        os.set_byte_var(var::SPOOL_HANDLE, self.spool);
        os.set_byte_var(var::WRCH_DEST, self.streams);
        vdu.colours.publish(os, &format)
    }

    /// Both windows made the whole of a destination of `format` that either
    /// does not fit, the text cursor home.
    fn fit(&mut self, format: &Format) {
        let g = self.graphics.window;
        let t = self.text.window;
        let (columns, rows) = format.cells();
        let fits = g.right < format.width().cast_signed()
            && g.top < format.height().cast_signed()
            && t.right < columns
            && t.bottom < rows;
        if fits {
            return;
        }
        let whole = Self::fresh(format, self.text.flags);
        self.graphics.window = whole.graphics.window;
        self.text.window = whole.text.window;
        self.text.windowed = false;
        self.text.x = 0;
        self.text.y = 0;
        self.text.editing = None;
    }

    /// Writes it in the drivers' layout at `address`, a save area of
    /// [`SAVE_AREA_BYTES`].
    pub(crate) fn write(&self, os: &mut Os<'_>, address: u32) -> Result<(), Fail> {
        let mut words = Words::default();
        words.put(WRITTEN);
        self.encode(&mut words);
        for (i, &word) in (0u32..).zip(&words.words[..words.at]) {
            os.write_word(address.wrapping_add(4 * i), word)?;
        }
        Ok(())
    }

    /// The state the save area at `address` holds: none where nothing the
    /// drivers wrote is there.
    pub(crate) fn read(os: &mut Os<'_>, address: u32) -> Result<Option<Self>, Fail> {
        let mut words = Words::default();
        for (i, word) in (0u32..).zip(words.words.iter_mut()) {
            *word = os.read_word(address.wrapping_add(4 * i))?;
        }
        if words.take() != Some(WRITTEN) {
            return Ok(None);
        }
        Ok(Self::decode(&mut words))
    }

    fn encode(&self, words: &mut Words) {
        put_text(words, &self.text);
        put_graphics(words, &self.graphics);
        put_colours(words, &self.colours);
        let queue = &self.queue;
        words.put(u32::from_le_bytes([
            queue.code,
            queue.waiting,
            self.spool,
            self.streams,
        ]));
        words.bytes(&queue.gathered[..8]);
        words.put(u32::from(queue.gathered[8]));
        for &word in &self.changed {
            words.put(word);
        }
    }

    fn decode(words: &mut Words) -> Option<Self> {
        let text = take_text(words)?;
        let graphics = take_graphics(words)?;
        let colours = take_colours(words)?;
        let [code, waiting, spool, streams] = words.take()?.to_le_bytes();
        let mut gathered = [0u8; 9];
        gathered[..8].copy_from_slice(&words.take_bytes()?);
        gathered[8] = u8::try_from(words.take()?).ok()?;
        let awaited = 0u8.wrapping_sub(waiting);
        if waiting != 0 && awaited > crate::stream::parameters(code) {
            return None;
        }
        let mut changed = [0u32; 5];
        for word in &mut changed {
            *word = words.take()?;
        }
        Some(Self {
            text,
            graphics,
            colours,
            queue: Queue {
                code,
                gathered,
                waiting,
            },
            changed,
            spool,
            streams,
        })
    }
}

fn put_text(words: &mut Words, text: &Text) {
    words.window(text.window);
    words.put(text.x);
    words.put(text.y);
    words.put(u32::from_le_bytes([
        text.flags,
        u8::from(text.vdu5) | (u8::from(text.windowed) << 1) | (u8::from(text.paged) << 2),
        text.cursor.start,
        text.cursor.end,
    ]));
    words.put(u32::from(text.cursor.appearance) | (u32::from(text.cursor.shown) << 8));
    let (editing, (x, y)) = text.editing.map_or((0, (0, 0)), |at| (1, at));
    words.put(editing);
    words.put(x);
    words.put(y);
}

fn take_text(words: &mut Words) -> Option<Text> {
    let window = words.take_window()?;
    let (x, y) = (words.take()?, words.take()?);
    let [flags, bits, start, end] = words.take()?.to_le_bytes();
    let shape = words.take()?;
    let editing = match words.take()? {
        0 => {
            words.take()?;
            words.take()?;
            None
        }
        1 => Some((words.take()?, words.take()?)),
        _ => return None,
    };
    let appearance = u8::try_from(shape & 0xFF).ok().filter(|&a| a <= 3)?;
    let shown = flag(shape >> 8)?;
    if bits > 7 || start > 31 || end > 31 {
        return None;
    }
    let text = Text {
        window,
        x,
        y,
        flags,
        vdu5: bits & 1 != 0,
        windowed: bits & 2 != 0,
        paged: bits & 4 != 0,
        cursor: CursorShape {
            start,
            end,
            appearance,
            shown,
        },
        editing,
    };
    let placed =
        cursor_within(&text, x, y) && editing.is_none_or(|(ex, ey)| cursor_within(&text, ex, ey));
    placed.then_some(text)
}

fn put_graphics(words: &mut Words, graphics: &Graphics) {
    let window = graphics.window;
    for (x, y) in [
        (window.left, window.bottom),
        (window.right, window.top),
        graphics.origin,
        graphics.cursor,
        graphics.cursor_pixels,
        graphics.old,
        graphics.older,
        graphics.new_point,
    ] {
        words.pair(x, y);
    }
    for (across, down) in [graphics.char_size, graphics.char_space] {
        words.put(across);
        words.put(down);
    }
}

fn take_graphics(words: &mut Words) -> Option<Graphics> {
    let (left, bottom) = words.take_pair()?;
    let (right, top) = words.take_pair()?;
    if left < 0 || bottom < 0 || left > right || bottom > top {
        return None;
    }
    Some(Graphics {
        window: crate::changed::Rect {
            left,
            bottom,
            right,
            top,
        },
        origin: words.take_pair()?,
        cursor: words.take_pair()?,
        cursor_pixels: words.take_pair()?,
        old: words.take_pair()?,
        older: words.take_pair()?,
        new_point: words.take_pair()?,
        char_size: (words.character()?, words.character()?),
        char_space: (words.character()?, words.character()?),
    })
}

fn put_colours(words: &mut Words, colours: &Colours) {
    words.gcol(&colours.fore);
    words.gcol(&colours.back);
    words.text_colour(&colours.text_fore);
    words.text_colour(&colours.text_back);
    for pattern in &colours.patterns {
        words.bytes(pattern);
    }
    words.put(u32::from(colours.native));
    words.put(u32::try_from(colours.dot & u64::from(u32::MAX)).unwrap_or(0));
    words.put(u32::try_from(colours.dot >> 32).unwrap_or(0));
    words.put(colours.dot_length);
    words.put(colours.dot_at);
    words.pair(colours.ecf_origin.0, colours.ecf_origin.1);
}

fn take_colours(words: &mut Words) -> Option<Colours> {
    let fore = words.take_gcol()?;
    let back = words.take_gcol()?;
    let text_fore = words.take_text_colour()?;
    let text_back = words.take_text_colour()?;
    let mut patterns = [[0u8; 8]; 4];
    for pattern in &mut patterns {
        *pattern = words.take_bytes()?;
    }
    let native = flag(words.take()?)?;
    let (low, high) = (words.take()?, words.take()?);
    Some(Colours {
        fore,
        back,
        text_fore,
        text_back,
        patterns,
        native,
        dot: (u64::from(high) << 32) | u64::from(low),
        dot_length: words.take().filter(|&n| (1..=MOST_DOTS).contains(&n))?,
        dot_at: words.take().filter(|&n| n < MOST_DOTS)?,
        ecf_origin: words.take_pair()?,
    })
}

/// Whether `(x, y)` along the window's axes is a place the text cursor can
/// be: within the window, or held one past its last column.
fn cursor_within(text: &Text, x: u32, y: u32) -> bool {
    let (width, height) = text.extent();
    x <= width && y < height
}

/// Nought or one as a flag.
fn flag(word: u32) -> Option<bool> {
    match word {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// The layout's words, written or read in order.
struct Words {
    words: [u32; WORDS],
    at: usize,
}

impl Default for Words {
    fn default() -> Self {
        Self {
            words: [0; WORDS],
            at: 0,
        }
    }
}

impl Words {
    fn put(&mut self, word: u32) {
        if let Some(slot) = self.words.get_mut(self.at) {
            *slot = word;
            self.at += 1;
        }
    }

    fn take(&mut self) -> Option<u32> {
        let word = *self.words.get(self.at)?;
        self.at += 1;
        Some(word)
    }

    fn pair(&mut self, a: i32, b: i32) {
        self.put(a.cast_unsigned());
        self.put(b.cast_unsigned());
    }

    fn take_pair(&mut self) -> Option<(i32, i32)> {
        Some((self.take()?.cast_signed(), self.take()?.cast_signed()))
    }

    fn window(&mut self, window: Window) {
        self.put(window.left);
        self.put(window.bottom);
        self.put(window.right);
        self.put(window.top);
    }

    fn take_window(&mut self) -> Option<Window> {
        let window = Window {
            left: self.take()?,
            bottom: self.take()?,
            right: self.take()?,
            top: self.take()?,
        };
        let ordered = window.left <= window.right && window.top <= window.bottom;
        (ordered && window.right <= MOST_CELLS && window.bottom <= MOST_CELLS).then_some(window)
    }

    fn bytes(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(4) {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            self.put(u32::from_le_bytes(word));
        }
    }

    fn take_bytes(&mut self) -> Option<[u8; 8]> {
        let (a, b) = (self.take()?.to_le_bytes(), self.take()?.to_le_bytes());
        Some([a[0], a[1], a[2], a[3], b[0], b[1], b[2], b[3]])
    }

    fn character(&mut self) -> Option<u32> {
        self.take().filter(|&n| n <= MOST_CHARACTER)
    }

    fn gcol(&mut self, gcol: &Gcol) {
        self.put(gcol.action);
        self.put(gcol.colour);
        self.put(gcol.tint);
        self.put(u32::from(gcol.literal) | u32::from(gcol.pattern.is_some()) << 1);
        for &word in &gcol.pattern.unwrap_or([0; 8]) {
            self.put(word);
        }
    }

    fn take_gcol(&mut self) -> Option<Gcol> {
        let action = self.take().filter(|&a| a <= 0xFF)?;
        let colour = self.take()?;
        let tint = self.take().filter(|&t| t <= 0xFF)?;
        let bits = self.take().filter(|&b| b <= 3)?;
        let mut pattern = [0u32; 8];
        for word in &mut pattern {
            *word = self.take()?;
        }
        Some(Gcol {
            action,
            colour,
            tint,
            literal: bits & 1 != 0,
            pattern: (bits & 2 != 0).then_some(pattern),
        })
    }

    fn text_colour(&mut self, colour: &TextColour) {
        self.put(colour.colour);
        self.put(colour.tint);
        self.put(u32::from(colour.literal));
    }

    fn take_text_colour(&mut self) -> Option<TextColour> {
        let colour = self.take()?;
        let tint = self.take().filter(|&t| t <= 0xFF)?;
        let literal = flag(self.take()?)?;
        Some(TextColour {
            colour,
            tint,
            literal,
        })
    }
}

#[cfg(test)]
mod tests;
