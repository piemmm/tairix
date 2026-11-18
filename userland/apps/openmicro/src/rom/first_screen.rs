//! Each model's boot screen as the tree's own ROMs draw it, for the card of
//! a machine never yet run ([`crate::front::first_screen`]): the model's
//! factory fitting powered on and run for [`STARTED_SECONDS`], its picture
//! packed. Drawn here, on the host, so no guest code runs in the front end.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::firmware::{C64_SCREEN, ELECTRON_SCREEN, MODEL_B_SCREEN};
use crate::front::assistant::{chosen, MODELS};
use crate::front::images::{Images, Rom};
use crate::front::library::Image;
use crate::machine::Model;

/// Each boot screen by its file's name, and its model.
pub const SCREENS: [(&str, Model); Model::ALL.len()] = [
    (MODEL_B_SCREEN, Model::ModelB),
    (ELECTRON_SCREEN, Model::Electron),
    (C64_SCREEN, Model::C64),
];

/// How long a machine runs from power-on before its screen is taken: past
/// every model's start-up, a C64's memory test among them.
pub const STARTED_SECONDS: u64 = 3;

/// `model`'s boot screen, packed: the factory fitting the assistant offers
/// first, each ROM the one of `built`, by its file's name, that the tree
/// builds.
///
/// # Errors
///
/// Why the machine could not be built, or the picture not packed.
pub fn draw(model: Model, built: &[(&str, Vec<u8>)]) -> Result<Vec<u8>, String> {
    let at = MODELS
        .iter()
        .position(|&offered| offered == model)
        .ok_or_else(|| alloc::format!("{model:?} is not offered"))?;
    let (fitting, _) = chosen(at, &[]).ok_or("no factory fitting")?;
    let images = Images::of(&fitting, &mut |image| match image {
        Image::Shipped(shipped) => built
            .iter()
            .find(|(name, _)| *name == shipped.file())
            .map(|(_, bytes)| Rom::Read(bytes.clone()))
            .ok_or_else(|| alloc::format!("{} is not built", shipped.file())),
        Image::Installed(_) | Image::Empty | Image::Ram => Err(String::from(
            "a factory fitting holds only what the bundle ships",
        )),
    })?;
    let mut machine = crate::worker::powered_on(&images.configuration())
        .map_err(|refused| alloc::format!("{model:?} would not power on: {refused:?}"))?;
    let until = machine.cycles() + machine.timing().clock() * STARTED_SECONDS;
    machine.run_until(until);
    let shown = machine.shown();
    let mut packed = vec![0; tairix_compress::max_compressed_len(shown.len())];
    let len = tairix_compress::compress(shown, &mut packed)
        .map_err(|err| alloc::format!("{model:?}'s screen would not pack: {err:?}"))?;
    packed.truncate(len);
    Ok(packed)
}
