//! aarch64 runtime `spawn` producer — `plans/SPAWN.md`
//! `SP3b`.
//!
//! [`Aarch64ProcessSpawn`] implements the architecture-neutral
//! [`tairix_kernel_core::ArchImageBuilder`] seam the boot pipeline installs
//! into the [`tairix_kernel_core::BootInfo`] hand-off
//! (`boot_aarch64::enter_kernel_core` → `BootInfo::with_spawn`). When a task
//! that holds `CAP_PROC_SPAWN` issues the `spawn` syscall, the kernel resolves
//! the requested path against the shared
//! [`crate::spawn_layout::PROGRAM_REGISTRY`], admits a parked **loading**
//! child, and returns its PID at once. On the child's own first scheduled
//! slice this producer's
//! [`build`](tairix_kernel_core::ArchImageBuilder::build) builds it a *fresh,
//! hardware-isolated* stage-1 address space and populates it through the
//! production capability-checked, audited spawn caller ([`spawn_image`], gated
//! on `CAP_PROC_SPAWN`). Unlike the PID-1 [`tairix_kernel_core::InitSpawn`]
//! seam (`init_spawn.rs`) it does **not** switch the active translation regime
//! or enter user mode from the caller: the spawning caller keeps running under
//! its own `TTBR0_EL1`, and the child enters user mode from its own loading
//! body when the scheduler next steps it (a true concurrent, non-blocking
//! spawn, not an `exec`-style hand-off).
//!
//! The child's `rxe`, its relocation bias ([`CHILD_USER_BIAS`]), and the
//! kernel's syscall CFI tag are baked at build time (`build.rs` →
//! `tairix_itest_harness::elf2rxe`), exactly like PID 1, so there is one
//! conversion path. Spawning is *not* a privileged
//! bypass: the child receives only the authority its registered program
//! declares intersected with its user's grants; this
//! seam only authorises the *act* of spawning under `CAP_PROC_SPAWN`.

use alloc::boxed::Box;
use core::ptr::NonNull;

use tairix_abi::rxe::LoadImage;
use tairix_abi::Errno;
use tairix_arch_aarch64::paging::{self, activate_user_root, AddressSpace as ArchAddressSpace};
use tairix_arch_aarch64::userentry::USER_MODE;
use tairix_arch_api::AddressTag;
use tairix_kernel_core::spawn::build_retained;
use tairix_kernel_core::{
    refuse_build, spawn_image, ArchImageBuilder, BuiltImage, ImageBuildCtx, SpawnMode,
};
use tairix_kernel_mem::{AddressSpace, FrameAllocator, FrameTableSource, PhysAddr, PhysMap};
use tairix_kernel_syscall::SYSCALL_TABLE_HASH;
use tairix_sync::Once;

use crate::spawn_layout::{self, CHILD_USER_BIAS};

/// The user virtual base every child image this producer builds is mapped
/// at — the build-time [`CHILD_USER_BIAS`] (64 GiB) `build.rs` bakes the
/// embedded programs' relocations for. Exported so a consumer handing this
/// producer's [`build`](tairix_kernel_core::ArchImageBuilder::build) an
/// *externally* converted `rxe` (the Stage 4.HW driver-spawn vertical) can
/// verify its image was relocated for the same bias and fail closed on a
/// mismatch rather than admit a child whose pointers do not match where it
/// is mapped.
pub const USER_IMAGE_BIAS: u64 = CHILD_USER_BIAS;

/// A spawned child's four fixed guarded-window bases (`plans/PI.md`
/// 5d-0-ii (b′)/(c)), derived from the one shared offset set the retained
/// [`LiveSpace`](tairix_kernel_mem::LiveSpace)'s window allocators are configured with.
const WINDOWS: spawn_layout::WindowBases = spawn_layout::window_bases(CHILD_USER_BIAS);

/// The kernel's direct physical map: the `TTBR1_EL1`-regime window at
/// [`paging::PHYSMAP_VMA_BASE`] the boot path sized from the discovered
/// memory map, where a covered physical `p` is reachable at
/// `PHYSMAP_VMA_BASE + p` (`plans/WIRING.md` W5b-3).
///
/// Every frame the kernel reaches by pointer goes through here — the
/// page-table walk's own table recovery, the process-image write, the
/// shared-region zero-on-free scrub, the slab page supply, the root-unlock
/// DMA pool. It lies in the kernel translation regime, so no user address
/// can name it and a process root pays no page and no slot for it.
///
/// Coverage is re-read from the live map on every translate
/// ([`paging::physmap_covers`]) rather than held as an extent, so no caller
/// can keep a stale one; and the map is deliberately sparse, because a
/// gigapage the board types Device gets no Normal-cacheable alias. A frame
/// the allocator draws from outside it fails the translate and the spawn
/// fails closed rather than building tables the walk cannot reach.
pub struct ConfiguredPhysMap;

impl PhysMap for ConfiguredPhysMap {
    fn translate(&self, phys: PhysAddr, len: usize) -> Option<NonNull<u8>> {
        let len_u64 = u64::try_from(len).ok()?;
        if !paging::physmap_covers(phys.as_u64(), len_u64) {
            return None;
        }
        let addr = usize::try_from(paging::physmap_virt(phys.as_u64())).ok()?;
        NonNull::new(addr as *mut u8)
    }

    fn reverse(&self, virt: usize) -> Option<PhysAddr> {
        // Recover the physical address of a direct-map virtual address (the
        // growable kernel heap hands a drained region back to the frame
        // allocator by its virtual base). Bound by the live map's coverage,
        // exactly as `translate` is, so a window or image address inverts to
        // nothing rather than to a bogus frame.
        let phys = (virt as u64).checked_sub(paging::PHYSMAP_VMA_BASE)?;
        paging::physmap_covers(phys, 1).then(|| PhysAddr::new(phys))
    }

    fn clean_invalidate(&self, phys: PhysAddr, len: usize) {
        self.clean_invalidate_each(&[phys], len);
    }

    fn clean_invalidate_each(&self, starts: &[PhysAddr], len: usize) {
        tairix_arch_aarch64::kernel_arch::clean_invalidate_dcache_ranges(
            starts
                .iter()
                .filter_map(|&phys| self.translate(phys, len))
                .map(|ptr| ptr.as_ptr() as usize),
            len,
        );
    }

    fn sync_instruction_cache(&self, phys: PhysAddr, len: usize) {
        // The loader fills a program's code pages through this direct map
        // (cacheable). The Cortex-A72's instruction cache is not
        // coherent with those data writes, so clean the written range to the
        // point of unification and invalidate the instruction cache before
        // the process fetches it — otherwise the PE executes stale bytes and
        // takes an EC=0 "unknown instruction" abort on valid code. `ic ivau`
        // is PA-effective, so syncing through this kernel alias covers the
        // code the process will fetch at its own user VA.
        if let Some(ptr) = self.translate(phys, len) {
            tairix_arch_aarch64::kernel_arch::sync_instruction_cache_range(
                ptr.as_ptr() as usize,
                len,
            );
        }
    }

    fn fence_instruction_fetch(&self) -> bool {
        // The sync's invalidate is inner-shareable, so it already reached
        // every PE, and each synchronises its context on its next exception
        // return before it can fetch the code.
        true
    }
}

/// The single, `'static` [`ConfiguredPhysMap`] the page-table frame source
/// borrows.
///
/// Also handed to the kernel core as the arch direct physical map the
/// shared-memory facility scrubs region frames through (`plans/USB.md`): it
/// covers the same RAM the allocator draws from, so any region frame is
/// reachable for the zero-on-free scrub.
pub static SPAWN_TABLE_PHYSMAP: ConfiguredPhysMap = ConfiguredPhysMap;

/// The single, `'static` allocator-backed page-table frame source every
/// spawned child's stage-1 hierarchy is built from.
///
/// This replaces the former fixed `[PageTablePool; 8]` `.bss` reserve that
/// hard-capped the runtime `spawn` syscall at eight live processes — a
/// capacity ceiling that wasted RAM on a small machine and starved a
/// large one. Page-table frames now come from the kernel's live
/// [`FrameAllocator`] through [`FrameTableSource`], so the spawn capacity
/// **scales with discovered RAM and grows on demand**: each child draws
/// only the handful of stage-1 tables it needs, and the system spawns
/// processes until physical RAM is genuinely exhausted, when
/// [`FrameTableSource::alloc_table`] returns `None` and the build fails
/// closed with [`Errno::NoSpace`] (deterministic
/// OOM, never a panic). The frames live exactly as long as the child: its
/// retained live space returns every table frame through
/// [`FrameTableSource::free_table`] when the task exits and the space is
/// dropped at reap (`plans/APPS.md` I2), so spawn/exit cycles hold the
/// allocator steady.
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
    let source = SPAWN_FRAME_SOURCE
        .call_once_infallible(|| FrameTableSource::new(frames, &SPAWN_TABLE_PHYSMAP))
        .map_err(|_| Errno::NotImplemented)?;
    // The fault-time access-flag fix-up walks whichever root is active
    // with no `AddressSpace` in hand, so it needs a source that reaches
    // any table in RAM. This one does — the direct map covers every
    // gigapage the allocator draws from — and publishing it here, where it
    // is created, means no consumer ordering can leave the fault path
    // without one.
    tairix_arch_api::frames::publish_active_frames(source);
    Ok(source)
}

/// The aarch64 runtime `spawn` producer installed into the
/// [`tairix_kernel_core::BootInfo`] hand-off by
/// `boot_aarch64::enter_kernel_core`.
pub struct Aarch64ProcessSpawn;

/// The single, `'static` [`Aarch64ProcessSpawn`] the boot path borrows.
pub static AARCH64_PROCESS_SPAWN: Aarch64ProcessSpawn = Aarch64ProcessSpawn;

impl ArchImageBuilder for Aarch64ProcessSpawn {
    fn build(
        &self,
        rxe: &[u8],
        ctx: &dyn ImageBuildCtx,
        args: &[&[u8]],
        env: &[&[u8]],
    ) -> Result<BuiltImage, Errno> {
        // The child's stage-1 hierarchy is drawn from the kernel's live
        // frame allocator: there is no fixed page-table reserve and so no
        // hard cap on how many processes can be spawned — the capacity
        // scales with discovered RAM and grows on demand. A build with no
        // `'static` allocator wired fails closed, as does genuine RAM
        // exhaustion below.
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

        // Build the child's empty root, which carries nothing of the
        // kernel's, and capture it *without* switching to it: the loading
        // child runs on its own kernel stack under the kernel's regime, so
        // the running task is never moved out from under itself. The child's
        // mappings below are written through the direct `physmap`. The
        // child's own root is reactivated by its `pre_resume` hook before
        // the scheduler first resumes it as a user task (`plans/SPAWN.md`
        // SP2).
        let arch = ArchAddressSpace::new_user(table_frames)
            .ok_or_else(|| refuse_build(ctx, "page_table_frames_exhausted"))?;
        let child_root_phys = arch.root_phys();

        // The loader fills the child's segment pages through this physmap and,
        // for an executable segment, makes them instruction-fetch-coherent
        // through its `sync_instruction_cache`. On the Cortex-A72 the I-cache
        // is not coherent with those cacheable data-side writes, so this must
        // be the physmap whose cache maintenance is *real*
        // (`ConfiguredPhysMap`: clean-to-PoU + I-cache invalidate), not
        // a plain `DirectPhysMap` whose `sync_instruction_cache` /
        // `clean_invalidate` are no-ops for I/O-coherent targets — otherwise a
        // freshly-loaded driver fetches stale bytes and dies on a wild fault.
        // It also backs the stored `BuiltImage.physmap` below, whose
        // `clean_invalidate` the shared-memory zero-on-free scrub relies on
        // being real here too.
        let physmap = ConfiguredPhysMap;

        // The image and stack are *user* pages, so they draw through the
        // reserve-gated user path: a spawn cannot dip into the kernel
        // reserve, nor steal a frame a prior `mem_map`/stack reservation is
        // guaranteeing, so it fails closed under genuine memory pressure
        // rather than overcommitting. (The child's page-table frames are
        // kernel structures drawn separately from the reserve above.)
        let frames = ctx.frames();
        // Build the child's EL0 image (emits `ProcessSpawned`), then hand its
        // space to the child's threads as their process address space whatever
        // the build answered, so their `mem_map` / `mmio_map` mutate exactly
        // the mappings the snapshot describes and a refusal returns the frames
        // rather than dropping the space bare. The live space's `PhysMap` must
        // be [`ConfiguredPhysMap`] so the child's `dma_alloc` post-zero
        // `clean_invalidate` is the real dcache clean+invalidate.
        let retained = build_retained(
            ctx.audit(),
            frames,
            AddressSpace::new(arch),
            |space| {
                // SAFETY: building the image is itself safe; the returned
                // `UserEntry` is only entered later, once the child is
                // dispatched and its switch-in hook has made `space` active
                // (the `spawn_image` contract).
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
            // The child's user-address-space reactivation hook (`plans/SPAWN.md`
            // SP2): the core runs it on the dispatcher's context immediately
            // before every switch into the child, so it `eret`s into EL0 under
            // its own `TTBR0_EL1` root. It captures only the `u64` root, so it is
            // `Send`. aarch64 reuses `SP_EL1` and ignores the stack-top argument.
            move |_stack_top: u64, _tls_base: u64, tag: AddressTag| {
                // SAFETY: the kernel runs from `TTBR1_EL1` and `child_root_phys`
                // is the L1 root of the child's space, live while the child
                // is — exactly `activate_user_root`'s contract.
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
