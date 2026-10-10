//! The system-notice registry: one machine-wide value per topic, and the
//! generation a subscriber converges on (`plans/NOTICE.md`).
//!
//! A notice is a **state edge**. Each [`NoticeTopic`] names one value; a
//! subscriber learns only that it moved and then reads the current one. There
//! is therefore no queue here, and nothing to overflow, drop, or hold back —
//! the whole state is `(generation, payload)` per topic, and a waiter's
//! `observed` generation is the whole of its subscription.
//!
//! # Where each topic's truth lives
//!
//! A topic whose value the *kernel* already owns is not stored twice:
//! [`generation`] reads it from its own source and [`payload`] renders it on
//! demand, so the registry cannot disagree with the thing it describes.
//! Only a topic published from user space is retained here.
//!
//! | Topic | Generation | Payload |
//! |---|---|---|
//! | `Desktop` | a counter bumped when the published record differs | retained here |
//! | `Mounts` | a counter bumped by every mount-table mutation | none |
//! | `MemoryPressure` | the published band's depth | rendered from the gauge |
//! | `DisplayLease` | the boot seat's lease word | the word and its holder's login session, read from the registry |
//! | `AudioCapture` | a counter bumped when the published count differs | retained here |
//!
//! The display lease's word only grows across transitions, so it is its own
//! generation, and both are read from the seat registry the caller runs
//! against rather than copied here.
//!
//! # Who may observe a topic
//!
//! Every topic is readable by any process bar the display lease: when a
//! console lease is taken and given up is what the gated seat inventory
//! reports, so only the two services that own the seat's devices and must
//! follow it — display and audio — are told it ([`may_observe`]).
//!
//! The memory-pressure generation is the band depth *itself* rather than a
//! counter, so a band that deepens and relaxes again before a waiter runs
//! correctly reports nothing to do. A counter cannot express that, and the
//! desktop record is too wide to be one, so a desktop value that moves and
//! moves back wakes its subscribers once with nothing changed — which costs
//! them one read and no work, and never misses a real change.
//!
//! # Why a global
//!
//! Like [`crate::waitset`], [`crate::callreg`], and [`crate::waitq`], this is
//! global pure data behind a [`SpinLock`] (never a `static mut`, so not the
//! global mutable static the charter forbids): a topic is published from a
//! syscall, from a mount-table mutation, and from whatever was spending
//! memory when the band moved, and read from every subscriber's wake. None of
//! those owns the others, so a global keyed by the topic is the rendezvous.

use core::sync::atomic::{AtomicU64, Ordering};

use tairix_abi::audio::AUDIO_ENDPOINT;
use tairix_abi::display_ipc::DISPLAY_ENDPOINT;
use tairix_abi::notice::{Notice, NoticeTopic, NOTICE_PAYLOAD_MAX};
use tairix_abi::{Errno, ProcId};
use tairix_kernel_ipc::EndpointId;
use tairix_kernel_sec::{CapTable, ProcessId, TaskCapabilities};
use tairix_seat::SeatOwner;
use tairix_sync::{RwLock, SpinLock};

use crate::seat::SeatRegistry;

/// The retained state of one user-published topic.
#[derive(Copy, Clone)]
struct Slot {
    /// Bumped only when a publish actually changes the payload, so
    /// re-publishing the value already in force wakes nobody.
    generation: u64,
    len: usize,
    bytes: [u8; NOTICE_PAYLOAD_MAX],
}

impl Slot {
    const EMPTY: Self = Self {
        generation: 0,
        len: 0,
        bytes: [0u8; NOTICE_PAYLOAD_MAX],
    };
}

/// The user-published topics' retained payloads, indexed by
/// [`NoticeTopic::as_u32`].
///
/// Sized by the topic set rather than grown: the set is closed, so an array is
/// the whole registry and a lookup is an index.
static SLOTS: SpinLock<[Slot; NoticeTopic::ALL.len()]> =
    SpinLock::new([Slot::EMPTY; NoticeTopic::ALL.len()]);

/// The mount table's change generation: bumped by every mutation of the
/// table, which is the whole of the `Mounts` topic's news.
///
/// An atomic rather than a slot in [`SLOTS`]: a mount mutation runs with the
/// filesystem's own locks held, so it must not take another.
static MOUNT_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Record that the mount table's composition changed, and flag a wake of
/// every `Mounts` subscriber.
///
/// Called from each [`crate::fs::MountTable`] mutator, so no call site that
/// attaches, re-backs, or removes a mount can forget to.
pub fn mounts_changed() {
    MOUNT_GENERATION.fetch_add(1, Ordering::Release);
    crate::waitq::notice_wake();
}

/// Whether the process `observer` may read `topic` or be woken by it.
///
/// The display lease is for the services owning the seat's devices alone: the
/// processes the kernel attests bound the reserved display and audio
/// rendezvous, which only a privileged bind can.
#[must_use]
pub fn may_observe(topic: NoticeTopic, observer: u64) -> bool {
    match topic {
        NoticeTopic::Desktop
        | NoticeTopic::Mounts
        | NoticeTopic::MemoryPressure
        | NoticeTopic::AudioCapture
        | NoticeTopic::AudioDevices => true,
        NoticeTopic::DisplayLease => {
            binds(DISPLAY_ENDPOINT, observer) || binds(AUDIO_ENDPOINT, observer)
        }
    }
}

/// Whether the process `process` is the one bound to the reserved call
/// endpoint `endpoint` — the identity a service is attested by.
#[must_use]
pub fn binds(endpoint: u64, process: u64) -> bool {
    crate::callreg::lookup(EndpointId(endpoint)).is_some_and(|bound| bound.owner() == process)
}

/// The generation of `topic` on a kernel whose seats are `seats`: the value a
/// subscriber's `observed` is compared against, and the one definition the
/// readiness scan uses for every topic.
#[must_use]
pub fn generation(topic: NoticeTopic, seats: &SeatRegistry) -> u64 {
    match topic {
        NoticeTopic::Desktop | NoticeTopic::AudioCapture | NoticeTopic::AudioDevices => {
            SLOTS.lock()[topic.as_u32() as usize].generation
        }
        NoticeTopic::Mounts => MOUNT_GENERATION.load(Ordering::Acquire),
        // The band depth *is* the generation, so a move and a move back
        // leave a waiter's view already correct.
        NoticeTopic::MemoryPressure => {
            u64::from(crate::memstats::MEM_STATS.published_band().depth())
        }
        NoticeTopic::DisplayLease => seats.boot_lease_epoch(),
    }
}

/// Write `topic`'s current payload on a kernel whose seats are `seats` and
/// whose processes are `caps` into `out`, answering its length.
///
/// A topic no publisher has reached yet answers `None`: there is no value to
/// converge on, and fabricating one would have a subscriber adopt a desktop
/// the session never described (fail closed).
///
/// # Errors
///
/// [`Errno::LengthOutOfRange`] if `out` cannot hold the topic's payload.
pub fn payload(
    topic: NoticeTopic,
    seats: &SeatRegistry,
    caps: &RwLock<CapTable>,
    out: &mut [u8],
) -> Result<Option<usize>, Errno> {
    match topic {
        NoticeTopic::Desktop | NoticeTopic::AudioCapture | NoticeTopic::AudioDevices => {
            let slot = SLOTS.lock()[topic.as_u32() as usize];
            if slot.len == 0 {
                return Ok(None);
            }
            let Some(dst) = out.get_mut(..slot.len) else {
                return Err(Errno::LengthOutOfRange);
            };
            dst.copy_from_slice(&slot.bytes[..slot.len]);
            Ok(Some(slot.len))
        }
        NoticeTopic::Mounts => Notice::Mounts.encode(out).map(Some),
        NoticeTopic::MemoryPressure => Notice::MemoryPressure {
            band: crate::memstats::MEM_STATS.published_band().depth(),
        }
        .encode(out)
        .map(Some),
        NoticeTopic::DisplayLease => {
            // The table before any seat's lock, never after: a dying process's
            // seats are reclaimed with no table lock held.
            let caps = caps.read();
            let session_of = |owner: SeatOwner| {
                caps.caps_of_process(ProcessId(owner.0))
                    .map_or(ProcId::KERNEL, TaskCapabilities::login_session)
            };
            Notice::DisplayLease(seats.boot_lease(&session_of))
                .encode(out)
                .map(Some)
        }
    }
}

/// Publish `notice` as `topic`'s current value, answering whether it moved.
///
/// A publish of the value already in force answers `false` and bumps no
/// generation, so a publisher may re-state the current value freely without
/// waking a single subscriber. A real change flags the wake; the unpark itself
/// runs at the next dispatcher-context drain, exactly as every other deferred
/// wake does.
///
/// # Errors
///
/// [`Errno::PermissionDenied`] for a topic the kernel owns — its value is not
/// user space's to assert. [`Errno::LengthOutOfRange`] is unreachable for a
/// well-formed [`Notice`] (its payload is bounded by the ceiling the slot is
/// sized to) and is reported rather than ignored.
pub fn publish(notice: &Notice) -> Result<bool, Errno> {
    let topic = notice.topic();
    if !matches!(
        topic,
        NoticeTopic::Desktop | NoticeTopic::AudioCapture | NoticeTopic::AudioDevices
    ) {
        return Err(Errno::PermissionDenied);
    }
    let mut bytes = [0u8; NOTICE_PAYLOAD_MAX];
    let len = notice.encode(&mut bytes)?;
    let moved = {
        let mut slots = SLOTS.lock();
        let slot = &mut slots[topic.as_u32() as usize];
        if slot.len == len && slot.bytes[..len] == bytes[..len] {
            false
        } else {
            slot.len = len;
            slot.bytes = bytes;
            slot.generation = slot.generation.wrapping_add(1);
            true
        }
    };
    if moved {
        crate::waitq::notice_wake();
    }
    Ok(moved)
}

/// Serialise the tests that *steer* a user-published topic, and clear the
/// registry for the one now holding the guard.
///
/// The registry is process-global while the test binary runs tests
/// concurrently, so two tests publishing the desktop at once would each see
/// the other's value and neither would be testing anything. Distinct payloads
/// do not prevent it; only one test being in the registry at a time does.
#[cfg(test)]
pub(crate) fn registry_guard() -> std::sync::MutexGuard<'static, ()> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *SLOTS.lock() = [Slot::EMPTY; NoticeTopic::ALL.len()];
    guard
}

#[cfg(test)]
mod tests {
    use super::{generation, mounts_changed, payload, publish, registry_guard};
    use crate::console::NULL_CONSOLE_INPUT;
    use crate::seat::{SeatRegistry, NULL_SEAT_REGISTRY};
    use tairix_abi::desktop::{Appearance, DesktopInfo};
    use tairix_abi::notice::{Notice, NoticeTopic, NOTICE_PAYLOAD_MAX};
    use tairix_abi::seat::{DisplayLease, ReleaseSurface, SEAT_PRIMARY};
    use tairix_abi::{Errno, ProcId, SpawnSession};
    use tairix_caps::CapabilitySet;
    use tairix_kernel_sec::{CapTable, ProcessId, TaskCapabilities, UserId};
    use tairix_log::Event;
    use tairix_seat::SeatOwner;
    use tairix_sync::RwLock;

    struct Quiet;

    impl tairix_log::Sink for Quiet {
        fn write_event(&self, _event: &Event<'_>) {}
    }

    fn no_processes() -> RwLock<CapTable> {
        RwLock::new(CapTable::new())
    }

    fn desktop(scale: u16, appearance: Appearance) -> Notice {
        match DesktopInfo::new(1024, 768, scale, appearance) {
            Ok(info) => Notice::Desktop(info),
            Err(_) => unreachable!("a valid desktop"),
        }
    }

    #[test]
    fn an_unpublished_topic_answers_nothing_rather_than_a_guess() {
        let _registry = registry_guard();
        let mut out = [0u8; NOTICE_PAYLOAD_MAX];
        assert_eq!(
            payload(
                NoticeTopic::Desktop,
                &NULL_SEAT_REGISTRY,
                &no_processes(),
                &mut out
            ),
            Ok(None)
        );
    }

    #[test]
    fn a_publish_moves_the_generation_and_is_readable_back() {
        let _registry = registry_guard();
        let before = generation(NoticeTopic::Desktop, &NULL_SEAT_REGISTRY);
        let notice = desktop(100, Appearance::Dark);
        assert_eq!(publish(&notice), Ok(true));
        assert_ne!(
            generation(NoticeTopic::Desktop, &NULL_SEAT_REGISTRY),
            before
        );
        let mut out = [0u8; NOTICE_PAYLOAD_MAX];
        let len = payload(
            NoticeTopic::Desktop,
            &NULL_SEAT_REGISTRY,
            &no_processes(),
            &mut out,
        )
        .expect("a published topic")
        .expect("a payload");
        assert_eq!(
            Notice::decode(NoticeTopic::Desktop, &out[..len]),
            Ok(notice)
        );
    }

    /// Re-stating the value already in force is not news: a publisher may do
    /// it freely without waking a single subscriber.
    #[test]
    fn republishing_the_same_value_moves_nothing() {
        let _registry = registry_guard();
        let notice = desktop(150, Appearance::Light);
        assert_eq!(publish(&notice), Ok(true));
        let after_first = generation(NoticeTopic::Desktop, &NULL_SEAT_REGISTRY);
        assert_eq!(publish(&notice), Ok(false));
        assert_eq!(
            generation(NoticeTopic::Desktop, &NULL_SEAT_REGISTRY),
            after_first
        );
    }

    #[test]
    fn each_distinct_value_is_one_edge() {
        let _registry = registry_guard();
        assert_eq!(publish(&desktop(100, Appearance::Dark)), Ok(true));
        let dark = generation(NoticeTopic::Desktop, &NULL_SEAT_REGISTRY);
        assert_eq!(publish(&desktop(100, Appearance::Light)), Ok(true));
        let light = generation(NoticeTopic::Desktop, &NULL_SEAT_REGISTRY);
        assert_ne!(dark, light);
        assert_eq!(publish(&desktop(100, Appearance::Light)), Ok(false));
        assert_eq!(generation(NoticeTopic::Desktop, &NULL_SEAT_REGISTRY), light);
    }

    /// The capture count is retained like the desktop, and moves its
    /// generation only when the count does.
    #[test]
    fn the_capture_count_is_retained_and_moves_only_on_a_change() {
        let _registry = registry_guard();
        let before = generation(NoticeTopic::AudioCapture, &NULL_SEAT_REGISTRY);
        assert_eq!(publish(&Notice::AudioCapture { live: 1 }), Ok(true));
        let recording = generation(NoticeTopic::AudioCapture, &NULL_SEAT_REGISTRY);
        assert_ne!(recording, before);
        assert_eq!(publish(&Notice::AudioCapture { live: 1 }), Ok(false));
        assert_eq!(
            generation(NoticeTopic::AudioCapture, &NULL_SEAT_REGISTRY),
            recording
        );
        let mut out = [0u8; NOTICE_PAYLOAD_MAX];
        let len = payload(
            NoticeTopic::AudioCapture,
            &NULL_SEAT_REGISTRY,
            &no_processes(),
            &mut out,
        )
        .expect("fits")
        .expect("published");
        assert_eq!(
            Notice::decode(NoticeTopic::AudioCapture, &out[..len]),
            Ok(Notice::AudioCapture { live: 1 })
        );
    }

    /// Every change the audio service counts is a new value, so each one
    /// wakes a subscriber, and anyone may read it.
    #[test]
    fn each_counted_device_change_moves_the_generation() {
        let _registry = registry_guard();
        let before = generation(NoticeTopic::AudioDevices, &NULL_SEAT_REGISTRY);
        assert_eq!(publish(&Notice::AudioDevices { changes: 1 }), Ok(true));
        let first = generation(NoticeTopic::AudioDevices, &NULL_SEAT_REGISTRY);
        assert_ne!(first, before);
        assert_eq!(publish(&Notice::AudioDevices { changes: 2 }), Ok(true));
        assert_ne!(
            generation(NoticeTopic::AudioDevices, &NULL_SEAT_REGISTRY),
            first
        );
        assert!(super::may_observe(NoticeTopic::AudioDevices, 77));
        let mut out = [0u8; NOTICE_PAYLOAD_MAX];
        let len = payload(
            NoticeTopic::AudioDevices,
            &NULL_SEAT_REGISTRY,
            &no_processes(),
            &mut out,
        )
        .expect("fits")
        .expect("published");
        assert_eq!(
            Notice::decode(NoticeTopic::AudioDevices, &out[..len]),
            Ok(Notice::AudioDevices { changes: 2 })
        );
    }

    #[test]
    fn a_kernel_owned_topic_refuses_a_publish() {
        let _registry = registry_guard();
        assert_eq!(publish(&Notice::Mounts), Err(Errno::PermissionDenied));
        assert_eq!(
            publish(&Notice::MemoryPressure { band: 0 }),
            Err(Errno::PermissionDenied)
        );
        assert_eq!(
            publish(&Notice::DisplayLease(DisplayLease::held(1, ProcId::KERNEL))),
            Err(Errno::PermissionDenied)
        );
    }

    fn lease_notice(seats: &SeatRegistry, caps: &RwLock<CapTable>) -> DisplayLease {
        let mut out = [0u8; NOTICE_PAYLOAD_MAX];
        let len = payload(NoticeTopic::DisplayLease, seats, caps, &mut out)
            .expect("fits")
            .expect("always has a value");
        match Notice::decode(NoticeTopic::DisplayLease, &out[..len]) {
            Ok(Notice::DisplayLease(lease)) => lease,
            other => panic!("the lease topic decodes as a lease, not {other:?}"),
        }
    }

    /// The topic is the registry's own lease: acquire and release both move
    /// it, and what a woken subscriber reads is the lease its wake described.
    #[test]
    fn the_lease_topic_is_the_boot_seats_lease_where_it_lives() {
        let seats = SeatRegistry::new(&NULL_CONSOLE_INPUT);
        let caps = no_processes();
        assert_eq!(lease_notice(&seats, &caps), DisplayLease::UNHELD);
        let unheld = generation(NoticeTopic::DisplayLease, &seats);

        let owner = SeatOwner(7);
        let lease = seats.acquire(SEAT_PRIMARY, owner).expect("free");
        let held = generation(NoticeTopic::DisplayLease, &seats);
        assert!(held > unheld);
        let read = lease_notice(&seats, &caps);
        assert_eq!(read.epoch(), held);
        assert_eq!(read.live_generation(), Some(lease.generation));

        seats
            .release(SEAT_PRIMARY, owner, ReleaseSurface::Text)
            .expect("held");
        assert!(generation(NoticeTopic::DisplayLease, &seats) > held);
        assert_eq!(lease_notice(&seats, &caps).live_generation(), None);
    }

    /// What a woken service reads names the login the holder was admitted
    /// into, as the capability table attests it — never a claim of the
    /// holder's own.
    #[test]
    fn the_lease_names_the_login_its_holder_lies_within() {
        let seats = SeatRegistry::new(&NULL_CONSOLE_INPUT);
        let desktop = ProcId::from_raw([0x7D; 16]);
        let caps = no_processes();
        {
            let mut table = caps.write();
            let placement = table
                .resolve_placement(ProcessId::KERNEL, SpawnSession::New, true)
                .expect("placeable");
            let set = CapabilitySet::empty();
            let record = TaskCapabilities::derive(ProcessId(7), UserId(1000), set, set, &Quiet)
                .with_proc_id(desktop);
            table.admit(record, placement).expect("admitted");
        }
        seats.acquire(SEAT_PRIMARY, SeatOwner(7)).expect("free");
        assert_eq!(lease_notice(&seats, &caps).session(), Some(desktop));

        // A holder no login encloses names none.
        seats
            .release(SEAT_PRIMARY, SeatOwner(7), ReleaseSurface::Handover)
            .expect("held");
        seats.acquire(SEAT_PRIMARY, SeatOwner(9)).expect("free");
        assert_eq!(lease_notice(&seats, &caps).session(), None);
    }

    /// Monotone in the shared counter (observe, bump, observe greater)
    /// rather than an exact step: the counter is process-global and the
    /// test binary runs tests concurrently, so pinning the increment would
    /// be a race, not a test.
    #[test]
    fn a_mount_mutation_moves_the_mounts_generation() {
        let before = generation(NoticeTopic::Mounts, &NULL_SEAT_REGISTRY);
        mounts_changed();
        assert!(generation(NoticeTopic::Mounts, &NULL_SEAT_REGISTRY) > before);
    }

    /// The payload-less topic still reads back — as nothing — so a subscriber
    /// that reads every topic the same way is not special-cased.
    #[test]
    fn the_mounts_topic_reads_back_an_empty_payload() {
        let mut out = [0xAAu8; NOTICE_PAYLOAD_MAX];
        assert_eq!(
            payload(
                NoticeTopic::Mounts,
                &NULL_SEAT_REGISTRY,
                &no_processes(),
                &mut out
            ),
            Ok(Some(0))
        );
        assert_eq!(out, [0xAA; NOTICE_PAYLOAD_MAX]);
    }

    #[test]
    fn a_short_buffer_is_refused_rather_than_truncating_a_payload() {
        let _registry = registry_guard();
        assert_eq!(publish(&desktop(100, Appearance::Dark)), Ok(true));
        let mut tiny = [0u8; DesktopInfo::WIRE_LEN - 1];
        assert_eq!(
            payload(
                NoticeTopic::Desktop,
                &NULL_SEAT_REGISTRY,
                &no_processes(),
                &mut tiny
            ),
            Err(Errno::LengthOutOfRange)
        );
    }
}
