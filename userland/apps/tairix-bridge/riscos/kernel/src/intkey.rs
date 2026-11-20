//! InternationalKeyboard (`plans/RISCOS-EMULATOR.md` §4.6): the key handler
//! a keyboard's keys go through, in the layout of the keyboard `OS_Byte` 71
//! selects — TAIRiX's own design of each country's, in [`layouts`].
//!
//! The handler's header and tables are in its workspace, for the kernel and
//! any program to read; its code is Rust. A character is given in the
//! alphabet `OS_Byte` 71 reads, as International's table for it has it, or
//! in UTF-8; a dead key's accent is composed with the next character by
//! Unicode's canonical composition. Alt with the keypad's digits gives a
//! character by its decimal number, as the PRM's Table D describes;
//! Alt, Ctrl and F1 select the UK keyboard, F2 the configured one, and F12
//! the keyboard whose country's dialling code Alt and the digits then give.

mod layouts;

use tairix_inline::ArrayVec;
use tairix_unicode::{normalize, Form};

use crate::byte::var;
use crate::international::{
    self, reason, ALPHABET_BYTE, READ_ALPHABET, READ_KEYBOARD, SET_KEYBOARD, UTF8,
};
use crate::keyboard::{status, KEY_HANDLER, NO_KEYBOARD};
use crate::memory::Bits;
use crate::module::{Builtin, Services};
use crate::os::{len32, Entry, Fail, Os, Regs, Unwind};
use crate::swi::{os, X};
use layouts::{Layout, FALLBACK, LAYOUTS};

/// `Service_Reset`.
const RESET: u32 = 0x27;
/// The keyboards whose keys it knows: an Archimedes', a PC's, an RCMM's,
/// and none yet, which numbers its keys as they do.
const KEYBOARDS: [u8; 4] = [1, 2, 3, NO_KEYBOARD];
/// The keys its translation table covers.
const KEYS: u32 = 0x70;
/// A key with no code of its own in the translation table.
const SPECIAL: u8 = 0xFF;
/// What its special code is asked: a shifting key up or down, a key's first
/// code.
const UP: u32 = 0;
const DOWN: u32 = 1;
const FIRST: u32 = 2;

/// Where its workspace keeps the handler: the header, the list a call
/// gives, the translation table, the internal keys' table, the shifting and
/// special keys' lists, and the offsets of the special keys' code.
const LIST_AT: u32 = 32;
const LIST: usize = 15;
const TRANSLATION_AT: u32 = LIST_AT + 1 + len32(LIST);
const INKEY_AT: u32 = TRANSLATION_AT + 4 * KEYS;
const SHIFTING_AT: u32 = INKEY_AT + 4 * 128;
const SPECIAL_AT: u32 = SHIFTING_AT + 16;
const CODES_AT: u32 = SPECIAL_AT + 1 + SPECIALS_MOST;
const WORKSPACE: u32 = CODES_AT + 4 * SPECIALS_MOST;
/// The most special keys a layout has: one for each key the handler knows.
const SPECIALS_MOST: u32 = key::ADJUST + 1;

/// Its routines: the handler's initialisation, its code for a pending Alt
/// and for a special key, and the callback that selects a keyboard.
const INIT: usize = 0;
const PENDING_ALT: usize = 1;
const SPECIAL_KEY: usize = 2;
const SELECT: usize = 3;

/// The low-level keys it names.
mod key {
    pub(super) const F1: u32 = 0x01;
    pub(super) const F2: u32 = 0x02;
    pub(super) const F12: u32 = 0x0C;
    pub(super) const SCROLL_LOCK: u32 = 0x0E;
    pub(super) const BREAK: u32 = 0x0F;
    pub(super) const NUM_LOCK: u32 = 0x22;
    pub(super) const TAB: u32 = 0x26;
    pub(super) const LEFT_CTRL: u32 = 0x3B;
    pub(super) const LEFT_SHIFT: u32 = 0x4C;
    pub(super) const RIGHT_SHIFT: u32 = 0x58;
    pub(super) const CAPS_LOCK: u32 = 0x5D;
    pub(super) const LEFT_ALT: u32 = 0x5E;
    pub(super) const SPACE: u32 = 0x5F;
    pub(super) const RIGHT_ALT: u32 = 0x60;
    pub(super) const RIGHT_CTRL: u32 = 0x61;
    pub(super) const KANA: u32 = 0x6D;
    pub(super) const FN: u32 = 0x6F;
    pub(super) const SELECT: u32 = 0x70;
    pub(super) const MENU: u32 = 0x71;
    pub(super) const ADJUST: u32 = 0x72;
}

/// The keys the kernel calls going down and up.
const SHIFTING: [u32; 11] = [
    key::LEFT_SHIFT,
    key::RIGHT_SHIFT,
    key::LEFT_CTRL,
    key::RIGHT_CTRL,
    key::LEFT_ALT,
    key::RIGHT_ALT,
    key::FN,
    key::ADJUST,
    key::MENU,
    key::SELECT,
    key::BREAK,
];

/// Codes of keys every layout shares: plain, with Shift, with Ctrl, with
/// both. The editing keys' are the keypad's too.
const fn function(n: u8) -> [u8; 4] {
    [0x80 + n, 0x90 + n, 0xA0 + n, 0xB0 + n]
}
const RETURN: [u8; 4] = [0x0D; 4];
const INSERT: [u8; 4] = [0xCD, 0xDD, 0xED, 0xFD];
const HOME: [u8; 4] = [0x1E; 4];
const PAGE_UP: [u8; 4] = [0x9F, 0x8F, 0xBF, 0xAF];
const DELETE: [u8; 4] = [0x7F; 4];
const END: [u8; 4] = [0x8B, 0x9B, 0xAB, 0xBB];
const PAGE_DOWN: [u8; 4] = [0x9E, 0x8E, 0xBE, 0xAE];
const CURSOR_UP: [u8; 4] = [0x8F, 0x9F, 0xAF, 0xBF];
const CURSOR_LEFT: [u8; 4] = [0x8C, 0x9C, 0xAC, 0xBC];
const CURSOR_DOWN: [u8; 4] = [0x8E, 0x9E, 0xAE, 0xBE];
const CURSOR_RIGHT: [u8; 4] = [0x8D, 0x9D, 0xAD, 0xBD];

/// The keys every layout gives the same codes, and the codes.
const FIXED: [(u32, [u8; 4]); 29] = [
    (0x00, [0x1B; 4]),
    (key::F1, function(1)),
    (key::F2, function(2)),
    (0x03, function(3)),
    (0x04, function(4)),
    (0x05, function(5)),
    (0x06, function(6)),
    (0x07, function(7)),
    (0x08, function(8)),
    (0x09, function(9)),
    (0x0A, [0xCA, 0xDA, 0xEA, 0xFA]),
    (0x0B, [0xCB, 0xDB, 0xEB, 0xFB]),
    (key::F12, [0xCC, 0xDC, 0xEC, 0xFC]),
    (0x0D, function(0)),
    (0x1E, [0x08; 4]),
    (0x1F, INSERT),
    (0x20, HOME),
    (0x21, PAGE_UP),
    (0x34, DELETE),
    (0x35, END),
    (0x36, PAGE_DOWN),
    (0x47, RETURN),
    (0x59, CURSOR_UP),
    (0x62, CURSOR_LEFT),
    (0x63, CURSOR_DOWN),
    (0x64, CURSOR_RIGHT),
    (0x68, [0xC0, 0xD0, 0xE0, 0xF0]),
    (0x69, [0xD0, 0xC0, 0xF0, 0xE0]),
    (0x6A, [0xC1, 0xD1, 0xE1, 0xF1]),
];

/// A keypad key: its character with Num Lock on, its code with it off, and
/// the digit it gives an Alt entry, if it is one of the ten.
#[derive(Debug, PartialEq, Eq)]
struct Pad {
    key: u32,
    on: u8,
    off: Option<u8>,
    digit: Option<u32>,
}

const fn pad(key: u32, on: u8, off: Option<u8>, digit: Option<u32>) -> Pad {
    Pad {
        key,
        on,
        off,
        digit,
    }
}

/// The keypad's decimal key, whose character with Num Lock on is the
/// layout's.
const DECIMAL: u32 = 0x66;
/// The keypad's keys.
const KEYPAD: [Pad; 17] = [
    pad(0x23, b'/', Some(b'/'), None),
    pad(0x24, b'*', Some(b'*'), None),
    pad(0x25, b'#', Some(b'#'), None),
    pad(0x37, b'7', Some(HOME[0]), Some(7)),
    pad(0x38, b'8', Some(CURSOR_UP[0]), Some(8)),
    pad(0x39, b'9', Some(PAGE_UP[0]), Some(9)),
    pad(0x3A, b'-', Some(b'-'), None),
    pad(0x48, b'4', Some(CURSOR_LEFT[0]), Some(4)),
    pad(0x49, b'5', None, Some(5)),
    pad(0x4A, b'6', Some(CURSOR_RIGHT[0]), Some(6)),
    pad(0x4B, b'+', Some(b'+'), None),
    pad(0x5A, b'1', Some(END[0]), Some(1)),
    pad(0x5B, b'2', Some(CURSOR_DOWN[0]), Some(2)),
    pad(0x5C, b'3', Some(PAGE_DOWN[0]), Some(3)),
    pad(0x65, b'0', Some(INSERT[0]), Some(0)),
    pad(DECIMAL, b'.', Some(DELETE[0]), None),
    pad(0x67, RETURN[0], Some(RETURN[0]), None),
];

/// Each low-level key's internal key number, as a PC keyboard's keys read.
const INTERNAL: [u8; 0x73] = [
    112, 113, 114, 115, 20, 116, 117, 22, 118, 119, 30, 28, 29, 32, 31, 44, // &00
    45, 48, 49, 17, 18, 19, 52, 36, 21, 38, 39, 23, 93, 46, 47, 61, // &10
    62, 63, 77, 74, 91, 90, 96, 16, 33, 34, 51, 35, 68, 53, 37, 54, // &20
    55, 56, 88, 120, 89, 105, 78, 27, 42, 43, 59, 4, 65, 81, 50, 67, // &30
    83, 84, 69, 70, 86, 87, 79, 73, 122, 123, 26, 58, 3, 94, 97, 66, // &40
    82, 99, 100, 85, 101, 102, 103, 104, 6, 57, 107, 124, 108, 64, 5, 98, // &50
    8, 7, 25, 41, 121, 106, 76, 60, 125, 126, 127, 109, 110, 111, 95, 12, // &60
    9, 10, 11, // &70
];
/// The internal keys that also read other keys: Shift, Ctrl and Alt either
/// side, and the BBC Micro's places of four.
const ALSO: [(u8, u32); 10] = [
    (0, key::RIGHT_SHIFT),
    (0, key::LEFT_SHIFT),
    (1, key::RIGHT_CTRL),
    (1, key::LEFT_CTRL),
    (2, key::RIGHT_ALT),
    (2, key::LEFT_ALT),
    (24, 0x16),
    (40, 0x1B),
    (71, 0x12),
    (72, 0x45),
];

/// The accents dead keys carry, by combining mark, and each one's spacing
/// form, which the space after its dead key gives.
const ACCENTS: [(char, char); 13] = [
    ('\u{300}', '`'),
    ('\u{301}', '\u{B4}'),
    ('\u{302}', '^'),
    ('\u{303}', '~'),
    ('\u{304}', '\u{AF}'),
    ('\u{306}', '\u{2D8}'),
    ('\u{307}', '\u{2D9}'),
    ('\u{308}', '\u{A8}'),
    ('\u{30A}', '\u{B0}'),
    ('\u{30B}', '\u{2DD}'),
    ('\u{30C}', '\u{2C7}'),
    ('\u{327}', '\u{B8}'),
    ('\u{328}', '\u{2DB}'),
];
/// What the layouts write for no character.
const NOTHING: char = '∅';
/// What the space bar gives with Alt.
const NO_BREAK_SPACE: char = '\u{A0}';

/// The countries' dialling codes an Alt entry gives after Alt, Ctrl and
/// F12, and the keyboards they select.
const DIALLING: [(u32, u32); 26] = [
    (1, 48),
    (7, 24),
    (27, 52),
    (30, 10),
    (31, 34),
    (32, 31),
    (33, 6),
    (34, 5),
    (39, 4),
    (41, 35),
    (43, 30),
    (44, 1),
    (45, 14),
    (46, 11),
    (47, 15),
    (49, 7),
    (52, 27),
    (61, 29),
    (81, 32),
    (90, 20),
    (351, 8),
    (353, 22),
    (354, 16),
    (358, 12),
    (852, 23),
    (972, 26),
];

/// InternationalKeyboard.
pub(crate) static INTERNATIONAL_KEYBOARD: Builtin = Builtin {
    title: "InternationalKeyboard",
    help_name: Some("Key Layouts"),
    bridge: false,
    swis: None,
    services: Some(Services {
        numbers: &[RESET, international::SERVICE, KEY_HANDLER],
        handler: service,
    }),
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[handler_init, pending_alt, special_key, select],
    vectors: &[],
    resources: &[],
    code: None,
};

/// The number typed in decimal with Alt held: its value so far, whether a
/// digit was typed, and whether it is a dialling code.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct AltNumber {
    value: u32,
    digits: bool,
    dial: bool,
}

/// InternationalKeyboard's state.
pub(crate) struct State {
    workspace: Option<u32>,
    /// The handler installed before its own.
    previous: u32,
    /// The layout its tables were made for.
    layout: usize,
    /// The Shift, Ctrl and Alt keys held, and the left Alt where it switches
    /// layers.
    shift: u8,
    ctrl: u8,
    alt: u8,
    left_alt: bool,
    /// The mouse buttons held: Adjust, Menu, Select from bit 0.
    buttons: u8,
    /// The dead key's accent awaiting a character.
    accent: Option<char>,
    entry: AltNumber,
    /// The keyboard a callback is to select, as `OS_Byte` 71 sets it.
    selecting: Option<u32>,
}

impl State {
    pub(crate) const fn new() -> Self {
        Self {
            workspace: None,
            previous: u32::MAX,
            layout: FALLBACK,
            shift: 0,
            ctrl: 0,
            alt: 0,
            left_alt: false,
            buttons: 0,
            accent: None,
            entry: AltNumber {
                value: 0,
                digits: false,
                dial: false,
            },
            selecting: None,
        }
    }
}

/// What a key gives at one level.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Level {
    Nothing,
    Char(char),
    Dead(char),
}

/// A key's levels as a layout writes them.
fn levels(token: &str) -> [Level; 4] {
    let mut levels = [Level::Nothing; 4];
    let mut count = 0;
    for (slot, c) in levels.iter_mut().zip(token.chars()) {
        *slot = match c {
            NOTHING => Level::Nothing,
            c if spacing(c).is_some() => Level::Dead(c),
            c => Level::Char(c),
        };
        count += 1;
    }
    if count == 1 {
        levels[1] = levels[0];
    }
    levels
}

/// The spacing form of the accent combining mark `mark` carries.
fn spacing(mark: char) -> Option<char> {
    ACCENTS.iter().find(|(m, _)| *m == mark).map(|&(_, s)| s)
}

/// `key`'s levels in `layer` of `layout`.
fn key_levels(layout: &Layout, layer: usize, key: u32) -> Option<[Level; 4]> {
    if key == key::SPACE {
        let space = Level::Char(' ');
        let hard = Level::Char(NO_BREAK_SPACE);
        return Some([space, space, hard, hard]);
    }
    layout.token(layer, key).map(levels)
}

/// The ASCII character `level` gives, if it gives one.
fn ascii(level: Level) -> Option<u8> {
    match level {
        Level::Char(c) if c.is_ascii() => u8::try_from(c).ok(),
        _ => None,
    }
}

/// What `key` gives with Ctrl: the control code of its plain or shifted
/// character where that is a letter or one of `@[\]^_`, else its digit's
/// value, else the control code of a letter elsewhere on it — another
/// level's or layer's — so Ctrl with a key reads alike in every script. The
/// space bar gives a space.
fn control(layout: &Layout, key: u32) -> Option<u8> {
    if key == key::SPACE {
        return Some(b' ');
    }
    let first = key_levels(layout, 0, key)?;
    let letter = |b: u8| b.is_ascii_alphabetic();
    for level in &first[..2] {
        match ascii(*level) {
            Some(b) if letter(b) || matches!(b, b'@' | b'[' | b'\\' | b']' | b'^' | b'_') => {
                return Some(b & 0x1F)
            }
            _ => {}
        }
    }
    if let Some(digit) = first[..2]
        .iter()
        .find_map(|&l| ascii(l).filter(u8::is_ascii_digit))
    {
        return Some(digit - b'0');
    }
    let others = (1..layout.layers.len()).filter_map(|layer| key_levels(layout, layer, key));
    first[2..]
        .iter()
        .copied()
        .chain(others.flatten())
        .find_map(|l| ascii(l).filter(|&b| letter(b)))
        .map(|b| b & 0x1F)
}

/// The kernel's codes for `key`, where it can make them itself: the same in
/// every layer, nothing by Alt, plain and shifted characters ASCII, a letter
/// only with its capital, and a code with Ctrl — so the kernel's Caps Lock
/// is the handler's.
fn simple_codes(layout: &Layout, key: u32) -> Option<[u8; 4]> {
    let first = key_levels(layout, 0, key)?;
    if (1..layout.layers.len()).any(|layer| key_levels(layout, layer, key) != Some(first)) {
        return None;
    }
    let [plain, shifted, Level::Nothing, Level::Nothing] = first else {
        return None;
    };
    let (plain, shifted) = (ascii(plain)?, ascii(shifted)?);
    if plain.is_ascii_control() || shifted.is_ascii_control() {
        return None;
    }
    let lettered = plain.is_ascii_alphabetic() || shifted.is_ascii_alphabetic();
    if lettered && !(plain.is_ascii_lowercase() && shifted == plain.to_ascii_uppercase()) {
        return None;
    }
    let ctrl = control(layout, key)?;
    Some([plain, shifted, ctrl, ctrl])
}

/// What a key is to the handler.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    /// Its codes are the kernel's to make.
    Simple([u8; 4]),
    Shift,
    Ctrl,
    Alt,
    /// The left Alt where it switches layers.
    LayerAlt,
    Fn,
    /// A mouse button, by its bit.
    Button(u8),
    Break,
    CapsLock,
    NumLock,
    ScrollLock,
    Kana,
    Keypad(&'static Pad),
    Tab,
    Character,
    Nothing,
}

impl Class {
    fn of(layout: &Layout, key: u32) -> Self {
        match key {
            key::LEFT_SHIFT | key::RIGHT_SHIFT => Self::Shift,
            key::LEFT_CTRL | key::RIGHT_CTRL => Self::Ctrl,
            key::LEFT_ALT if layout.left_alt_switches => Self::LayerAlt,
            key::LEFT_ALT | key::RIGHT_ALT => Self::Alt,
            key::FN => Self::Fn,
            key::SELECT => Self::Button(4),
            key::MENU => Self::Button(2),
            key::ADJUST => Self::Button(1),
            key::BREAK => Self::Break,
            key::CAPS_LOCK => Self::CapsLock,
            key::NUM_LOCK => Self::NumLock,
            key::SCROLL_LOCK => Self::ScrollLock,
            key::KANA if layout.layers.len() > 1 => Self::Kana,
            key::TAB => Self::Tab,
            key::SPACE => Self::Character,
            _ => {
                if let Some(&(_, codes)) = FIXED.iter().find(|(k, _)| *k == key) {
                    Self::Simple(codes)
                } else if let Some(pad) = KEYPAD.iter().find(|p| p.key == key) {
                    Self::Keypad(pad)
                } else if let Some(codes) = simple_codes(layout, key) {
                    Self::Simple(codes)
                } else if layout.token(0, key).is_some() {
                    Self::Character
                } else {
                    Self::Nothing
                }
            }
        }
    }
}

/// The layer the status selects: the second, where there is one, while
/// `NoKanaLock` is clear.
fn layer_of(layout: &Layout, state: u8) -> usize {
    if state & status::NO_KANA_LOCK == 0 {
        layout.layers.len().saturating_sub(1).min(1)
    } else {
        0
    }
}

/// The level of `levels` from `base` — plain or Alt — that Shift and Caps
/// Lock select: Caps Lock gives a lower-case letter's capital, its shifted
/// level where that is a capital, and in Shift-Caps mode Shift then gives
/// the letter.
fn with_caps(levels: &[Level; 4], base: usize, shift: bool, state: u8) -> Level {
    let (lower, upper) = (levels[base], levels[base + 1]);
    let chosen = if shift { upper } else { lower };
    if state & status::NO_CAPS_LOCK != 0 {
        return chosen;
    }
    let pair = matches!((lower, upper), (Level::Char(l), Level::Char(u)) if l.is_lowercase() && u.is_uppercase());
    if shift {
        return if pair && state & status::SHIFT_CAPS != 0 {
            lower
        } else {
            chosen
        };
    }
    if pair {
        return upper;
    }
    match lower {
        Level::Char(l) if l.is_lowercase() => {
            let mut capital = l.to_uppercase();
            match (capital.next(), capital.next()) {
                (Some(c), None) => Level::Char(c),
                _ => lower,
            }
        }
        _ => lower,
    }
}

/// `c` with the accent `mark` composed onto it, and the character to give
/// should the composed one have no code: a space gives the accent alone.
fn accented(c: char, mark: char) -> (char, Option<char>) {
    if c == ' ' {
        return (spacing(mark).unwrap_or(c), Some(c));
    }
    let mut composed = ['\0'; 4];
    match normalize([c, mark], Form::Nfc, &mut composed) {
        Ok(1) => (composed[0], Some(c)),
        _ => (c, None),
    }
}

/// The keyboard a dialling code selects.
fn dialled(code: u32) -> Option<u32> {
    DIALLING
        .iter()
        .find(|&&(dial, _)| dial == code)
        .map(|&(_, keyboard)| keyboard)
}

/// The layout keyboard `number` uses.
fn layout_for(number: u32) -> usize {
    LAYOUTS
        .iter()
        .position(|layout| layout.keyboards.contains(&number))
        .unwrap_or(FALLBACK)
}

/// The bytes a call gives the keyboard buffer.
type Output = ArrayVec<u8, LIST>;

impl Os<'_> {
    /// `OS_InstallKeyHandler` with R0: its R0.
    fn key_handler_call(&mut self, r0: u32) -> Result<u32, Fail> {
        let mut regs = Regs::default();
        regs.r[0] = r0;
        self.perform(X | os::INSTALL_KEY_HANDLER, &mut regs)?;
        Ok(regs.r[0])
    }

    /// `c` as the alphabet gives it to the keyboard buffer: whether it has
    /// a code there.
    fn encode_key(&mut self, c: char, out: &mut Output) -> Result<bool, Fail> {
        if let Some(ascii) = u8::try_from(c).ok().filter(u8::is_ascii) {
            give_code(ascii, out);
            return Ok(true);
        }
        let code = u32::from(c);
        let alphabet = self.os_byte(ALPHABET_BYTE, READ_ALPHABET)?;
        if alphabet == UTF8 {
            let mut utf8 = [0; 4];
            let bytes = c.encode_utf8(&mut utf8).as_bytes();
            if out.remaining_capacity() < 2 * bytes.len() {
                return Ok(false);
            }
            for &byte in bytes {
                let _ = out.try_extend_from_slice(&[0, byte]);
            }
            return Ok(true);
        }
        let mut ask = Regs::default();
        ask.r[1..4].copy_from_slice(&[international::SERVICE, reason::UCS_TABLE, alphabet]);
        self.service(&mut ask)?;
        let found = if ask.r[1] == 0 {
            let table = ask.r[4];
            let mut found = None;
            for byte in 0x80..=0xFF_u8 {
                if self.read_word(table.wrapping_add(4 * u32::from(byte)))? == code {
                    found = Some(byte);
                    break;
                }
            }
            found
        } else {
            u8::try_from(code).ok()
        };
        Ok(found.is_some_and(|byte| out.try_extend_from_slice(&[0, byte]).is_ok()))
    }

    /// `c` given, the pending accent composed onto it and used up: the
    /// character without it where the accented one has no code.
    fn give_char(&mut self, c: char, state: &mut u8, out: &mut Output) -> Result<(), Fail> {
        let keys = &mut self.world.intkey;
        let accent = keys.accent.take();
        if keys.alt == 0 {
            *state &= !status::PENDING_ALT;
        }
        let (c, fallback) = accent.map_or((c, None), |mark| accented(c, mark));
        if !self.encode_key(c, out)? {
            if let Some(base) = fallback {
                self.encode_key(base, out)?;
            }
        }
        Ok(())
    }

    /// The keyboard `request`, as `OS_Byte` 71 sets one, selected by a
    /// callback, out of the handler.
    fn select_later(&mut self, request: u32) -> Result<(), Fail> {
        if self.world.intkey.selecting.is_some() {
            self.world.intkey.selecting = Some(request);
            return Ok(());
        }
        let (Some(callback), Some(workspace)) = (
            self.routine_of(&INTERNATIONAL_KEYBOARD, SELECT),
            self.world.intkey.workspace,
        ) else {
            return Ok(());
        };
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[callback, workspace]);
        self.perform(X | os::ADD_CALLBACK, &mut regs)?;
        self.world.intkey.selecting = Some(request);
        Ok(())
    }

    /// The Alt entry ended with the last Alt key: its character, or its
    /// dialling code's keyboard.
    fn end_entry(&mut self, out: &mut Output) -> Result<(), Fail> {
        let entry = core::mem::take(&mut self.world.intkey.entry);
        if !entry.digits {
            return Ok(());
        }
        if entry.dial {
            return match dialled(entry.value) {
                Some(keyboard) => self.select_later(SET_KEYBOARD | keyboard),
                None => Ok(()),
            };
        }
        if self.os_byte(ALPHABET_BYTE, READ_ALPHABET)? == UTF8 {
            if let Some(c) = char::from_u32(entry.value & 0x7FFF_FFFF) {
                self.encode_key(c, out)?;
            }
            return Ok(());
        }
        let _ = out.try_extend_from_slice(&[0, entry.value.to_le_bytes()[0]]);
        Ok(())
    }

    /// A keypad key with Alt held: a digit of the entry, or the entry
    /// abandoned.
    fn entry_digit(&mut self, pad: &Pad) {
        let entry = &mut self.world.intkey.entry;
        if let Some(digit) = pad.digit {
            entry.value = entry.value.wrapping_mul(10).wrapping_add(digit);
            entry.digits = true;
        } else {
            *entry = AltNumber::default();
        }
    }

    /// The handler's header and the tables every layout shares, written to
    /// its workspace at `workspace`: the internal keys', the shifting keys'
    /// list, and every special key's code, which is one routine.
    fn write_fixed_tables(&mut self, workspace: u32) -> Result<(), Fail> {
        let routine = |os: &Self, index| os.routine_of(&INTERNATIONAL_KEYBOARD, index).unwrap_or(0);
        let (init, pending, special) = (
            routine(self, INIT),
            routine(self, PENDING_ALT),
            routine(self, SPECIAL_KEY),
        );
        let header = [
            TRANSLATION_AT,
            KEYS,
            INKEY_AT,
            SHIFTING_AT,
            SPECIAL_AT,
            CODES_AT,
            init.wrapping_sub(workspace),
            pending.wrapping_sub(workspace),
        ];
        for (i, word) in (0..).zip(header) {
            self.write_word(workspace + 4 * i, word)?;
        }
        let mut inkey = [u32::MAX; 128];
        for (low, &internal) in (0..).zip(INTERNAL.iter()) {
            add_internal(&mut inkey, internal, low);
        }
        for &(internal, low) in &ALSO {
            add_internal(&mut inkey, internal, low);
        }
        for (i, word) in (0..).zip(inkey) {
            self.write_word(workspace + INKEY_AT + 4 * i, word)?;
        }
        let mut shifting = ArrayVec::<u8, 16>::new();
        let _ = shifting.try_push(u8::try_from(SHIFTING.len()).unwrap_or(0));
        for key in SHIFTING.iter().filter_map(|&key| u8::try_from(key).ok()) {
            let _ = shifting.try_push(key);
        }
        self.write_bytes(workspace + SHIFTING_AT, shifting.as_slice())?;
        let at = workspace + CODES_AT;
        for i in 0..SPECIALS_MOST {
            self.write_word(at + 4 * i, special.wrapping_sub(at))?;
        }
        Ok(())
    }

    /// The tables `layout` changes, written to the workspace at
    /// `workspace`: the translation table and the special keys' list.
    fn write_layout_tables(&mut self, workspace: u32, layout: &Layout) -> Result<(), Fail> {
        let mut translation = [SPECIAL; 4 * KEYS as usize];
        let mut specials = ArrayVec::<u8, { SPECIALS_MOST as usize }>::new();
        for key in 0..=key::ADJUST {
            match Class::of(layout, key) {
                Class::Simple(codes) if key < KEYS => {
                    translation[4 * key as usize..][..4].copy_from_slice(&codes);
                }
                Class::Nothing => {}
                _ => {
                    if let Ok(key) = u8::try_from(key) {
                        let _ = specials.try_push(key);
                    }
                }
            }
        }
        self.write_bytes(workspace + TRANSLATION_AT, &translation)?;
        self.write_byte(
            workspace + SPECIAL_AT,
            u8::try_from(specials.len()).unwrap_or(0),
        )?;
        self.write_bytes(workspace + SPECIAL_AT + 1, specials.as_slice())?;
        Ok(())
    }

    /// The handler made ready for the keyboard and its number: its tables
    /// for the layout, and itself installed where no other handler is.
    /// Whether it is the handler, for a keyboard it knows.
    fn set_up_keyboard(&mut self) -> Result<bool, Fail> {
        let Some(workspace) = self.world.intkey.workspace else {
            return Ok(false);
        };
        let id = self.key_handler_call(1)?;
        let installed = self.key_handler_call(0)?;
        if !KEYBOARDS.iter().any(|&k| u32::from(k) == id) {
            if installed == workspace {
                let previous = self.world.intkey.previous;
                self.key_handler_call(previous)?;
            }
            return Ok(false);
        }
        if installed != workspace && installed != u32::MAX {
            return Ok(false);
        }
        let number = self.os_byte(ALPHABET_BYTE, READ_KEYBOARD)?;
        let layout = layout_for(number);
        self.world.intkey.layout = layout;
        self.write_layout_tables(workspace, &LAYOUTS[layout])?;
        if installed != workspace {
            self.world.intkey.previous = installed;
            self.key_handler_call(workspace)?;
        }
        Ok(true)
    }
}

/// A code as the kernel gives one itself: nought as two noughts.
fn give_code(code: u8, out: &mut Output) {
    let bytes = if code == 0 { &[0, 0][..] } else { &[code][..] };
    let _ = out.try_extend_from_slice(bytes);
}

/// Lists low-level key `low` in internal key `internal`'s word of the table
/// `INKEY` reads, where the word has room.
fn add_internal(table: &mut [u32; 128], internal: u8, low: u32) {
    let Some(word) = table.get_mut(127 - usize::from(internal & 0x7F)) else {
        return;
    };
    let Ok(low) = u8::try_from(low) else {
        return;
    };
    let mut bytes = word.to_le_bytes();
    if let Some(slot) = bytes.iter_mut().find(|b| **b == 0xFF) {
        *slot = low;
    }
    *word = u32::from_le_bytes(bytes);
}

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(WORKSPACE)?;
    os.write_word(regs.r[12], workspace)?;
    os.world.intkey = State::new();
    os.world.intkey.workspace = Some(workspace);
    os.write_fixed_tables(workspace)?;
    let number = os.os_byte(ALPHABET_BYTE, READ_KEYBOARD)?;
    os.os_byte(ALPHABET_BYTE, number | SET_KEYBOARD)?;
    os.set_up_keyboard()?;
    Ok(())
}

fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let Some(workspace) = os.world.intkey.workspace else {
        return Ok(());
    };
    if os.world.intkey.selecting.is_some() {
        if let Some(callback) = os.routine_of(&INTERNATIONAL_KEYBOARD, SELECT) {
            let mut regs = Regs::default();
            regs.r[..2].copy_from_slice(&[callback, workspace]);
            os.perform_quietly(X | os::REMOVE_CALLBACK, &mut regs)?;
        }
    }
    if os.key_handler_call(0)? == workspace {
        let previous = os.world.intkey.previous;
        os.key_handler_call(previous)?;
    }
    os.world.intkey = State::new();
    Ok(())
}

fn service(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    let set_up = match regs.r[1] {
        RESET | KEY_HANDLER => true,
        international::SERVICE => regs.r[2] == reason::KEYBOARD,
        _ => false,
    };
    if !set_up {
        return Ok(());
    }
    match os.set_up_keyboard() {
        Ok(known) => {
            if known && regs.r[1] == KEY_HANDLER {
                regs.r[1] = 0;
            }
            Ok(())
        }
        Err(Fail::Unwind(unwind)) => Err(unwind),
        Err(Fail::Error(_)) => Ok(()),
    }
}

/// The handler's initialisation: no key held, nothing pending, the first
/// layer.
#[expect(clippy::unnecessary_wraps, reason = "a key handler entry's signature")]
fn handler_init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let keys = &mut os.world.intkey;
    keys.shift = 0;
    keys.ctrl = 0;
    keys.alt = 0;
    keys.left_alt = false;
    keys.buttons = 0;
    keys.accent = None;
    keys.entry = AltNumber::default();
    let state = regs.byte(5) & !(status::SHIFT | status::CTRL | status::PENDING_ALT);
    regs.r[5] = u32::from(state | status::NO_KANA_LOCK);
    regs.r[7] = 0;
    Ok(())
}

/// Gives `out` as the list R6 points at, if it holds anything.
fn give(os: &mut Os<'_>, regs: &mut Regs, out: &Output) -> Result<(), Fail> {
    if out.is_empty() {
        return Ok(());
    }
    let Some(workspace) = os.world.intkey.workspace else {
        return Ok(());
    };
    let list = workspace + LIST_AT;
    os.write_byte(list, u8::try_from(out.len()).unwrap_or(0))?;
    os.write_bytes(list + 1, out.as_slice())?;
    regs.r[6] = list;
    Ok(())
}

/// The code for a key the kernel made a code for while Alt is held or an
/// accent waits: R2 the key, R3 the code.
fn pending_alt(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (action, key, code) = (regs.r[1], regs.r[2], regs.r[3]);
    let mut state = regs.byte(5);
    let mut out = Output::new();
    let (alt, entry, accent) = (
        os.world.intkey.alt > 0,
        os.world.intkey.entry,
        os.world.intkey.accent,
    );
    let code = u8::try_from(code).unwrap_or(SPECIAL);
    let hot = alt && state & status::CTRL != 0 && action == FIRST;
    // A key off the keypad has no part in an Alt entry, and gives nothing.
    if !(alt && entry.digits) {
        if hot && matches!(key, key::F1 | key::F2 | key::F12) {
            match key {
                key::F1 => os.select_later(SET_KEYBOARD | crate::territory::UK)?,
                key::F2 => os.select_later(SET_KEYBOARD)?,
                _ => os.world.intkey.entry.dial = true,
            }
        } else if accent.is_some() && code < 0x80 {
            os.give_char(char::from(code), &mut state, &mut out)?;
        } else if !alt {
            os.world.intkey.accent = None;
            state &= !status::PENDING_ALT;
            give_code(code, &mut out);
        }
    }
    regs.r[5] = u32::from(state);
    give(os, regs, &out)
}

/// The code for a special key: R1 what it is asked, R2 the key, R3 the
/// kernel's exits.
fn special_key(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (action, key) = (regs.r[1], regs.r[2]);
    let mut state = regs.byte(5);
    let mut out = Output::new();
    let layout = &LAYOUTS[os.world.intkey.layout];
    let first = action == FIRST;
    match Class::of(layout, key) {
        Class::Shift => {
            let keys = &mut os.world.intkey;
            if keys.left_alt && keys.shift == 0 && action == DOWN {
                state ^= status::NO_KANA_LOCK;
            }
            keys.shift = held(keys.shift, action);
            state = flag(state, status::SHIFT, keys.shift > 0);
        }
        Class::Ctrl => {
            let keys = &mut os.world.intkey;
            keys.ctrl = held(keys.ctrl, action);
            state = flag(state, status::CTRL, keys.ctrl > 0);
        }
        Class::LayerAlt => {
            os.world.intkey.left_alt = action == DOWN;
            if action == DOWN && state & status::SHIFT != 0 {
                state ^= status::NO_KANA_LOCK;
            }
        }
        Class::Alt => alt_key(os, action, &mut state, &mut out)?,
        Class::Button(bit) => {
            let keys = &mut os.world.intkey;
            keys.buttons = flag(keys.buttons, bit, action == DOWN);
            let mut exit = *regs;
            exit.r[0] = u32::from(keys.buttons);
            return take_exit(os, regs, exit, 0);
        }
        Class::Break => return take_exit(os, regs, *regs, 4),
        Class::CapsLock if first => {
            state = if state & status::SHIFT != 0 {
                (state & !status::NO_CAPS_LOCK) | status::SHIFT_CAPS
            } else {
                (state ^ status::NO_CAPS_LOCK) & !status::SHIFT_CAPS
            };
        }
        Class::NumLock if first => state ^= status::NO_NUM_LOCK,
        Class::ScrollLock if first => state ^= status::SCROLL_LOCK,
        Class::Kana if first => state ^= status::NO_KANA_LOCK,
        Class::Fn
        | Class::Nothing
        | Class::CapsLock
        | Class::NumLock
        | Class::ScrollLock
        | Class::Kana => {}
        Class::Keypad(pad) => keypad_key(os, layout, pad, first, &mut state, &mut out),
        Class::Tab => {
            if os.world.intkey.alt == 0 {
                let mut code = os.byte_var(var::TAB_CHAR);
                if code >= 0x80 {
                    code = shifted(code, state);
                }
                if os.world.intkey.accent.is_some() && code < 0x80 {
                    os.give_char(char::from(code), &mut state, &mut out)?;
                } else {
                    os.world.intkey.accent = None;
                    state &= !status::PENDING_ALT;
                    give_code(code, &mut out);
                }
            }
        }
        Class::Character => character(os, layout, key, &mut state, &mut out)?,
        Class::Simple(codes) => {
            let column = usize::from(state & status::SHIFT != 0)
                + 2 * usize::from(state & status::CTRL != 0);
            give_code(codes[column], &mut out);
        }
    }
    regs.r[5] = u32::from(state);
    give(os, regs, &out)
}

/// An Alt key going down or up: held, the entry begun with the first and
/// ended with the last.
fn alt_key(os: &mut Os<'_>, action: u32, state: &mut u8, out: &mut Output) -> Result<(), Fail> {
    let keys = &mut os.world.intkey;
    if keys.alt == 0 {
        keys.entry = AltNumber::default();
    }
    keys.alt = held(keys.alt, action);
    if keys.alt > 0 {
        *state |= status::PENDING_ALT;
        return Ok(());
    }
    os.end_entry(out)?;
    if os.world.intkey.accent.is_none() {
        *state &= !status::PENDING_ALT;
    }
    Ok(())
}

/// A keypad key: a digit of an Alt entry, or its code for Num Lock, the
/// waiting accent dropped.
fn keypad_key(
    os: &mut Os<'_>,
    layout: &Layout,
    pad: &Pad,
    first: bool,
    state: &mut u8,
    out: &mut Output,
) {
    if os.world.intkey.alt > 0 {
        if first {
            os.entry_digit(pad);
        }
        return;
    }
    os.world.intkey.accent = None;
    *state &= !status::PENDING_ALT;
    let value = if *state & status::NO_NUM_LOCK != 0 {
        pad.off
    } else if pad.key == DECIMAL {
        Some(layout.decimal)
    } else {
        Some(pad.on)
    };
    if let Some(value) = value {
        let mut code = value
            .wrapping_sub(b'0')
            .wrapping_add(os.byte_var(var::KEYPAD_BASE));
        if os.byte_var(var::KEYPAD_SHIFTS) == 0 && code >= 0x80 {
            code = shifted(code, *state);
        }
        let _ = out.try_push(code);
    }
}

/// A character key: its level for the modifiers and Caps Lock, or an accent
/// awaiting it; nothing while an Alt entry has digits.
fn character(
    os: &mut Os<'_>,
    layout: &Layout,
    key: u32,
    state: &mut u8,
    out: &mut Output,
) -> Result<(), Fail> {
    let Some(levels) = key_levels(layout, layer_of(layout, *state), key) else {
        return Ok(());
    };
    let (alt, entry) = (os.world.intkey.alt > 0, os.world.intkey.entry);
    if alt && entry.digits {
        return Ok(());
    }
    if *state & status::CTRL != 0 {
        return match control(layout, key).filter(|_| !alt) {
            Some(code) => os.give_char(char::from(code), state, out),
            None => Ok(()),
        };
    }
    match with_caps(
        &levels,
        if alt { 2 } else { 0 },
        *state & status::SHIFT != 0,
        *state,
    ) {
        Level::Nothing => Ok(()),
        Level::Dead(mark) => {
            os.world.intkey.accent = Some(mark);
            *state |= status::PENDING_ALT;
            Ok(())
        }
        Level::Char(c) => os.give_char(c, state, out),
    }
}

/// A code from &80 moved for the modifiers: &10 for Shift, &20 for Ctrl.
fn shifted(code: u8, state: u8) -> u8 {
    let mut code = code;
    if state & status::SHIFT != 0 {
        code ^= 0x10;
    }
    if state & status::CTRL != 0 {
        code ^= 0x20;
    }
    code
}

/// A held key's count after `action`.
fn held(count: u8, action: u32) -> u8 {
    if action == UP {
        count.saturating_sub(1)
    } else {
        count.saturating_add(1)
    }
}

/// `state` with `bit` set where `set`, clear where not.
fn flag(state: u8, bit: u8, set: bool) -> u8 {
    if set {
        state | bit
    } else {
        state & !bit
    }
}

/// The kernel's exit at `offset` from R3 taken with `exit`, as a branch to
/// it would: what it returns is what the code returns.
fn take_exit(os: &mut Os<'_>, regs: &mut Regs, mut exit: Regs, offset: u32) -> Result<(), Fail> {
    let bits: Bits = os.caller_bits();
    os.call_routine(
        regs.r[3].wrapping_add(offset),
        bits,
        Entry::SupervisorNoIrq,
        &mut exit,
    )?;
    *regs = exit;
    Ok(())
}

/// The callback that selects the keyboard a hot key or a dialling code
/// asked for.
fn select(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    if let Some(request) = os.world.intkey.selecting.take() {
        os.os_byte(ALPHABET_BYTE, request)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
