//! Unit tests for picturing a RISC OS application by its own sprite.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write as _;

use tairix_abi::fs::FileId;
use tairix_abi::time::Time64;
use tairix_geometry::Rect;

use super::{
    application_picture, boot_sprite_files, is_application_path, SpriteNames,
    MAX_APPLICATION_PICTURE_BYTES, MAX_APPLICATION_SPRITES_BYTES, MAX_BOOT_BYTES, MAX_SPRITE_FILES,
};
use crate::artwork::{ArtworkRasteriser, ArtworkReader};
use crate::picture::Fitted;
use crate::thumbnail::{ArtworkDocument, DocumentStamp};

const APP: &str = "/Storage/disc/Apps/!Edit";
const SIDE: u32 = 4;

fn files(boot: &str) -> Vec<String> {
    boot_sprite_files(boot.as_bytes())
}

#[test]
fn an_application_is_shown_by_its_name_large_and_behind_sm_small() {
    let names = SpriteNames::of("!Edit").expect("a sprite name");
    assert_eq!(names.large(), b"!edit");
    assert_eq!(names.small(), b"sm!edit");
    let long = SpriteNames::of("!StrongHelpPlus").expect("cut to a sprite name");
    assert_eq!(long.large(), b"!stronghelpp");
    assert_eq!(long.small(), b"sm!stronghel");
    assert_eq!(SpriteNames::of(""), None);
    assert_eq!(SpriteNames::of("!Ä"), None);
    assert_eq!(SpriteNames::of("!a b"), None);
}

#[test]
fn icon_sprites_names_a_file_inside_the_application() {
    assert_eq!(files("IconSprites <Obey$Dir>.!Sprites"), ["!Sprites"]);
    assert_eq!(
        files("  **iconsprites   <obey$dir>.Themes.!Sprites  \r\n"),
        ["Themes/!Sprites"]
    );
    assert_eq!(
        files("%IconSprites <Obey$Dir>.Res/ources.!Sprites"),
        ["Res.ources/!Sprites"],
        "a slash in a RISC OS name is a dot here"
    );
}

#[test]
fn the_variables_a_boot_file_sets_are_read_as_risc_os_reads_them() {
    let boot = "| !Boot file for !Edit\n\
                Set Edit$Dir <Obey$Dir>\n\
                IconSprites <Edit$Dir>.!Sprites\n";
    assert_eq!(files(boot), ["!Sprites"]);
    let idiom = "If \"<Edit$Dir>\"=\"\" Then Set Edit$Dir <Obey$Dir>\n\
                 IconSprites <Edit$Dir>.Icons";
    assert_eq!(files(idiom), ["Icons"]);
    let macro_ = "SetMacro Here <Obey$Dir>\nIconSprites <Here>.!Sprites";
    assert_eq!(files(macro_), ["!Sprites"]);
    let path = "Set Edit$Path <Obey$Dir>.,<Obey$Dir>.Resources.\nIconSprites Edit:!Sprites";
    assert_eq!(files(path), ["!Sprites", "Resources/!Sprites"]);
    let unset = "Set Here <Obey$Dir>\nUnset Here\nIconSprites <Here>.!Sprites";
    assert!(
        files(unset).is_empty(),
        "an unset variable expands to nothing"
    );
}

#[test]
fn an_if_takes_the_branch_its_comparison_chooses_and_no_other() {
    let chosen = "Set Mode wide\n\
                  If \"<Mode>\"=\"wide\" Then IconSprites <Obey$Dir>.Wide Else IconSprites <Obey$Dir>.Narrow\n\
                  If \"<Mode>\"<>\"wide\" Then IconSprites <Obey$Dir>.Never";
    assert_eq!(files(chosen), ["Wide"]);
    let otherwise = "If \"a\"=\"b\" Then IconSprites <Obey$Dir>.A Else IconSprites <Obey$Dir>.B";
    assert_eq!(files(otherwise), ["B"]);
    let unjudged = "If Boot$OSVersion >= 500 Then IconSprites <Obey$Dir>.New";
    assert!(
        files(unjudged).is_empty(),
        "a condition not judged runs neither branch"
    );
    let nested = "If \"a\"=\"a\" Then If \"b\"=\"b\" Then IconSprites <Obey$Dir>.Deep";
    assert!(
        files(nested).is_empty(),
        "no If runs inside another's branch"
    );
    let quoted_then =
        "If \"x Then y\"=\"\" Then IconSprites <Obey$Dir>.A Else IconSprites <Obey$Dir>.B";
    assert_eq!(files(quoted_then), ["B"], "a keyword inside quotes is text");
}

#[test]
fn a_file_outside_the_application_is_never_named() {
    for boot in [
        "IconSprites $.Secrets.!Sprites",
        "IconSprites ADFS::HardDisc4.$.Apps.!Edit.!Sprites",
        "IconSprites <Wimp$ScrapDir>.!Sprites",
        "IconSprites <Obey$Dir>.^.!Other.!Sprites",
        "IconSprites <Obey$Dir>.a.b.c.d.e",
        "IconSprites <Obey$Dir>",
        "IconSprites <Obey$Dir>.",
        "IconSprites <Obey$Dir>..x",
        "IconSprites <Obey$Dir>.*",
        "IconSprites <Obey$Dir>./",
        "IconSprites Resources:$.Apps.!Sprites",
        "Set Obey$Dir $.Elsewhere\nIconSprites $.Elsewhere.!Sprites",
    ] {
        assert!(files(boot).is_empty(), "{boot:?}");
    }
}

#[test]
fn each_file_is_named_once_and_comments_name_none() {
    let boot = "| IconSprites <Obey$Dir>.Commented\n\
                IconSprites <Obey$Dir>.!Sprites\n\
                IconSprites <Obey$Dir>.!sprites\n\
                IconSprites <Obey$Dir>.!Sprites\n\
                Run <Obey$Dir>.!Run\n\
                IconSprites <Obey$Dir>.Second";
    assert_eq!(
        files(boot),
        ["!Sprites", "Second"],
        "names alike but for case are one"
    );
}

/// Macros nested to fan out cost a bounded scan: each reference expanded is
/// counted, so a file cannot ask for work beyond the bound however it nests
/// them, and what it named before the bound is kept.
#[test]
fn nested_macros_cost_a_bounded_scan() {
    let fan = |name: &str| alloc::format!("<{name}>").repeat(60);
    let mut boot = String::from("IconSprites <Obey$Dir>.Icons\n");
    for (name, inner) in [("a", "z"), ("b", "a"), ("c", "b"), ("d", "c")] {
        writeln!(boot, "SetMacro {name} {}", fan(inner)).expect("a string takes any line");
    }
    for _ in 0..40 {
        writeln!(boot, "IconSprites {}", fan("d")).expect("a string takes any line");
    }
    writeln!(boot, "IconSprites <Obey$Dir>.Late").expect("a string takes any line");
    assert!(boot.len() <= MAX_BOOT_BYTES);
    assert_eq!(files(&boot), ["Icons"]);
}

#[test]
fn a_path_inside_an_application_is_plain_names_no_deeper_than_the_bound() {
    for inside in [
        "!Sprites",
        "Themes/!Sprites",
        "Res.ources/!Sprites",
        "a/b/c/d",
    ] {
        assert!(is_application_path(inside), "{inside}");
    }
    for outside in [
        "",
        "/abs",
        "..",
        "a/../b",
        "./x",
        "a//b",
        "a/b/c/d/e",
        "a\0b",
    ] {
        assert!(!is_application_path(outside), "{outside:?}");
    }
}

#[test]
fn a_boot_file_beyond_its_bounds_is_not_read() {
    let mut long = String::from("IconSprites <Obey$Dir>.!Sprites\n");
    long.extend(core::iter::repeat_n(' ', MAX_BOOT_BYTES));
    assert!(boot_sprite_files(long.as_bytes()).is_empty());
    assert!(boot_sprite_files(b"IconSprites <Obey$Dir>.!Sprites\n\0").is_empty());
    let mut many = String::new();
    for index in 0..MAX_SPRITE_FILES * 2 {
        writeln!(many, "IconSprites <Obey$Dir>.S{index}").expect("a string takes any line");
    }
    assert_eq!(boot_sprite_files(many.as_bytes()).len(), MAX_SPRITE_FILES);
    let line = alloc::format!("IconSprites <Obey$Dir>.{}", "x".repeat(300));
    assert!(files(&line).is_empty(), "a line past RISC OS's own length");
    let loop_ = "SetMacro A <B>\nSetMacro B <A>\nIconSprites <A>.!Sprites";
    assert!(files(loop_).is_empty(), "macros naming each other end");
    let latin1 = b"Set A \xa3\nIconSprites <Obey$Dir>.!Sprites";
    assert_eq!(boot_sprite_files(latin1), ["!Sprites"]);
}

/// An in-memory file whose bytes say whether it holds the application's
/// sprite, and the size it is listed at.
struct Memory(Vec<u8>, u64);

impl ArtworkDocument for Memory {
    fn stamp(&self) -> DocumentStamp {
        DocumentStamp {
            size: self.1,
            modified: Time64::UNIX_EPOCH,
            id: FileId::NONE,
            content_gen: 0,
        }
    }

    fn read_at(&mut self, offset: u64, into: &mut [u8]) -> Option<usize> {
        let held = self.0.get(usize::try_from(offset).ok()?..)?;
        let len = held.len().min(into.len());
        into[..len].copy_from_slice(&held[..len]);
        Some(len)
    }
}

/// An application's files, by path, served only through the confined open:
/// the picture reads nothing any other way.
#[derive(Default)]
struct Files {
    held: Vec<(String, Vec<u8>, u64)>,
    opened: Vec<String>,
}

impl Files {
    fn with(self, name: &str, bytes: &[u8]) -> Self {
        let size = bytes.len() as u64;
        self.listed(name, bytes, size)
    }

    /// `name` holding `bytes` but listed at `size`.
    fn listed(mut self, name: &str, bytes: &[u8], size: u64) -> Self {
        self.held
            .push((alloc::format!("{APP}/{name}"), bytes.to_vec(), size));
        self
    }
}

impl ArtworkReader for Files {
    fn read(&mut self, _path: &str) -> Option<Vec<u8>> {
        None
    }

    fn open_inside(&mut self, dir: &str, inside: &str) -> Option<Box<dyn ArtworkDocument + '_>> {
        let path = alloc::format!("{dir}/{inside}");
        let held = self.held.iter().find(|(held, ..)| *held == path).cloned();
        self.opened.push(path);
        let (_, bytes, size) = held?;
        Some(Box::new(Memory(bytes, size)))
    }
}

/// A rasteriser that draws an area holding `HAS`, recording the names it was
/// asked for, and reads a `!Boot` as the sandbox does unless told what to
/// answer.
#[derive(Default)]
struct Sprites {
    asked: Vec<(Vec<u8>, Vec<u8>)>,
    short: bool,
    answer: Option<Vec<String>>,
}

impl ArtworkRasteriser for Sprites {
    fn rasterise(&mut self, _side: u32, _bytes: &[u8]) -> Option<Vec<u8>> {
        None
    }

    fn boot_sprite_files(&mut self, boot: &[u8]) -> Option<Vec<String>> {
        Some(
            self.answer
                .clone()
                .unwrap_or_else(|| boot_sprite_files(boot)),
        )
    }

    fn application_sprite(
        &mut self,
        side: u32,
        names: &SpriteNames,
        document: &mut dyn ArtworkDocument,
    ) -> Option<Fitted> {
        self.asked
            .push((names.large().to_vec(), names.small().to_vec()));
        let mut head = [0u8; 3];
        (document.read_at(0, &mut head)? == 3 && head == *b"HAS").then(|| Fitted {
            pixels: vec![0xFF; side as usize * side as usize * 4 - usize::from(self.short)],
            bounds: Rect::new(1, 1, side - 2, side - 2),
        })
    }
}

fn opened(files: &Files) -> Vec<&str> {
    files
        .opened
        .iter()
        .map(|path| path.strip_prefix(APP).unwrap_or(path))
        .collect()
}

#[test]
fn the_boot_files_sprites_come_first_each_at_its_finest() {
    let mut files = Files::default()
        .with("!Boot", b"IconSprites <Obey$Dir>.Themes.Icons")
        .with("Themes/Icons22", b"HAS")
        .with("!Sprites", b"HAS");
    let mut sprites = Sprites::default();
    let picture =
        application_picture(&mut files, &mut sprites, APP, SIDE).expect("the application's sprite");
    assert_eq!(
        picture.frame(),
        None,
        "an application's sprite is an icon, never framed as a thumbnail is"
    );
    assert_eq!(
        opened(&files),
        ["/!Boot", "/Themes/Icons11", "/Themes/Icons22"]
    );
    assert_eq!(sprites.asked, [(b"!edit".to_vec(), b"sm!edit".to_vec())]);
}

#[test]
fn with_no_boot_file_the_application_s_own_sprites_are_sniffed() {
    let mut files = Files::default()
        .with("!Sprites22", b"not it")
        .with("!Sprites", b"HAS");
    let mut sprites = Sprites::default();
    assert!(application_picture(&mut files, &mut sprites, APP, SIDE).is_some());
    assert_eq!(
        opened(&files),
        ["/!Boot", "/!Sprites11", "/!Sprites22", "/!Sprites"],
        "every resolution, finest first, until one holds the sprite"
    );
}

#[test]
fn a_boot_file_naming_no_sprite_that_serves_falls_back_to_the_applications_own() {
    let mut files = Files::default()
        .with("!Boot", b"IconSprites <Obey$Dir>.Elsewhere")
        .with("!Sprites", b"HAS");
    assert!(application_picture(&mut files, &mut Sprites::default(), APP, SIDE).is_some());
    assert_eq!(
        opened(&files),
        [
            "/!Boot",
            "/Elsewhere11",
            "/Elsewhere22",
            "/Elsewhere",
            "/!Sprites11",
            "/!Sprites22",
            "/!Sprites"
        ]
    );
}

#[test]
fn an_application_with_no_sprite_that_serves_has_no_picture_of_its_own() {
    let mut empty = Files::default();
    assert!(application_picture(&mut empty, &mut Sprites::default(), APP, SIDE).is_none());
    let mut refused = Files::default().with("!Sprites", b"nothing");
    assert!(application_picture(&mut refused, &mut Sprites::default(), APP, SIDE).is_none());
    let mut files = Files::default().with("!Sprites", b"HAS");
    assert!(application_picture(&mut files, &mut Sprites::default(), APP, 0).is_none());
    let mut named = Files::default().with("!Sprites", b"HAS");
    assert!(
        application_picture(
            &mut named,
            &mut Sprites::default(),
            "/Storage/disc/!Ä",
            SIDE
        )
        .is_none(),
        "a name no sprite can be called"
    );
}

/// What the sandbox answers for a `!Boot` is believed only as files inside
/// the application, so a lying worker cannot have a file outside it read.
#[test]
fn a_file_the_scan_answers_outside_the_application_is_never_opened() {
    let mut files = Files::default()
        .with("!Boot", b"anything")
        .with("Inside", b"HAS");
    let mut sprites = Sprites {
        answer: Some(
            ["../Other/!Sprites", "a/b/c/d/e", "/abs", "Inside"]
                .map(String::from)
                .to_vec(),
        ),
        ..Sprites::default()
    };
    assert!(application_picture(&mut files, &mut sprites, APP, SIDE).is_some());
    assert_eq!(
        opened(&files),
        ["/!Boot", "/Inside11", "/Inside22", "/Inside"]
    );
}

/// The application is named for its sprites as RISC OS spells it, a dot in
/// the name being RISC OS's slash.
#[test]
fn an_application_is_sought_by_the_name_risc_os_gives_it() {
    let dir = "/Storage/disc/Apps/!My.App";
    let mut files = Files::default();
    files
        .held
        .push((alloc::format!("{dir}/!Sprites"), b"HAS".to_vec(), 3));
    let mut sprites = Sprites::default();
    assert!(application_picture(&mut files, &mut sprites, dir, SIDE).is_some());
    assert_eq!(
        sprites.asked,
        [(b"!my/app".to_vec(), b"sm!my/app".to_vec())]
    );
}

/// The sprite files one picture streams to the sandbox are bounded in total
/// as well as one by one.
#[test]
fn an_application_streams_no_more_than_its_pictures_budget() {
    let each = MAX_APPLICATION_SPRITES_BYTES;
    let mut files = Files::default().with("!Boot", b"anything");
    for name in ["One", "Two", "Three"] {
        files = files.listed(name, b"not it", each);
    }
    let mut sprites = Sprites {
        answer: Some(["One", "Two", "Three"].map(String::from).to_vec()),
        ..Sprites::default()
    };
    assert!(application_picture(&mut files, &mut sprites, APP, SIDE).is_none());
    assert_eq!(MAX_APPLICATION_PICTURE_BYTES, 2 * each);
    assert_eq!(sprites.asked.len(), 2, "the third would pass the budget");
}

#[test]
fn a_reply_not_exactly_the_square_asked_for_is_not_believed() {
    let mut files = Files::default().with("!Sprites", b"HAS");
    let mut sprites = Sprites {
        short: true,
        ..Sprites::default()
    };
    assert!(application_picture(&mut files, &mut sprites, APP, SIDE).is_none());
}

#[test]
fn a_sprite_file_past_its_bound_is_not_streamed_to_the_sandbox() {
    let mut big = b"HAS".to_vec();
    big.resize(
        usize::try_from(MAX_APPLICATION_SPRITES_BYTES).expect("fits") + 1,
        0,
    );
    let mut files = Files::default().with("!Sprites", &big);
    let mut sprites = Sprites::default();
    assert!(application_picture(&mut files, &mut sprites, APP, SIDE).is_none());
    assert!(sprites.asked.is_empty());
}

/// The application's picture is the one decode `render_artwork` makes of its
/// key, and never one a thread owing a frame makes inline: it reads the
/// application's own files.
#[test]
fn an_application_is_rendered_through_its_key_and_never_inline() {
    use crate::artwork::{render_artwork, ArtworkKey, ArtworkResolver, InlineArtwork, Resolved};
    let key = ArtworkKey::RiscOsApplication {
        dir: String::from(APP),
        stamp: None,
    };
    let mut files = Files::default().with("!Sprites", b"HAS");
    assert!(render_artwork(&mut files, &mut Sprites::default(), &key, SIDE).is_some());
    let mut inline = InlineArtwork::new(
        Files::default().with("!Sprites", b"HAS"),
        Sprites::default(),
    );
    assert!(matches!(inline.resolve(&key, SIDE), Resolved::Done(None)));
}
