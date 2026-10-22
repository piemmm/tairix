//! Discover the program bundles' `Help/` trees and `Resources/` files
//! from the source tree and emit them as embedded `[HelpFile]` /
//! `[ResourceFile]` tables.
//!
//! The image builder plants each bundle's internationalised help onto the
//! read-only `/System/<store>/<name>.app/Help/<locale>/<file>` store, and
//! each app's bundle resources onto
//! `/System/<store>/<name>.app/Resources/<file>`, where `<store>` is the
//! store the bundle's own declared kind installs it to. The *source of truth* for
//! both is the bundle's own on-disk directory — never a hand-maintained
//! per-bundle list in the image builder, which would force an edit to a
//! central file every time a bundle is added (the duplication the charter
//! forbids). This script walks the app roots, finds every
//! `Help/<locale>/<doc>.md` and every `Resources/<file>`, and generates an
//! `include_bytes!` row per file, so adding a bundle's payload is dropping
//! files on disk — it is rediscovered on the next build.
//!
//! A build script is host-only build tooling: a genuine build-environment
//! failure (a missing `OUT_DIR`, an unreadable source tree) fails the build
//! loudly rather than emitting a silently-incomplete image.
//!
//! Alongside the per-bundle help and resource families, this script
//! discovers the desktop's own asset families — icons, wallpapers, cursor
//! sets, screensaver previews and sounds — through one `ASSET_FAMILIES` table
//! and loop, and emits one `[AssetFile]` table the image builder plants under
//! `/System`. Each asset is held to its family's own contract as it is
//! discovered, so one no consumer could use fails the build instead of
//! shipping.

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

// The one walk the bundle composer shares, so a bundle's payload and the
// bundle itself are found by one rule.
#[path = "src/bundles.rs"]
mod bundles;

// The sounds' authoring contract, shared with `cargo xtask sound-encode`.
extern crate alloc;
#[path = "src/sound.rs"]
mod sound;

/// One single-tree desktop asset family: where its files come from, how
/// large one may be, which names are legal, and what else its contract
/// demands of the bytes.
///
/// A family is one tree rather than a per-bundle one, so
/// [`emit_asset_table`] walks each row directly. Adding a family is adding a
/// row, never a second copy of the walk.
struct AssetFamily {
    /// Source directory, relative to the workspace root.
    source_root: &'static str,
    /// Whether this family's assets sit directly in `source_root` or one
    /// directory level deeper, and — when deeper — whether one such
    /// directory name is legal for the family.
    ///
    /// `None` is a flat family (icons): the files are `source_root`'s own
    /// children. `Some(is_legal)` is a categorised family (wallpapers): each
    /// child of `source_root` is a category directory holding the files, and
    /// `is_legal` is the family's own runtime definition of a category name a
    /// consumer could resolve, so a directory no chooser could offer fails
    /// the build rather than shipping unreachable artwork.
    categorise: Option<fn(&str) -> bool>,
    /// The name of the `AssetFile::family` variant (`tairix_syshelp::
    /// AssetFamilyKind`) this family's rows are tagged with.
    ///
    /// This build script cannot depend on the very crate it generates code
    /// for, so it cannot name that enum as a Rust type; instead this is
    /// spliced verbatim into each emitted row as
    /// `AssetFamilyKind::<family_variant>`, which then resolves against
    /// the one enum `src/lib.rs` defines once the generated file is
    /// `include!`d into it. Where the family is planted is not written down
    /// here at all — `AssetFamilyKind::target_dir` is its one definition.
    family_variant: &'static str,
    /// Largest byte size a file in this family may be.
    max_bytes: usize,
    /// Whether `name` is a legal shipped file name for this family, and if
    /// so the identifier the duplicate-id check keys on.
    identify: fn(&str) -> Option<String>,
    /// What else the family demands of a file's bytes.
    verify: Option<Verify>,
}

/// A check of an asset's bytes, answering why they fail it.
type Verify = fn(&[u8]) -> Result<(), String>;

/// The desktop's single-tree asset families: the icon class masters, the
/// shipped wallpaper masters, the shipped cursor sets, the screensaver
/// previews, and the sound themes.
const ASSET_FAMILIES: &[AssetFamily] = &[
    AssetFamily {
        source_root: "lib/icon/assets",
        categorise: None,
        family_variant: "Icon",
        max_bytes: tairix_icon::MAX_ARTWORK_BYTES,
        // A class master is either a `<asset-id>.png` raster or a
        // `<asset-id>.svg` vector, and both identify as the asset id — so a
        // kind offered in both formats trips the duplicate-id check below
        // rather than shipping a master the raster-first resolution order
        // could never select.
        identify: |name| {
            tairix_icon::artwork_kind_for_file(name).map(|kind| kind.asset_id().to_string())
        },
        verify: None,
    },
    AssetFamily {
        source_root: "lib/wallpaper/assets",
        categorise: Some(tairix_wallpaper::is_wallpaper_category_name),
        family_variant: "Wallpaper",
        max_bytes: tairix_wallpaper::MAX_WALLPAPER_BYTES,
        identify: |name| tairix_wallpaper::is_wallpaper_file_name(name).then(|| name.to_string()),
        verify: None,
    },
    AssetFamily {
        source_root: "lib/cursor/assets",
        categorise: Some(tairix_cursor::is_cursor_set_name),
        family_variant: "Cursor",
        max_bytes: tairix_cursor::MAX_CURSOR_ASSET_BYTES,
        // An asset identifies as the cursor *kind* it draws, so a set
        // shipping two files for one kind trips the duplicate-id check
        // rather than shipping artwork the loader could never ask for.
        identify: |name| {
            tairix_cursor::cursor_asset_kind_for_file(name).map(|kind| kind.asset_id().to_string())
        },
        verify: None,
    },
    AssetFamily {
        source_root: "lib/wallpaper/screensavers",
        categorise: None,
        family_variant: "ScreensaverPreview",
        max_bytes: tairix_wallpaper::MAX_SCREENSAVER_PREVIEW_BYTES,
        // A preview identifies as the desktop screensaver it shows, so two
        // files for one trip the duplicate-id check. A saver bundle ships its
        // own preview in its Resources/.
        identify: |name| {
            tairix_wallpaper::ScreensaverChoice::of_preview(name)
                .map(|choice| choice.as_str().to_string())
        },
        verify: None,
    },
    AssetFamily {
        source_root: "lib/soundtheme/assets",
        categorise: Some(tairix_soundtheme::is_theme_name),
        family_variant: "Sound",
        max_bytes: tairix_soundtheme::MAX_SOUND_BYTES,
        identify: |name| {
            tairix_soundtheme::SoundAsset::from_file_name(name)
                .map(|asset| asset.name().to_string())
        },
        // A bad sound plays as silence, which looks like working, so every
        // asset is decoded in full here.
        verify: Some(|bytes| {
            sound::shipped_samples(bytes, tairix_sound::SoundFormat::Flac).map(|_| ())
        }),
    },
];

fn main() {
    let manifest = PathBuf::from(env("CARGO_MANIFEST_DIR"));
    // <workspace>/tools/syshelp -> <workspace>
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .expect("crate lives at <workspace>/tools/syshelp")
        .to_path_buf();

    // A program crate added anywhere the walk looks reruns the build.
    let programs = bundles::program_crates(&workspace.join("userland"), |dir| {
        println!("cargo:rerun-if-changed={}", dir.display());
    })
    .expect("the userland tree is readable");

    let mut rows = String::from("[\n");
    for crate_dir in &programs {
        let help = crate_dir.join("Help");
        if !help.is_dir() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", help.display());
        let (bundle, store) = bundle_identity(crate_dir);
        for locale in sorted_children(&help) {
            let locale_dir = help.join(&locale);
            if !locale_dir.is_dir() {
                continue;
            }
            println!("cargo:rerun-if-changed={}", locale_dir.display());
            for file in sorted_children(&locale_dir) {
                let path = locale_dir.join(&file);
                let is_md = path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
                if !path.is_file() || !is_md {
                    continue;
                }
                println!("cargo:rerun-if-changed={}", path.display());
                let abs = path.to_str().expect("help path is valid UTF-8");
                writeln!(
                    rows,
                    "    HelpFile {{ store: {store:?}, bundle: {bundle:?}, locale: {locale:?}, file: {file:?}, bytes: include_bytes!({abs:?}) }},"
                )
                .expect("write to String");
            }
        }
    }
    rows.push(']');

    let dest = PathBuf::from(env("OUT_DIR")).join("help_files.rs");
    fs::File::create(&dest)
        .and_then(|mut f| f.write_all(rows.as_bytes()))
        .expect("write generated help table");

    let mut resource_rows = String::from("[\n");
    for crate_dir in &programs {
        let resources = crate_dir.join("Resources");
        if !resources.is_dir() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", resources.display());
        let (bundle, store) = bundle_identity(crate_dir);
        for file in sorted_children(&resources) {
            let path = resources.join(&file);
            if !path.is_file() {
                continue;
            }
            println!("cargo:rerun-if-changed={}", path.display());
            let abs = path.to_str().expect("resource path is valid UTF-8");
            writeln!(
                resource_rows,
                "    ResourceFile {{ store: {store:?}, bundle: {bundle:?}, file: {file:?}, bytes: include_bytes!({abs:?}) }},"
            )
            .expect("write to String");
        }
    }
    resource_rows.push(']');

    let dest = PathBuf::from(env("OUT_DIR")).join("resource_files.rs");
    fs::File::create(&dest)
        .and_then(|mut f| f.write_all(resource_rows.as_bytes()))
        .expect("write generated resource table");

    let mut crates = String::from("[\n");
    for crate_dir in &programs {
        let relative = crate_dir
            .strip_prefix(&workspace)
            .expect("the walk stays inside the workspace");
        let relative = relative.to_str().expect("program path is valid UTF-8");
        writeln!(crates, "    {relative:?},").expect("write to String");
    }
    crates.push(']');
    let dest = PathBuf::from(env("OUT_DIR")).join("program_crates.rs");
    fs::File::create(&dest)
        .and_then(|mut f| f.write_all(crates.as_bytes()))
        .expect("write generated program-crate table");

    emit_asset_table(&workspace);

    println!("cargo:rerun-if-changed=build.rs");
}

/// Discover every [`AssetFamily`]'s assets and write `asset_files.rs`, one
/// `AssetFile` row per asset, planted under the family's
/// `AssetFamilyKind::target_dir()`.
///
/// Each file is held to its family's contract before it is emitted: a legal
/// name, at most `max_bytes`, no identifier claimed twice in one directory,
/// and whatever `verify` demands of its bytes. Families, directories and
/// files are walked in a fixed order, so the table and the image are
/// reproducible.
fn emit_asset_table(workspace: &Path) {
    let mut rows = String::from("[\n");
    for family in ASSET_FAMILIES {
        let assets = workspace.join(family.source_root);
        println!("cargo:rerun-if-changed={}", assets.display());
        if !assets.is_dir() {
            continue;
        }

        let max_bytes = u64::try_from(family.max_bytes).expect("bound fits u64");
        let mut seen_ids: Vec<(Option<String>, String)> = Vec::new();
        for (category, dir) in family_directories(family, &assets) {
            println!("cargo:rerun-if-changed={}", dir.display());
            for file in sorted_children(&dir) {
                let path = dir.join(&file);
                if !path.is_file() {
                    continue;
                }
                // A name the family's own runtime consumer would never
                // resolve (an unknown id, a wrong extension, a path
                // separator) can never be offered, so shipping it is a build
                // error, not a silent fallback.
                let id = (family.identify)(&file).unwrap_or_else(|| {
                    panic!(
                        "{}: not a legal shipped name for the {} asset family",
                        path.display(),
                        family.family_variant
                    )
                });
                // The byte bound is a fixed validation limit on untrusted
                // input: an over-large asset is refused before it is decoded,
                // so one that exceeds it must never reach the image.
                let len = fs::metadata(&path)
                    .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
                    .len();
                assert!(
                    len <= max_bytes,
                    "{}: {len} bytes exceeds the {}-byte {} bound",
                    path.display(),
                    family.max_bytes,
                    family.family_variant
                );
                let claim = (category.clone(), id);
                assert!(
                    !seen_ids.contains(&claim),
                    "{}: two files claim the identifier `{}`",
                    path.display(),
                    claim.1
                );
                seen_ids.push(claim);
                if let Some(verify) = family.verify {
                    let bytes =
                        fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
                    if let Err(why) = verify(&bytes) {
                        panic!("{}: {why}", path.display());
                    }
                }

                println!("cargo:rerun-if-changed={}", path.display());
                let abs = path.to_str().expect("asset path is valid UTF-8");
                let variant = family.family_variant;
                let category = match &category {
                    Some(name) => format!("Some({name:?})"),
                    None => String::from("None"),
                };
                writeln!(
                    rows,
                    "    AssetFile {{ family: AssetFamilyKind::{variant}, category: {category}, file: {file:?}, bytes: include_bytes!({abs:?}) }},"
                )
                .expect("write to String");
            }
        }
    }
    rows.push(']');

    let dest = PathBuf::from(env("OUT_DIR")).join("asset_files.rs");
    fs::File::create(&dest)
        .and_then(|mut f| f.write_all(rows.as_bytes()))
        .expect("write generated asset table");
}

/// The directories one asset family's files are discovered in, each with
/// the category it plants under: the family's own root and `None` for a flat
/// family, or every category directory beneath it in name order for a
/// categorised one.
///
/// A categorised family's root holds category directories and nothing else:
/// a file sitting there would be planted nowhere and offered by nothing, and
/// a directory whose name the family's own consumer could not resolve would
/// ship artwork no chooser could reach — so either fails the build rather
/// than passing silently.
fn family_directories(family: &AssetFamily, assets: &Path) -> Vec<(Option<String>, PathBuf)> {
    let Some(is_legal_category) = family.categorise else {
        return vec![(None, assets.to_path_buf())];
    };
    sorted_children(assets)
        .into_iter()
        .map(|name| {
            let path = assets.join(&name);
            assert!(
                path.is_dir(),
                "{}: the {} asset family files its assets in category \
                 directories, so its root holds only directories",
                path.display(),
                family.family_variant
            );
            assert!(
                is_legal_category(&name),
                "{}: not a legal category directory for the {} asset family",
                path.display(),
                family.family_variant
            );
            (Some(name), path)
        })
        .collect()
}

/// The planted bundle directory (`<name>.app`) of the app crate at
/// `crate_dir` and the `/System` store it installs to, from its
/// `AppInfo.toml` manifest source's `name` and `kind` keys — the same source
/// of truth the app-bundle composer plants under, never the crate
/// directory's own name. The store matters as much as the name: a payload
/// planted under the wrong store would leave the installed bundle missing
/// content its signed digest covers, and the load gate would refuse the
/// bundle — so the store comes from the one shared kind → store mapping the
/// composer itself uses. A crate that ships a `Help/` or `Resources/`
/// payload without a readable manifest name and kind is a broken source
/// tree: fail the build loudly rather than plant the payload under a
/// guessed bundle.
fn bundle_identity(crate_dir: &Path) -> (String, &'static str) {
    let manifest = crate_dir.join(bundles::MANIFEST_SOURCE);
    let text = fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));
    let value_of = |key: &str| {
        bundles::string_entry(&text, key).unwrap_or_else(|| {
            panic!(
                "{}: no top-level `{key} = \"...\"` entry",
                manifest.display()
            )
        })
    };
    let name = value_of("name");
    assert!(
        bundles::is_command_word(name),
        "{}: `name` must be a plain command word, got {name:?}",
        manifest.display()
    );
    let kind = value_of("kind");
    let store = tairix_abi::ProgramKind::from_key(kind)
        .unwrap_or_else(|| panic!("{}: unknown kind `{kind}`", manifest.display()))
        .store_dir();
    (format!("{name}{}", tairix_abi::BUNDLE_SUFFIX), store)
}

/// The names of a directory's entries, sorted, so the generated table (and
/// therefore the planted store) is deterministic across builds and hosts.
fn sorted_children(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !bundles::is_host_metadata(name))
        .collect();
    names.sort();
    names
}

/// A required build-environment variable Cargo always sets; its absence is a
/// broken build host, not a recoverable condition.
fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("{key} is set by cargo"))
}
