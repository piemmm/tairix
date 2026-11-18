use alloc::string::String;
use alloc::vec::Vec;

use super::*;
use crate::front::library::{Fitting, Shipped};

/// Each ROM read as the `Run` it ships in carries it, an installed one as the
/// store holds it.
fn read(image: &Image) -> Result<Rom, String> {
    match image {
        Image::Shipped(shipped) => Ok(Rom::Carried(shipped.image())),
        Image::Installed(name) => Ok(Rom::Read(name.as_bytes().to_vec())),
        Image::Empty | Image::Ram => Err(String::from("asked of no ROM")),
    }
}

#[test]
fn a_model_b_is_configured_with_its_slots_as_fitted() {
    let mut fitting = Fitting::model_b();
    if let Fitting::ModelB { slots, .. } = &mut fitting {
        slots[3] = Image::Ram;
        slots[2] = Image::Installed(String::from("ELITE"));
    }
    let images = Images::of(&fitting, &mut read).expect("read");
    let Configuration::ModelB { os, slots, .. } = images.configuration() else {
        panic!("a Model B");
    };
    assert_eq!(os, Shipped::Mos.image());
    assert!(matches!(slots[15], Slot::Rom(rom) if rom == Shipped::Basic.image()));
    assert!(matches!(slots[14], Slot::Rom(rom) if rom == Shipped::Dfs.image()));
    assert!(matches!(slots[13], Slot::Rom(rom) if rom == Shipped::Vim.image()));
    for slot in crate::front::library::SIDEWAYS_RAM_SLOTS {
        assert!(
            matches!(slots[slot], Slot::Ram),
            "slot {slot}'s sideways RAM"
        );
    }
    assert!(matches!(slots[3], Slot::Ram));
    assert!(matches!(slots[2], Slot::Rom(b"ELITE")));
    let empty = slots
        .iter()
        .filter(|slot| matches!(slot, Slot::Empty))
        .count();
    assert_eq!(empty, SLOTS - 9);
}

#[test]
fn a_socket_needing_a_rom_refuses_one_holding_none_and_a_read_s_failure_stands() {
    let mut fitting = Fitting::electron(true);
    if let Fitting::Electron { basic, .. } = &mut fitting {
        *basic = Image::Empty;
    }
    assert_eq!(
        Images::of(&fitting, &mut read).err().as_deref(),
        Some(NO_ROM)
    );
    let mut asked: Vec<String> = Vec::new();
    let refused = Images::of(
        &Fitting::c64(Standard::Pal, sid::Model::Mos6581, 1),
        &mut |image| {
            asked.push(String::from(image.called()));
            Err(String::from("unreadable"))
        },
    );
    assert_eq!(refused.err().as_deref(), Some("unreadable"));
    assert_eq!(asked.len(), 1, "the first refusal ends the read");
}

#[test]
fn an_electron_s_plus_1_and_a_c64_s_drives_are_configured_as_fitted() {
    let images = Images::of(&Fitting::electron(true), &mut read).expect("read");
    let Configuration::Electron { plus1, .. } = images.configuration() else {
        panic!("an Electron");
    };
    let plus1 = plus1.expect("fitted");
    assert_eq!(plus1.rom, Shipped::Plus1.image());
    assert!(plus1.cartridges.as_flattened().iter().all(Option::is_none));
    let images = Images::of(
        &Fitting::c64(Standard::Pal, sid::Model::Mos6581, 1),
        &mut read,
    )
    .expect("read");
    let Configuration::C64 { drives, kernal, .. } = images.configuration() else {
        panic!("a C64");
    };
    assert_eq!(kernal, Shipped::C64Kernal.image());
    assert_eq!(drives, [Some(Shipped::C1541Dos.image()), None]);
}
