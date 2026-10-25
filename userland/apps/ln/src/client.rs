//! The link engine: decide each target's link name, free a taken name only
//! when told to, and create the link.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::{Errno, FileId, FileKind, RealpathMode};
use tairix_help::{own_short_help, HelpSource};
use tairix_path::{dir_name, is_relative_spelling, join, leaf_name};
use tairix_util::output::Carried;
use tairix_util::quote::quote;

use crate::command::{Clobber, Command, Options, TargetMode};
use crate::error::LnError;
use crate::io::{FileSystem, FinalLink, Node, Output, Prompt};

/// The usage banner the short-help switches print when `ln`'s own Help tree
/// is unavailable.
pub const USAGE: &str = "\
usage: ln [-srLPdFfinvT] [-t dir] [--] target... [link_name]

  -s, --symbolic             make symbolic links
  -r, --relative             store the target relative to the link (needs -s)
  -L, --logical              hard-link what a link target names
  -P, --physical             hard-link the target as spelled (default)
  -d, -F, --directory        accept a directory operand (still refused)
  -f, --force                replace an existing link name
  -i, --interactive          ask before replacing an existing link name
  -n, --no-dereference       treat a link-to-directory destination as a name
  -v, --verbose              report each link made
  -t dir, --target-directory=dir
                             create every link in dir
  -T, --no-target-directory  treat the destination as a link name
  -h, -?, --help             show this message

Without -s a hard link is made: a second directory entry for the target's
own inode, so both names reach one file and its storage survives until the
last name goes. Both names must be on one volume, and a directory is never
given a second name.

With one operand the link is made in the working directory under the
target's own name. With two, the second is a directory to fill when it is
one and the link's name otherwise. With three or more, the last must
already be a directory. `--` ends option parsing: every later argument is
an operand.
";

/// `ln`'s own command word: the short-help switches render its own Help
/// document through the same engine as any other command's.
const OWN_WORD: &str = "ln";

/// Run one [`Command`], creating its links through `fs`. `locale` is the
/// user's `LANG` preference, if set; `help` is the tool's own `Help/` tree,
/// read by the short-help switches.
///
/// Each target gets one link: at the destination itself for the two-operand
/// name form, inside the destination directory under the target's own leaf
/// name otherwise (`./name` for the one-operand form, as GNU names it). A
/// hard link's target is reached first. A link name that is already taken is
/// refused by default, replaced under `-f`, and asked about through `prompt`
/// under `-i`; a declined question skips that link and fails the run, as GNU's
/// does. The new link takes the old name's place in one step, only once it
/// exists, and one that would take the place of the very file it names is
/// refused. `-v` reports each link on `out`; otherwise `ln` writes nothing on
/// success.
///
/// A link that cannot be made is reported on `err` and the next is still
/// made, answering `Ok(false)` at the end.
///
/// # Errors
///
/// * [`LnError::Destination`] — the last of several operands is missing or
///   not a directory.
/// * [`LnError::NotADirectory`] — the `-t` directory is not one.
/// * [`LnError::Stat`] — a name could not be reached: a link name, the `-t`
///   directory (missing: [`Errno::NotFound`]), or a hard link's target.
/// * [`LnError::Canonicalize`] — `-r` could not canonicalise the target or
///   the link's own directory.
/// * [`LnError::Create`] — the link could not be created (a taken name
///   without `-f`/`-i` is [`Errno::AlreadyExists`]; a format that stores no
///   link of the asked-for kind is [`Errno::NotSupported`]; a hard link to a
///   directory is [`Errno::IsADirectory`] and one across volumes is
///   [`Errno::CrossVolume`]).
/// * [`LnError::SameFile`] — the new link would take the place of the very
///   file it names.
/// * [`LnError::Prompt`] — a confirmation could not be read (never treated
///   as consent).
/// * [`LnError::Output`] — writing the banner or a `-v` report failed.
pub fn run(
    command: Command,
    locale: Option<&str>,
    fs: &dyn FileSystem,
    prompt: &dyn Prompt,
    help: &dyn HelpSource,
    out: &dyn Output,
    err: &dyn Output,
) -> Result<bool, LnError> {
    match command {
        Command::Help => short_help(locale, help, out).map(|()| true),
        Command::Link {
            options,
            targets,
            destination,
        } => link_all(
            &targets,
            destination.as_deref(),
            options,
            fs,
            prompt,
            out,
            err,
        ),
    }
}

/// Render `ln`'s own short help (`NAME` + `SYNOPSIS` + compact `OPTIONS`)
/// from its own Help tree through the one shared engine; when no document
/// can be served (a build without the bundle's documents) the usage banner
/// stands in — the tool's own text, not fabricated help content — so `-h`
/// never fails.
fn short_help(
    locale: Option<&str>,
    help: &dyn HelpSource,
    out: &dyn Output,
) -> Result<(), LnError> {
    let bytes =
        own_short_help(help, locale, OWN_WORD).unwrap_or_else(|| String::from(USAGE).into_bytes());
    out.write_all(&bytes).map_err(LnError::Output)
}

/// Create one link per target, deciding once whether the destination is a
/// directory to fill or the single link's own name.
fn link_all(
    targets: &[String],
    destination: Option<&str>,
    options: Options,
    fs: &dyn FileSystem,
    prompt: &dyn Prompt,
    out: &dyn Output,
    err: &dyn Output,
) -> Result<bool, LnError> {
    let into_directory = match (destination, options.target_mode) {
        // Neither form fills a directory: the single-operand form links
        // under the target's own leaf name here, and `-T` links at the
        // destination whatever it holds.
        (None, _) | (Some(_), TargetMode::NoDirectory) => None,
        // `-t dir`: the destination must already be a directory.
        (Some(dir), TargetMode::Directory) => match occupant_of(fs, dir)? {
            occupant if occupant.receives_links(options.no_dereference) => Some(dir),
            Occupant::Vacant => return Err(LnError::Stat(String::from(dir), Errno::NotFound)),
            _ => return Err(LnError::NotADirectory(String::from(dir))),
        },
        // The inferred form: a destination that receives links is a
        // directory to fill; anything else is the one link's own name, which
        // several targets cannot share.
        (Some(dest), TargetMode::Inferred) => match occupant_of(fs, dest)? {
            occupant if occupant.receives_links(options.no_dereference) => Some(dest),
            _ if targets.len() == 1 => None,
            Occupant::Vacant => {
                return Err(LnError::Destination(String::from(dest), Errno::NotFound));
            }
            _ => {
                return Err(LnError::Destination(
                    String::from(dest),
                    Errno::NotADirectory,
                ))
            }
        },
    };

    let mut carried = Carried::new(err, OWN_WORD);
    for target in targets {
        let link = match (into_directory, destination) {
            (Some(dir), _) => join(dir, leaf_name(target)),
            // `-T` and the inferred name form both link at the destination.
            (None, Some(dest)) => String::from(dest),
            // The single-operand form: the target's own leaf name, here.
            (None, None) => join(".", leaf_name(target)),
        };
        let made = carried.absorb(link_target(target, &link, options, fs, prompt, out))?;
        if made == Some(Made::Declined) {
            carried.fail_quietly();
        }
    }
    Ok(carried.clean())
}

/// What one link came to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Made {
    /// The link is in place: made, or already the very link asked for.
    Linked,
    /// The user declined to replace the name it would take.
    Declined,
}

/// Make the link `link` to `target`, reporting it under `-v`.
fn link_target(
    target: &str,
    link: &str,
    options: Options,
    fs: &dyn FileSystem,
    prompt: &dyn Prompt,
    out: &dyn Output,
) -> Result<Made, LnError> {
    // `-r` rewrites what gets *stored*; the operand still names the target,
    // and the link name is untouched.
    let stored = if options.relative {
        relative_target(target, link, fs)?
    } else {
        String::from(target)
    };
    let made = link_one(&stored, link, options, fs, prompt)?;
    if made == Made::Linked && options.verbose {
        // GNU marks a hard link `=>` and a symbolic one `->`.
        let arrow = if options.symbolic { "->" } else { "=>" };
        out.write_all(format!("{} {arrow} {}\n", quote(link), quote(&stored)).as_bytes())
            .map_err(LnError::Output)?;
    }
    Ok(made)
}

/// The `-r` target: `target` spelled relative to the directory `link` sits
/// in.
///
/// Both halves are canonicalised by the **filesystem** first, which is what
/// makes the arithmetic below safe to do lexically: two canonical paths hold
/// no `.`, no `..`, and no symbolic link, so the `..`-and-names spelling
/// between them resolves back to exactly the node the target named. Doing
/// the same arithmetic on the paths as typed would be the lexical-`..`
/// collapse the resolver forbids — it would name a different node the moment
/// a link were involved.
///
/// [`RealpathMode::Missing`] is the reading asked for on both: a symbolic
/// link may legitimately name something that does not exist yet, and the
/// link's own directory may be about to be created.
fn relative_target(target: &str, link: &str, fs: &dyn FileSystem) -> Result<String, LnError> {
    let canonical_target = canonicalize(fs, target)?;
    let link_directory = dir_name(link);
    let canonical_base = canonicalize(fs, link_directory)?;
    Ok(relative_spelling(&canonical_base, &canonical_target))
}

/// Canonicalise `path` through the filesystem, surfacing a refusal as
/// [`LnError::Canonicalize`].
fn canonicalize(fs: &dyn FileSystem, path: &str) -> Result<String, LnError> {
    fs.canonicalize(path, RealpathMode::Missing)
        .map_err(|errno| LnError::Canonicalize(String::from(path), errno))
}

/// `target` spelled relative to `base`, both already canonical.
///
/// The shared leading components are dropped, one `..` is emitted for each
/// component of `base` that remains, and the rest of `target` follows. An
/// empty result — the two naming the same node — is spelled `.`, as GNU
/// `realpath -m --relative-to=/a/b /a/b` prints it.
fn relative_spelling(base: &str, target: &str) -> String {
    fn parts(path: &str) -> Vec<&str> {
        path.split('/').filter(|part| !part.is_empty()).collect()
    }
    let base = parts(base);
    let target = parts(target);
    let shared = base
        .iter()
        .zip(target.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let mut spelled: Vec<&str> = alloc::vec::Vec::new();
    spelled.resize(base.len() - shared, "..");
    spelled.extend_from_slice(&target[shared..]);
    if spelled.is_empty() {
        return String::from(".");
    }
    spelled.join("/")
}

/// Create the single link `link` naming `target`, in place of a taken name
/// when `-f`/`-i` approved it. The new link takes the old name's place in one
/// step, so nothing is lost when it cannot be made.
fn link_one(
    target: &str,
    link: &str,
    options: Options,
    fs: &dyn FileSystem,
    prompt: &dyn Prompt,
) -> Result<Made, LnError> {
    let refused = |errno| LnError::Create {
        link: String::from(link),
        target: String::from(target),
        symbolic: options.symbolic,
        errno,
    };
    let same_file = || LnError::SameFile {
        target: String::from(target),
        link: String::from(link),
    };
    let source = if options.symbolic {
        None
    } else {
        Some(
            hard_link_source(target, options, fs).map_err(|errno| match errno {
                Errno::IsADirectory => refused(errno),
                _ => LnError::Stat(String::from(target), errno),
            })?,
        )
    };
    let Some(own) = node(fs, link, FinalLink::Keep)? else {
        return make(target, link, options, fs, false)
            .map(|()| Made::Linked)
            .map_err(refused);
    };
    // A directory is never replaced: the destination readings above already
    // routed a link *into* one, so a directory still standing here is a name
    // this link cannot take.
    if own.kind == FileKind::Directory {
        return Err(refused(Errno::AlreadyExists));
    }
    match options.clobber {
        Clobber::Refuse => return Err(refused(Errno::AlreadyExists)),
        Clobber::Ask => {
            if !prompt
                .confirm(&format!("replace {}?", quote(link)))
                .map_err(LnError::Prompt)?
            {
                return Ok(Made::Declined);
            }
        }
        Clobber::Replace => {}
    }
    match source {
        // The name is already the target's node: forcing a node onto its own
        // entry is refused as GNU refuses it, and anything else is done.
        Some(source) if same_node(source.id, own.id) => {
            let follow = following(options);
            if options.clobber == Clobber::Replace
                && (own.links == 1 || same_entry(fs, (target, follow), link))
            {
                return Err(same_file());
            }
            return Ok(Made::Linked);
        }
        None if names_what_it_replaces(fs, target, link, own) => return Err(same_file()),
        _ => {}
    }
    make(target, link, options, fs, true).map_err(refused)?;
    Ok(Made::Linked)
}

/// Make the link `options` ask for, in place of what `link` holds when
/// `replace`.
fn make(
    target: &str,
    link: &str,
    options: Options,
    fs: &dyn FileSystem,
    replace: bool,
) -> Result<(), Errno> {
    // `-s` stores the target verbatim; without it the target's own inode
    // gains a second name, and `-L` decides whether a target that is itself
    // a symbolic link is resolved first.
    if options.symbolic {
        fs.symlink(target, link, replace)
    } else {
        fs.link(target, link, options.dereference_target, replace)
    }
}

/// How a hard link's target is taken: through a final link under `-L`.
const fn following(options: Options) -> FinalLink {
    if options.dereference_target {
        FinalLink::Follow
    } else {
        FinalLink::Keep
    }
}

/// The node a hard link to `target` would name, reached before the link's own
/// name is looked at, as GNU reaches it: [`Errno::NotFound`] when there is
/// none, [`Errno::IsADirectory`] for a directory.
fn hard_link_source(target: &str, options: Options, fs: &dyn FileSystem) -> Result<Node, Errno> {
    match fs.node(target, following(options))? {
        Some(node) if node.kind == FileKind::Directory => Err(Errno::IsADirectory),
        Some(node) => Ok(node),
        None => Err(Errno::NotFound),
    }
}

/// Whether a symbolic link at `link` storing `target` would name `own`, the
/// node it replaces, so that putting it there would destroy what it points
/// at: the target, read from the link's own directory as the link will read
/// it, is that node, reached through `link`'s own entry or as its only name.
fn names_what_it_replaces(fs: &dyn FileSystem, target: &str, link: &str, own: Node) -> bool {
    let from_link = if is_relative_spelling(target) {
        join(dir_name(link), target)
    } else {
        String::from(target)
    };
    let Ok(Some(named)) = fs.node(&from_link, FinalLink::Follow) else {
        return false;
    };
    same_node(named.id, own.id)
        && (own.links == 1 || same_entry(fs, (&from_link, FinalLink::Follow), link))
}

/// Whether two identities name one node; a backing that offers none never
/// does.
fn same_node(a: FileId, b: FileId) -> bool {
    !a.is_none() && a == b
}

/// Whether `path` (its final link followed or not) and the name `link` are one
/// directory entry, by their canonical spellings. An entry that cannot be
/// canonicalised is not taken to be the other.
fn same_entry(fs: &dyn FileSystem, (path, link_kept): (&str, FinalLink), link: &str) -> bool {
    match (entry(fs, path, link_kept), entry(fs, link, FinalLink::Keep)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// The canonical spelling of the directory entry `path` names: its directory
/// canonicalised and its own name kept, or the whole path resolved when its
/// final link is followed.
fn entry(fs: &dyn FileSystem, path: &str, link: FinalLink) -> Option<String> {
    match link {
        FinalLink::Follow => fs.canonicalize(path, RealpathMode::Existing).ok(),
        FinalLink::Keep => fs
            .canonicalize(dir_name(path), RealpathMode::Existing)
            .ok()
            .map(|dir| join(&dir, leaf_name(path))),
    }
}

/// The node `path` leads to, surfacing a failure to reach it as
/// [`LnError::Stat`].
fn node(fs: &dyn FileSystem, path: &str, link: FinalLink) -> Result<Option<Node>, LnError> {
    fs.node(path, link)
        .map_err(|errno| LnError::Stat(String::from(path), errno))
}

/// What a destination name holds, in the distinctions `ln`'s operand rules
/// turn on: a **directory** receives the links inside it; a **link to a
/// directory** does too, unless `-n` makes it the plain name it also is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Occupant {
    /// Nothing of that name.
    Vacant,
    /// A directory.
    Directory,
    /// A regular file.
    File,
    /// A symbolic link whose target resolves to a directory.
    LinkToDirectory,
    /// A symbolic link that resolves to something other than a directory,
    /// or that dangles.
    Link,
}

impl Occupant {
    /// Whether a link created at this name would land *inside* it — a
    /// directory, or a link to one when `-n` was not given: the whole of GNU's
    /// two-operand rule, so the destination reading and `-n` cannot disagree.
    const fn receives_links(self, no_dereference: bool) -> bool {
        match self {
            Self::Directory => true,
            Self::LinkToDirectory => !no_dereference,
            Self::Vacant | Self::File | Self::Link => false,
        }
    }
}

/// What the name `path` holds as a destination: a link is resolved just far
/// enough to say whether it names a directory, and one that dangles, or that
/// the caller may not follow, is not one.
fn occupant_of(fs: &dyn FileSystem, path: &str) -> Result<Occupant, LnError> {
    Ok(match node(fs, path, FinalLink::Keep)?.map(|own| own.kind) {
        None => Occupant::Vacant,
        Some(FileKind::Directory) => Occupant::Directory,
        Some(FileKind::Regular) => Occupant::File,
        Some(FileKind::Symlink) => {
            let named = fs.node(path, FinalLink::Follow).ok().flatten();
            if named.is_some_and(|named| named.kind == FileKind::Directory) {
                Occupant::LinkToDirectory
            } else {
                Occupant::Link
            }
        }
    })
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
