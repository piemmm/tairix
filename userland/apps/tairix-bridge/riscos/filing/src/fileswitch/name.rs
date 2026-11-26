//! Names (`plans/RISCOS-EMULATOR.md` §6): a name parsed into the filing
//! system its prefix chooses, its special field and its path, and the path
//! made absolute against the filing system's directories and its wildcards
//! matched, as RISC OS 5's FileSwitch does before a filing system sees it.
//!
//! - A prefix is `fs:`, `-fs-`, `fs#special:`, `-fs#special-`, `#special:`
//!   for the current filing system, or `var:` where `var$Path` is set.
//! - A path is made absolute: `$` is its disc's root, `:disc` a named disc's,
//!   `@`, `\`, `&` and `%` the current, previous, user root and library
//!   directories, `^` a parent, and anything else is relative to the current
//!   directory. A directory never set is its disc's root, the disc asked of
//!   the filing system as RISC OS 5 asks it, by `FSEntry_Func` 0 and 11;
//!   the library is `&.Library`, else `$.Library`, else the current one.
//! - A wildcarded part is matched against its directory's names, read by
//!   `FSEntry_Func` 14; the first that matches is taken, and a part nothing
//!   matches is passed on as it is.

use alloc::vec::Vec;

use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::filing::fs;
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::call::{self, Info, Named, Need, DIRECTORY, IMAGE, NOT_FOUND};
use super::system::{info, name_byte, Calls};
use super::{error, with};

/// The longest name FileSwitch takes, or path it builds: a containment bound
/// on what the guest hands it.
pub const NAME_MOST: usize = 1024;

/// How many names `FSEntry_Func` 14 is asked for at once, as RISC OS 5
/// asks, and the room they are read into.
const NAMES_ASKED: u32 = 0x400;
const NAMES_ROOM: u32 = 1024;
/// The most reads of one directory a wildcard is matched over: a filing
/// system that never ends its listing is stopped there.
const LISTINGS_MOST: u32 = 4096;
/// The room `FSEntry_Func` 11 writes a disc's name and boot option in.
const DISC_ROOM: u32 = 260;

/// One of a filing system's four directories.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    /// The currently selected directory, `@`.
    Current,
    /// The previously selected one, `\`.
    Previous,
    /// The user root, `&`.
    UserRoot,
    /// The library, `%`.
    Library,
}

/// A directory as it was set: its path and its special field.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Selection {
    /// The path, as set.
    pub path: Vec<u8>,
    /// The special field it was set with.
    pub special: Option<Vec<u8>>,
}

/// A filing system's directories, each unset until something sets it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Directories {
    set: [Option<Selection>; 4],
}

impl Directories {
    /// `which`, if set.
    #[must_use]
    pub fn get(&self, which: Dir) -> Option<&Selection> {
        self.set[which as usize].as_ref()
    }

    /// Sets `which`, or unsets it.
    pub fn put(&mut self, which: Dir, selection: Option<Selection>) {
        self.set[which as usize] = selection;
    }

    /// Sets the current directory, the one it replaces, set or not, made the
    /// previous one.
    pub fn enter(&mut self, selection: Selection) {
        let left = self.set[Dir::Current as usize].replace(selection);
        self.set[Dir::Previous as usize] = left;
    }

    /// `one` and `other` exchanged.
    pub fn exchange(&mut self, one: Dir, other: Dir) {
        self.set.swap(one as usize, other as usize);
    }
}

/// What a name's prefix chooses.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Prefix {
    /// No prefix: the current filing system.
    Current,
    /// A filing system, by its number.
    System(u8),
    /// A path variable, by the name before its `$Path`.
    Variable(Vec<u8>),
}

/// A name split into its prefix, special field and path.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Parsed {
    /// What its prefix chooses.
    pub prefix: Prefix,
    /// Its special field, without its `#`: an empty one kept, since what it
    /// means is the filing system's to decide.
    pub special: Option<Vec<u8>>,
    /// The rest.
    pub path: Vec<u8>,
}

/// Where in a resolved path an image's contents begin, and the image filing
/// system's handle for the image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Inside {
    /// The image filing system's handle for the image.
    pub image: u32,
    /// Where the path within the image begins.
    pub at: usize,
}

/// A name resolved: the filing system to call, the absolute path on its host
/// and the special field, and, within an image, where.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Resolved {
    /// The filing system to call: within an image, the image filing system.
    pub calls: Calls,
    /// The absolute path on the host, which a canonical name gives.
    pub path: Vec<u8>,
    /// The special field.
    pub special: Option<Vec<u8>>,
    /// The image the path lies within, if it does.
    pub inside: Option<Inside>,
}

impl Resolved {
    /// The special field, borrowed.
    #[must_use]
    pub fn special(&self) -> Option<&[u8]> {
        self.special.as_deref()
    }

    /// What a call into its filing system names: within an image, the path
    /// within it and the image's handle.
    #[must_use]
    pub fn named(&self) -> Named<'_> {
        self.part(self.path.len())
    }

    /// Its whole name: the absolute path on the host and the special field,
    /// as an upcall names the object even within an image.
    #[must_use]
    pub fn whole(&self) -> Named<'_> {
        Named::new(&self.path, self.special())
    }

    /// As [`Self::named`], for its path up to `end`.
    fn part(&self, end: usize) -> Named<'_> {
        match self.inside {
            Some(inside) => Named {
                path: self.path.get(inside.at.min(end)..end).unwrap_or_default(),
                special: None,
                image: Some(inside.image),
            },
            None => Named::new(&self.path[..end], self.special()),
        }
    }

    /// Where the path its filing system is handed begins.
    fn base(&self) -> usize {
        self.inside.map_or(0, |inside| inside.at)
    }
}

/// Whether a path part is wildcarded.
pub(crate) fn wild(part: &[u8]) -> bool {
    part.iter().any(|&b| b == b'*' || b == b'#')
}

/// Whether `name` matches the wildcarded `pattern`, ignoring ASCII case:
/// `*` any run and `#` any one character.
pub(crate) fn matches(pattern: &[u8], name: &[u8]) -> bool {
    tairix_fsmeta::preset::acorn::wildcard_matches(pattern, name, |b| b.to_ascii_uppercase())
}

/// The name at `address`, as FileSwitch reads one: past leading spaces, to
/// its first space or control character; no pointer at all is a string that
/// cannot be read.
pub(crate) fn read(os: &mut Os<'_>, address: u32) -> Result<Vec<u8>, Fail> {
    Ok(read_from(os, address)?.0)
}

/// As [`read`], with the address the name starts at.
pub(crate) fn read_from(os: &mut Os<'_>, address: u32) -> Result<(Vec<u8>, u32), Fail> {
    if address == 0 {
        return Err(os.error(tairix_riscos_kernel::error::BAD_STRING));
    }
    let mut at = address;
    while os.read_byte(at)? == b' ' {
        at = at.wrapping_add(1);
    }
    let (name, ended) = os.read_until(at, NAME_MOST, |b| b <= b' ')?;
    if !ended {
        return Err(error::bad_name(os));
    }
    Ok((name, at))
}

/// The number of the filing system named `name`, if one is registered.
fn system_named(os: &mut Os<'_>, name: &[u8]) -> Result<Option<u8>, Fail> {
    with(os, |state| {
        state
            .systems
            .iter()
            .find(|system| system.named(name))
            .map(|system| system.number)
    })
}

/// Whether the system variable `name$Path` is set.
fn path_variable_set(os: &mut Os<'_>, name: &[u8]) -> Result<bool, Fail> {
    let mut full = Vec::new();
    if full.try_reserve(name.len() + b"$Path".len()).is_err() {
        return Err(os.no_memory());
    }
    full.extend_from_slice(name);
    full.extend_from_slice(b"$Path");
    Ok(os.variable_text(&full)?.is_some())
}

/// How a prefix naming nothing is taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Unknown {
    /// Refused: "not present".
    Refused,
    /// As no prefix at all, the whole name a path: `OS_FSControl` 11.
    Plain,
}

/// `name` split into its prefix, special field and path.
pub(crate) fn parse(os: &mut Os<'_>, name: &[u8]) -> Result<Parsed, Fail> {
    parse_taking(os, name, Unknown::Refused)
}

/// As [`parse`], a prefix that names nothing taken as `unknown` says.
pub(crate) fn parse_taking(os: &mut Os<'_>, name: &[u8], unknown: Unknown) -> Result<Parsed, Fail> {
    let plain = |name: &[u8]| Parsed {
        prefix: Prefix::Current,
        special: None,
        path: name.to_vec(),
    };
    if let Some(rest) = name.strip_prefix(b"-") {
        let Some(end) = rest.iter().position(|&b| b == b'-') else {
            return Ok(plain(name));
        };
        let (system, special) = split_special(&rest[..end]);
        if !system.iter().all(|&b| name_byte(b)) {
            return Ok(plain(name));
        }
        return match system_named(os, system)? {
            Some(number) => Ok(Parsed {
                prefix: Prefix::System(number),
                special: special.map(<[u8]>::to_vec),
                path: rest[end + 1..].to_vec(),
            }),
            None => Err(error::no_prefix(os, system)),
        };
    }
    if let Some(rest) = name.strip_prefix(b"#") {
        let Some(end) = rest.iter().position(|&b| b == b':' || b == b'-') else {
            return Ok(plain(name));
        };
        return Ok(Parsed {
            prefix: Prefix::Current,
            special: Some(rest[..end].to_vec()),
            path: rest[end + 1..].to_vec(),
        });
    }
    let Some(end) = name.iter().position(|&b| b == b':' || b == b'#') else {
        return Ok(plain(name));
    };
    let head = &name[..end];
    if head.is_empty() || !head.iter().all(|&b| name_byte(b)) {
        return Ok(plain(name));
    }
    if let Some(number) = system_named(os, head)? {
        let rest = &name[end + 1..];
        if name[end] == b':' {
            return Ok(Parsed {
                prefix: Prefix::System(number),
                special: None,
                path: rest.to_vec(),
            });
        }
        let close = rest.iter().position(|&b| b == b':').unwrap_or(rest.len());
        return Ok(Parsed {
            prefix: Prefix::System(number),
            special: Some(rest[..close].to_vec()),
            path: rest.get(close + 1..).unwrap_or_default().to_vec(),
        });
    }
    if name[end] != b':' {
        return Ok(plain(name));
    }
    if path_variable_set(os, head)? {
        return Ok(Parsed {
            prefix: Prefix::Variable(head.to_vec()),
            special: None,
            path: name[end + 1..].to_vec(),
        });
    }
    match unknown {
        Unknown::Refused => Err(error::no_prefix(os, head)),
        Unknown::Plain => Ok(plain(name)),
    }
}

/// A `-fs-` prefix's inside split at its `#`.
fn split_special(inside: &[u8]) -> (&[u8], Option<&[u8]>) {
    match inside.iter().position(|&b| b == b'#') {
        Some(at) => (&inside[..at], Some(&inside[at + 1..])),
        None => (inside, None),
    }
}

/// The filing system `prefix` chooses, a name with none the temporary one:
/// what a call into it needs, and its directories.
fn chosen(os: &mut Os<'_>, prefix: &Prefix) -> Result<(Calls, Directories), Fail> {
    let found = with(os, |state| {
        let number = match prefix {
            Prefix::System(number) => Some(*number),
            Prefix::Current | Prefix::Variable(_) => state.temporary(),
        }?;
        let calls = state.system(number)?.calls();
        let directories = state
            .directories
            .iter()
            .find(|(n, _)| *n == number)
            .map(|(_, d)| d.clone())
            .unwrap_or_default();
        Some((calls, directories))
    })?;
    found.ok_or_else(|| os.error(tairix_riscos_kernel::filing::NOT_PRESENT))
}

/// `path`'s first part and the rest after its full stop.
fn first_part(path: &[u8]) -> (&[u8], &[u8]) {
    match path.iter().position(|&b| b == b'.') {
        Some(at) => (&path[..at], &path[at + 1..]),
        None => (path, &[]),
    }
}

/// `base`, then a full stop and `rest` if there is a rest.
fn joined(os: &mut Os<'_>, base: &[u8], rest: &[u8]) -> Result<Vec<u8>, Fail> {
    let length = base.len() + if rest.is_empty() { 0 } else { 1 + rest.len() };
    if length > NAME_MOST {
        return Err(error::bad_name(os));
    }
    let mut out = Vec::new();
    if out.try_reserve_exact(length).is_err() {
        return Err(os.no_memory());
    }
    out.extend_from_slice(base);
    if !rest.is_empty() {
        out.push(b'.');
        out.extend_from_slice(rest);
    }
    Ok(out)
}

/// Whether `path` is a disc's root: `$` or `:disc.$`.
pub(crate) fn is_root(path: &[u8]) -> bool {
    path == b"$"
        || (path.starts_with(b":")
            && path.ends_with(b".$")
            && !path[1..path.len() - 2].contains(&b'.'))
}

/// `path` with each `^` part taken back over the part before it; a root's
/// parent is the root.
fn normalised(path: Vec<u8>) -> Vec<u8> {
    if !path.split(|&b| b == b'.').any(|part| part == b"^") {
        return path;
    }
    let mut parts: Vec<&[u8]> = Vec::new();
    for part in path.split(|&b| b == b'.') {
        if part == b"^" {
            let root = parts.last().is_some_and(|&p| p == b"$");
            if !root && parts.len() > 1 {
                parts.pop();
            }
        } else {
            parts.push(part);
        }
    }
    parts.join(&b'.')
}

/// The disc `selector` names selected by `FSEntry_Func` 0, then its name
/// and boot option read by `FSEntry_Func` 11: no name where the filing
/// system gives none a path could hold.
pub(crate) fn read_disc(
    os: &mut Os<'_>,
    calls: &Calls,
    selector: Named<'_>,
) -> Result<(Vec<u8>, u8), Fail> {
    let mut regs = Regs::default();
    call::func(
        os,
        calls,
        fs::func::SET_DIRECTORY,
        selector,
        0,
        &mut regs,
        |_, _, _| Ok(()),
    )?;
    let mut name = Vec::new();
    let mut boot = 0;
    let mut regs = Regs::default();
    call::func(
        os,
        calls,
        fs::func::READ_DISC,
        Named::NOTHING,
        DISC_ROOM,
        &mut regs,
        |os, _, room| {
            let length = os.read_byte(room)?;
            let bytes = os.read_vec(room + 1, usize::from(length))?;
            boot = os.read_byte(room + 1 + u32::from(length))?;
            if bytes.iter().all(|&b| b > b' ' && b != b'.' && b != b':') {
                name = bytes;
            }
            Ok(())
        },
    )?;
    Ok((name, boot))
}

/// The root of the disc `path` lies on: `:disc.$`, or `$`.
pub(crate) fn disc_root_of(path: &[u8]) -> &[u8] {
    if path.starts_with(b":") {
        let mut dots = path
            .iter()
            .enumerate()
            .filter(|(_, &b)| b == b'.')
            .map(|(at, _)| at);
        if let (Some(_), Some(second)) = (dots.next(), dots.next()) {
            return &path[..second];
        }
        return path;
    }
    b"$"
}

/// `:disc.$`, or `$` for no disc.
fn root_named(os: &mut Os<'_>, disc: &[u8]) -> Result<Vec<u8>, Fail> {
    if disc.is_empty() {
        return Ok(b"$".to_vec());
    }
    let mut colon = Vec::new();
    if colon.try_reserve_exact(disc.len() + 1).is_err() {
        return Err(os.no_memory());
    }
    colon.push(b':');
    colon.extend_from_slice(disc);
    joined(os, &colon, b"$")
}

/// What resolving a path needs and learns: the filing system, its
/// directories, the special field the path was given with and, from the
/// filing system, that field made canonical and what a library never set
/// was found to be.
struct Context<'a> {
    calls: Calls,
    directories: &'a Directories,
    special: Option<&'a [u8]>,
    canonical: Option<call::Canonical>,
    library: Option<Info>,
}

impl Context<'_> {
    /// The special field the path's calls take: as the filing system made it
    /// canonical, where it has, else as given, an empty one only rooting the
    /// path.
    fn special(&self) -> Option<&[u8]> {
        match &self.canonical {
            Some(canonical) => canonical.special.as_deref(),
            None => self.given(),
        }
    }

    /// The special field as given, an empty one none.
    fn given(&self) -> Option<&[u8]> {
        self.special.filter(|special| !special.is_empty())
    }

    /// The root of the disc `disc` names — `@` the filing system's own — as
    /// RISC OS 5 finds it: an image filing system's host makes the disc and
    /// the special field canonical by `FSEntry_Func` 23; any other selects
    /// the disc by `FSEntry_Func` 0 and reads its name by `FSEntry_Func` 11,
    /// a special field asking neither. A filing system that gives no name has
    /// the root `$` alone.
    fn disc_root(&mut self, os: &mut Os<'_>, disc: &[u8]) -> Result<Vec<u8>, Fail> {
        let typed = disc.strip_prefix(b":").unwrap_or_default();
        if self.calls.has(info::IMAGES) {
            let named = Some(typed).filter(|d| !d.is_empty());
            let canonical = call::canonical_names(os, &self.calls, self.given(), named)?;
            let root = root_named(os, canonical.disc.as_deref().unwrap_or_default());
            self.canonical = Some(canonical);
            return root;
        }
        if self.special.is_some() {
            return root_named(os, typed);
        }
        let (name, _) = read_disc(os, &self.calls, Named::new(disc, None))?;
        root_named(os, if name.is_empty() { typed } else { &name })
    }

    /// `which` directory's absolute path: as set, where it was set with this
    /// special field, else its default.
    fn directory(&mut self, os: &mut Os<'_>, which: Dir) -> Result<Vec<u8>, Fail> {
        if let Some(selection) = self.directories.get(which) {
            if selection.special.as_deref() == self.special {
                return self.absolute(os, &selection.path.clone(), true);
            }
        }
        match which {
            Dir::Current | Dir::Previous | Dir::UserRoot => self.disc_root(os, b"@"),
            Dir::Library => self.default_library(os),
        }
    }

    /// The library never set, as RISC OS 5 finds it: the first of
    /// `&.Library` and `$.Library` there is — looked for with no special
    /// field, whatever this path was given — taken as a directory set there
    /// is, else the current directory.
    fn default_library(&mut self, os: &mut Os<'_>) -> Result<Vec<u8>, Fail> {
        let mut plain = Context {
            calls: self.calls,
            directories: self.directories,
            special: None,
            canonical: None,
            library: None,
        };
        for base in [Dir::UserRoot, Dir::Current] {
            let root = match base {
                Dir::UserRoot => plain.directory(os, Dir::UserRoot)?,
                _ => plain.disc_root(os, b"@")?,
            };
            let library = Resolved {
                calls: self.calls,
                path: joined(os, &root, b"Library")?,
                special: None,
                inside: None,
            };
            let found = read_walking(os, &library, Need::Length)?;
            if found.kind != NOT_FOUND {
                self.library = Some(found);
                return self.absolute(os, &library.path, true);
            }
        }
        self.directory(os, Dir::Current)
    }

    /// `path` made absolute. A directory as it was set is `stored`: what it
    /// does not root itself is taken from its disc's root, never from a
    /// directory, so no directory can be defined by itself.
    fn absolute(&mut self, os: &mut Os<'_>, path: &[u8], stored: bool) -> Result<Vec<u8>, Fail> {
        let (head, rest) = first_part(path);
        let (base, rest) = match head {
            b"$" if stored => (self.disc_root(os, b"@")?, rest),
            b"$" => {
                let current = self.directory(os, Dir::Current)?;
                (disc_root_of(&current).to_vec(), rest)
            }
            _ if head.starts_with(b":") => {
                // A special field's path names no disc but one typed in it.
                let root = if stored && self.special.is_some() && !self.calls.has(info::IMAGES) {
                    root_named(os, b"")?
                } else {
                    self.disc_root(os, head)?
                };
                let rest = if rest.starts_with(b"$") {
                    first_part(rest).1
                } else {
                    rest
                };
                (root, rest)
            }
            _ if stored => (self.disc_root(os, b"@")?, path),
            b"@" => (self.directory(os, Dir::Current)?, rest),
            b"\\" => (self.directory(os, Dir::Previous)?, rest),
            b"&" => (self.directory(os, Dir::UserRoot)?, rest),
            b"%" => (self.directory(os, Dir::Library)?, rest),
            b"^" => {
                let current = self.directory(os, Dir::Current)?;
                (normalised(joined(os, &current, b"^")?), rest)
            }
            _ => (self.directory(os, Dir::Current)?, path),
        };
        Ok(normalised(joined(os, &base, rest)?))
    }
}

/// `named` read as `need` asks, a file an image filing system reads
/// answered as an image.
pub(crate) fn read_object(
    os: &mut Os<'_>,
    calls: &Calls,
    named: Named<'_>,
    need: Need,
) -> Result<Info, Fail> {
    let found = call::read_info(os, calls, named, need)?;
    super::image::kind_of(os, found)
}

/// `FSEntry_File` 5 on what `host` names on its host, as RISC OS 5's
/// FileSwitch reads an object: a root is a directory whatever the filing
/// system says, and where nothing is found each directory above it is read in
/// turn up to the first that is.
fn read_walking(os: &mut Os<'_>, host: &Resolved, need: Need) -> Result<Info, Fail> {
    let mut found = read_object(os, &host.calls, host.named(), need)?;
    if found.kind == NOT_FOUND {
        if is_root(&host.path) {
            found.kind = DIRECTORY;
            return Ok(found);
        }
        first_above(os, host, host.path.len(), need)?;
    }
    Ok(found)
}

/// The object `resolved` names, read as RISC OS 5's FileSwitch reads one: a
/// host's root is a directory whatever it says, and where nothing is found
/// each directory above it is read in turn up to the first that is — and if
/// that is an image, the image is opened, `resolved` made to name what lies
/// within it, and the name read again there.
pub(crate) fn info(os: &mut Os<'_>, resolved: &mut Resolved, need: Need) -> Result<Info, Fail> {
    for _ in 0..=super::image::NESTED_MOST {
        let mut found = read_object(os, &resolved.calls, resolved.named(), need)?;
        if found.kind != NOT_FOUND {
            return Ok(found);
        }
        if resolved.inside.is_none() && is_root(&resolved.path) {
            found.kind = DIRECTORY;
            return Ok(found);
        }
        let end = resolved.path.len();
        if !read_up(os, resolved, end, need)? {
            return Ok(found);
        }
    }
    Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT))
}

/// [`first_above`], and whether what it found was an image, which is then
/// opened and `resolved` made to name what lies within it.
fn read_up(os: &mut Os<'_>, resolved: &mut Resolved, end: usize, need: Need) -> Result<bool, Fail> {
    match first_above(os, resolved, end, need)? {
        Some((at, above)) if above.kind == IMAGE => {
            super::image::open(os, resolved, at, &above)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Each directory above the part of `resolved` that ends at `end` read in
/// turn, as RISC OS 5 reads up from a name that found nothing: the first
/// found and where its part ends, or none by the root.
fn first_above(
    os: &mut Os<'_>,
    resolved: &Resolved,
    mut end: usize,
    need: Need,
) -> Result<Option<(usize, Info)>, Fail> {
    let base = resolved.base();
    while let Some(dot) = resolved.path[base..end].iter().rposition(|&b| b == b'.') {
        end = base + dot;
        let above = read_object(os, &resolved.calls, resolved.part(end), need)?;
        if above.kind != NOT_FOUND {
            return Ok(Some((end, above)));
        }
        if resolved.inside.is_none() && is_root(&resolved.path[..end]) {
            break;
        }
    }
    Ok(None)
}

/// The first name in the directory `directory` names that `pattern` matches.
fn first_match(
    os: &mut Os<'_>,
    calls: &Calls,
    directory: Named<'_>,
    pattern: &[u8],
) -> Result<Option<Vec<u8>>, Fail> {
    let mut offset = 0;
    for _ in 0..LISTINGS_MOST {
        let mut regs = Regs::default();
        regs.r[3] = NAMES_ASKED;
        regs.r[4] = offset;
        regs.r[5] = NAMES_ROOM;
        let mut found = None;
        call::func(
            os,
            calls,
            fs::func::READ_NAMES,
            directory,
            NAMES_ROOM,
            &mut regs,
            |os, regs, room| {
                let mut at = room;
                for _ in 0..regs.r[3] {
                    let (name, ended) = os.read_until(at, NAME_MOST, |b| b == 0)?;
                    if !ended || at.wrapping_sub(room) >= NAMES_ROOM {
                        break;
                    }
                    at = at.wrapping_add(tairix_riscos_kernel::os::len32(name.len()) + 1);
                    if matches(pattern, &name) {
                        found = Some(name);
                        break;
                    }
                }
                Ok(())
            },
        )?;
        if found.is_some() || regs.r[3] == 0 || regs.r[4] == u32::MAX || regs.r[4] == offset {
            return Ok(found);
        }
        offset = regs.r[4];
    }
    Ok(None)
}

/// The byte range of the first wildcarded part of `path` from `from` on.
fn next_wild(path: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut start = from;
    while start <= path.len() {
        let end = path[start..]
            .iter()
            .position(|&b| b == b'.')
            .map_or(path.len(), |dot| start + dot);
        if wild(&path[start..end]) {
            return Some((start, end));
        }
        start = end + 1;
    }
    None
}

/// `resolved` with each wildcarded part matched, left to right, to the first
/// name its directory lists that it matches — an image passed through opened
/// and its part matched by its image filing system, the image file read on
/// its host where it was open already, as RISC OS 5 reads it. The directory
/// is read first, and read up from where it is not there, then listed
/// whatever it is; a part nothing matches, or whose directory its filing
/// system will not list, ends the matching there.
fn matched(os: &mut Os<'_>, resolved: &mut Resolved) -> Result<(), Fail> {
    let mut from = first_part(&resolved.path).0.len() + 1;
    if resolved.inside.is_some() {
        from = resolved.base();
    }
    let mut opened_here = false;
    for _ in 0..NAME_MOST {
        let Some((start, end)) = next_wild(&resolved.path, from) else {
            return Ok(());
        };
        let directory = start.saturating_sub(1);
        let at_image_root = resolved.inside.is_some() && start == resolved.base();
        if at_image_root && !opened_here {
            let host = with(os, |state| {
                state
                    .system(resolved.calls.number)
                    .map(super::system::System::calls)
            })?;
            if let Some(host) = host {
                call::read_info(
                    os,
                    &host,
                    Named::new(&resolved.path[..directory], resolved.special()),
                    Need::Length,
                )?;
            }
        }
        if !at_image_root {
            let above = read_object(os, &resolved.calls, resolved.part(directory), Need::Kind)?;
            let entered = if above.kind == IMAGE {
                super::image::open(os, resolved, directory, &above)?;
                true
            } else {
                above.kind == NOT_FOUND && read_up(os, resolved, directory, Need::Kind)?
            };
            if entered {
                opened_here = true;
                from = resolved.base();
                continue;
            }
        }
        let pattern = resolved.path[start..end].to_vec();
        let listing = if at_image_root {
            resolved.part(resolved.base())
        } else {
            resolved.part(directory)
        };
        let first = match first_match(os, &resolved.calls, listing, &pattern) {
            Err(Fail::Error(_)) => None,
            listed => listed?,
        };
        match first {
            Some(name) => {
                let mut path = Vec::new();
                if path
                    .try_reserve_exact(resolved.path.len() - pattern.len() + name.len())
                    .is_err()
                {
                    return Err(os.no_memory());
                }
                path.extend_from_slice(&resolved.path[..start]);
                path.extend_from_slice(&name);
                path.extend_from_slice(&resolved.path[end..]);
                resolved.path = path;
                from = start + name.len() + 1;
            }
            None => return Ok(()),
        }
    }
    Ok(())
}

/// `parsed`, which has no path variable, resolved.
fn resolved(os: &mut Os<'_>, parsed: &Parsed) -> Result<Resolved, Fail> {
    let (calls, directories) = chosen(os, &parsed.prefix)?;
    if parsed.path.is_empty() {
        if !calls.has(info::NULL_NAMES) {
            return Err(error::needs_name(os, calls.number));
        }
        let special = rooting(calls.has(info::SPECIAL_FIELDS), parsed.special.as_deref());
        return Ok(Resolved {
            calls,
            path: Vec::new(),
            special: special.map(<[u8]>::to_vec),
            inside: None,
        });
    }
    if !passes(&parsed.path) {
        return Err(error::bad_name(os));
    }
    let mut context = Context {
        calls,
        directories: &directories,
        special: rooting(calls.has(info::SPECIAL_FIELDS), parsed.special.as_deref()),
        canonical: None,
        library: None,
    };
    let path = context.absolute(os, &parsed.path, false)?;
    let special = context.special().map(<[u8]>::to_vec);
    let mut resolved = Resolved {
        calls,
        path,
        special,
        inside: None,
    };
    super::image::route(os, &mut resolved)?;
    matched(os, &mut resolved)?;
    Ok(resolved)
}

/// Whether RISC OS 5's FileSwitch hands `path` on: a first part that names
/// a disc with no colon in its name — its root may follow — or a directory by
/// its sign, or is [`ordinary`]; every other part ordinary.
pub(crate) fn passes(path: &[u8]) -> bool {
    let mut parts = path.split(|&b| b == b'.').peekable();
    let Some(first) = parts.next() else {
        return false;
    };
    let disc = first.strip_prefix(b":");
    let named = match disc {
        Some(disc) => !disc.contains(&b':'),
        None => matches!(first, b"$" | b"@" | b"&" | b"%" | b"\\") || ordinary(first),
    };
    if !named {
        return false;
    }
    if disc.is_some() && parts.peek() == Some(&&b"$"[..]) {
        parts.next();
    }
    parts.all(ordinary)
}

/// Whether FileSwitch hands on `part` of a path past where it is rooted:
/// the parent, `^`, or a name neither beginning with a directory's sign, `:`
/// or `|` nor holding `$`, `:` or `|` anywhere.
fn ordinary(part: &[u8]) -> bool {
    part == b"^"
        || !(part.is_empty()
            || matches!(
                part[0],
                b'@' | b'^' | b'\\' | b'$' | b'&' | b'%' | b':' | b'|'
            )
            || part.iter().any(|&b| matches!(b, b'$' | b':' | b'|')))
}

/// The special field `given` as it roots a path on a filing system that
/// takes `special_fields` or not: an empty one is none to a filing system
/// with special fields, but to one without them still a special field,
/// rooting the path without asking for its disc or its directories, as RISC
/// OS 5 has it.
fn rooting(special_fields: bool, given: Option<&[u8]>) -> Option<&[u8]> {
    if special_fields {
        given.filter(|special| !special.is_empty())
    } else {
        given
    }
}

/// The current filing system's library resolved, as `*LCat` and `*LEx`
/// list it without reading it: with what the search for a library never set
/// found it to be.
pub(crate) fn library(os: &mut Os<'_>) -> Result<(Resolved, Option<Info>), Fail> {
    let (calls, directories) = chosen(os, &Prefix::Current)?;
    let mut context = Context {
        calls,
        directories: &directories,
        special: None,
        canonical: None,
        library: None,
    };
    let path = context.directory(os, Dir::Library)?;
    let special = context.special().map(<[u8]>::to_vec);
    let mut resolved = Resolved {
        calls,
        path,
        special,
        inside: None,
    };
    super::image::route(os, &mut resolved)?;
    Ok((resolved, context.library))
}

/// How a name is looked up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Search<'a> {
    /// As given.
    Plain,
    /// Through the path variable named, where the name is relative.
    Variable(&'a [u8]),
    /// Through the comma-separated path given, where the name is relative.
    Path(&'a [u8]),
}

/// Whether a path variable may be put before `parsed`: it has no prefix and
/// roots itself nowhere.
fn relative(parsed: &Parsed) -> bool {
    parsed.prefix == Prefix::Current
        && parsed.special.as_deref().is_none_or(<[u8]>::is_empty)
        && !matches!(
            parsed.path.first(),
            Some(b'$' | b'&' | b'%' | b'@' | b'^' | b'\\' | b':')
        )
}

/// How deep path variables may name each other: a variable naming itself
/// ends there rather than looping.
const VARIABLES_DEEP: u32 = 8;

/// What a lookup tries: each element of a path before `name`, in turn, a
/// path variable among them expanded where it stands.
fn candidates(os: &mut Os<'_>, parsed: Parsed, search: Search<'_>) -> Result<Vec<Parsed>, Fail> {
    let elements = match (&parsed.prefix, search) {
        (Prefix::Variable(_), _) => None,
        (_, Search::Variable(variable)) if relative(&parsed) => os.variable_text(variable)?,
        (_, Search::Path(path)) if relative(&parsed) => Some(path.to_vec()),
        _ => return Ok(alloc::vec![parsed]),
    };
    let mut tries = Vec::new();
    match elements {
        Some(elements) => expand(os, &elements, &parsed.path, 0, &mut tries)?,
        None => expand_variable(os, parsed, 0, &mut tries)?,
    }
    if tries.is_empty() {
        return Err(error::bad_name(os));
    }
    Ok(tries)
}

/// Each comma-separated element of `elements` before `rest`, parsed into
/// `tries`.
fn expand(
    os: &mut Os<'_>,
    elements: &[u8],
    rest: &[u8],
    depth: u32,
    tries: &mut Vec<Parsed>,
) -> Result<(), Fail> {
    for element in elements.split(|&b| b == b',') {
        let mut name = Vec::new();
        if name.try_reserve(element.len() + rest.len()).is_err() {
            return Err(os.no_memory());
        }
        name.extend_from_slice(element);
        name.extend_from_slice(rest);
        let parsed = parse(os, &name)?;
        expand_variable(os, parsed, depth, tries)?;
    }
    Ok(())
}

/// `parsed` into `tries`, or its path variable's elements where it names
/// one.
fn expand_variable(
    os: &mut Os<'_>,
    parsed: Parsed,
    depth: u32,
    tries: &mut Vec<Parsed>,
) -> Result<(), Fail> {
    let Prefix::Variable(variable) = &parsed.prefix else {
        if tries.try_reserve(1).is_err() {
            return Err(os.no_memory());
        }
        tries.push(parsed);
        return Ok(());
    };
    if depth >= VARIABLES_DEEP {
        return Ok(());
    }
    let mut name = Vec::new();
    if name.try_reserve(variable.len() + b"$Path".len()).is_err() {
        return Err(os.no_memory());
    }
    name.extend_from_slice(variable);
    name.extend_from_slice(b"$Path");
    let elements = os.variable_text(&name)?.unwrap_or_default();
    expand(os, &elements, &parsed.path, depth + 1, tries)
}

/// The name at `address` resolved for reading: through `search`, the first
/// candidate an object answers to, else the last tried; with what was found
/// there.
pub(crate) fn find(
    os: &mut Os<'_>,
    address: u32,
    search: Search<'_>,
    need: Need,
) -> Result<(Resolved, Info), Fail> {
    let name = read(os, address)?;
    find_named(os, &name, search, need)
}

/// As [`find`], for the name `name`.
pub(crate) fn find_named(
    os: &mut Os<'_>,
    name: &[u8],
    search: Search<'_>,
    need: Need,
) -> Result<(Resolved, Info), Fail> {
    let parsed = parse(os, name)?;
    let tries = candidates(os, parsed, search)?;
    first_found(os, tries, need)
}

/// The name at `address` resolved as a disc's operation takes it: plainly,
/// but with no path for the current directory, which RISC OS 5 reads there
/// although it refuses that name to anything else.
pub(crate) fn find_on_disc(os: &mut Os<'_>, address: u32) -> Result<(Resolved, Info), Fail> {
    let name = read(os, address)?;
    let parsed = parse(os, &name)?;
    let mut tries = candidates(os, parsed, Search::Plain)?;
    for parsed in tries.iter_mut().filter(|parsed| parsed.path.is_empty()) {
        parsed.path = b"@".to_vec();
    }
    first_found(os, tries, Need::Length)
}

/// The first of `tries` an object answers to, else the last, read as `need`
/// asks.
fn first_found(os: &mut Os<'_>, tries: Vec<Parsed>, need: Need) -> Result<(Resolved, Info), Fail> {
    let count = tries.len();
    for (index, parsed) in tries.into_iter().enumerate() {
        let mut resolved = resolved(os, &parsed)?;
        let found = info(os, &mut resolved, need)?;
        if found.kind != NOT_FOUND || index + 1 == count {
            return Ok((resolved, found));
        }
    }
    Err(error::bad_name(os))
}

/// The name at `address` resolved for writing: through `search`, its first
/// candidate.
pub(crate) fn locate(os: &mut Os<'_>, address: u32, search: Search<'_>) -> Result<Resolved, Fail> {
    let name = read(os, address)?;
    locate_named(os, &name, search)
}

/// As [`locate`], for the name `name`.
pub(crate) fn locate_named(
    os: &mut Os<'_>,
    name: &[u8],
    search: Search<'_>,
) -> Result<Resolved, Fail> {
    let parsed = parse(os, name)?;
    let first = candidates(os, parsed, search)?.into_iter().next();
    match first {
        Some(parsed) => resolved(os, &parsed),
        None => Err(error::bad_name(os)),
    }
}

/// The name at `address` resolved as `OS_FSControl` 37 makes it canonical:
/// through `search`, the first candidate an object answers to, the last
/// taken unread.
pub(crate) fn canonical(
    os: &mut Os<'_>,
    address: u32,
    search: Search<'_>,
) -> Result<Resolved, Fail> {
    let name = read(os, address)?;
    let parsed = parse(os, &name)?;
    let tries = candidates(os, parsed, search)?;
    let count = tries.len();
    for (index, parsed) in tries.into_iter().enumerate() {
        let mut resolved = resolved(os, &parsed)?;
        if index + 1 == count || info(os, &mut resolved, Need::Length)?.kind != NOT_FOUND {
            return Ok(resolved);
        }
    }
    Err(error::bad_name(os))
}

/// The leaf of the name at `address` as it was given, its wildcards
/// unmatched: the part of its path after its last full stop.
pub(crate) fn given_leaf(os: &mut Os<'_>, address: u32) -> Result<Vec<u8>, Fail> {
    let name = read(os, address)?;
    let parsed = parse(os, &name)?;
    Ok(leaf(&parsed.path).to_vec())
}

/// `resolved`'s leaf: the part after its last full stop.
#[must_use]
pub(crate) fn leaf(path: &[u8]) -> &[u8] {
    path.rsplit(|&b| b == b'.').next().unwrap_or(path)
}

/// The error FileSwitch gives for `error` naming `name`.
pub(crate) fn named_error(os: &mut Os<'_>, error: Error, name: &[u8]) -> Fail {
    os.error_with(error, &[name])
}

#[cfg(test)]
mod tests;
