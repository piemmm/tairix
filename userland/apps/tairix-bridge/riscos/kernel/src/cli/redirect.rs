//! Redirection: the first `{ < file }`, `{ > file }` or `{ >> file }` group
//! in a command line, the line run without it while its files take the
//! input and output, and closed when it ends. Redirection does not nest: a
//! group's files replace any redirection in their direction.

use alloc::vec::Vec;

use crate::byte::var;
use crate::os::{Fail, Os, Regs};
use crate::swi::os::args::{READ_EXTENT, WRITE_POINTER};
use crate::swi::os::find::{INPUT, MUST_OPEN, OUTPUT, UPDATE};
use crate::swi::X;

/// "The redirection could not be set up".
const FAILS: crate::error::Error = crate::error::Error {
    number: 0x140,
    token: "RedirectionFailed",
};

/// A file a group redirects to: whether it is input, and the reason to open
/// it with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Spec {
    reason: u32,
    /// The filename's place in the line.
    name: (usize, usize),
}

/// A group: where it starts and ends in the line, and its files.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct Group {
    brace: usize,
    after: usize,
    specs: Vec<Spec>,
}

/// The first redirection group in `line`, outside quotes.
pub(crate) fn find(line: &[u8]) -> Option<Group> {
    let mut quoted = false;
    for (at, &byte) in line.iter().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b'{' if !quoted => {
                if let Some(group) = parse(line, at) {
                    return Some(group);
                }
            }
            _ => {}
        }
    }
    None
}

/// The group whose `{` is at `brace`, if it is one: its elements — the
/// brace, each direction and its name, the closing brace — each exactly one
/// space from the next.
fn parse(line: &[u8], brace: usize) -> Option<Group> {
    let mut at = brace + 1;
    let mut specs = Vec::new();
    loop {
        if line.get(at) != Some(&b' ') {
            return None;
        }
        at += 1;
        let reason = match (line.get(at), line.get(at + 1)) {
            (Some(b'<'), _) => INPUT,
            (Some(b'>'), Some(b'>')) => UPDATE,
            (Some(b'>'), _) => OUTPUT,
            (Some(b'}'), _) if !specs.is_empty() => {
                return Some(Group {
                    brace,
                    after: at + 1,
                    specs,
                });
            }
            _ => return None,
        };
        at += if reason == UPDATE { 2 } else { 1 };
        if line.get(at) != Some(&b' ') {
            return None;
        }
        at += 1;
        let start = at;
        while line.get(at).is_some_and(|&b| b > b' ') {
            at += 1;
        }
        if at == start {
            return None;
        }
        specs.try_reserve(1).ok()?;
        specs.push(Spec {
            reason,
            name: (start, at),
        });
    }
}

/// `line` without `group`: the text before its `{`, then the text after
/// its `}` past the spaces there.
pub(crate) fn rewrite(line: &[u8], group: &Group) -> Vec<u8> {
    let rest = &line[group.after..];
    let rest = &rest[rest.iter().take_while(|&&b| b == b' ').count()..];
    let mut out = Vec::new();
    if out.try_reserve_exact(group.brace + rest.len()).is_ok() {
        out.extend_from_slice(&line[..group.brace]);
        out.extend_from_slice(rest);
    }
    out
}

impl Os<'_> {
    /// Opens `group`'s files, `line` holding their names, each taking its
    /// direction's redirection from any file before it.
    pub(crate) fn redirect(&mut self, line: &[u8], group: &Group) -> Result<(), Fail> {
        for spec in &group.specs {
            let input = spec.reason == INPUT;
            let (buffer, number) = self.cli_buffer();
            let name = &line[spec.name.0..spec.name.1];
            let opened = self
                .write_terminated(buffer, name, b'\r', super::BUFFER as usize)
                .and_then(|()| self.redirect_one(buffer, spec.reason, input));
            // Whatever ends the group, its files are closed.
            if let Err(fail) = self.cli_buffer_done(number, opened) {
                self.end_redirection();
                return Err(fail);
            }
        }
        Ok(())
    }

    /// Opens the file named at `name` with `reason`, the redirection in its
    /// direction closed first.
    fn redirect_one(&mut self, name: u32, reason: u32, input: bool) -> Result<(), Fail> {
        let old = self.redirection(input, 0)?;
        self.close_handle(old)?;
        let handle = self.open_file(reason, name)?;
        if handle == 0 {
            return Err(self.error(FAILS));
        }
        if reason == UPDATE {
            self.pointer_to_end(handle)?;
        }
        self.redirection(input, handle)?;
        Ok(())
    }

    /// Sets the input or output redirection's handle; the old one.
    fn redirection(&mut self, input: bool, handle: u32) -> Result<u32, Fail> {
        let mut change = Regs::default();
        change.r[..2].copy_from_slice(&if input {
            [handle, 0x100]
        } else {
            [0x100, handle]
        });
        self.change_redirection(&mut change)?;
        Ok(if input { change.r[0] } else { change.r[1] })
    }

    /// The files the current context's streams hold — its spool and exec
    /// files, its redirection's — closed: they are its own, so nothing could
    /// reach them once it ends.
    pub(crate) fn close_streams(&mut self) {
        for held in [var::SPOOL_HANDLE, var::EXEC_HANDLE] {
            // Written as nought, the handle it held answered.
            let mut byte = Regs::default();
            byte.r[0] = u32::from(held);
            if self.perform(X | crate::swi::os::BYTE, &mut byte).is_ok() {
                let _ = self.close_handle(byte.r[1]);
            }
        }
        self.end_redirection();
    }

    /// Ends all redirection, `WrchV` given up before both files are closed.
    pub(crate) fn end_redirection(&mut self) {
        let mut change = Regs::default();
        change.r[..2].copy_from_slice(&[0, 0]);
        if self.change_redirection(&mut change).is_ok() {
            for handle in [change.r[1], change.r[0]] {
                let _ = self.close_handle(handle);
            }
        }
    }

    /// The file `name` names opened with `reason`, refused if it is missing
    /// or a directory: its handle.
    pub(crate) fn open_file(&mut self, reason: u32, name: u32) -> Result<u32, Fail> {
        let mut find = Regs::default();
        find.r[..2].copy_from_slice(&[reason | MUST_OPEN, name]);
        self.perform(X | crate::swi::os::FIND, &mut find)?;
        Ok(find.r[0])
    }

    /// `handle`'s pointer moved to its end.
    pub(crate) fn pointer_to_end(&mut self, handle: u32) -> Result<(), Fail> {
        let mut extent = Regs::default();
        extent.r[..2].copy_from_slice(&[READ_EXTENT, handle]);
        self.perform(X | crate::swi::os::ARGS, &mut extent)?;
        self.set_pointer(handle, extent.r[2])
    }

    /// `handle`'s pointer set to `pointer`.
    pub(crate) fn set_pointer(&mut self, handle: u32, pointer: u32) -> Result<(), Fail> {
        let mut args = Regs::default();
        args.r[..3].copy_from_slice(&[WRITE_POINTER, handle, pointer]);
        self.perform(X | crate::swi::os::ARGS, &mut args)
    }

    /// Closes `handle`, if it is one.
    pub(crate) fn close_handle(&mut self, handle: u32) -> Result<(), Fail> {
        if handle == 0 {
            return Ok(());
        }
        let mut close = Regs::default();
        close.r[..2].copy_from_slice(&[0, handle]);
        self.perform(X | crate::swi::os::FIND, &mut close)
    }
}
