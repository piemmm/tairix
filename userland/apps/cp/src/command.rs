//! The parsed shape of a `cp` command line.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub use tairix_fscopy::{Attributes, Sparse};
use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};

use crate::error::CpError;

/// What happens when a destination file already exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Clobber {
    /// Overwrite it (the default).
    Overwrite,
    /// Ask before overwriting (`-i`).
    Prompt,
    /// Skip it silently (`-n`).
    Skip,
}

/// How the destination operand is interpreted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetMode {
    /// The last operand; copied *into* when it is an existing directory
    /// (the default).
    Inferred,
    /// An explicit target directory (`-t`); every operand is a source and
    /// the destination must be an existing directory.
    Directory,
    /// The last operand, always treated as a normal file (`-T`); exactly
    /// one source is permitted.
    NoDirectory,
}

/// The full option set of one `cp` run.
// The bools mirror GNU `cp`'s independent switches one-to-one (the `ls`/`rm`/
// `du` Options precedent); folding them into state enums would obscure the
// grammar they transcribe. The two that *are* alternatives rather than
// independent flags — `-l`/`-s` — are the `Contents` enum below.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Options {
    /// Descend into and reproduce directories (`-r`/`-R`). Without it, a
    /// directory source is a [`CpError::IsDirectory`].
    pub recursive: bool,
    /// Remove a destination that cannot be created and retry once (`-f`).
    pub force: bool,
    /// The existing-destination policy (`-i` / `-n`, last one wins).
    pub clobber: Clobber,
    /// Report each copy on standard output (`-v`).
    pub verbose: bool,
    /// How the destination operand is interpreted (`-t` / `-T`).
    pub target_mode: TargetMode,
    /// What a non-directory source becomes at the destination (`-l` / `-s`).
    pub contents: Contents,
    /// Which symbolic links in the sources are followed (`-H` / `-L` / `-P`,
    /// last one wins; `-d` is `-P` with `preserve_links`).
    pub dereference: Dereference,
    /// Give two sources naming one node two names at the destination rather
    /// than two copies (`--preserve=links`; `-d` sets this too).
    pub preserve_links: bool,
    /// How a sparse file's holes are copied (`--sparse=WHEN`).
    pub sparse: Sparse,
    /// How each copy's extended attributes are carried: as far as the
    /// destination keeps them by default, every one (`--preserve=xattr`), or
    /// none (`--no-preserve=xattr`).
    pub attributes: Attributes,
    /// Whether a file or directory the copy makes is born with its source's
    /// permission bits, under the umask, as GNU's is by default; with
    /// `--no-preserve=mode` it is born with the mode a new node takes when
    /// nothing names one.
    pub source_modes: bool,
    /// What each copy keeps of its source besides its bytes and attributes
    /// (`-p`, `-a`, `--preserve=mode,ownership,timestamps`).
    pub keep: Keep,
}

/// What a copy keeps of its source: its exact mode, its owner as far as the
/// copier may give it, and its access and modification times as nearly as
/// the destination holds them.
// Three independent `--preserve` members, transcribed one-to-one.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Keep {
    /// `--preserve=mode`.
    pub mode: bool,
    /// `--preserve=ownership`.
    pub ownership: bool,
    /// `--preserve=timestamps`.
    pub timestamps: bool,
}

impl Keep {
    /// What `-p` and a bare `--preserve` keep: all three.
    pub const ALL: Self = Self {
        mode: true,
        ownership: true,
        timestamps: true,
    };
}

/// What a non-directory source becomes at the destination.
///
/// `-l` and `-s` are alternatives to copying the bytes, not modifiers of it,
/// so they are one state rather than two flags — the later of the two wins,
/// as in the GNU tool, and neither can silently combine with the other.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Contents {
    /// A copy of the source's bytes (the default).
    #[default]
    Bytes,
    /// A second name for the source's own node (`-l`/`--link`).
    HardLink,
    /// A symbolic link naming the source (`-s`/`--symbolic-link`).
    SymbolicLink,
}

/// Which symbolic links in the sources a copy follows rather than
/// reproduces as links.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Dereference {
    /// GNU's default: an operand's link is followed unless the copy is
    /// recursive, and inside a tree none is, unless `-l` hard-links what the
    /// links name.
    #[default]
    Default,
    /// None (`-P`/`--no-dereference`).
    Never,
    /// The operands' own, not those met inside a tree (`-H`).
    Operands,
    /// Every one (`-L`/`--dereference`).
    Always,
}

impl Options {
    /// The defaults of a bare `cp`.
    pub const DEFAULT: Self = Self {
        recursive: false,
        force: false,
        clobber: Clobber::Overwrite,
        verbose: false,
        target_mode: TargetMode::Inferred,
        contents: Contents::Bytes,
        dereference: Dereference::Default,
        preserve_links: false,
        sparse: Sparse::Auto,
        attributes: Attributes::Carry,
        source_modes: true,
        keep: Keep {
            mode: false,
            ownership: false,
            timestamps: false,
        },
    };
}

/// One thing the `cp` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// Copy `sources` to `dest`, in operand order.
    Copy {
        /// The copy options.
        options: Options,
        /// The source paths to copy, in order. Always at least one.
        sources: Vec<String>,
        /// The destination path. Under [`TargetMode::Inferred`], when it
        /// names an existing directory (and always when there is more than
        /// one source), each source is copied into it under the source's
        /// base name; under [`TargetMode::Directory`] it is the `-t`
        /// directory; under [`TargetMode::NoDirectory`] it is used exactly
        /// as given.
        dest: String,
    },
    /// Print the usage banner (`-h`/`--help`).
    Help,
}

/// What each of `cp`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Recursive,
    Force,
    Interactive,
    NoClobber,
    Verbose,
    TargetDirectory,
    NoTargetDirectory,
    Link,
    SymbolicLink,
    NoDereference,
    Dereference,
    /// `-H`.
    DereferenceOperands,
    /// `-d`, which is `-P` and `--preserve=links`.
    NoDereferencePreserveLinks,
    Preserve,
    NoPreserve,
    /// `-p`, a bare `--preserve`.
    PreserveAll,
    /// `-a`, `-dR --preserve=all`.
    Archive,
    Sparse,
    Help,
}

/// `cp`'s options, as GNU's `cp` spells them and in its order, and the
/// reserved help switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('a', &["archive"], Flag::Archive),
    Opt::letter('d', Takes::Nothing, Flag::NoDereferencePreserveLinks),
    Opt::flag('L', &["dereference"], Flag::Dereference),
    Opt::flag('f', &["force"], Flag::Force),
    Opt::letter('H', Takes::Nothing, Flag::DereferenceOperands),
    Opt::flag('i', &["interactive"], Flag::Interactive),
    Opt::flag('l', &["link"], Flag::Link),
    Opt::flag('n', &["no-clobber"], Flag::NoClobber),
    Opt::flag('P', &["no-dereference"], Flag::NoDereference),
    Opt::letter('p', Takes::Nothing, Flag::PreserveAll),
    Opt::long(&["no-preserve"], Takes::Value, Flag::NoPreserve),
    Opt::flag('T', &["no-target-directory"], Flag::NoTargetDirectory),
    Opt::long(&["preserve"], Takes::Attached, Flag::Preserve),
    Opt::flag('r', &["recursive"], Flag::Recursive),
    Opt::letter('R', Takes::Nothing, Flag::Recursive),
    Opt::long(&["sparse"], Takes::Value, Flag::Sparse),
    Opt::flag('s', &["symbolic-link"], Flag::SymbolicLink),
    Opt::valued('t', &["target-directory"], Flag::TargetDirectory),
    Opt::flag('v', &["verbose"], Flag::Verbose),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is the GNU `cp` surface,
/// `cp [-adfHilLnPprRsvT] [-t dir] [--] source... dest`, read as GNU's
/// `getopt_long` reads it ([`Scan`]): options anywhere among the operands,
/// clustered, a long one by any unambiguous prefix, `--` ending them. The
/// help switches (`-h`, `-?`, `--help`) answer at once. The later of `-i`
/// and `-n`, of `-l` and `-s`, and of `-H`, `-L` and `-P` wins; `-d` is `-P`
/// with `--preserve=links`, `-p` is `--preserve=mode,ownership,timestamps`,
/// and `-a` is `-dR --preserve=all`. The values:
///
/// * `--preserve[=LIST]` — `mode` gives each copy its source's exact mode,
///   `ownership` its owner and group as far as the copier may give them,
///   `timestamps` its access and modification times as nearly as the
///   destination holds them, `links` keeps two sources' shared node shared,
///   `xattr` makes every extended attribute a copy must carry, failing one
///   the destination cannot keep, and `all` all but `xattr`'s failing; a
///   bare `--preserve` is `mode,ownership,timestamps`.
/// * `--no-preserve=LIST` — the same members, undone: `mode` also makes each
///   new node in the default mode rather than its source's, `xattr` carries
///   no extended attribute, `links` copies a shared node twice.
/// * `--sparse=WHEN` — `auto` keeps a sparse file's holes, `always` also
///   leaves its stored zeros unwritten, `never` writes every byte.
///
/// Without `-t`, the last path operand is the destination and the rest are
/// the sources; at least two path operands are required. With `-t`, every
/// path operand is a source and at least one is required.
///
/// # Errors
///
/// * [`CpError::Option`] — an option `getopt` refuses, in its words.
/// * [`CpError::Unsupported`] — `--preserve=context`: this system keeps no
///   security context to carry.
/// * [`CpError::Usage`] — an empty `-t` directory, an unknown `--sparse`,
///   `--preserve` or `--no-preserve` value, `-t` combined with `-T`, or too
///   few path operands.
pub fn parse(args: &[&str]) -> Result<Command, CpError> {
    let mut options = Options::DEFAULT;
    let mut target_dir: Option<String> = None;
    let mut no_target_dir = false;
    let mut operands = Vec::new();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(CpError::Option)? {
            Arg::Operand(path) => operands.push(String::from(path)),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(Flag::TargetDirectory, dir) => {
                let dir = dir.filter(|dir| !dir.is_empty()).ok_or(CpError::Usage)?;
                target_dir = Some(String::from(dir));
            }
            Arg::Opt(Flag::NoTargetDirectory, _) => no_target_dir = true,
            Arg::Opt(Flag::Preserve, list) => options.preserve(list)?,
            Arg::Opt(Flag::NoPreserve, list) => options.no_preserve(list.unwrap_or_default())?,
            Arg::Opt(Flag::Sparse, when) => {
                options.sparse = match when {
                    Some("auto") => Sparse::Auto,
                    Some("always") => Sparse::Always,
                    Some("never") => Sparse::Never,
                    _ => return Err(CpError::Usage),
                };
            }
            Arg::Opt(flag, _) => options.set(flag),
        }
    }
    if no_target_dir && target_dir.is_some() {
        return Err(CpError::Usage);
    }
    if no_target_dir {
        options.target_mode = TargetMode::NoDirectory;
    }
    let (sources, dest) = if let Some(dir) = target_dir {
        options.target_mode = TargetMode::Directory;
        if operands.is_empty() {
            return Err(CpError::Usage);
        }
        (operands, dir)
    } else {
        // The last operand is the destination; everything before it is a
        // source. `cp` needs at least one source and a destination.
        let Some(dest) = operands.pop() else {
            return Err(CpError::Usage);
        };
        if operands.is_empty() {
            return Err(CpError::Usage);
        }
        (operands, dest)
    };
    Ok(Command::Copy {
        options,
        sources,
        dest,
    })
}

impl Options {
    /// Apply the flag `flag` names.
    fn set(&mut self, flag: Flag) {
        match flag {
            Flag::Recursive => self.recursive = true,
            Flag::Force => self.force = true,
            Flag::Interactive => self.clobber = Clobber::Prompt,
            Flag::NoClobber => self.clobber = Clobber::Skip,
            Flag::Verbose => self.verbose = true,
            Flag::Link => self.contents = Contents::HardLink,
            Flag::SymbolicLink => self.contents = Contents::SymbolicLink,
            Flag::NoDereference => self.dereference = Dereference::Never,
            Flag::Dereference => self.dereference = Dereference::Always,
            Flag::DereferenceOperands => self.dereference = Dereference::Operands,
            Flag::NoDereferencePreserveLinks => {
                self.dereference = Dereference::Never;
                self.preserve_links = true;
            }
            Flag::PreserveAll => self.keep = Keep::ALL,
            Flag::Archive => {
                self.recursive = true;
                self.dereference = Dereference::Never;
                self.keep_all();
            }
            Flag::TargetDirectory
            | Flag::NoTargetDirectory
            | Flag::Preserve
            | Flag::NoPreserve
            | Flag::Sparse
            | Flag::Help => {}
        }
    }

    /// Keep everything `--preserve=all` names: the mode, the owner, the
    /// times, a shared node shared, and each extended attribute the
    /// destination keeps, a failure to keep one not ending the copy.
    fn keep_all(&mut self) {
        self.keep = Keep::ALL;
        self.preserve_links = true;
        self.attributes = Attributes::Carry;
    }

    /// Apply `--preserve[=LIST]`, a bare one keeping what `-p` keeps.
    ///
    /// # Errors
    ///
    /// [`CpError::Unsupported`] for `context`, a security context this
    /// system does not have, and [`CpError::Usage`] for a member `cp` does
    /// not know.
    fn preserve(&mut self, list: Option<&str>) -> Result<(), CpError> {
        let Some(list) = list else {
            self.keep = Keep::ALL;
            return Ok(());
        };
        for member in list.split(',') {
            match member {
                "mode" => self.keep.mode = true,
                "ownership" => self.keep.ownership = true,
                "timestamps" => self.keep.timestamps = true,
                "links" => self.preserve_links = true,
                "xattr" => self.attributes = Attributes::Require,
                "all" => self.keep_all(),
                "context" => return Err(CpError::Unsupported(format!("--preserve={list}"))),
                _ => return Err(CpError::Usage),
            }
        }
        Ok(())
    }

    /// Apply `--no-preserve=LIST`.
    ///
    /// # Errors
    ///
    /// [`CpError::Usage`] for a member `cp` does not know.
    fn no_preserve(&mut self, list: &str) -> Result<(), CpError> {
        for member in list.split(',') {
            match member {
                "links" => self.preserve_links = false,
                "xattr" => self.attributes = Attributes::Skip,
                "mode" => {
                    self.source_modes = false;
                    self.keep.mode = false;
                }
                "ownership" => self.keep.ownership = false,
                "timestamps" => self.keep.timestamps = false,
                "all" => {
                    self.preserve_links = false;
                    self.attributes = Attributes::Skip;
                    self.source_modes = false;
                    self.keep = Keep::default();
                }
                // No security context is ever kept, so already as asked.
                "context" => {}
                _ => return Err(CpError::Usage),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        parse, Attributes, Clobber, Command, Contents, Dereference, Keep, Options, Sparse,
        TargetMode,
    };
    use crate::error::CpError;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    fn copy_with(options: Options, sources: &[&str], dest: &str) -> Command {
        Command::Copy {
            options,
            sources: sources.iter().map(|p| String::from(*p)).collect::<Vec<_>>(),
            dest: dest.to_string(),
        }
    }

    fn copy(recursive: bool, force: bool, sources: &[&str], dest: &str) -> Command {
        copy_with(
            Options {
                recursive,
                force,
                ..Options::DEFAULT
            },
            sources,
            dest,
        )
    }

    #[test]
    fn two_operands_parse_as_source_and_dest() {
        assert_eq!(parse(&["a", "b"]), Ok(copy(false, false, &["a"], "b")));
    }

    #[test]
    fn several_sources_precede_the_destination() {
        assert_eq!(
            parse(&["a", "b", "dst"]),
            Ok(copy(false, false, &["a", "b"], "dst"))
        );
    }

    #[test]
    fn too_few_operands_is_usage() {
        assert_eq!(parse(&[]), Err(CpError::Usage));
        assert_eq!(parse(&["only"]), Err(CpError::Usage));
        assert_eq!(parse(&["-r"]), Err(CpError::Usage));
    }

    #[test]
    fn recursive_and_force_spellings_parse() {
        for args in [
            ["-r", "a", "b"],
            ["-R", "a", "b"],
            ["--recursive", "a", "b"],
        ] {
            assert_eq!(parse(&args), Ok(copy(true, false, &["a"], "b")));
        }
        assert_eq!(parse(&["-rf", "a", "b"]), Ok(copy(true, true, &["a"], "b")));
        assert_eq!(
            parse(&["--force", "a", "b"]),
            Ok(copy(false, true, &["a"], "b"))
        );
    }

    #[test]
    fn interactive_and_no_clobber_last_one_wins() {
        assert_eq!(
            parse(&["-i", "a", "b"]),
            Ok(copy_with(
                Options {
                    clobber: Clobber::Prompt,
                    ..Options::DEFAULT
                },
                &["a"],
                "b",
            ))
        );
        assert_eq!(
            parse(&["-n", "a", "b"]),
            Ok(copy_with(
                Options {
                    clobber: Clobber::Skip,
                    ..Options::DEFAULT
                },
                &["a"],
                "b",
            ))
        );
        assert_eq!(
            parse(&["-i", "-n", "a", "b"]),
            Ok(copy_with(
                Options {
                    clobber: Clobber::Skip,
                    ..Options::DEFAULT
                },
                &["a"],
                "b",
            ))
        );
        assert_eq!(
            parse(&["--no-clobber", "--interactive", "a", "b"]),
            Ok(copy_with(
                Options {
                    clobber: Clobber::Prompt,
                    ..Options::DEFAULT
                },
                &["a"],
                "b",
            ))
        );
    }

    #[test]
    fn verbose_parses() {
        assert_eq!(
            parse(&["-v", "a", "b"]),
            Ok(copy_with(
                Options {
                    verbose: true,
                    ..Options::DEFAULT
                },
                &["a"],
                "b",
            ))
        );
    }

    #[test]
    fn target_directory_takes_a_value() {
        let expected = copy_with(
            Options {
                target_mode: TargetMode::Directory,
                ..Options::DEFAULT
            },
            &["a", "b"],
            "dst",
        );
        assert_eq!(parse(&["-t", "dst", "a", "b"]), Ok(expected.clone()));
        assert_eq!(parse(&["-tdst", "a", "b"]), Ok(expected.clone()));
        assert_eq!(
            parse(&["--target-directory", "dst", "a", "b"]),
            Ok(expected.clone())
        );
        assert_eq!(parse(&["--target-directory=dst", "a", "b"]), Ok(expected));
        // A missing value is refused by `getopt`, an empty one by `cp`.
        assert_eq!(refused(&["a", "-t"]), "option requires an argument -- 't'");
        assert_eq!(parse(&["--target-directory=", "a"]), Err(CpError::Usage));
        // With -t, at least one source is still required.
        assert_eq!(parse(&["-t", "dst"]), Err(CpError::Usage));
    }

    #[test]
    fn no_target_directory_parses_and_excludes_t() {
        assert_eq!(
            parse(&["-T", "a", "b"]),
            Ok(copy_with(
                Options {
                    target_mode: TargetMode::NoDirectory,
                    ..Options::DEFAULT
                },
                &["a"],
                "b",
            ))
        );
        assert_eq!(
            parse(&["--no-target-directory", "a", "b"]),
            Ok(copy_with(
                Options {
                    target_mode: TargetMode::NoDirectory,
                    ..Options::DEFAULT
                },
                &["a"],
                "b",
            ))
        );
        assert_eq!(parse(&["-T", "-t", "d", "a"]), Err(CpError::Usage));
    }

    #[test]
    fn help_flags_win() {
        assert_eq!(parse(&["-h"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-rh", "a", "b"]), Ok(Command::Help));
    }

    /// Every locale's `OPTIONS` section documents exactly the switches this
    /// parser accepts (`plans/APPS.md` §3.1): the flag tokens are
    /// language-neutral, so each translated document must carry the same
    /// keys as the canonical one. The documents are read from the bundle's
    /// own on-disk `Help/` tree — the single source the image builder plants
    /// — never a copy embedded in this crate.
    #[test]
    fn help_documents_the_parser_switches() {
        extern crate std;
        use alloc::format;
        use std::fs;

        let help_root = format!("{}/Help", env!("CARGO_MANIFEST_DIR"));
        let locales = tairix_help::REQUIRED_LOCALES;
        for locale in locales {
            let path = format!("{help_root}/{locale}/cp.md");
            let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
            for switch in [
                "`-r, -R, --recursive`",
                "`-f, --force`",
                "`-i, --interactive`",
                "`-n, --no-clobber`",
                "`-l, --link`",
                "`-s, --symbolic-link`",
                "`-P, --no-dereference`",
                "`-L, --dereference`",
                "`-H`",
                "`--preserve=links`",
                "`--preserve=xattr`",
                "`--no-preserve=xattr`",
                "`--no-preserve=mode`",
                "`--sparse=WHEN`",
                "`-d`",
                "`-v, --verbose`",
                "`-t dir, --target-directory=dir`",
                "`-T, --no-target-directory`",
                "`-h, -?, --help`",
            ] {
                assert!(
                    text.contains(switch),
                    "{locale}/cp.md must document {switch}"
                );
            }
        }
    }

    #[test]
    fn the_link_switches_are_one_last_wins_state() {
        // `-l` and `-s` are alternatives to copying the bytes, not modifiers
        // of it, so they cannot silently combine.
        let hard = parse(&["-l", "a", "b"]).expect("parse");
        assert!(
            matches!(hard, Command::Copy { options, .. } if options.contents == Contents::HardLink)
        );
        let soft = parse(&["--symbolic-link", "a", "b"]).expect("parse");
        assert!(
            matches!(soft, Command::Copy { options, .. } if options.contents == Contents::SymbolicLink)
        );
        let last = parse(&["-l", "-s", "a", "b"]).expect("parse");
        assert!(
            matches!(last, Command::Copy { options, .. } if options.contents == Contents::SymbolicLink)
        );
        let back = parse(&["-s", "-l", "a", "b"]).expect("parse");
        assert!(
            matches!(back, Command::Copy { options, .. } if options.contents == Contents::HardLink)
        );
    }

    #[test]
    fn h_l_and_p_choose_which_links_are_followed_and_the_last_one_wins() {
        let followed = |args: &[&str]| match parse(args).expect("parse") {
            Command::Copy { options, .. } => options.dereference,
            Command::Help => panic!("expected a copy"),
        };
        assert_eq!(followed(&["a", "b"]), Dereference::Default);
        assert_eq!(followed(&["-L", "a", "b"]), Dereference::Always);
        assert_eq!(followed(&["--dereference", "a", "b"]), Dereference::Always);
        assert_eq!(followed(&["-H", "a", "b"]), Dereference::Operands);
        assert_eq!(followed(&["-P", "a", "b"]), Dereference::Never);
        assert_eq!(followed(&["-LP", "a", "b"]), Dereference::Never);
        assert_eq!(followed(&["-PL", "a", "b"]), Dereference::Always);
        assert_eq!(followed(&["-Hd", "a", "b"]), Dereference::Never);
    }

    #[test]
    fn d_is_exactly_no_dereference_plus_preserve_links() {
        // GNU defines `-d` as that pair, so it is spelled as the pair rather
        // than as a third state that could drift from either half.
        let d = parse(&["-d", "a", "b"]).expect("parse");
        let Command::Copy { options, .. } = d else {
            panic!("expected a copy");
        };
        assert_eq!(options.dereference, Dereference::Never);
        assert!(options.preserve_links);
        let pair = parse(&["-P", "--preserve=links", "a", "b"]).expect("parse");
        assert!(matches!(pair, Command::Copy { options: o, .. } if o == options));
    }

    #[test]
    fn preserve_members_keep_what_they_name_and_p_and_a_are_gnus() {
        let options = |args: &[&str]| match parse(args) {
            Ok(Command::Copy { options, .. }) => options,
            other => panic!("expected a copy: {other:?}"),
        };
        for args in [&["-p", "a", "b"][..], &["--preserve", "a", "b"]] {
            assert_eq!(options(args).keep, Keep::ALL, "{args:?}");
        }
        let one = options(&["--preserve=timestamps", "a", "b"]).keep;
        assert_eq!(
            one,
            Keep {
                timestamps: true,
                ..Keep::default()
            }
        );
        let archive = options(&["-a", "a", "b"]);
        assert_eq!(
            archive,
            Options {
                recursive: true,
                dereference: Dereference::Never,
                preserve_links: true,
                keep: Keep::ALL,
                ..Options::DEFAULT
            },
            "-a is -dR --preserve=all"
        );
        assert_eq!(options(&["--archive", "a", "b"]), archive);
        let undone = options(&["-p", "--no-preserve=ownership,timestamps", "a", "b"]).keep;
        assert_eq!(
            undone,
            Keep {
                mode: true,
                ..Keep::default()
            }
        );
        assert_eq!(
            parse(&["--preserve=context", "a", "b"]),
            Err(CpError::Unsupported(String::from("--preserve=context")))
        );
        assert_eq!(parse(&["--preserve=colour", "a", "b"]), Err(CpError::Usage));
    }

    #[test]
    fn attributes_are_carried_unless_preserve_or_no_preserve_says_otherwise() {
        let attributes = |args: &[&str]| match parse(args) {
            Ok(Command::Copy { options, .. }) => options.attributes,
            other => panic!("expected a copy: {other:?}"),
        };
        assert_eq!(attributes(&["a", "b"]), Attributes::Carry);
        assert_eq!(
            attributes(&["--preserve=xattr", "a", "b"]),
            Attributes::Require
        );
        assert_eq!(
            attributes(&["--no-preserve=xattr", "a", "b"]),
            Attributes::Skip
        );
        assert_eq!(
            attributes(&["--no-preserve=all", "a", "b"]),
            Attributes::Skip
        );
        assert_eq!(
            attributes(&["--no-preserve=mode,timestamps", "a", "b"]),
            Attributes::Carry
        );
        assert_eq!(
            parse(&["--no-preserve=bogus", "a", "b"]),
            Err(CpError::Usage)
        );
    }

    /// GNU 9.4: a new copy takes its source's bits unless `--no-preserve`
    /// names `mode`, alone or through `all`.
    #[test]
    fn source_modes_are_kept_unless_no_preserve_names_mode() {
        let source_modes = |args: &[&str]| match parse(args) {
            Ok(Command::Copy { options, .. }) => options.source_modes,
            other => panic!("expected a copy: {other:?}"),
        };
        assert!(source_modes(&["a", "b"]));
        assert!(source_modes(&["--no-preserve=xattr", "a", "b"]));
        assert!(!source_modes(&["--no-preserve=mode", "a", "b"]));
        assert!(!source_modes(&["--no-preserve=timestamps,mode", "a", "b"]));
        assert!(!source_modes(&["--no-preserve=all", "a", "b"]));
    }

    #[test]
    fn sparse_takes_auto_always_or_never() {
        let sparse = |args: &[&str]| match parse(args) {
            Ok(Command::Copy { options, .. }) => options.sparse,
            other => panic!("expected a copy: {other:?}"),
        };
        assert_eq!(sparse(&["a", "b"]), Sparse::Auto);
        assert_eq!(sparse(&["--sparse=always", "a", "b"]), Sparse::Always);
        assert_eq!(sparse(&["--sparse=never", "a", "b"]), Sparse::Never);
        assert_eq!(
            sparse(&["--sparse=never", "--sparse=auto", "a", "b"]),
            Sparse::Auto
        );
        assert_eq!(sparse(&["--sparse", "always", "a", "b"]), Sparse::Always);
        assert_eq!(
            parse(&["--sparse=sometimes", "a", "b"]),
            Err(CpError::Usage)
        );
        assert_eq!(
            refused(&["a", "b", "--sparse"]),
            "option '--sparse' requires an argument"
        );
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x", "a", "b"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["--frobnicate", "a", "b"]),
            "unrecognized option '--frobnicate'"
        );
        // A bundle with one bad flag is refused whole.
        assert_eq!(refused(&["-rx", "a", "b"]), "invalid option -- 'x'");
        assert_eq!(
            refused(&["a", "--target-directory"]),
            "option '--target-directory' requires an argument"
        );
        assert_eq!(
            refused(&["--force=1", "a", "b"]),
            "option '--force' doesn't allow an argument"
        );
    }

    /// GNU 9.4 reads `cp --rec --verb a b` as `cp --recursive --verbose a b`.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(
            parse(&["--rec", "--verb", "a", "b"]),
            Ok(copy_with(
                Options {
                    recursive: true,
                    verbose: true,
                    ..Options::DEFAULT
                },
                &["a"],
                "b",
            ))
        );
        assert_eq!(
            refused(&["--no", "a", "b"]),
            "option '--no' is ambiguous; possibilities: '--no-clobber' '--no-dereference' \
             '--no-preserve' '--no-target-directory'"
        );
    }

    #[test]
    fn double_dash_ends_options() {
        assert_eq!(
            parse(&["--", "-r", "b"]),
            Ok(copy(false, false, &["-r"], "b"))
        );
    }

    #[test]
    fn bare_dash_is_a_path() {
        assert_eq!(parse(&["-", "b"]), Ok(copy(false, false, &["-"], "b")));
    }
}
