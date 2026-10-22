//! Host tests of keeping a saver's pictures: where they go and what they are
//! called, never over another, what is refused and why, and that the file is
//! the picture.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::CivilTime;
use tairix_abi::Errno;
use tairix_raster::{Pixel, Surface};

use super::{keep, kept_at, Picture, PictureFiles, Unkept, SCRATCH_NAME};

/// A filesystem of folders and files held in memory.
#[derive(Default)]
struct Table {
    folders: Vec<String>,
    files: BTreeMap<String, Vec<u8>>,
    /// The folder that cannot be made, and why.
    refuse_folder: Option<(String, Errno)>,
    /// Why every file write fails, when it does.
    refuse_write: Option<Errno>,
    /// Why every rename fails, when it does.
    refuse_place: Option<Errno>,
    /// How many times the scratch file was written.
    scratch_writes: usize,
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

    fn write_scratch(&mut self, path: &str, bytes: &[u8]) -> Result<(), Errno> {
        if let Some(errno) = self.refuse_write {
            return Err(errno);
        }
        self.scratch_writes += 1;
        self.files.insert(String::from(path), bytes.to_vec());
        Ok(())
    }

    fn place(&mut self, scratch: &str, path: &str) -> Result<(), Errno> {
        if let Some(errno) = self.refuse_place {
            return Err(errno);
        }
        if self.files.contains_key(path) {
            return Err(Errno::AlreadyExists);
        }
        let bytes = self.files.remove(scratch).ok_or(Errno::NotFound)?;
        self.files.insert(String::from(path), bytes);
        Ok(())
    }

    fn discard(&mut self, path: &str) {
        self.files.remove(path);
    }
}

impl Table {
    /// The pictures kept, the scratch file aside.
    fn kept(&self) -> Vec<&String> {
        self.files
            .keys()
            .filter(|path| !path.ends_with(SCRATCH_NAME))
            .collect()
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

/// A small picture called `name` whose every pixel differs from its
/// neighbours, of the Ray Tracer.
fn picture(name: &str) -> Picture {
    let mut surface = Surface::new(7, 5).expect("a surface");
    for (at, pixel) in (0u32..).zip(surface.pixels_mut()) {
        *pixel = Pixel {
            r: u8::try_from(at * 7 % 256).expect("a byte"),
            g: u8::try_from(at * 13 % 256).expect("a byte"),
            b: u8::try_from(at * 29 % 256).expect("a byte"),
            a: u8::MAX,
        };
    }
    Picture {
        folder: String::from("Ray Tracer"),
        name: String::from(name),
        surface,
    }
}

#[test]
fn a_picture_is_kept_in_the_savers_own_pictures_named_for_when_it_was_kept() {
    let mut files = Table::default();
    let kept = keep(
        &mut files,
        Some("/Users/ada/"),
        &picture("Meadow"),
        Some(FINISHED),
    )
    .expect("kept");
    assert_eq!(
        kept,
        "/Users/ada/Home/Pictures/Ray Tracer/Meadow 2026-10-01 14.03.07.png"
    );
    assert_eq!(
        files.folders,
        [
            "/Users/ada/Home",
            "/Users/ada/Home/Pictures",
            "/Users/ada/Home/Pictures/Ray Tracer",
        ]
    );
    let name = kept.rsplit('/').next().expect("a name");
    tairix_path::validate_file_name(name).expect("a name the filesystem takes");
}

/// The file is the picture: decoded, it is every pixel as traced.
#[test]
fn the_file_kept_decodes_to_the_picture() {
    let mut files = Table::default();
    let picture = picture("Coast");
    let kept = keep(&mut files, Some("/Users/ada"), &picture, Some(FINISHED)).expect("kept");
    let bytes = files.files.get(&kept).expect("the file");
    let limits = tairix_image::DecodeLimits::new(64, 64, 4096, 0);
    let decoded = tairix_image::decode(bytes, &limits).expect("a PNG");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (picture.surface.width(), picture.surface.height())
    );
    let traced: Vec<u8> = picture
        .surface
        .pixels()
        .iter()
        .flat_map(|pixel| [pixel.r, pixel.g, pixel.b, pixel.a])
        .collect();
    assert_eq!(decoded.pixels(), traced.as_slice());
}

/// However many pictures of one name are kept in one second, none is written
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
                &picture("Forest"),
                Some(FINISHED),
            )
            .expect("kept"),
        );
    }
    let folder = "/Users/ada/Home/Pictures/Ray Tracer";
    assert_eq!(
        kept,
        [
            alloc::format!("{folder}/Forest 2026-10-01 14.03.07.png"),
            alloc::format!("{folder}/Forest 2026-10-01 14.03.07 2.png"),
            alloc::format!("{folder}/Forest 2026-10-01 14.03.07 3.png"),
        ]
    );
    assert_eq!(files.kept().len(), 3);
    assert_eq!(files.files.len(), 3, "no scratch file left behind");
}

/// A picture is written once, to the scratch file, and only renamed onto its
/// name, so a write cut short never stands under one; a scratch file an
/// earlier keep left is written over.
#[test]
fn a_picture_is_written_aside_and_renamed_onto_its_name() {
    let mut files = Table::default();
    let folder = "/Users/ada/Home/Pictures/Ray Tracer";
    files.files.insert(
        alloc::format!("{folder}/{SCRATCH_NAME}"),
        alloc::vec![1, 2, 3],
    );
    files.files.insert(
        alloc::format!("{folder}/Coast 2026-10-01 14.03.07.png"),
        Vec::new(),
    );
    let kept = keep(
        &mut files,
        Some("/Users/ada"),
        &picture("Coast"),
        Some(FINISHED),
    )
    .expect("kept");
    assert!(kept.ends_with("Coast 2026-10-01 14.03.07 2.png"), "{kept}");
    assert_eq!(
        files.scratch_writes, 1,
        "written once, whichever name it took"
    );
    assert_eq!(files.files.len(), 2, "the stale scratch went with it");

    let mut refused = Table {
        refuse_place: Some(Errno::NoSpace),
        ..Table::default()
    };
    let unkept = keep(
        &mut refused,
        Some("/Users/ada"),
        &picture("Coast"),
        Some(FINISHED),
    );
    assert!(
        matches!(unkept, Err(Unkept::Write(_, Errno::NoSpace))),
        "{unkept:?}"
    );
    assert!(refused.files.is_empty(), "the scratch file is removed");
}

/// With the clock not set the picture is named for itself alone, never for a
/// date it was not kept on, and numbered as the names are taken.
#[test]
fn with_no_clock_the_picture_is_named_for_itself() {
    let mut files = Table::default();
    let mut kept = || keep(&mut files, Some("/Users/ada"), &picture("Valley"), None).expect("kept");
    assert!(kept().ends_with("/Ray Tracer/Valley.png"));
    assert!(kept().ends_with("/Ray Tracer/Valley 2.png"));
}

/// A name the saver chose, or its title, that is no plain file name keeps
/// nothing: neither may climb out of the folder, hide, or be empty.
#[test]
fn a_name_that_is_no_plain_file_name_keeps_nothing() {
    for (folder, name) in [
        ("Ray Tracer", "../escape"),
        ("Ray Tracer", ".hidden"),
        ("Ray Tracer", "a/b"),
        ("..", "Meadow"),
        ("Ray/Tracer", "Meadow"),
        ("Ray Tracer", "bell\u{7}"),
    ] {
        let mut files = Table::default();
        let mut named = picture(name);
        named.folder = String::from(folder);
        assert_eq!(
            keep(&mut files, Some("/Users/ada"), &named, Some(FINISHED)),
            Err(Unkept::Unnamed),
            "{folder:?} {name:?}"
        );
        assert!(files.folders.is_empty() && files.files.is_empty());
    }
}

#[test]
fn what_cannot_be_kept_says_why_and_writes_nothing() {
    let picture = picture("Desert");
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
            String::from("/Users/ada/Home/Pictures"),
            Errno::PermissionDenied,
        )),
        ..Table::default()
    };
    assert_eq!(
        keep(&mut files, Some("/Users/ada"), &picture, Some(FINISHED)),
        Err(Unkept::Folder(
            String::from("/Users/ada/Home/Pictures"),
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
    let picture = picture("Winter");
    for _ in 0..super::MOST_NAMES {
        keep(&mut files, Some("/Users/ada"), &picture, Some(FINISHED)).expect("kept");
    }
    assert_eq!(
        keep(&mut files, Some("/Users/ada"), &picture, Some(FINISHED)),
        Err(Unkept::Crowded(String::from(
            "/Users/ada/Home/Pictures/Ray Tracer"
        )))
    );
    assert_eq!(
        files.kept().len(),
        usize::try_from(super::MOST_NAMES).expect("small"),
        "and its scratch file is gone"
    );
}

/// A picture is named for when it was kept in the user's own zone, and for
/// no moment at all on a clock that was never set.
#[test]
fn a_picture_is_named_for_the_moment_in_the_users_own_zone() {
    use tairix_abi::time::{Time64, WallClockReading, WallTimeState};
    use tairix_tz::{CivilClock, ZoneInfo};
    use tairix_wallpaper::{civil_clock, DesktopSettings, ZoneName};

    // 2026-10-18 13:05:00 UTC.
    let instant = WallClockReading::new(Time64::from_secs(1_792_328_700), WallTimeState::Trusted);
    let utc = kept_at(Some(instant), &CivilClock::UTC).expect("a moment");
    assert_eq!((utc.hour, utc.minute), (13, 5));
    let bytes: &'static [u8] =
        alloc::boxed::Box::leak(tairix_tz::testkit::store().into_boxed_slice());
    let zones: &'static ZoneInfo<'static> = alloc::boxed::Box::leak(alloc::boxed::Box::new(
        ZoneInfo::parse(bytes).expect("a store"),
    ));
    let settings = DesktopSettings {
        time_zone: ZoneName::new("America/New_York"),
        ..DesktopSettings::default()
    };
    let (new_york, _) = civil_clock(Some(zones), &settings, None);
    let local = kept_at(Some(instant), &new_york).expect("a moment");
    assert_eq!((local.day, local.hour, local.minute), (18, 9, 5));
    assert_eq!(
        kept_at(Some(WallClockReading::UNSET), &CivilClock::UTC),
        None
    );
    assert_eq!(kept_at(None, &CivilClock::UTC), None);
}
