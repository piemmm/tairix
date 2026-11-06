//! x86_64 runtime `spawn` producer — `plans/PI.md` `X3b`.
//!
//! [`X86_64ProcessSpawn`] implements the architecture-neutral
//! [`tairix_kernel_core::ArchImageBuilder`] seam the boot pipeline installs
//! into the [`tairix_kernel_core::BootInfo`] hand-off (`boot::try_boot` →
//! `BootInfo::with_spawn`). It is the cross-port sibling of the aarch64
//! `spawn_producer` (`plans/SPAWN.md` `SP3b`): when a task that holds
//! `CAP_PROC_SPAWN` issues the `spawn` syscall, the kernel resolves the
//! requested path against the shared
//! [`crate::spawn_layout::PROGRAM_REGISTRY`], admits a parked **loading**
//! child, and returns its PID at once. On the child's own first scheduled
//! slice this producer's
//! [`build`](tairix_kernel_core::ArchImageBuilder::build) builds it a *fresh,
//! hardware-isolated* PML4 hierarchy and populates it through the production
//! capability-checked, audited spawn caller ([`spawn_image`], gated on
//! `CAP_PROC_SPAWN`). Unlike the PID-1 [`tairix_kernel_core::InitSpawn`] seam
//! (`init_spawn_x86_64.rs`) it does **not** switch CR3 or enter ring 3 from
//! the caller: the spawning caller keeps running under its own root, and the
//! child enters ring 3 from its own loading body when the scheduler next steps
//! it (a true concurrent, non-blocking spawn, not an `exec`-style hand-off).
//!
//! # Building the child without switching CR3
//!
//! The runtime `spawn` syscall is issued by PID 1 `init`, so the producer runs
//! under PID 1's own root, which [`init_spawn`](crate::x86_64::init_spawn)
//! built with [`ArchAddressSpace::new_process_root`]: the higher-half kernel
//! window plus the direct physical map, and no identity map. Every table the
//! walk recovers and every image frame it writes is reached through that map
//! ([`ConfiguredPhysMap`]), which PID 1's root carries like every other root,
//! so the producer builds the child's tables *through the caller's active
//! CR3*, never switching it. The child's own CR3 is reloaded by its
//! `pre_resume` hook before the scheduler first resumes it
//! (`plans/SPAWN.md` SP2, `plans/PI.md` X1).
//!
//! Spawning is *not* a privileged bypass: the child receives only the authority
//! its registered program declares intersected with its user's grants; this seam only authorises the *act* of spawning
//! under `CAP_PROC_SPAWN`.

use core::ptr::NonNull;

use alloc::boxed::Box;

use tairix_abi::rxe::LoadImage;
use tairix_abi::Errno;
use tairix_arch_api::AddressTag;
use tairix_arch_x86_64::paging::{self, activate_user_root, AddressSpace as ArchAddressSpace};
use tairix_arch_x86_64::syscall_entry;
use tairix_arch_x86_64::userentry::{set_user_thread_pointer, USER_MODE};
use tairix_kernel_core::spawn::build_retained;
use tairix_kernel_core::{
    refuse_build, spawn_image, ArchImageBuilder, BuiltImage, ImageBuildCtx, SpawnMode,
};
use tairix_kernel_mem::{
    AddressSpace, DirectPhysMap, FrameAllocator, FrameTableSource, PhysAddr, PhysMap,
};
use tairix_kernel_syscall::SYSCALL_TABLE_HASH;
use tairix_sync::Once;

use crate::spawn_layout::{self, CHILD_USER_BIAS};

/// A spawned child's four fixed guarded-window bases (`plans/PI.md`
/// 5d-0-ii (b′)/(c)), derived from the one shared offset set the retained
/// [`LiveSpace`](tairix_kernel_mem::LiveSpace)'s window allocators are configured with.
const WINDOWS: spawn_layout::WindowBases = spawn_layout::window_bases(CHILD_USER_BIAS);

/// The kernel's direct physical map: the higher-half window at
/// [`paging::PHYSMAP_VMA_BASE`] the boot path sized from the discovered
/// memory map, where physical `p` is reachable at `PHYSMAP_VMA_BASE + p`.
///
/// It is the view every kernel path that reaches a frame by pointer uses —
/// the child image write, the shared-region zero-on-free scrub, the remap
/// window's record store, the slab page supply, and the page-table walk's
/// own table recovery — so there is one map, not a second that could cover
/// different RAM. Being in the kernel half is what lets every process root
/// carry it: its extent is bounded by the architecture rather than by where
/// user space begins, and the tables beneath it are shared rather than
/// redrawn per process.
///
/// The limit is re-derived from the live map on every call
/// ([`paging::physmap_bytes`]) rather than frozen at a build-time gigabyte
/// count a real machine outgrows. A frame outside it still fails the
/// translate and its consumer fails closed rather than fabricating a
/// pointer.
pub struct ConfiguredPhysMap;

impl ConfiguredPhysMap {
    /// The live map as a linear window, re-read so a caller can never
    /// hold a stale extent.
    ///
    /// `None` when the live map covers nothing a pointer could address, so
    /// every consumer fails closed rather than reaching a fabricated one.
    fn window() -> Option<DirectPhysMap> {
        // SAFETY: the boot paging code installed this direct map in every
        // translation root it builds and never tears it down, so the window
        // is live for as long as the kernel runs.
        unsafe { DirectPhysMap::new(paging::PHYSMAP_VMA_BASE, paging::physmap_bytes()) }
    }
}

impl PhysMap for ConfiguredPhysMap {
    fn translate(&self, phys: PhysAddr, len: usize) -> Option<NonNull<u8>> {
        Self::window()?.translate(phys, len)
    }

    fn reverse(&self, virt: usize) -> Option<PhysAddr> {
        Self::window()?.reverse(virt)
    }

    fn clean_invalidate(&self, _phys: PhysAddr, _len: usize) {
        // Deliberate no-op: x86_64 DMA is I/O-coherent, so a device sees the
        // kernel's cacheable writes without maintenance.
    }

    fn sync_instruction_cache(&self, _phys: PhysAddr, _len: usize) {
        // Deliberate no-op: the x86_64 instruction cache is coherent with
        // kernel data writes, so freshly loaded code needs no maintenance.
    }
}

/// The single, `'static` [`ConfiguredPhysMap`] the page-table frame
/// source borrows.
///
/// Also handed to the kernel core as the arch direct physical map
/// (`plans/USB.md`): it covers the same RAM the allocator draws from, so
/// any frame the kernel must reach by pointer is reachable.
pub static SPAWN_TABLE_PHYSMAP: ConfiguredPhysMap = ConfiguredPhysMap;

/// The single, `'static` allocator-backed page-table frame source every
/// spawned child's PML4 hierarchy is built from.
///
/// This replaces the former fixed `[PageTablePool; 8]` `.bss` reserve that
/// hard-capped the runtime `spawn` syscall at eight live processes — a
/// capacity ceiling that wasted RAM on a small machine and starved a
/// large one. Page-table frames now come from the kernel's live
/// [`FrameAllocator`] through [`FrameTableSource`], so the spawn capacity
/// **scales with discovered RAM and grows on demand**, failing closed with
/// [`Errno::NoSpace`] only when physical RAM is genuinely exhausted
/// (deterministic OOM, never a panic). The frames live exactly as long as
/// the child: its retained live space returns every table frame through
/// [`FrameTableSource::free_table`] when the task exits and the space is
/// dropped at reap (`plans/APPS.md` I2), so spawn/exit cycles hold the
/// allocator steady. Mirrors the aarch64 producer.
///
/// Initialised on the first `spawn` from the boot-threaded `'static`
/// allocator and reused thereafter — the source is stateless (its state
/// lives in the allocator), so one shared instance serves every CPU.
static SPAWN_FRAME_SOURCE: Once<FrameTableSource> = Once::new();

/// Borrow the `'static` allocator-backed page-table frame source,
/// initialising it from `frames` on the first call.
///
/// Fails closed with [`Errno::NotImplemented`] if the one-shot initialiser
/// was poisoned by a panicking earlier attempt — [`FrameTableSource::new`]
/// cannot panic, so this is unreachable in practice, but it is never
/// papered over.
pub(crate) fn page_table_source(
    frames: &'static FrameAllocator,
) -> Result<&'static FrameTableSource, Errno> {
    SPAWN_FRAME_SOURCE
        .call_once_infallible(|| FrameTableSource::new(frames, &SPAWN_TABLE_PHYSMAP))
        .map_err(|_| Errno::NotImplemented)
}

/// The x86_64 runtime `spawn` producer installed into the
/// [`tairix_kernel_core::BootInfo`] hand-off by `boot::try_boot`.
pub struct X86_64ProcessSpawn;

/// The single, `'static` [`X86_64ProcessSpawn`] the boot path borrows.
pub static X86_64_PROCESS_SPAWN: X86_64ProcessSpawn = X86_64ProcessSpawn;

impl ArchImageBuilder for X86_64ProcessSpawn {
    fn build(
        &self,
        rxe: &[u8],
        ctx: &dyn ImageBuildCtx,
        args: &[&[u8]],
        env: &[&[u8]],
    ) -> Result<BuiltImage, Errno> {
        // The child's PML4 hierarchy is drawn from the kernel's live frame
        // allocator: there is no fixed page-table reserve
        // and so no hard cap on how many processes can be spawned — the
        // capacity scales with discovered RAM and grows on demand. A build
        // with no `'static` allocator wired fails closed,
        // as does genuine RAM exhaustion below.
        let pt_frames = ctx
            .page_table_allocator()
            .ok_or_else(|| refuse_build(ctx, "page_table_allocator_unwired"))?;
        let table_frames = page_table_source(pt_frames)?;
        let dynamic = spawn_layout::dynamic_windows(pt_frames, &WINDOWS, super::USER_VA_TOP);

        // Parsed against the kernel's own compiled-in syscall CFI tag (a
        // mismatch fails closed), placed, and given its CPU set before the
        // child's space exists, so none of these refusals leaves anything
        // behind.
        let image = LoadImage::parse(rxe, &SYSCALL_TABLE_HASH).map_err(|_| Errno::BadMagic)?;
        let (request, stack_span, load_base) =
            spawn_layout::child_request(ctx, &image, rxe, args, env, dynamic.heap)?;
        let tlb = ctx
            .space_tlb()
            .map_err(|_| refuse_build(ctx, "space_tlb_unallocated"))?;

        // Build the child's PML4 and capture its root *without* switching
        // CR3: the spawning caller (PID 1) stays active under its own root,
        // so the running parent is never moved out from under itself. The
        // child's tables and image are written through the direct physical
        // map, which the caller's active root carries, so the build does not
        // require the child space to be active.
        // The child's own CR3 is reloaded by its `pre_resume` hook before the
        // scheduler first resumes it (`plans/SPAWN.md` SP2, `plans/PI.md` X1).
        let arch = ArchAddressSpace::new_process_root(table_frames)
            .ok_or_else(|| refuse_build(ctx, "page_table_frames_exhausted"))?;
        let child_root_phys = arch.pml4_phys();

        let physmap = ConfiguredPhysMap;

        // The image and stack are *user* pages, so they draw through the
        // reserve-gated user path: a spawn cannot dip into the kernel
        // reserve, nor steal a frame a prior `mem_map`/stack reservation is
        // guaranteeing, so it fails closed under genuine memory pressure
        // rather than overcommitting. (The child's page-table frames are
        // kernel structures drawn separately from the reserve above.)
        let frames = ctx.frames();
        // Build the child's ring-3 image (emits `ProcessSpawned`), then hand
        // its space to the child's threads as their process address space
        // (`plans/PI.md` 5d-0-ii (b′)) whatever the build answered, so their
        // `mem_map` / `mmio_map` mutate exactly the mappings the snapshot
        // describes and a refusal returns the frames rather than dropping the
        // space bare.
        let retained = build_retained(
            ctx.audit(),
            frames,
            AddressSpace::new(arch),
            |space| {
                // SAFETY: building the image is itself safe; the returned
                // `UserEntry` is only entered later, once the child is
                // dispatched and its switch-in hook has made `space` active
                // (the `spawn_image` contract). The tables and the image are
                // written through the direct map the caller's active root
                // carries.
                unsafe {
                    spawn_image(
                        &spawn_layout::SpawnAuthority,
                        SpawnMode::General,
                        ctx.audit(),
                        space,
                        &physmap,
                        &request,
                        move || frames.alloc_user(spawn_layout::SPAWN_IMAGE_CLASS).ok(),
                    )
                }
            },
            |space| {
                spawn_layout::live_space(
                    space,
                    tlb,
                    ConfiguredPhysMap,
                    pt_frames,
                    &WINDOWS,
                    &dynamic,
                )
            },
            // The child's switch-in hook (`plans/SPAWN.md` SP2, `plans/PI.md`
            // X1): the core runs it on the dispatcher's context immediately
            // before every switch into any thread of the child. It reloads CR3
            // to the child's own root (isolation), repoints the per-CPU
            // `syscall` entry stack at the switching-in thread's own kernel
            // stack, and reinstalls that thread's thread pointer. It captures
            // only the `u64` root, so it is `Send`.
            move |stack_top: u64, tls_base: u64, tag: AddressTag| {
                // `set_kernel_rsp0` repoints **both** the child's `syscall` entry
                // stack (`gs:0`) and its trap entry stack (`TSS.RSP0`) at the
                // child's own kernel stack — the latter is what makes an involuntary
                // LAPIC-timer preemption (P-1c), delivered through the IDT interrupt
                // gate which reads `TSS.RSP0`, land on the child's own stack rather
                // than corrupt a concurrently parked task's frame: one per-task
                // kernel stack serves both entry kinds. A rejected
                // value (validated canonical/aligned/kernel-half) leaves the slots
                // unchanged and the next entry faults loudly (fail closed).
                // The one CPU production x86_64 runs; a second needs the resuming
                // CPU named here (`plans/OPEN-DEFECTS.md` D378).
                let _ =
                    syscall_entry::set_kernel_rsp0(tairix_arch_api::BOOT_CPU as usize, stack_top);
                // The `FS` base is privileged on this port, so the kernel — not
                // the thread — maintains it: every switch-in reinstalls the
                // switching-in thread's own value (`plans/THREADS.md` decision 7).
                // SAFETY: this runs at CPL 0 on the dispatcher's context, on the
                // CPU about to enter that thread, which is exactly
                // `set_user_thread_pointer`'s contract.
                unsafe { set_user_thread_pointer(tls_base) };
                // SAFETY: paging is enabled and `child_root_phys` is the PML4 of
                // the child's space, which mirrors the higher-half kernel window
                // the running dispatcher executes from and carries the direct
                // physical map — exactly `activate_user_root`'s contract.
                unsafe { activate_user_root(child_root_phys, tag) };
            },
            &USER_MODE,
        )?;

        Ok(BuiltImage {
            frozen: retained.frozen,
            physmap: Box::new(physmap),
            stack_span,
            load_base,
            live: retained.live,
            pre_resume: retained.pre_resume,
            entry: retained.entry,
        })
    }
}
