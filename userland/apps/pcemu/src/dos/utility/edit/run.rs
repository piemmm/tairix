//! The editor run: its screen drawn and the cursor placed, a key or the
//! mouse waited for and given to it, and what it asks for done, until it
//! asks to end. A key that changes nothing the editor holds draws nothing,
//! so the pointer moving costs no frame.

use super::files::{EditList, EditOpen, EditPrint, EditSave, EditSaveAs, EditSettings};
use super::hooks::EditPointer;
use super::keys::own;
use super::screen::{blit, place, reshape, shape};
use super::ui::{Action, Done, Input};
use super::view::Cells;
use super::{at, keep, load, ui_of, with_ui, Hooks, Video};
use crate::bios::bda;
use crate::dos::program::{Flow, Frame, Id, Int, Program, Routine};
use crate::frames::{locals, take, Outcome};
use alloc::vec::Vec;

locals! {
    /// The editor run until it asks to end; `shape` is the cursor's shape
    /// it is to have once placed.
    pub struct EditLoop {
        shape: u16,
    }
}

mod phase {
    pub const HIDDEN: u8 = 1;
    pub const SHOWN: u8 = 2;
    pub const PLACED: u8 = 3;
    pub const SHAPED: u8 = 4;
    pub const KEYED: u8 = 5;
    pub const ANSWERED: u8 = 6;
    pub const DONE: u8 = 7;
}

/// The bytes of the editor's state and the document's layout, to tell
/// whether a key changed anything.
fn held(p: &mut Program<'_, '_>) -> Vec<u8> {
    let mut bytes = alloc::vec![0u8; usize::from(at::HELD_LEN)];
    p.g.read_bytes(p.own, at::UI, &mut bytes);
    bytes
}

/// What [`held`] gave, kept while the mouse is asked what it did.
fn remember(p: &mut Program<'_, '_>) {
    let bytes = held(p);
    p.g.write_bytes(p.own, at::BEFORE, &bytes);
}

fn recalled(p: &mut Program<'_, '_>) -> Vec<u8> {
    let mut bytes = alloc::vec![0u8; usize::from(at::HELD_LEN)];
    p.g.read_bytes(p.own, at::BEFORE, &mut bytes);
    bytes
}

impl EditLoop {
    /// The screen composed and written, the mouse's pointer hidden from it
    /// meanwhile where a driver draws one.
    fn draw(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let hooks: Hooks = load(p, at::HOOKS);
        if hooks.mouse {
            *phase = phase::HIDDEN;
            return Flow::Call(Frame::of(&Int::bios(p.own, 0x33, 0x0002)));
        }
        Self::write(p);
        self.cursor(p, phase)
    }

    fn write(p: &mut Program<'_, '_>) {
        let cells = with_ui(p, |ui, text| {
            let mut cells = Cells::new(ui.screen.rows, ui.screen.columns);
            ui.draw(&mut cells, text);
            cells
        });
        blit(p, &cells);
    }

    /// The cursor placed where the editor stands, or hidden, and the
    /// shape it is to have kept for when it is.
    fn cursor(&mut self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let video: Video = load(p, at::VIDEO);
        let ui = ui_of(p);
        let at = ui.cursor();
        self.shape = shape(&video, at.is_some(), ui.overtype());
        let (row, column) = at.unwrap_or((0, 0));
        *phase = phase::PLACED;
        Flow::Call(place(p, &video, row, column))
    }

    /// The cursor shaped as the editor stands, unless it is already.
    fn reshape(self, p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let video: Video = load(p, at::VIDEO);
        if self.shape == video.shown {
            return Self::wait(p, phase);
        }
        keep(
            p,
            at::VIDEO,
            Video {
                shown: self.shape,
                ..video
            },
        );
        *phase = phase::SHAPED;
        Flow::Call(reshape(p, self.shape))
    }

    fn wait(p: &Program<'_, '_>, phase: &mut u8) -> Flow {
        *phase = phase::KEYED;
        Flow::Call(Frame::of(&Int::bios(p.own, 0x16, 0x0000)))
    }

    /// A key read given to the editor, or the mouse's key to the routine
    /// that asks the driver what it did.
    fn keyed(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, code: u16) -> Flow {
        if code == u16::from(own::MOUSE) << 8 {
            remember(p);
            *phase = phase::ANSWERED;
            return Flow::Call(Frame::of(&EditPointer::default()));
        }
        let flags = p.g.bda8(bda::SHIFT_FLAGS);
        let before = held(p);
        let action = with_ui(p, |ui, text| ui.input(Input::Key { code, flags }, text));
        self.act(p, phase, action, &before)
    }

    /// What the editor asked for done: a routine called for it, or the
    /// screen drawn again where `before` is no longer what the editor
    /// holds.
    fn act(
        &mut self,
        p: &mut Program<'_, '_>,
        phase: &mut u8,
        action: Action,
        before: &[u8],
    ) -> Flow {
        let routine = match action {
            Action::Wait if held(p) == before => return Self::wait(p, phase),
            Action::Wait => return self.draw(p, phase),
            Action::Exit => return Flow::Return(Ok(0)),
            Action::List => Frame::of(&EditList::default()),
            Action::Open => Frame::of(&EditOpen::default()),
            Action::SaveAs => Frame::of(&EditSaveAs::default()),
            Action::Save => Frame::of(&EditSave::default()),
            Action::Print { selection } => Frame::of(&EditPrint {
                selection,
                ..EditPrint::default()
            }),
            Action::Settings => Frame::of(&EditSettings {
                write: true,
                ..EditSettings::default()
            }),
        };
        *phase = phase::DONE;
        Flow::Call(routine)
    }
}

impl Routine for EditLoop {
    const ID: Id = Id::EditLoop;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            phase::HIDDEN => {
                take!(child);
                Self::write(p);
                *phase = phase::SHOWN;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x33, 0x0001)))
            }
            phase::SHOWN => {
                take!(child);
                self.cursor(p, phase)
            }
            phase::PLACED => {
                take!(child);
                self.reshape(p, phase)
            }
            phase::SHAPED => {
                take!(child);
                Self::wait(p, phase)
            }
            phase::KEYED => {
                let code = u16::try_from(take!(child)).unwrap_or(0);
                self.keyed(p, phase, code)
            }
            phase::ANSWERED => {
                let action = Action::of(take!(child));
                let before = recalled(p);
                self.act(p, phase, action, &before)
            }
            phase::DONE => {
                let done = Done::of(take!(child));
                let action = with_ui(p, |ui, text| ui.done(done, text));
                self.act(p, phase, action, &[])
            }
            _ => self.draw(p, phase),
        }
    }
}
