//! A store of plain bytes for the editor's tests, a document in one, and
//! the editor run over it a key at a time, its state read back from its
//! record after each as the program reads it from memory.

use super::text::{Gap, Store, Text};
use super::ui::{Action, Done, Input, Pointer, Screen, Settings, Shade, Ui};
use super::view::Cells;
use crate::bios::shift;
use crate::frames::Field;
use alloc::vec::Vec;

pub struct Bytes(pub Vec<u8>);

impl Store for Bytes {
    fn capacity(&self) -> u32 {
        u32::try_from(self.0.len()).unwrap()
    }

    fn read(&mut self, at: u32, out: &mut [u8]) {
        let at = usize::try_from(at).unwrap();
        out.copy_from_slice(&self.0[at..at + out.len()]);
    }

    fn write(&mut self, at: u32, data: &[u8]) {
        let at = usize::try_from(at).unwrap();
        self.0[at..at + data.len()].copy_from_slice(data);
    }
}

/// A store of `size` bytes.
pub fn store(size: usize) -> Bytes {
    Bytes(alloc::vec![0; size])
}

/// The whole of a store as an empty document.
pub fn empty(store: &mut Bytes) -> Text<'_, Bytes> {
    let capacity = store.capacity();
    Text::new(
        store,
        Gap {
            start: 0,
            end: capacity,
        },
        capacity,
    )
}

/// The document's bytes.
pub fn whole(text: &mut Text<'_, Bytes>) -> Vec<u8> {
    let len = text.len();
    text.read(0..len)
}

pub const SHIFT: u8 = shift::LEFT_SHIFT;
pub const CTRL: u8 = shift::CTRL;
pub const ALT: u8 = shift::ALT;

/// The editor over a store of its own, on a screen of 25 rows.
pub struct Run {
    store: Bytes,
    gap: Gap,
    capacity: u32,
    pub ui: Ui,
}

impl Run {
    pub fn new(size: usize) -> Self {
        let store = store(size);
        let capacity = store.capacity();
        let screen = Screen {
            rows: 25,
            columns: 80,
            shade: Shade::Colour,
        };
        Self {
            store,
            gap: Gap {
                start: 0,
                end: capacity,
            },
            capacity,
            ui: Ui::new(screen, Settings::default()),
        }
    }

    /// `f` run on the editor and its document, the editor's state packed
    /// and unpacked first.
    pub fn with<T>(&mut self, f: impl FnOnce(&mut Ui, &mut Text<'_, Bytes>) -> T) -> T {
        let mut bytes = alloc::vec![0u8; <Ui as Field>::SIZE];
        self.ui.put(&mut bytes, &mut 0);
        let mut ui = Ui::take(&bytes, &mut 0);
        assert_eq!(ui, self.ui, "the state read back as it was kept");
        let mut text = Text::new(&mut self.store, self.gap, self.capacity);
        let out = f(&mut ui, &mut text);
        (self.gap, self.capacity) = (text.gap(), text.capacity());
        self.ui = ui;
        out
    }

    pub fn key(&mut self, code: u16, flags: u8) -> Action {
        self.with(|ui, text| ui.input(Input::Key { code, flags }, text))
    }

    /// Each byte of `typed` typed, as the keypad's Alt codes give them.
    pub fn typed(&mut self, typed: &[u8]) {
        for &byte in typed {
            assert_eq!(self.key(u16::from(byte), 0), Action::Wait);
        }
    }

    pub fn pointer(&mut self, pointer: Pointer) -> Action {
        self.with(|ui, text| ui.input(Input::Pointer(pointer), text))
    }

    /// A click of the left button at `at`, at tick `now`.
    pub fn click(&mut self, at: (u8, u8), now: u16) -> Action {
        self.pointer(Pointer {
            presses: 1,
            pressed_at: at,
            releases: 1,
            held: false,
            at,
            flags: 0,
            now,
        })
    }

    pub fn done(&mut self, done: Done) -> Action {
        self.with(|ui, text| ui.done(done, text))
    }

    pub fn document(&mut self) -> Vec<u8> {
        self.with(|_, text| whole(text))
    }

    pub fn screen(&mut self) -> Cells {
        self.with(|ui, text| {
            let mut cells = Cells::new(25, 80);
            ui.draw(&mut cells, text);
            cells
        })
    }

    /// Row `n` of the screen's characters.
    pub fn row(&mut self, n: u8) -> Vec<u8> {
        let cells = self.screen();
        let start = usize::from(n) * 80;
        cells.grid[start..start + 80]
            .iter()
            .map(|cell| cell.to_le_bytes()[0])
            .collect()
    }

    /// The document's capacity, which the clipboard and a listing take
    /// from.
    pub const fn capacity(&self) -> u32 {
        self.capacity
    }
}

/// Whether `row` holds `text`.
pub fn holds(row: &[u8], text: &[u8]) -> bool {
    row.windows(text.len()).any(|window| window == text)
}
