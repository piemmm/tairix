use alloc::rc::Rc;
use alloc::string::String;
use core::cell::RefCell;

use tairix_reclaim::pressure::Unpressured;
use tairix_riscos_kernel::desktop::DesktopFont;
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_kernel::memory::policy::Ceiling;
use tairix_riscos_kernel::os::{Ending, Kernel, Pulse};
use tairix_riscos_kernel::relay::{Relayed, Side};

use super::*;

/// The bands a test's worker reported, in order.
#[derive(Clone, Default)]
struct Reported(Rc<RefCell<Vec<PressureBand>>>);

impl Memory for Reported {
    fn gauge(&self) -> &'static (dyn PressureGauge + 'static) {
        &Unpressured
    }

    fn report(&self, band: PressureBand) {
        self.0.borrow_mut().push(band);
    }
}

/// A host with nothing for the guest: no input, no time passing, no discs.
struct Idle;

impl Host for Idle {
    fn monotonic(&mut self) -> u64 {
        0
    }

    fn real_time(&mut self) -> u64 {
        0x58_0000_0000
    }

    fn pulse(&mut self) -> Pulse {
        Pulse::Continue
    }

    fn wait(&mut self, _: Option<u64>, _: Ending) -> Pulse {
        Pulse::Stop
    }

    fn input(&mut self) -> Option<InputEvent> {
        None
    }

    fn relay(&mut self, _: Side, _: &[u8], _: &mut Vec<u8>, _: bool) -> Relayed {
        Relayed::Failed
    }
}

fn started(zone: Option<(Vec<u8>, String)>) -> Started {
    Started {
        ceiling: Ceiling::derive(256 << 20),
        memo_pages: 16,
        zone,
        font: DesktopFont::SHIPPED,
        pressure: PressureBand::Moderate,
    }
}

fn worker() -> (Worker<Reported>, Reported) {
    let reported = Reported::default();
    (Worker::new(reported.clone(), HashSeed::UNKEYED), reported)
}

#[test]
fn a_session_starts_with_every_crate_s_modules_together() {
    let (mut worker, reported) = worker();
    let config = worker.config(Box::new(Idle), started(None)).unwrap();
    let count: usize = MODULES.iter().map(|modules| modules.len()).sum();
    assert_eq!(config.modules.len(), count);
    assert!(Kernel::new(config).is_ok(), "the session's kernel made");
    assert_eq!(
        *reported.0.borrow(),
        [PressureBand::Moderate],
        "its start's band"
    );
}

#[test]
fn a_session_reads_the_zone_it_was_started_in() {
    let (mut worker, _) = worker();
    let store = tairix_tz::testkit::store();
    let zoned = Some((store, String::from("America/New_York")));
    let config = worker.config(Box::new(Idle), started(zoned)).unwrap();
    assert_eq!(config.zone.name(), "America/New_York");
    let config = worker.config(Box::new(Idle), started(None)).unwrap();
    assert_eq!(config.zone.name(), Zone::UTC.name(), "none: UTC");
}

#[test]
fn a_zone_the_store_does_not_hold_or_a_refused_store_is_no_session_s() {
    let (mut worker, _) = worker();
    let store = tairix_tz::testkit::store();
    let absent = Some((store.clone(), String::from("Atlantis/Capital")));
    assert_eq!(
        worker.config(Box::new(Idle), started(absent)).err(),
        Some(Unconfigured::Refused("a zone its store does not hold"))
    );
    let mut broken = store;
    broken.truncate(8);
    let refused = Some((broken, String::from("Europe/London")));
    assert_eq!(
        worker.config(Box::new(Idle), started(refused)).err(),
        Some(Unconfigured::Refused(
            "a zone store the zone reader refuses"
        ))
    );
}

#[test]
fn the_machine_s_pressure_is_handed_on() {
    let (mut worker, reported) = worker();
    worker.pressure(PressureBand::Critical);
    assert_eq!(*reported.0.borrow(), [PressureBand::Critical]);
}
