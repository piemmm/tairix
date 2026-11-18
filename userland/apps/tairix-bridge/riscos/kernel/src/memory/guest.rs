//! What the address space asks of the rest of the kernel while it changes:
//! running a dynamic area's handler, issuing a service call or an upcall,
//! and naming the current active object.
//!
//! Each call is made with the address space consistent, and is handed it, so
//! the code that answers may itself call the memory SWIs. The kernel makes
//! the calls by running the guest's code, writing a page block to guest
//! memory first and reading the handler's answer back after.

use super::{Handler, Memory};

/// A guest error: the address of its error block.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GuestError(pub u32);

/// A page block entry, as a grow handler reads and writes it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct PageEntry {
    /// The physical page number: none until chosen.
    pub page: Option<u32>,
    /// Where it is mapped.
    pub logical: u32,
    /// Its physical address.
    pub physical: u32,
}

/// A service call the address space issues.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Service<'a> {
    /// `Service_Memory`: application space is to change by `change` bytes,
    /// with the current active object at `active_object`. Claiming it
    /// refuses the change.
    Memory {
        /// The change, negative when application space shrinks.
        change: i32,
        /// The current active object.
        active_object: u32,
    },
    /// `Service_MemoryMoved`: `bytes` moved, from `area`'s point of view.
    MemoryMoved {
        /// The bytes, negative when the area shrank.
        bytes: i32,
        /// The area, -1 for application space.
        area: i32,
    },
    /// `Service_DynamicAreaCreate`.
    DynamicAreaCreate {
        /// The new area.
        area: u32,
    },
    /// `Service_DynamicAreaRemove`.
    DynamicAreaRemove {
        /// The area removed.
        area: u32,
    },
    /// `Service_DynamicAreaRenumber`.
    DynamicAreaRenumber {
        /// Its number before.
        old: u32,
        /// Its number now.
        new: u32,
    },
    /// `Service_PagesUnsafe` (and its 64-bit form): these pages are about
    /// to change.
    PagesUnsafe {
        /// The pages.
        pages: &'a [PageEntry],
    },
    /// `Service_PagesSafe` (and its 64-bit form): the pages were replaced.
    PagesSafe {
        /// The pages before.
        old: &'a [PageEntry],
        /// The pages that replaced them.
        new: &'a [PageEntry],
    },
    /// `Service_ClaimFIQ`, around a page's replacement.
    ClaimFiq,
    /// `Service_ReleaseFIQ`.
    ReleaseFiq,
}

/// What a `PreGrow` handler allowed beyond the grow itself.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Allowed {
    /// It answered "RESV": the pages it names may be ones `OS_Memory` 23
    /// reserved.
    pub reserved: bool,
}

/// The rest of the kernel, as the address space sees it.
pub trait Guest {
    /// `PreGrow`: `bytes` are about to be added to an area of `size`. A
    /// handler of an area that needs particular pages names them in
    /// `block`; any other's block is empty.
    fn pre_grow(
        &mut self,
        memory: &mut Memory,
        handler: Handler,
        block: &mut [PageEntry],
        bytes: u32,
        size: u32,
    ) -> Result<Allowed, Option<GuestError>>;

    /// `PostGrow`: `bytes` were added, making `size`.
    fn post_grow(
        &mut self,
        memory: &mut Memory,
        handler: Handler,
        block: &[PageEntry],
        bytes: u32,
        size: u32,
    );

    /// `PreShrink`: may the area of `size` lose `bytes`? The answer is the
    /// bytes it may.
    fn pre_shrink(
        &mut self,
        memory: &mut Memory,
        handler: Handler,
        bytes: u32,
        size: u32,
    ) -> Result<u32, Option<GuestError>>;

    /// `PostShrink`: `bytes` went, leaving `size`.
    fn post_shrink(&mut self, memory: &mut Memory, handler: Handler, bytes: u32, size: u32);

    /// `TestShrink`: the bytes a shrinkable area of `size` could give up.
    fn test_shrink(&mut self, memory: &mut Memory, handler: Handler, size: u32) -> u32;

    /// `ResizePMP`: an `OS_ChangeDynamicArea` of `pages` on a physical
    /// memory pool, named `area` by its caller.
    fn resize_pmp(
        &mut self,
        memory: &mut Memory,
        handler: Handler,
        pages: i32,
        area: i32,
    ) -> Result<(), Option<GuestError>>;

    /// Issues a service call; true if a module claimed it.
    fn service(&mut self, memory: &mut Memory, service: Service<'_>) -> bool;

    /// `UpCall_MovingMemory` for `bytes`; true if claimed.
    fn moving_memory(&mut self, memory: &mut Memory, bytes: u32) -> bool;

    /// The current active object's address.
    fn active_object(&self) -> u32;

    /// Whether the call is made from an interrupt.
    fn in_interrupt(&self) -> bool;
}
