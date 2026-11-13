//! The ROM images `Run` carries are the ones the builder makes from the
//! sources in this tree, as the `beeb-roms` gate also holds them.

use std::path::Path;

use tairix_beeb::firmware::{DIR, IMAGES};
use tairix_beeb::rom::SHIPPED;

#[test]
fn each_carried_image_is_the_one_the_tree_builds() {
    let committed = Path::new(env!("CARGO_MANIFEST_DIR")).join(DIR);
    for ((name, build), (carried, image)) in SHIPPED.iter().zip(IMAGES) {
        assert_eq!(*name, carried, "the builds and the images are listed alike");
        let built = build().expect("the image assembles").bytes;
        let file = std::fs::read(committed.join(name)).expect("the image is committed");
        assert!(
            file == built && image == built.as_slice(),
            "{name} is not the image the tree builds: run `cargo xtask beeb --write-roms`"
        );
    }
}
