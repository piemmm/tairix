//! The Wimp's file-type sprites for the RISC OS emulator's ResourceFS
//! (`plans/RISCOS-EMULATOR.md` §4.6): each file type the content-type
//! registry knows, and the untyped file, drawn from the TAIRiX desktop's own
//! master for it at RISC OS's sizes — `file_xxx` 34 pixels square and
//! `small_xxx` 18, at 90 dots an inch — so a guest's Save box shows the
//! desktop's pictures.
//!
//! The pixels are sixteen-bit colour with an eight-bit alpha mask: the
//! masters' soft edges and shadows survive, in a file small enough for the
//! 26-bit layout's megabyte of host pages.

use std::path::{Path, PathBuf};

use tairix_abi::fs::Filetype;
use tairix_browse::media::{media_for_filetype, MediaType};
use tairix_icon::IconKind;
use tairix_image::{
    encode_sprite_area, DecodeLimits, Picture, SpriteInput, SpriteMode, SpriteName, SpritePalette,
};
use tairix_raster::{resample, Region, Rgba8Image};

use super::super::icons::ASSETS;
use crate::Context;

/// Where the file is committed, from the workspace's root.
const SPRITES: &str = "userland/apps/riscos/kernel/resources/Wimp/Sprites22";
/// The sides of a file's large and small icons.
const LARGE: u32 = 34;
const SMALL: u32 = 18;
/// Sixteen-bit colour at 90 dots an inch each way, with an alpha mask.
const MODE: u32 = 1 << 31 | 5 << 27 | 90 << 14 | 90 << 1 | 1;
/// What an icon master may be: its sides, and its pixels.
const MASTER_SIDE: u32 = tairix_icon::MAX_ARTWORK_SIDE;
const MASTER_PIXELS: u64 = MASTER_SIDE as u64 * MASTER_SIDE as u64;

/// The file the tree draws, a sprite area.
pub(super) fn draw(root: &Path) -> Result<Vec<u8>, String> {
    let mode = SpriteMode::from_value(MODE)
        .ok_or("riscos-sprites: the mode is not one the encoder writes")?;
    let mut icons: Vec<(String, IconKind)> = (0..=0xFFF_u16)
        .filter_map(|raw| {
            let filetype = Filetype::new(raw)?;
            let media = media_for_filetype(filetype)?;
            let hex = filetype.hex();
            Some((String::from_utf8_lossy(&hex).into_owned(), media.icon()))
        })
        .collect();
    icons.push(("xxx".into(), MediaType::ApplicationOctetStream.icon()));
    let mut pictures = Vec::new();
    for (suffix, kind) in &icons {
        let master = master(root, *kind)?;
        for (prefix, side) in [("file_", LARGE), ("small_", SMALL)] {
            let name = SpriteName::new(&format!("{prefix}{suffix}"))
                .ok_or_else(|| format!("riscos-sprites: {prefix}{suffix} is no sprite name"))?;
            pictures.push((name, scaled(&master, side, *kind)?));
        }
    }
    let inputs: Vec<SpriteInput<'_>> = pictures
        .iter()
        .map(|(name, picture)| SpriteInput::Picture {
            name: *name,
            mode,
            palette: &SpritePalette::Implied,
            masked: true,
            source: picture,
        })
        .collect();
    encode_sprite_area(&inputs).map_err(|e| format!("riscos-sprites: {e:?}"))
}

/// `kind`'s raster master, decoded.
fn master(root: &Path, kind: IconKind) -> Result<(u32, Vec<u8>), String> {
    let path: PathBuf = root.join(ASSETS).join(format!("{}.png", kind.asset_id()));
    let bytes =
        std::fs::read(&path).map_err(|e| format!("riscos-sprites: {}: {e}", path.display()))?;
    let limits = DecodeLimits::new(MASTER_SIDE, MASTER_SIDE, MASTER_PIXELS, 0);
    let image = tairix_image::decode(&bytes, &limits)
        .map_err(|e| format!("riscos-sprites: {}: {e:?}", path.display()))?;
    if image.width() != image.height() {
        return Err(format!("riscos-sprites: {} is not square", path.display()));
    }
    Ok((image.width(), image.into_pixels()))
}

/// `master` drawn `side` pixels square.
fn scaled(master: &(u32, Vec<u8>), side: u32, kind: IconKind) -> Result<Picture, String> {
    let (master_side, pixels) = master;
    let refused = |e| format!("riscos-sprites: {}: {e:?}", kind.asset_id());
    let source = Rgba8Image::new(*master_side, *master_side, pixels).map_err(refused)?;
    let whole = Region {
        x: 0,
        y: 0,
        width: *master_side,
        height: *master_side,
    };
    let rgba = resample(&source, whole, side, side).map_err(refused)?;
    Picture::rgba(side, side, rgba)
        .map_err(|e| format!("riscos-sprites: {}: {e:?}", kind.asset_id()))
}

/// The `riscos-sprites` static gate: the committed file is the one the
/// masters draw.
pub fn check(ctx: &Context) -> Result<(), String> {
    let path = ctx.workspace_root.join(SPRITES);
    noteln!("xtask: [riscos-sprites] {}", path.display());
    let drawn = draw(&ctx.workspace_root)?;
    match std::fs::read(&path) {
        Ok(committed) if committed == drawn => Ok(()),
        Ok(_) => Err(format!(
            "riscos-sprites: {SPRITES} is not what the icon masters draw; run `cargo xtask riscos --write-sprites`"
        )),
        Err(e) => Err(format!("riscos-sprites: {}: {e}", path.display())),
    }
}

/// `cargo xtask riscos --write-sprites`: the file drawn and committed.
pub(super) fn write(ctx: &Context) -> Result<(), String> {
    let path = ctx.workspace_root.join(SPRITES);
    noteln!("xtask: [riscos --write-sprites] {}", path.display());
    let drawn = draw(&ctx.workspace_root)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("riscos-sprites: {}: {e}", dir.display()))?;
    }
    std::fs::write(&path, drawn).map_err(|e| format!("riscos-sprites: {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tairix_image::{decode_named, find_named, DecodeLimits, SpriteName};

    fn workspace() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("xtask sits two levels below the workspace root")
    }

    /// The drawn area's sprite `name`: its sides and pixels.
    fn sprite(area: &[u8], name: &str) -> (u32, u32, Vec<u8>) {
        let name = SpriteName::new(name).expect("a sprite name");
        let found = find_named(area, &name).expect("read").expect("present");
        let limits = DecodeLimits::new(64, 64, 64 * 64, 0);
        let image = decode_named(area, &found, &limits).expect("decoded");
        (image.width(), image.height(), image.into_pixels())
    }

    #[test]
    fn every_type_is_drawn_large_and_small_with_its_soft_edges() {
        let area = super::draw(workspace()).expect("drawn");
        for (name, side) in [
            ("file_fff", 34),
            ("small_fff", 18),
            ("file_xxx", 34),
            ("small_ffb", 18),
        ] {
            let (width, height, pixels) = sprite(&area, name);
            assert_eq!((width, height), (side, side), "{name}");
            let alphas: Vec<u8> = pixels.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
            assert!(alphas.contains(&0) && alphas.contains(&u8::MAX), "{name}");
            assert!(
                alphas.iter().any(|&a| a > 0 && a < u8::MAX),
                "{name}: an edge between"
            );
        }
        let count = u32::from_le_bytes(area[..4].try_into().expect("a count"));
        let types = (0..=0xFFF_u16)
            .filter_map(tairix_abi::fs::Filetype::new)
            .filter(|&t| tairix_browse::media::media_for_filetype(t).is_some())
            .count();
        assert_eq!(
            count as usize,
            2 * (types + 1),
            "each type and the untyped file, twice"
        );
    }
}
