//! `cargo xtask icons` implementation.
//!
//! The file-type icon masters in `lib/icon/assets/` are drawn by code
//! (`icons/masters.rs`) rather than painted by hand, so the whole family
//! shares one sheet, one light and one finish, and a new type is a drawing
//! added beside the others. The bare form draws every master in memory and
//! compares it byte for byte with the committed file, so no master drifts from
//! its drawing; `--write` writes them; `--sheets` renders the set, at full
//! size and at the sizes the desktop draws it, into `images/icon-sheet.png`.

mod canvas;
mod masters;
mod sheet;
#[cfg(test)]
mod tests;
mod text;

use std::ffi::OsString;
use std::path::Path;

use tairix_icon::IconKind;

use crate::Context;

use masters::{Draw, MASTERS};
use text::Fonts;

/// Where the masters are committed.
const ASSETS: &str = "lib/icon/assets";

/// Where `--sheets` renders the contact sheet.
const SHEET: &str = "images/icon-sheet.png";

/// What a run does with the masters it draws.
enum Mode {
    Check,
    Write,
    Sheets,
}

pub fn run(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    let mode = match args {
        [] => Mode::Check,
        [flag] if flag == "--write" => Mode::Write,
        [flag] if flag == "--sheets" => Mode::Sheets,
        _ => return Err("icons: usage: cargo xtask icons [--write | --sheets]".to_string()),
    };
    let fonts = Fonts::load(&ctx.workspace_root)?;
    let drawn = draw_all(&fonts)?;
    let root = &ctx.workspace_root;
    match mode {
        Mode::Check => {
            noteln!("xtask: [icons] {ASSETS}");
            check(root, &drawn)
        }
        Mode::Write => {
            noteln!("xtask: [icons --write] {ASSETS}");
            for master in &drawn {
                let path = root.join(ASSETS).join(master.file());
                std::fs::write(&path, &master.png)
                    .map_err(|e| format!("icons: cannot write {}: {e}", path.display()))?;
            }
            Ok(())
        }
        Mode::Sheets => {
            noteln!("xtask: [icons --sheets] {SHEET}");
            let path = root.join(SHEET);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("icons: cannot create {}: {e}", parent.display()))?;
            }
            std::fs::write(&path, contact_sheet(&drawn)?)
                .map_err(|e| format!("icons: cannot write {}: {e}", path.display()))
        }
    }
}

/// A master drawn: which icon it is, its pixels, and the PNG they encode to.
struct Drawn {
    kind: IconKind,
    rgba: Vec<u8>,
    png: Vec<u8>,
}

impl Drawn {
    /// The master's file name, its icon's asset id.
    fn file(&self) -> String {
        format!("{}.png", self.kind.asset_id())
    }
}

/// Every master, drawn and encoded in registry order, a few at a time on
/// threads of their own.
fn draw_all(fonts: &Fonts) -> Result<Vec<Drawn>, String> {
    let at_once = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    let mut drawn = Vec::with_capacity(MASTERS.len());
    for batch in MASTERS.chunks(at_once) {
        let finished: Vec<Result<Drawn, String>> = std::thread::scope(|scope| {
            let workers: Vec<_> = batch
                .iter()
                .map(|&(kind, draw)| scope.spawn(move || draw_one(kind, draw, fonts)))
                .collect();
            workers
                .into_iter()
                .map(|worker| {
                    worker
                        .join()
                        .unwrap_or_else(|_| Err("icons: a drawing panicked".to_string()))
                })
                .collect()
        });
        for master in finished {
            drawn.push(master?);
        }
    }
    Ok(drawn)
}

/// `kind`'s master, drawn by `draw` and encoded.
fn draw_one(kind: IconKind, draw: Draw, fonts: &Fonts) -> Result<Drawn, String> {
    let rgba = draw(fonts)?.master();
    let side = u32::from(canvas::UNITS);
    let picture = tairix_image::Picture::rgba(side, side, rgba.clone())
        .map_err(|e| format!("icons: {}: {e:?}", kind.asset_id()))?;
    let png = tairix_image::encode_png(&picture)
        .map_err(|e| format!("icons: {} would not encode: {e:?}", kind.asset_id()))?;
    Ok(Drawn { kind, rgba, png })
}

/// Fail unless every committed master is exactly what its drawing makes.
fn check(root: &Path, drawn: &[Drawn]) -> Result<(), String> {
    let stale: Vec<String> = drawn
        .iter()
        .filter(|master| {
            std::fs::read(root.join(ASSETS).join(master.file()))
                .ok()
                .as_deref()
                != Some(master.png.as_slice())
        })
        .map(Drawn::file)
        .collect();
    if stale.is_empty() {
        return Ok(());
    }
    Err(format!(
        "icons: {} in `{ASSETS}` differ from their drawings; run \
         `cargo xtask icons --write` and commit the result.",
        stale.join(", ")
    ))
}

/// The sizes a master is shown at beneath it on the contact sheet: the
/// desktop's large and small icons at each scale it draws them.
const SHEET_SIZES: [u32; 4] = [64, 48, 32, 24];

/// Masters to a row of the contact sheet.
const SHEET_COLUMNS: usize = 7;

/// The contact sheet: each master on a light ground, the sizes in
/// [`SHEET_SIZES`] beneath it, as a PNG.
fn contact_sheet(drawn: &[Drawn]) -> Result<Vec<u8>, String> {
    let side = u32::from(canvas::UNITS);
    let margin = 20;
    let cell = (side + 2 * margin, side + 3 * margin + SHEET_SIZES[0]);
    let rows = u32::try_from(drawn.len().div_ceil(SHEET_COLUMNS)).unwrap_or(1);
    let columns = u32::try_from(SHEET_COLUMNS).unwrap_or(1);
    let (width, height) = (cell.0 * columns, cell.1 * rows);
    let mut sheet = Ground::new(width, height, [0xec, 0xef, 0xf3]);
    for (index, master) in (0u32..).zip(drawn) {
        let (x, y) = ((index % columns) * cell.0, (index / columns) * cell.1);
        sheet.over(&master.rgba, side, x + margin, y + margin);
        let image = tairix_raster::Rgba8Image::new(side, side, &master.rgba)
            .map_err(|e| format!("icons: {e:?}"))?;
        let mut along = x + margin;
        for size in SHEET_SIZES {
            let small = tairix_raster::resample(&image, image.whole(), size, size)
                .map_err(|e| format!("icons: {e:?}"))?;
            sheet.over(&small, size, along, y + side + 2 * margin);
            along += size + margin / 2;
        }
    }
    let picture = tairix_image::Picture::rgba(width, height, sheet.rgba)
        .map_err(|e| format!("icons: {e:?}"))?;
    tairix_image::encode_png(&picture).map_err(|e| format!("icons: {e:?}"))
}

/// An opaque picture straight-alpha artwork is laid over.
struct Ground {
    width: u32,
    rgba: Vec<u8>,
}

impl Ground {
    fn new(width: u32, height: u32, colour: [u8; 3]) -> Self {
        let pixels = width as usize * height as usize;
        let rgba = (0..pixels)
            .flat_map(|_| [colour[0], colour[1], colour[2], 0xFF])
            .collect();
        Self { width, rgba }
    }

    /// Lay the `side`-square straight-alpha `art` over the ground at `(x, y)`.
    fn over(&mut self, art: &[u8], side: u32, x: u32, y: u32) {
        for (row, line) in (0u32..).zip(art.chunks_exact(side as usize * 4)) {
            let start = ((y + row) * self.width + x) as usize * 4;
            let Some(dest) = self.rgba.get_mut(start..start + line.len()) else {
                continue;
            };
            for (d, s) in dest
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(line.as_chunks::<4>().0)
            {
                let alpha = u32::from(s[3]);
                for c in 0..3 {
                    let mixed = u32::from(s[c]) * alpha + u32::from(d[c]) * (255 - alpha);
                    d[c] = tairix_raster::div255(mixed);
                }
            }
        }
    }
}
