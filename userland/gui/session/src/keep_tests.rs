//! Host tests of keeping a file: where it goes and what it is called, the one
//! naming rule every kept file shares, never over another, what is refused
//! and why, and that the file is the picture.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::{CivilTime, Time64};
use tairix_abi::Errno;
use tairix_raster::{Pixel, Surface};
use tairix_tz::CivilClock;

use super::{
    claim_scratch, keep_picture, keep_png, kept_at, kept_name, make_folders, place_kept,
    scratch_path, Found, Keeping, KeptFiles, Picture, Unkept, PNG_ENDING, SCRATCH,
};

/// A filesystem of folders and files held in memory.
#[derive(Default)]
pub(crate) struct Table {
    pub(crate) folders: Vec<String>,
    pub(crate) files: BTreeMap<String, Vec<u8>>,
    /// When each file last changed, where a test says.
    pub(crate) changed: BTreeMap<String, Time64>,
    /// The folder that cannot be made, and why.
    pub(crate) refuse_folder: Option<(String, Errno)>,
    /// Why every file write fails, when it does.
    pub(crate) refuse_write: Option<Errno>,
    /// Why every rename fails, when it does.
    pub(crate) refuse_place: Option<Errno>,
    /// How many times a scratch file was written.
    pub(crate) scratch_writes: usize,
    /// How many times a folder was asked to be made.
    pub(crate) folders_made: usize,
}

impl KeptFiles for Table {
    fn has_folder(&mut self, path: &str) -> bool {
        self.folders.iter().any(|folder| folder == path)
    }

    fn make_folder(&mut self, path: &str) -> Result<(), Errno> {
        self.folders_made += 1;
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
        if self.files.contains_key(path) {
            return Err(Errno::AlreadyExists);
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

    fn found(&mut self, path: &str) -> Option<Found> {
        let bytes = self.files.get(path)?;
        Some(Found {
            changed: self
                .changed
                .get(path)
                .copied()
                .unwrap_or(Time64::UNIX_EPOCH),
            len: u64::try_from(bytes.len()).ok()?,
        })
    }
}

impl Table {
    /// The files kept, the scratch files aside.
    pub(crate) fn kept(&self) -> Vec<&String> {
        self.files
            .keys()
            .filter(|path| {
                !path
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.starts_with(SCRATCH))
            })
            .collect()
    }
}

pub(crate) const FINISHED: CivilTime = CivilTime {
    year: 2026,
    month: 10,
    day: 1,
    hour: 14,
    minute: 3,
    second: 7,
};

/// A small picture whose every pixel differs from its neighbours.
pub(crate) fn striped(width: u32, height: u32) -> Surface {
    let mut surface = Surface::new(width, height).expect("a surface");
    for (at, pixel) in (0u32..).zip(surface.pixels_mut()) {
        *pixel = Pixel {
            r: u8::try_from(at * 7 % 256).expect("a byte"),
            g: u8::try_from(at * 13 % 256).expect("a byte"),
            b: u8::try_from(at * 29 % 256).expect("a byte"),
            a: u8::MAX,
        };
    }
    surface
}

/// A picture called `name`, of the Ray Tracer.
fn picture(name: &str) -> Picture {
    Picture {
        folder: String::from("Ray Tracer"),
        name: String::from(name),
        surface: striped(7, 5),
    }
}

#[test]
fn a_picture_is_kept_in_the_savers_own_pictures_named_for_when_it_was_kept() {
    let mut files = Table::default();
    let kept = keep_picture(
        &mut files,
        Some("/Users/ada/"),
        &picture("Meadow"),
        (&CivilClock::UTC, Some(FINISHED)),
    )
    .expect("kept");
    assert_eq!(
        kept,
        "/Users/ada/Home/Pictures/Ray Tracer/Meadow 2026-10-01 at 14.03.07.png"
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

/// A capture and a saver's picture are named by one rule: what it is, the
/// moment in the user's zone, the number from two, the ending.
#[test]
fn one_rule_names_every_kept_file() {
    let still = Keeping {
        folders: &[],
        what: "Screenshot",
        ending: PNG_ENDING,
    };
    let recording = Keeping {
        folders: &[],
        what: "Screen Recording",
        ending: "webm",
    };
    assert_eq!(
        kept_name(&still, Some(FINISHED), 1),
        "Screenshot 2026-10-01 at 14.03.07.png"
    );
    assert_eq!(
        kept_name(&still, Some(FINISHED), 2),
        "Screenshot 2026-10-01 at 14.03.07 2.png"
    );
    assert_eq!(
        kept_name(&recording, Some(FINISHED), 64),
        "Screen Recording 2026-10-01 at 14.03.07 64.webm"
    );
    assert_eq!(kept_name(&still, None, 1), "Screenshot.png");
    assert_eq!(kept_name(&still, None, 3), "Screenshot 3.png");
}

/// The file is the picture: decoded, it is every pixel as it was.
#[test]
fn the_file_kept_decodes_to_the_picture() {
    let mut files = Table::default();
    let picture = picture("Coast");
    let kept = keep_picture(
        &mut files,
        Some("/Users/ada"),
        &picture,
        (&CivilClock::UTC, Some(FINISHED)),
    )
    .expect("kept");
    let bytes = files.files.get(&kept).expect("the file");
    let limits = tairix_image::DecodeLimits::new(64, 64, 4096, 0);
    let decoded = tairix_image::decode(bytes, &limits).expect("a PNG");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (picture.surface.width(), picture.surface.height())
    );
    let shown: Vec<u8> = picture
        .surface
        .pixels()
        .iter()
        .flat_map(|pixel| [pixel.r, pixel.g, pixel.b, pixel.a])
        .collect();
    assert_eq!(decoded.pixels(), shown.as_slice());
}

/// A picture kept with a density says so in its file.
#[test]
fn a_picture_records_the_density_it_was_kept_with() {
    let density = tairix_image::Density::whole(144, 144, tairix_image::DensityUnit::Inch);
    let still = Keeping {
        folders: &["Desktop"],
        what: "Screenshot",
        ending: PNG_ENDING,
    };
    let mut files = Table::default();
    let kept = keep_png(
        &mut files,
        Some("/Users/ada"),
        &still,
        &striped(3, 2),
        density,
        Some(FINISHED),
    )
    .expect("kept");
    assert_eq!(
        kept,
        "/Users/ada/Desktop/Screenshot 2026-10-01 at 14.03.07.png"
    );
    let bytes = files.files.get(&kept).expect("the file");
    assert!(bytes.windows(4).any(|chunk| chunk == b"pHYs"));
}

/// However many pictures of one name are kept in one second, none is written
/// over another: each takes the next number.
#[test]
fn a_name_already_taken_takes_the_next_number_and_nothing_is_overwritten() {
    let mut files = Table::default();
    let mut kept = Vec::new();
    for _ in 0..3 {
        kept.push(
            keep_picture(
                &mut files,
                Some("/Users/ada"),
                &picture("Forest"),
                (&CivilClock::UTC, Some(FINISHED)),
            )
            .expect("kept"),
        );
    }
    let folder = "/Users/ada/Home/Pictures/Ray Tracer";
    assert_eq!(
        kept,
        [
            alloc::format!("{folder}/Forest 2026-10-01 at 14.03.07.png"),
            alloc::format!("{folder}/Forest 2026-10-01 at 14.03.07 2.png"),
            alloc::format!("{folder}/Forest 2026-10-01 at 14.03.07 3.png"),
        ]
    );
    assert_eq!(files.kept().len(), 3);
    assert_eq!(files.files.len(), 3, "no scratch file left behind");
}

/// A picture is written once, to a scratch file of its own, and only renamed
/// onto its name, so a write cut short never stands under one.
#[test]
fn a_picture_is_written_aside_and_renamed_onto_its_name() {
    let mut files = Table::default();
    let folder = "/Users/ada/Home/Pictures/Ray Tracer";
    files.files.insert(
        alloc::format!("{folder}/Coast 2026-10-01 at 14.03.07.png"),
        Vec::new(),
    );
    let kept = keep_picture(
        &mut files,
        Some("/Users/ada"),
        &picture("Coast"),
        (&CivilClock::UTC, Some(FINISHED)),
    )
    .expect("kept");
    assert!(
        kept.ends_with("Coast 2026-10-01 at 14.03.07 2.png"),
        "{kept}"
    );
    assert_eq!(
        files.scratch_writes, 1,
        "written once, whichever name it took"
    );
    assert_eq!(files.files.len(), 2, "no scratch file left behind");
    let mut refused = Table {
        refuse_place: Some(Errno::NoSpace),
        ..Table::default()
    };
    let unkept = keep_picture(
        &mut refused,
        Some("/Users/ada"),
        &picture("Coast"),
        (&CivilClock::UTC, Some(FINISHED)),
    );
    assert!(
        matches!(unkept, Err(Unkept::Write(_, Errno::NoSpace))),
        "{unkept:?}"
    );
    assert!(refused.files.is_empty(), "the scratch file is removed");
}

/// A scratch file an earlier keep left is never written over, since it may be
/// the only copy of what it holds: a still takes the next one.
#[test]
fn a_still_never_writes_over_a_scratch_file_left_before_it() {
    let mut files = Table::default();
    let folder = "/Users/ada/Home/Pictures";
    let left = scratch_path(folder, PNG_ENDING, 1);
    files.files.insert(left.clone(), alloc::vec![1, 2, 3]);
    let still = Keeping {
        folders: &["Home", "Pictures"],
        what: "Screenshot",
        ending: PNG_ENDING,
    };
    let kept = keep_png(
        &mut files,
        Some("/Users/ada"),
        &still,
        &striped(3, 2),
        None,
        Some(FINISHED),
    )
    .expect("kept");
    assert_eq!(
        kept,
        alloc::format!("{folder}/Screenshot 2026-10-01 at 14.03.07.png")
    );
    assert_eq!(
        files.files.get(&left).map(Vec::as_slice),
        Some([1, 2, 3].as_slice()),
        "the earlier scratch file is left as it was"
    );
}

/// No sweep at the session's start reaches a saver's folder, so a keep there
/// first settles what an earlier one left: a whole scratch file named for its
/// saver and the moment it last changed, an empty one removed.
#[test]
fn what_an_earlier_keep_left_in_a_savers_folder_is_settled_before_a_picture_is_kept() {
    let mut files = Table::default();
    let folder = "/Users/ada/Home/Pictures/Ray Tracer";
    let whole = scratch_path(folder, PNG_ENDING, 1);
    let empty = scratch_path(folder, PNG_ENDING, 3);
    files.files.insert(whole.clone(), alloc::vec![1, 2, 3]);
    // 2026-10-01 14:03:07 UTC.
    files
        .changed
        .insert(whole, Time64::from_secs(1_790_863_387));
    files.files.insert(empty, Vec::new());
    let kept = keep_picture(
        &mut files,
        Some("/Users/ada"),
        &picture("Coast"),
        (&CivilClock::UTC, Some(FINISHED)),
    )
    .expect("kept");
    assert_eq!(
        kept,
        alloc::format!("{folder}/Coast 2026-10-01 at 14.03.07.png")
    );
    assert_eq!(
        files
            .files
            .get(&alloc::format!(
                "{folder}/Ray Tracer 2026-10-01 at 14.03.07.png"
            ))
            .map(Vec::as_slice),
        Some([1, 2, 3].as_slice()),
        "the earlier picture is named for its saver"
    );
    assert_eq!(files.files.len(), 2, "no scratch file left behind");
}

/// A file already whole that cannot be given a name is left where it is: it
/// may be the only copy of a recording.
#[test]
fn a_whole_file_that_cannot_be_named_is_left_in_place() {
    let folder = "/Users/ada/Desktop";
    let scratch = scratch_path(folder, "webm", 1);
    let recording = Keeping {
        folders: &[],
        what: "Screen Recording",
        ending: "webm",
    };
    let mut files = Table {
        refuse_place: Some(Errno::ReadOnly),
        ..Table::default()
    };
    files.files.insert(scratch.clone(), alloc::vec![7; 9]);
    assert!(matches!(
        place_kept(&mut files, folder, &scratch, &recording, Some(FINISHED)),
        Err(Unkept::Write(_, Errno::ReadOnly))
    ));
    assert!(files.files.contains_key(&scratch));
    files.refuse_place = None;
    assert_eq!(
        place_kept(&mut files, folder, &scratch, &recording, None),
        Ok(alloc::format!("{folder}/Screen Recording.webm"))
    );
}

/// With the clock not set the picture is named for itself alone, never for a
/// date it was not kept on, and numbered as the names are taken.
#[test]
fn with_no_clock_the_picture_is_named_for_itself() {
    let mut files = Table::default();
    let mut kept = || {
        keep_picture(
            &mut files,
            Some("/Users/ada"),
            &picture("Valley"),
            (&CivilClock::UTC, None),
        )
        .expect("kept")
    };
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
            keep_picture(
                &mut files,
                Some("/Users/ada"),
                &named,
                (&CivilClock::UTC, Some(FINISHED))
            ),
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
        keep_picture(
            &mut Table::default(),
            None,
            &picture,
            (&CivilClock::UTC, Some(FINISHED))
        ),
        Err(Unkept::NoHome)
    );
    assert_eq!(
        keep_picture(
            &mut Table::default(),
            Some("/"),
            &picture,
            (&CivilClock::UTC, Some(FINISHED))
        ),
        Err(Unkept::NoHome)
    );
    let mut relative = Table::default();
    assert_eq!(
        keep_picture(
            &mut relative,
            Some("Users/ada"),
            &picture,
            (&CivilClock::UTC, Some(FINISHED))
        ),
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
        keep_picture(
            &mut files,
            Some("/Users/ada"),
            &picture,
            (&CivilClock::UTC, Some(FINISHED))
        ),
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
    let refused = keep_picture(
        &mut full,
        Some("/Users/ada"),
        &picture,
        (&CivilClock::UTC, Some(FINISHED)),
    );
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
        keep_picture(
            &mut files,
            Some("/Users/ada"),
            &picture,
            (&CivilClock::UTC, Some(FINISHED)),
        )
        .expect("kept");
    }
    assert_eq!(
        keep_picture(
            &mut files,
            Some("/Users/ada"),
            &picture,
            (&CivilClock::UTC, Some(FINISHED))
        ),
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

/// A file is named for when it was kept in the user's own zone, and for no
/// moment at all on a clock that was never set.
#[test]
fn a_file_is_named_for_the_moment_in_the_users_own_zone() {
    use tairix_abi::time::{WallClockReading, WallTimeState};
    use tairix_tz::ZoneInfo;
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

/// A name that would not fit the filesystem with its moment and its number
/// keeps nothing, rather than writing a file no rename could ever name.
#[test]
fn a_name_too_long_with_its_moment_keeps_nothing() {
    let mut files = Table::default();
    let long = "a".repeat(240);
    assert_eq!(
        keep_picture(
            &mut files,
            Some("/Users/ada"),
            &picture(&long),
            (&CivilClock::UTC, Some(FINISHED))
        ),
        Err(Unkept::Unnamed)
    );
    assert!(files.files.is_empty(), "nothing was written");
}

/// A scratch file is claimed afresh, numbered past those already taken, so no
/// two keeps share one; past the bound the folder is crowded.
#[test]
fn a_scratch_file_is_claimed_past_those_taken() {
    let folder = "/Users/ada/Desktop";
    let mut taken = alloc::vec![
        scratch_path(folder, "webm", 1),
        scratch_path(folder, "webm", 2)
    ];
    let claimed = claim_scratch(folder, "webm", |path| {
        if taken.iter().any(|held| held == path) {
            return Err(Errno::AlreadyExists);
        }
        taken.push(String::from(path));
        Ok(())
    });
    assert_eq!(
        claimed,
        Ok((alloc::format!("{folder}/.keeping 3.webm"), ()))
    );
    assert_eq!(
        claim_scratch(folder, "webm", |_| Err::<(), _>(Errno::AlreadyExists)),
        Err(Unkept::Crowded(String::from(folder)))
    );
}

#[test]
fn folders_already_there_are_looked_for_and_only_the_missing_one_made() {
    let mut files = Table {
        folders: alloc::vec![
            String::from("/Users/u/Home"),
            String::from("/Users/u/Home/Documents")
        ],
        ..Table::default()
    };
    let made = make_folders(&mut files, "/Users/u", &["Home", "Documents", "Open Micro"]);
    assert_eq!(made, Ok(String::from("/Users/u/Home/Documents/Open Micro")));
    assert_eq!(
        files.folders_made, 1,
        "a make refused for a folder there is audited"
    );
    assert_eq!(
        make_folders(&mut files, "/Users/u", &["Home", "Documents", "Open Micro"]),
        made
    );
    assert_eq!(files.folders_made, 1, "nothing is made twice");
}
