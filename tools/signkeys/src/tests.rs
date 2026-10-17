//! Unit tests for the build's signing key set.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{
    create_at, load_at, location_from, Domain, KeyError, SigningKeys, ENV, PUBLISHED_SEEDS,
    SEED_LEN,
};

/// An entropy source counting up from `start`, so every seed it fills differs.
fn counter(start: u8) -> impl FnMut(&mut [u8]) -> io::Result<()> {
    let mut next = start;
    move |out: &mut [u8]| {
        for byte in out {
            *byte = next;
            next = next.wrapping_add(1);
        }
        Ok(())
    }
}

/// Three distinct seeds.
fn seeds() -> [[u8; SEED_LEN]; 3] {
    let mut fill = counter(1);
    let mut seeds = [[0u8; SEED_LEN]; 3];
    for seed in &mut seeds {
        fill(seed).expect("a counter never fails");
    }
    seeds
}

/// A key-set document holding `seeds`, one line a domain.
fn document(seeds: [[u8; SEED_LEN]; 3]) -> String {
    let mut text = String::new();
    for (domain, seed) in Domain::ALL.iter().zip(seeds) {
        text.push_str(domain.key());
        text.push(' ');
        text.extend(
            seed.iter()
                .flat_map(|&byte| tairix_util::hex::digits(byte))
                .map(char::from),
        );
        text.push('\n');
    }
    text
}

/// A fresh directory for one test, emptied of an earlier run's.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tairix-signkeys-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("workspace")).expect("scratch directory");
    dir
}

#[test]
fn a_key_set_reads_back_as_it_was_drawn() {
    let drawn = SigningKeys::generate(&mut counter(1)).expect("a key set");
    let encoded = drawn.encode();
    let text = core::str::from_utf8(&encoded).expect("text");
    assert!(text.starts_with('#'), "it opens with its warning");
    let read = SigningKeys::parse(text).expect("it reads back");
    for domain in Domain::ALL {
        assert_eq!(read.seed(domain), drawn.seed(domain), "{domain:?}");
    }
    assert_eq!(read.seed(Domain::AppSigning), &seeds()[1]);
}

#[test]
fn a_key_set_holding_a_seed_once_published_in_the_tree_is_refused() {
    for (slot, domain) in Domain::ALL.into_iter().enumerate() {
        let mut held = seeds();
        held[slot] = PUBLISHED_SEEDS[slot];
        assert!(
            matches!(SigningKeys::parse(&document(held)), Err(KeyError::Published(refused)) if refused == domain),
            "{domain:?}"
        );
    }
}

#[test]
fn a_key_set_missing_repeating_or_garbling_a_seed_is_refused() {
    let whole = document(seeds());
    let missing: String = whole.split_inclusive('\n').take(2).collect();
    assert!(matches!(
        SigningKeys::parse(&missing),
        Err(KeyError::MissingSeed(Domain::AppPublisher))
    ));
    let repeated = format!("{whole}{}\n", whole.lines().next().unwrap_or_default());
    assert!(matches!(
        SigningKeys::parse(&repeated),
        Err(KeyError::Malformed { line: 4, .. })
    ));
    let unknown = format!("{whole}update-signing 00\n");
    assert!(matches!(
        SigningKeys::parse(&unknown),
        Err(KeyError::Malformed { line: 4, .. })
    ));
    let short = whole.replacen(' ', " 0", 1);
    assert!(matches!(
        SigningKeys::parse(&short),
        Err(KeyError::Malformed { line: 1, .. })
    ));
    assert!(SigningKeys::parse(&format!("# a comment\n\n{whole}")).is_ok());
}

#[test]
fn shared_or_constant_seeds_are_refused() {
    let [driver, app, _] = seeds();
    assert!(matches!(
        SigningKeys::parse(&document([driver, app, driver])),
        Err(KeyError::Shared)
    ));
    assert!(matches!(
        SigningKeys::parse(&document([driver, [7; SEED_LEN], app])),
        Err(KeyError::Constant(Domain::AppSigning))
    ));
    let mut stuck = |out: &mut [u8]| {
        out.fill(0);
        Ok(())
    };
    assert!(matches!(
        SigningKeys::generate(&mut stuck),
        Err(KeyError::Constant(Domain::DriverSigning))
    ));
}

#[test]
fn the_location_is_the_named_file_then_the_data_home_then_home() {
    let vars = |set: &[(&str, &str)]| {
        let set: Vec<(String, OsString)> = set
            .iter()
            .map(|(name, value)| ((*name).to_owned(), OsString::from(value)))
            .collect();
        move |name: &str| {
            set.iter()
                .find(|(set, _)| set == name)
                .map(|(_, value)| value.clone())
        }
    };
    let all = vars(&[
        (ENV, "/keys/build"),
        ("XDG_DATA_HOME", "/data"),
        ("HOME", "/home/ada"),
    ]);
    assert_eq!(location_from(&all).ok(), Some(PathBuf::from("/keys/build")));
    let data = vars(&[(ENV, ""), ("XDG_DATA_HOME", "/data"), ("HOME", "/home/ada")]);
    assert_eq!(
        location_from(&data).ok(),
        Some(PathBuf::from("/data/tairix/signing-keys"))
    );
    let home = vars(&[("XDG_DATA_HOME", "relative"), ("HOME", "/home/ada")]);
    assert_eq!(
        location_from(&home).ok(),
        Some(PathBuf::from("/home/ada/.local/share/tairix/signing-keys"))
    );
    assert!(matches!(
        location_from(&vars(&[(ENV, "keys/build")])),
        Err(KeyError::Relative(_))
    ));
    assert!(matches!(
        location_from(&vars(&[])),
        Err(KeyError::Unlocated)
    ));
}

#[test]
fn a_key_set_is_created_for_its_owner_alone_and_never_replaced() {
    let dir = scratch("create");
    let workspace = dir.join("workspace");
    let path = dir.join("keys").join("signing-keys");
    create_at(&path, &workspace, &mut counter(1)).expect("created");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let loaded = load_at(&path, &workspace).expect("it loads");
    assert_eq!(loaded.seed(Domain::AppPublisher), &seeds()[2]);
    assert!(matches!(
        create_at(&path, &workspace, &mut counter(100)),
        Err(KeyError::Exists(_))
    ));
    let still = load_at(&path, &workspace).expect("still loads");
    assert_eq!(still.seed(Domain::DriverSigning), &seeds()[0]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_key_set_inside_the_workspace_is_refused() {
    let dir = scratch("inside");
    let workspace = dir.join("workspace");
    let inside = workspace.join("target").join("signing-keys");
    assert!(matches!(
        create_at(&inside, &workspace, &mut counter(1)),
        Err(KeyError::InsideWorkspace(_))
    ));
    assert!(!inside.exists(), "nothing is written there");
    let outside = dir.join("signing-keys");
    create_at(&outside, &workspace, &mut counter(1)).expect("created outside");
    fs::create_dir_all(inside.parent().unwrap_or(&workspace)).expect("target directory");
    fs::copy(&outside, &inside).expect("a copy inside");
    assert!(matches!(
        load_at(&inside, &workspace),
        Err(KeyError::InsideWorkspace(_))
    ));
    let _ = fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_key_set_others_can_read_is_refused() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = scratch("exposed");
    let workspace = dir.join("workspace");
    let path = dir.join("signing-keys");
    create_at(&path, &workspace, &mut counter(1)).expect("created");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("widened");
    assert!(matches!(
        load_at(&path, &workspace),
        Err(KeyError::Exposed(_))
    ));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_key_set_says_how_to_create_one() {
    let dir = scratch("missing");
    let refused = load_at(&dir.join("signing-keys"), Path::new(&dir.join("workspace")))
        .err()
        .expect("refused");
    assert!(matches!(refused, KeyError::Missing(_)));
    assert!(refused.to_string().contains("cargo xtask keys init"));
    let _ = fs::remove_dir_all(&dir);
}
