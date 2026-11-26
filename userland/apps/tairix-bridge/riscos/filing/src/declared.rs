//! A module that is one filing system: the workspace it declares itself
//! from — after its messages' file, its information block, its name and an
//! empty start-up text — and its declaration to FileSwitch and removal.

use tairix_riscos_kernel::filing::fs::{block, Declaration};
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs, Unwind};
use tairix_riscos_kernel::swi::os::{fs_control, FS_CONTROL};
use tairix_riscos_kernel::swi::X;

/// What a module that is one filing system declares.
pub(crate) struct Declared {
    pub(crate) module: &'static Builtin,
    pub(crate) messages: OwnFile,
    pub(crate) name: &'static [u8],
    pub(crate) first_entry: usize,
    pub(crate) gbpb: bool,
    pub(crate) info: u32,
    pub(crate) extra: u32,
}

impl Declared {
    const fn block_at(&self) -> u32 {
        self.messages.room()
    }

    /// Where its workspace keeps its name.
    pub(crate) const fn name_at(&self) -> u32 {
        self.block_at() + 4 * len32(block::WORDS)
    }

    const fn empty_at(&self) -> u32 {
        (self.name_at() + len32(self.name.len() + 1)).next_multiple_of(4)
    }

    /// Its workspace claimed, laid out and kept in the private word at
    /// `private`, which the kernel frees it from when the module dies: the
    /// workspace and its messages.
    pub(crate) fn made(&self, os: &mut Os<'_>, private: u32) -> Result<(u32, Own), Fail> {
        let workspace = os.rma_claim(self.empty_at() + 4)?;
        if let Err(fail) = os.write_word(private, workspace) {
            os.free_rma_quietly(workspace);
            return Err(fail);
        }
        let messages = self.messages.place(os, workspace)?;
        os.write_terminated(
            workspace + self.name_at(),
            self.name,
            0,
            self.name.len() + 1,
        )?;
        os.write_word(workspace + self.empty_at(), 0)?;
        Ok((workspace, messages))
    }

    /// The filing system declared to FileSwitch from `workspace`.
    pub(crate) fn declare(&self, os: &mut Os<'_>, workspace: u32) -> Result<(), Fail> {
        os.declare_filing_system(Declaration {
            module: self.module,
            first_entry: self.first_entry,
            gbpb: self.gbpb,
            block: workspace + self.block_at(),
            name: workspace + self.name_at(),
            startup: workspace + self.empty_at(),
            info: self.info,
            extra: self.extra,
            workspace,
        })
    }

    /// The filing system taken from FileSwitch, which closes its files.
    pub(crate) fn remove(&self, os: &mut Os<'_>, workspace: u32) -> Result<(), Unwind> {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[fs_control::REMOVE, workspace + self.name_at()]);
        os.perform_quietly(X | FS_CONTROL, &mut regs).map(drop)
    }
}
