//! The ROM images `Run` carries are the ones the builder makes from the
//! sources in this tree, and each model's boot screen the one they draw, as
//! the `openmicro-roms` gate also holds them.

use std::path::Path;

use tairix_openmicro::firmware::{DIR, IMAGES};
use tairix_openmicro::rom::first_screen::{draw, SCREENS};
use tairix_openmicro::rom::SHIPPED;

#[test]
fn each_carried_image_is_the_one_the_tree_builds() {
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join(DIR);
    for ((name, build), (carried, image)) in SHIPPED.iter().zip(IMAGES) {
        assert_eq!(*name, carried, "the builds and the images are listed alike");
        let built = build().expect("the image assembles").bytes;
        let file = std::fs::read(committed.join(name)).expect("the image is committed");
        assert!(
            file == built && image == built.as_slice(),
            "{name} is not the image the tree builds: run `cargo xtask openmicro --write-roms`"
        );
    }
}

#[test]
fn each_carried_boot_screen_is_the_one_the_tree_s_images_draw() {
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join(DIR);
    let built: Vec<(&str, Vec<u8>)> = SHIPPED
        .iter()
        .map(|&(name, build)| (name, build().expect("the image assembles").bytes))
        .collect();
    for (name, model) in SCREENS {
        let drawn = draw(model, &built).expect("drawn");
        let file = std::fs::read(committed.join(name)).expect("the screen is committed");
        assert!(
            file == drawn,
            "{name} is not the screen the tree draws: run `cargo xtask openmicro --write-roms`"
        );
    }
}
