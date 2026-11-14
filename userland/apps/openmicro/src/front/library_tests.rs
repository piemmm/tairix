use alloc::string::String;
use alloc::vec::Vec;

use tairix_crt::screen::Scaling;
use tairix_crt::{Tube, FULL};
use tairix_machines::{Kept, Library};

use super::*;
use crate::front::keys::Mapping;

fn kept(machines: Vec<Machine>) -> Library<Machine> {
    let mut library = Library::new();
    for machine in machines {
        let id = library.mint().expect("room");
        library
            .add(Machine { id, ..machine })
            .expect("a machine it holds");
    }
    library
}

fn a_model_b() -> Machine {
    Machine::new(0, String::from("Model B"), Fitting::model_b())
}

fn an_electron(plus1: bool) -> Machine {
    Machine::new(0, String::from("Electron"), Fitting::electron(plus1))
}

/// Every model, as made and as changed, reads back exactly from its record.
#[test]
fn every_model_reads_back_from_its_record() {
    let mut changed = an_electron(true);
    if let Some(image) = changed.fitting.socket_mut(Socket::Cartridge(3)) {
        *image = Image::Installed(String::from("rom.elite"));
    }
    changed.tube = Tube {
        scanlines: 450,
        ..Tube::default()
    };
    changed.scaling = Scaling::Smooth;
    changed.keyboard = Mapping::Physical;
    changed.volume = 250;
    changed.fast_tape = false;
    changed.folder = Some(String::from("Electron"));
    changed.tape = Some(Held {
        name: String::from("Blank.uef"),
        digest: Some([9; 32]),
    });
    let mut disced = a_model_b();
    disced.folder = Some(String::from("Model B"));
    disced.discs[1] = Some(Held {
        name: String::from("Games.ssd"),
        digest: None,
    });
    let library = kept(alloc::vec![
        a_model_b(),
        an_electron(false),
        changed,
        disced
    ]);
    let record = library.record().expect("a record");
    let read = Library::<Machine>::decode(&record.bytes).expect("it reads back");
    assert_eq!(read.machines(), library.machines());
}

/// A record naming what no socket may hold, or media the machine cannot hold
/// or name, is refused.
#[test]
fn a_machine_holding_what_it_may_not_is_refused() {
    let refused = |change: fn(&mut Machine)| {
        let mut machine = a_model_b();
        change(&mut machine);
        machine.invalid()
    };
    assert_eq!(a_model_b().invalid(), None);
    assert_eq!(an_electron(true).invalid(), None);
    assert_eq!(
        refused(|m| *m.fitting.socket_mut(Socket::ModelBOs).expect("an OS") = Image::Empty),
        Some(Invalid::Fitting),
        "a machine with no OS"
    );
    assert_eq!(
        refused(|m| {
            *m.fitting.socket_mut(Socket::Slot(3)).expect("a slot") =
                Image::Shipped(Shipped::ElectronOs);
        }),
        Some(Invalid::Fitting),
        "an OS in a sideways slot"
    );
    assert_eq!(
        refused(|m| {
            *m.fitting.socket_mut(Socket::Slot(3)).expect("a slot") =
                Image::Installed(String::from("rom."));
        }),
        Some(Invalid::Fitting),
        "an installed image with no name"
    );
    assert_eq!(
        refused(|m| {
            if let Fitting::ModelB { links, .. } = &mut m.fitting {
                links.mode = 8;
            }
        }),
        Some(Invalid::Fitting)
    );
    assert_eq!(refused(|m| m.tube.glow = FULL + 1), Some(Invalid::Display));
    assert_eq!(
        refused(|m| m.volume = FULL_VOLUME + 1),
        Some(Invalid::Volume)
    );
    assert_eq!(
        refused(|m| m.folder = Some(String::from(".hidden"))),
        Some(Invalid::Folder)
    );
    assert_eq!(
        refused(|m| {
            m.tape = Some(Held {
                name: String::from("Tape.uef"),
                digest: None,
            });
        }),
        Some(Invalid::Folder),
        "media of a folder it has not got"
    );
    let mut electron = an_electron(false);
    electron.folder = Some(String::from("Electron"));
    electron.discs[0] = Some(Held {
        name: String::from("Disc.ssd"),
        digest: None,
    });
    assert_eq!(
        electron.invalid(),
        Some(Invalid::Media),
        "no drives to hold it"
    );
    let mut library = Library::new();
    let id = library.mint().expect("room");
    let mut machine = a_model_b();
    machine.id = id;
    machine.volume = FULL_VOLUME + 1;
    assert!(library.add(machine).is_err(), "the library refuses it");
}

#[test]
fn the_links_read_as_osbyte_ff_does() {
    assert_eq!(Links::default().byte(), 0xC0 | 3 << 4 | 1 << 3 | 7);
    let links = Links {
        mode: 1,
        shift_boots: false,
        disc_timing: 0,
    };
    assert_eq!(links.byte(), 0xC1);
}

/// A Model B's sockets run from its OS down its slots; an Electron's from its
/// OS through BASIC to the Plus 1's ROM and its cartridges.
#[test]
fn the_sockets_are_listed_as_the_settings_show_them() {
    let model_b: Vec<Socket> = Fitting::model_b().sockets().map(|(s, _)| s).collect();
    assert_eq!(model_b.first(), Some(&Socket::ModelBOs));
    assert_eq!(model_b.get(1), Some(&Socket::Slot(15)));
    assert_eq!(model_b.last(), Some(&Socket::Slot(0)));
    assert_eq!(model_b.len(), 1 + SLOTS);
    let electron: Vec<Socket> = Fitting::electron(true).sockets().map(|(s, _)| s).collect();
    assert_eq!(
        electron,
        [
            Socket::ElectronOs,
            Socket::Basic,
            Socket::Plus1,
            Socket::Cartridge(0),
            Socket::Cartridge(1),
            Socket::Cartridge(2),
            Socket::Cartridge(3),
        ]
    );
    assert_eq!(Fitting::electron(false).sockets().count(), 2);
    assert!(Fitting::electron(false).socket_mut(Socket::Plus1).is_none());
    assert_eq!(Socket::Cartridge(3).label(), "Cartridge 2, ROM 2");
}

#[test]
fn a_machine_is_described_by_its_model_and_what_it_adds() {
    assert_eq!(describe(&a_model_b()), "BBC Micro Model B with the DFS");
    assert_eq!(describe(&an_electron(true)), "Acorn Electron with a Plus 1");
    assert_eq!(describe(&an_electron(false)), "Acorn Electron");
    let mut bare = a_model_b();
    if let Fitting::ModelB { slots, .. } = &mut bare.fitting {
        **slots = core::array::from_fn(|_| Image::Empty);
    }
    assert_eq!(describe(&bare), "BBC Micro Model B");
}
