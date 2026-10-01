//! Host tests of keeping pictures: where they go and what they are called,
//! never over another, what is refused and why, and that the file is the
//! picture.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::CivilTime;
use tairix_abi::Errno;
use tairix_raster::Pixel;
use tairix_raytrace::Setting;

use super::{keep, Picture, PictureFiles, Unkept, FOLDERS};

/// A filesystem of folders and files held in memory.
#[derive(Default)]
struct Table {
    folders: Vec<String>,
    files: BTreeMap<String, Vec<u8>>,
    /// The folder that cannot be made, and why.
    refuse_folder: Option<(String, Errno)>,
    /// Why every file write fails, when it does.
    refuse_write: Option<Errno>,
}

impl PictureFiles for Table {
    fn make_folder(&mut self, path: &str) -> Result<(), Errno> {
        if let Some((refused, errno)) = &self.refuse_folder {
            if refused == path {
                return Err(*errno);
            }
        }
        if !self.folders.iter().any(|folder| folder == path) {
            self.folders.push(String::from(path));
        }
        Ok(())
    }

    fn create(&mut self, path: &str, bytes: &[u8]) -> Result<(), Errno> {
        if let Some(errno) = self.refuse_write {
            return Err(errno);
        }
        if self.files.contains_key(path) {
            return Err(Errno::AlreadyExists);
        }
        self.files.insert(String::from(path), bytes.to_vec());
        Ok(())
    }
}

const FINISHED: CivilTime = CivilTime {
    year: 2026,
    month: 10,
    day: 1,
    hour: 14,
    minute: 3,
    second: 7,
};

/// A small picture whose every pixel differs from its neighbours.
fn picture(setting: Setting) -> Picture {
    let size = (7u32, 5u32);
    let pixels = (0..size.0 * size.1)
        .map(|at| Pixel {
            r: u8::try_from(at * 7 % 256).expect("a byte"),
            g: u8::try_from(at * 13 % 256).expect("a byte"),
            b: u8::try_from(at * 29 % 256).expect("a byte"),
            a: u8::MAX,
        })
        .collect();
    Picture {
        setting,
        seed: 0x00c0_ffee_0000_0042,
        size,
        pixels,
    }
}

#[test]
fn a_picture_is_kept_in_the_homes_raytracing_pictures_named_for_when_it_was_finished() {
    let mut files = Table::default();
    let kept = keep(
        &mut files,
        Some("/Users/ada/"),
        &picture(Setting::Meadow),
        Some(FINISHED),
    )
    .expect("kept");
    assert_eq!(
        kept,
        "/Users/ada/Documents/Pictures/Raytracing/Meadow 2026-10-01 14.03.07.png"
    );
    assert_eq!(
        files.folders,
        [
            "/Users/ada/Documents",
            "/Users/ada/Documents/Pictures",
            "/Users/ada/Documents/Pictures/Raytracing",
        ]
    );
    assert_eq!(FOLDERS, ["Documents", "Pictures", "Raytracing"]);
    let name = kept.rsplit('/').next().expect("a name");
    tairix_path::validate_file_name(name).expect("a name the filesystem takes");
}

/// The file is the picture: decoded, it is every pixel as traced.
#[test]
fn the_file_kept_decodes_to_the_picture() {
    let mut files = Table::default();
    let picture = picture(Setting::Coast);
    let kept = keep(&mut files, Some("/Users/ada"), &picture, Some(FINISHED)).expect("kept");
    let bytes = files.files.get(&kept).expect("the file");
    let limits = tairix_image::DecodeLimits::new(64, 64, 4096, 0);
    let decoded = tairix_image::decode(bytes, &limits).expect("a PNG");
    assert_eq!((decoded.width(), decoded.height()), picture.size);
    let traced: Vec<u8> = picture
        .pixels
        .iter()
        .flat_map(|pixel| [pixel.r, pixel.g, pixel.b, pixel.a])
        .collect();
    assert_eq!(decoded.pixels(), traced.as_slice());
}

/// However many pictures of a setting finish in one second, none is written
/// over another: each takes the next number, and there is no limit but the
/// probing's own.
#[test]
fn a_name_already_taken_takes_the_next_number_and_nothing_is_overwritten() {
    let mut files = Table::default();
    let mut kept = Vec::new();
    for _ in 0..3 {
        kept.push(
            keep(
                &mut files,
                Some("/Users/ada"),
                &picture(Setting::Forest),
                Some(FINISHED),
            )
            .expect("kept"),
        );
    }
    let folder = "/Users/ada/Documents/Pictures/Raytracing";
    assert_eq!(
        kept,
        [
            alloc::format!("{folder}/Forest 2026-10-01 14.03.07.png"),
            alloc::format!("{folder}/Forest 2026-10-01 14.03.07 2.png"),
            alloc::format!("{folder}/Forest 2026-10-01 14.03.07 3.png"),
        ]
    );
    assert_eq!(files.files.len(), 3);
}

/// With the clock not set the picture is named for its scene, never for a
/// date it was not finished on.
#[test]
fn with_no_clock_the_picture_is_named_for_its_scene() {
    let mut files = Table::default();
    let kept = keep(
        &mut files,
        Some("/Users/ada"),
        &picture(Setting::Valley),
        None,
    )
    .expect("kept");
    assert!(
        kept.ends_with("/Raytracing/Valley 00c0ffee00000042.png"),
        "{kept}"
    );
}

#[test]
fn what_cannot_be_kept_says_why_and_writes_nothing() {
    let picture = picture(Setting::Desert);
    assert_eq!(
        keep(&mut Table::default(), None, &picture, Some(FINISHED)),
        Err(Unkept::NoHome)
    );
    assert_eq!(
        keep(&mut Table::default(), Some("/"), &picture, Some(FINISHED)),
        Err(Unkept::NoHome)
    );
    let mut relative = Table::default();
    assert_eq!(
        keep(&mut relative, Some("Users/ada"), &picture, Some(FINISHED)),
        Err(Unkept::NoHome),
        "a home that is not absolute is no home"
    );
    assert!(relative.folders.is_empty());

    let mut files = Table {
        refuse_folder: Some((
            String::from("/Users/ada/Documents/Pictures"),
            Errno::PermissionDenied,
        )),
        ..Table::default()
    };
    assert_eq!(
        keep(&mut files, Some("/Users/ada"), &picture, Some(FINISHED)),
        Err(Unkept::Folder(
            String::from("/Users/ada/Documents/Pictures"),
            Errno::PermissionDenied
        ))
    );
    assert!(files.files.is_empty());

    let mut full = Table {
        refuse_write: Some(Errno::NoSpace),
        ..Table::default()
    };
    let refused = keep(&mut full, Some("/Users/ada"), &picture, Some(FINISHED));
    assert!(
        matches!(&refused, Err(Unkept::Write(_, Errno::NoSpace))),
        "{refused:?}"
    );
    assert!(full.files.is_empty());
    let said = alloc::format!("{}", refused.expect_err("refused"));
    assert!(said.contains("could not be written"), "{said}");
}

/// Past its bound the probing gives up, and says the folder is crowded,
/// rather than trying names without end.
#[test]
fn a_folder_with_every_name_taken_refuses_the_picture() {
    let mut files = Table::default();
    let picture = picture(Setting::Winter);
    for _ in 0..super::MOST_NAMES {
        keep(&mut files, Some("/Users/ada"), &picture, Some(FINISHED)).expect("kept");
    }
    assert_eq!(
        keep(&mut files, Some("/Users/ada"), &picture, Some(FINISHED)),
        Err(Unkept::Crowded(String::from(
            "/Users/ada/Documents/Pictures/Raytracing"
        )))
    );
}
