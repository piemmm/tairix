//! Writing a flattened device tree, for a build tool emitting one for firmware
//! to read — a device-tree overlay among them.
//!
//! The blob is the Devicetree Specification v0.4 layout the reader takes: the
//! header, an empty memory-reservation block (its terminating entry), the
//! structure block and the strings block, each property name stored once.

use alloc::vec::Vec;

use crate::{FDT_BEGIN_NODE, FDT_END, FDT_END_NODE, FDT_MAGIC, FDT_PROP};

/// Bytes of the header.
const HEADER_LEN: usize = 40;

/// Bytes of the memory-reservation block: its terminating entry alone.
const RESERVATION_LEN: usize = 16;

/// The version written, and the oldest it stays readable as.
const VERSION: u32 = 17;
const LAST_COMPATIBLE_VERSION: u32 = 16;

/// Why a tree could not be written.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FdtWriteError {
    /// Nodes that do not nest into one root: a close with none open, a
    /// property or a second root outside the first, or a node left open.
    Unbalanced,
    /// A node or property name holding a NUL, or an empty property name.
    BadName,
    /// A block a 32-bit offset cannot reach.
    TooLarge,
}

/// A tree being written, node by node.
pub struct FdtWriter {
    strings: Vec<u8>,
    structure: Vec<u8>,
    depth: usize,
    roots: usize,
    fault: Option<FdtWriteError>,
}

impl Default for FdtWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl FdtWriter {
    /// An empty tree.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            strings: Vec::new(),
            structure: Vec::new(),
            depth: 0,
            roots: 0,
            fault: None,
        }
    }

    /// Keep the first fault met.
    fn fail(&mut self, fault: FdtWriteError) {
        self.fault.get_or_insert(fault);
    }

    fn token(&mut self, token: u32) {
        self.structure.extend_from_slice(&token.to_be_bytes());
    }

    fn pad(&mut self) {
        let padded = self.structure.len().next_multiple_of(4);
        self.structure.resize(padded, 0);
    }

    fn offset(&mut self, at: usize) -> u32 {
        u32::try_from(at).unwrap_or_else(|_| {
            self.fail(FdtWriteError::TooLarge);
            u32::MAX
        })
    }

    /// The strings-block offset of `name`, stored on first use.
    fn intern(&mut self, name: &str) -> u32 {
        let wanted = name.as_bytes();
        let found = self
            .strings
            .split(|&byte| byte == 0)
            .scan(0, |start, stored| {
                let at = *start;
                *start += stored.len() + 1;
                Some((at, stored))
            })
            .find(|&(_, stored)| stored == wanted)
            .map(|(at, _)| at);
        let at = found.unwrap_or_else(|| {
            let at = self.strings.len();
            self.strings.extend_from_slice(wanted);
            self.strings.push(0);
            at
        });
        self.offset(at)
    }

    /// Open a node named `name`, the root's being empty.
    pub fn begin_node(&mut self, name: &str) {
        if name.contains('\0') {
            self.fail(FdtWriteError::BadName);
        }
        if self.depth == 0 {
            self.roots += 1;
            if self.roots > 1 {
                self.fail(FdtWriteError::Unbalanced);
            }
        }
        self.depth += 1;
        self.token(FDT_BEGIN_NODE);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        self.pad();
    }

    /// Close the node opened last.
    pub fn end_node(&mut self) {
        match self.depth.checked_sub(1) {
            Some(depth) => self.depth = depth,
            None => self.fail(FdtWriteError::Unbalanced),
        }
        self.token(FDT_END_NODE);
    }

    /// A property of the open node holding `value` as it is.
    pub fn prop(&mut self, name: &str, value: &[u8]) {
        if name.is_empty() || name.contains('\0') {
            self.fail(FdtWriteError::BadName);
        }
        if self.depth == 0 {
            self.fail(FdtWriteError::Unbalanced);
        }
        let name_offset = self.intern(name);
        let len = self.offset(value.len());
        self.token(FDT_PROP);
        self.structure.extend_from_slice(&len.to_be_bytes());
        self.structure.extend_from_slice(&name_offset.to_be_bytes());
        self.structure.extend_from_slice(value);
        self.pad();
    }

    /// A property holding one cell.
    pub fn prop_u32(&mut self, name: &str, value: u32) {
        self.prop(name, &value.to_be_bytes());
    }

    /// A property holding `cells`, in order.
    pub fn prop_cells(&mut self, name: &str, cells: &[u32]) {
        let bytes: Vec<u8> = cells.iter().flat_map(|cell| cell.to_be_bytes()).collect();
        self.prop(name, &bytes);
    }

    /// A property holding one string.
    pub fn prop_str(&mut self, name: &str, value: &str) {
        self.prop_strs(name, &[value]);
    }

    /// A property holding a string list.
    pub fn prop_strs(&mut self, name: &str, values: &[&str]) {
        let mut bytes = Vec::new();
        for value in values {
            if value.contains('\0') {
                self.fail(FdtWriteError::BadName);
            }
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
        }
        self.prop(name, &bytes);
    }

    /// The blob as written, whether or not it is well formed.
    fn emit(mut self) -> Vec<u8> {
        self.token(FDT_END);
        let structure_at = HEADER_LEN + RESERVATION_LEN;
        let strings_at = structure_at + self.structure.len();
        let total = strings_at + self.strings.len();
        let header = [
            FDT_MAGIC,
            self.offset(total),
            self.offset(structure_at),
            self.offset(strings_at),
            self.offset(HEADER_LEN),
            VERSION,
            LAST_COMPATIBLE_VERSION,
            0,
            self.offset(self.strings.len()),
            self.offset(self.structure.len()),
        ];
        let mut blob = Vec::with_capacity(total);
        for field in header {
            blob.extend_from_slice(&field.to_be_bytes());
        }
        blob.resize(structure_at, 0);
        blob.extend_from_slice(&self.structure);
        blob.extend_from_slice(&self.strings);
        blob
    }

    /// The finished blob.
    ///
    /// # Errors
    ///
    /// The first [`FdtWriteError`] the tree met, or
    /// [`FdtWriteError::Unbalanced`] for no root or one left open.
    pub fn finish(self) -> Result<Vec<u8>, FdtWriteError> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if self.roots != 1 || self.depth != 0 {
            return Err(FdtWriteError::Unbalanced);
        }
        let blob = self.emit();
        // `emit` saturates an offset it cannot write, so a blob past a
        // 32-bit reach is found only once it is laid out.
        if u32::try_from(blob.len()).is_err() {
            return Err(FdtWriteError::TooLarge);
        }
        Ok(blob)
    }

    /// The blob a test laid out, malformed or not, for the reader to meet.
    #[cfg(any(test, feature = "test-fixtures"))]
    #[must_use]
    pub fn build(self) -> Vec<u8> {
        self.emit()
    }
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod tests;
