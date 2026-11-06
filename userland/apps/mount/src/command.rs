//! The parsed shape of a `mount` command line.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::imageattach::ImageAccess;
use tairix_abi::volume::{validate_volume_name, FormatName, ReplayAnswer, VolumeFsTypes};
use tairix_abi::STORAGE_CATALOG;
use tairix_util::getopt::{Arg, Opt, Order, Scan, Takes};

use crate::error::MountError;

/// One thing the `mount` tool can do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// List the mount table (no operands), as [`Listing`] narrows it.
    List(Listing),
    /// Mount a disk image (one or two operands).
    Mount(MountRequest),
    /// Answer for the writes a volume holds awaiting one (`--replay-held`,
    /// `--discard-held`).
    Answer(HeldAnswer),
    /// Print the tool's own short help (`-h`, `-?`, `--help`).
    Help,
}

/// What a listing shows.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Listing {
    /// The types it shows, as `-t` names them.
    pub types: TypeFilter,
    /// Only the volumes holding writes awaiting an answer (`--list-held`).
    pub held: bool,
}

/// An answer for the writes the volume `name` names holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeldAnswer {
    /// The volume, as given.
    pub name: String,
    /// What becomes of its writes.
    pub answer: ReplayAnswer,
    /// Whether to say what became of them (`-v`).
    pub verbose: bool,
}

/// A disk image to mount, and how.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MountRequest {
    /// The image file, as given.
    pub image: String,
    /// The name it is to take in `/Storage` (a `/Storage/NAME` target, or
    /// `-o name=NAME`), or `None` for the one its volume's label gives.
    pub name: Option<String>,
    /// The directory it is also to be shown at, as given.
    pub target: Option<String>,
    /// The filesystems the image must hold one of (`-t`), or `None` for
    /// whatever it is recognised as.
    pub expects: Option<VolumeFsTypes>,
    /// How writable it is to be: read-only (`-r`), read-write or not at all
    /// (`-w`), or read-write where it can be.
    pub access: ImageAccess,
    /// Whether to say where it was mounted (`-v`).
    pub verbose: bool,
}

/// The filesystem types a util-linux `-t` list names: comma-separated, a
/// type spelled `no<type>` left out, and a list beginning `no` naming every
/// type it does not. A listing's list is compared without regard to ASCII
/// case; a mount's only as spelled, as the type reaches the kernel.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TypeFilter {
    /// The list as given; empty names every type.
    list: String,
    exact: bool,
}

impl TypeFilter {
    /// The filter `list` spells, matching as spelled where `exact`.
    ///
    /// # Errors
    ///
    /// [`MountError::BadOption`] for a list naming an empty type.
    fn parse(list: &str, exact: bool) -> Result<Self, MountError> {
        let named = list.strip_prefix("no").unwrap_or(list);
        if !list.is_empty() && named.split(',').any(str::is_empty) {
            return Err(MountError::BadOption);
        }
        Ok(Self {
            list: String::from(list),
            exact,
        })
    }

    /// Whether the list names `fstype`.
    #[must_use]
    pub fn shows(&self, fstype: &[u8]) -> bool {
        if self.list.is_empty() {
            return true;
        }
        let same = |name: &str| {
            if self.exact {
                name.as_bytes() == fstype
            } else {
                name.as_bytes().eq_ignore_ascii_case(fstype)
            }
        };
        let (negated, named) = match self.list.strip_prefix("no") {
            Some(rest) => (true, rest),
            None => (false, self.list.as_str()),
        };
        for element in named.split(',') {
            if element.strip_prefix("no").is_some_and(same) {
                return false;
            }
            if same(element) {
                return !negated;
            }
        }
        negated
    }
}

/// What each of `mount`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    ReadOnly,
    ReadWrite,
    Types,
    Options,
    Verbose,
    ShowLabels,
    ListHeld,
    ReplayHeld,
    DiscardHeld,
    Help,
}

/// `mount`'s options, as util-linux spells them, TAIRiX's own for held
/// writes, and the reserved help switches. The held-write options are long
/// alone, so they take no letter util-linux means otherwise, and leave every
/// abbreviation an older option had as it was.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::flag('r', &["read-only"], Flag::ReadOnly),
    Opt::flag('w', &["rw", "read-write"], Flag::ReadWrite),
    Opt::valued('t', &["types"], Flag::Types),
    Opt::valued('o', &["options"], Flag::Options),
    Opt::flag('v', &["verbose"], Flag::Verbose),
    Opt::flag('l', &["show-labels"], Flag::ShowLabels),
    Opt::long(&["list-held"], Takes::Nothing, Flag::ListHeld),
    Opt::long(&["replay-held"], Takes::Value, Flag::ReplayHeld),
    Opt::long(&["discard-held"], Takes::Value, Flag::DiscardHeld),
    Opt::flag('h', &["help"], Flag::Help),
    Opt::letter('?', Takes::Nothing, Flag::Help),
];

/// Parse `args` (the tool's arguments, excluding the program name) into a
/// [`Command`].
///
/// The grammar is `mount [-lrvw] [-t TYPE] [-o OPTIONS] [--] [IMAGE [TARGET]]`,
/// or `mount [-v] --list-held | --replay-held NAME | --discard-held NAME`,
/// read as GNU's `getopt_long` reads it ([`Scan`]): options anywhere among
/// the operands, clustered, a value attached or the next argument, a long
/// option by any unambiguous prefix, `--` ending them.
///
/// * no operands — list the mount table, only the types `-t` names; `-v`
///   and `-l` change nothing of it, the listing carrying no labels.
/// * `IMAGE` — mount the disk image `IMAGE`.
/// * `IMAGE /Storage/NAME` — mount it as `NAME`.
/// * `IMAGE DIRECTORY` — mount it, and show it at `DIRECTORY` too.
/// * `-r`/`-w` — read-only or read-write; the last of them, and of `-o ro`
///   and `-o rw`, wins. `-w` as util-linux's: read-write, or not mounted,
///   until a later `-r` takes it back.
/// * `-t LIST` — the filesystems `IMAGE` must hold one of, as util-linux
///   reads a list and the mount table names each, or `auto` for whatever it
///   holds; what it holds is recognised, so the order chooses nothing.
/// * `-o LIST` — a comma-separated subset of `ro`, `rw`, `noexec`, `loop`
///   and `name=NAME`.
/// * `-v` — say where it was mounted, or what became of held writes.
/// * `--list-held` — list only the volumes holding writes awaiting an answer.
/// * `--replay-held NAME`, `--discard-held NAME` — answer for the writes the
///   volume `NAME` names holds: written to its medium, or given up.
/// * `-h`, `-?`, `--help` — the short help (wins immediately).
///
/// # Errors
///
/// * [`MountError::Option`] — an option `getopt` refuses, in its words.
/// * [`MountError::Usage`] — more than two operands, `-r`, `-w` or `-o` with
///   nothing to mount, two different names asked for, an operand beside
///   `--list-held`, or an answer that is not one alone naming a volume.
/// * [`MountError::BadOption`] — an unknown or empty `-o` element, or a `-t`
///   list naming an empty type.
/// * [`MountError::BadName`] — a name no volume may take.
/// * [`MountError::UnknownType`] — a `-t` list naming no filesystem a disk
///   image holds.
pub fn parse(args: &[&str]) -> Result<Command, MountError> {
    let Some(asked) = Asked::scan(args)? else {
        return Ok(Command::Help);
    };
    if !asked.answers.is_empty() {
        return asked.answer();
    }
    let Asked {
        writability,
        verbose,
        types,
        mut name,
        operands,
        asks_a_mount,
        held,
        ..
    } = asked;
    let (image, target) = match operands.as_slice() {
        [] if asks_a_mount => return Err(MountError::Usage),
        [] => {
            let types = TypeFilter::parse(types.unwrap_or_default(), false)?;
            return Ok(Command::List(Listing { types, held }));
        }
        _ if held => return Err(MountError::Usage),
        [image] => (*image, None),
        [image, target] => (*image, Some(*target)),
        _ => return Err(MountError::Usage),
    };
    let target = match target.map(storage_name) {
        Some(Some(named)) => {
            if name.as_deref().is_some_and(|asked| asked != named) {
                return Err(MountError::Usage);
            }
            name = Some(String::from(named));
            None
        }
        _ => target.map(String::from),
    };
    if let Some(name) = &name {
        validate_volume_name(name.as_bytes()).map_err(|_| MountError::BadName(name.clone()))?;
    }
    let expects = match types {
        None | Some("auto") => None,
        Some(asked) => Some(
            image_formats(asked)?.ok_or_else(|| MountError::UnknownType {
                image: String::from(image),
                name: String::from(asked),
            })?,
        ),
    };
    Ok(Command::Mount(MountRequest {
        image: String::from(image),
        name,
        target,
        expects,
        access: writability.access(),
        verbose,
    }))
}

/// How writable `-r`, `-w` and `-o` have asked a mount to be: the last of
/// `-r` and `-o ro` against `-w` and `-o rw` decides read-only, and `-w`'s
/// read-write-or-nothing stands until a later `-r` takes it back.
#[derive(Clone, Copy, Default)]
struct Writability {
    read_only: bool,
    forced: bool,
}

impl Writability {
    /// What the attach is asked for.
    const fn access(self) -> ImageAccess {
        if self.read_only {
            ImageAccess::ReadOnly
        } else if self.forced {
            ImageAccess::Writable
        } else {
            ImageAccess::WritableWherePossible
        }
    }
}

/// What a command line asked, as the scan read it.
#[derive(Default)]
struct Asked<'a> {
    writability: Writability,
    verbose: bool,
    types: Option<&'a str>,
    name: Option<String>,
    operands: Vec<&'a str>,
    /// `-r`, `-w` or `-o`, which only a mount takes.
    asks_a_mount: bool,
    held: bool,
    /// Each `--replay-held` and `--discard-held`, with the volume it names.
    answers: Vec<(ReplayAnswer, &'a str)>,
}

impl<'a> Asked<'a> {
    /// Read `args` as `getopt_long` does, or `None` for the short help, which
    /// wins immediately.
    fn scan(args: &'a [&'a str]) -> Result<Option<Self>, MountError> {
        let mut asked = Self::default();
        for arg in Scan::new(OPTIONS, args, Order::Permute) {
            let arg = arg.map_err(MountError::Option)?;
            asked.asks_a_mount |= matches!(
                arg,
                Arg::Opt(Flag::ReadOnly | Flag::ReadWrite | Flag::Options, _)
            );
            match arg {
                Arg::Operand(operand) => asked.operands.push(operand),
                Arg::Opt(Flag::Help, _) => return Ok(None),
                Arg::Opt(Flag::ReadOnly, _) => {
                    asked.writability = Writability {
                        read_only: true,
                        forced: false,
                    };
                }
                Arg::Opt(Flag::ReadWrite, _) => {
                    asked.writability = Writability {
                        read_only: false,
                        forced: true,
                    };
                }
                Arg::Opt(Flag::Verbose, _) => asked.verbose = true,
                Arg::Opt(Flag::ShowLabels, _) => {}
                Arg::Opt(Flag::Types, value) => asked.types = Some(value.unwrap_or_default()),
                Arg::Opt(Flag::Options, value) => asked.options(value.unwrap_or_default())?,
                Arg::Opt(Flag::ListHeld, _) => asked.held = true,
                Arg::Opt(Flag::ReplayHeld, value) => {
                    asked
                        .answers
                        .push((ReplayAnswer::Write, value.unwrap_or_default()));
                }
                Arg::Opt(Flag::DiscardHeld, value) => {
                    asked
                        .answers
                        .push((ReplayAnswer::Discard, value.unwrap_or_default()));
                }
            }
        }
        Ok(Some(asked))
    }

    /// Take the `-o` list `list`.
    fn options(&mut self, list: &str) -> Result<(), MountError> {
        for element in list.split(',') {
            match MountOption::parse(element)? {
                MountOption::ReadOnly(asked) => self.writability.read_only = asked,
                MountOption::Name(asked) => self.name = Some(String::from(asked)),
                MountOption::Implied => {}
            }
        }
        Ok(())
    }

    /// The one answer asked for, naming one volume, with nothing to mount or
    /// list beside it.
    fn answer(&self) -> Result<Command, MountError> {
        let [(answer, name)] = self.answers.as_slice() else {
            return Err(MountError::Usage);
        };
        if name.is_empty()
            || self.held
            || self.asks_a_mount
            || self.types.is_some()
            || !self.operands.is_empty()
        {
            return Err(MountError::Usage);
        }
        Ok(Command::Answer(HeldAnswer {
            name: String::from(*name),
            answer: *answer,
            verbose: self.verbose,
        }))
    }
}

/// One element of an `-o` list.
enum MountOption<'a> {
    /// `ro`, or `rw`.
    ReadOnly(bool),
    /// `name=NAME`.
    Name(&'a str),
    /// What a disk image is mounted as anyway: `noexec`, and `loop`.
    Implied,
}

impl<'a> MountOption<'a> {
    /// The option `element` names.
    ///
    /// There are no setuid programs or device files to restrict, so `nosuid`
    /// and `nodev` are refused rather than taken as no-ops, and an image is
    /// never executable from, so `exec` is refused.
    fn parse(element: &'a str) -> Result<Self, MountError> {
        Ok(match element {
            "ro" => Self::ReadOnly(true),
            "rw" => Self::ReadOnly(false),
            "noexec" | "loop" => Self::Implied,
            _ => match element.strip_prefix("name=") {
                Some(name) => Self::Name(name),
                None => return Err(MountError::BadOption),
            },
        })
    }
}

/// The filesystems a disk image may hold that the `-t` list `list` names,
/// each as the mount table spells it, or `None` for none: a network protocol
/// is no image's filesystem, and an element naming none is passed over, as
/// util-linux tries the next type.
///
/// # Errors
///
/// [`MountError::BadOption`] for a list naming an empty type.
fn image_formats(list: &str) -> Result<Option<VolumeFsTypes>, MountError> {
    if list.is_empty() {
        return Ok(None);
    }
    let filter = TypeFilter::parse(list, true)?;
    Ok(VolumeFsTypes::of(
        FormatName::ALL
            .into_iter()
            .filter(|format| filter.shows(format.as_str().as_bytes()))
            .filter_map(FormatName::family),
    ))
}

/// The name a `/Storage/NAME` target asks for, or `None` for any other
/// directory.
fn storage_name(target: &str) -> Option<&str> {
    let mut components = target.split('/').filter(|part| !part.is_empty());
    let named = match (
        target.starts_with('/'),
        components.next(),
        components.next(),
    ) {
        (true, Some(catalog), Some(name)) if catalog == STORAGE_CATALOG => name,
        _ => return None,
    };
    components.next().is_none().then_some(named)
}

#[cfg(test)]
mod tests {
    use super::{parse, Command, HeldAnswer, Listing, MountRequest, TypeFilter};
    use crate::error::MountError;
    use alloc::string::{String, ToString};
    use tairix_abi::imageattach::ImageAccess;
    use tairix_abi::volume::ReplayAnswer;
    use tairix_abi::volume::VolumeFsType;
    use tairix_abi::volume::VolumeFsTypes;

    fn image(image: &str) -> MountRequest {
        MountRequest {
            image: String::from(image),
            name: None,
            target: None,
            expects: None,
            access: ImageAccess::WritableWherePossible,
            verbose: false,
        }
    }

    fn mounted(request: MountRequest) -> Command {
        Command::Mount(request)
    }

    #[test]
    fn no_arguments_lists_the_mount_table() {
        let every = Ok(Command::List(Listing::default()));
        assert_eq!(parse(&[]), every);
        assert_eq!(parse(&["--"]), every);
        assert_eq!(parse(&["-v"]), every, "verbose is no mount option");
        assert_eq!(parse(&["-l"]), every, "the listing carries no labels");
        assert_eq!(parse(&["--show-labels", "-v"]), every);
    }

    /// util-linux's `-t` list: `mount -t affs,iso9660` lists those types,
    /// `-t noaffs,iso9660` every other, and `-t iso9660,noaffs` only
    /// `iso9660`, matched without regard to case.
    #[test]
    fn a_listing_shows_the_types_its_list_names() {
        let listed = |args: &[&str]| match parse(args) {
            Ok(Command::List(Listing { types, held: false })) => {
                ["affs", "ISO9660", "fat32", "none"]
                    .into_iter()
                    .filter(|fstype| types.shows(fstype.as_bytes()))
                    .collect::<alloc::vec::Vec<_>>()
            }
            other => panic!("{args:?} is no listing: {other:?}"),
        };
        assert_eq!(listed(&["-t", "affs,iso9660"]), ["affs", "ISO9660"]);
        assert_eq!(listed(&["-t", "noaffs,iso9660"]), ["fat32", "none"]);
        assert_eq!(listed(&["-tiso9660,noaffs"]), ["ISO9660"]);
        assert_eq!(listed(&["--types=FAT32"]), ["fat32"]);
        assert_eq!(listed(&["-t", ""]), ["affs", "ISO9660", "fat32", "none"]);
        for empty in ["affs,", "no", ",affs"] {
            assert_eq!(parse(&["-t", empty]), Err(MountError::BadOption), "{empty}");
        }
    }

    #[test]
    fn one_operand_mounts_an_image_under_its_own_name() {
        assert_eq!(parse(&["wb.adf"]), Ok(mounted(image("wb.adf"))));
    }

    #[test]
    fn a_storage_target_names_the_volume_and_any_other_shows_it_there() {
        let named = |name: &str| MountRequest {
            name: Some(String::from(name)),
            ..image("wb.adf")
        };
        assert_eq!(
            parse(&["wb.adf", "/Storage/workbench"]),
            Ok(mounted(named("workbench")))
        );
        assert_eq!(
            parse(&["wb.adf", "/Storage/workbench/"]),
            Ok(mounted(named("workbench")))
        );
        for elsewhere in [
            "/Users/ada/Home/wb",
            "/Storage",
            "/Storage/a/b",
            "wb",
            "Storage/wb",
        ] {
            assert_eq!(
                parse(&["wb.adf", elsewhere]),
                Ok(mounted(MountRequest {
                    target: Some(String::from(elsewhere)),
                    ..image("wb.adf")
                })),
                "{elsewhere}"
            );
        }
    }

    #[test]
    fn every_option_parses() {
        assert_eq!(
            parse(&[
                "-rv",
                "-t",
                "affs",
                "-o",
                "loop,noexec,name=workbench",
                "wb.adf",
            ]),
            Ok(mounted(MountRequest {
                name: Some(String::from("workbench")),
                expects: only(VolumeFsType::Affs),
                access: ImageAccess::ReadOnly,
                verbose: true,
                ..image("wb.adf")
            }))
        );
        assert_eq!(
            parse(&["--types=iso9660", "--verbose", "--read-only", "disc.iso"]),
            Ok(mounted(MountRequest {
                expects: only(VolumeFsType::Iso9660),
                access: ImageAccess::ReadOnly,
                verbose: true,
                ..image("disc.iso")
            }))
        );
    }

    #[test]
    fn the_last_of_read_only_and_read_write_wins_and_w_never_falls_back() {
        let access = |args: &[&str]| match parse(args) {
            Ok(Command::Mount(request)) => request.access,
            other => panic!("{other:?}"),
        };
        assert_eq!(access(&["i"]), ImageAccess::WritableWherePossible);
        assert_eq!(
            access(&["-o", "rw", "i"]),
            ImageAccess::WritableWherePossible
        );
        assert_eq!(access(&["-r", "i"]), ImageAccess::ReadOnly);
        assert_eq!(access(&["-r", "-w", "i"]), ImageAccess::Writable);
        assert_eq!(access(&["-r", "--rw", "i"]), ImageAccess::Writable);
        assert_eq!(access(&["-w", "-o", "rw,ro", "i"]), ImageAccess::ReadOnly);
        assert_eq!(
            access(&["-o", "ro", "--read-write", "i"]),
            ImageAccess::Writable
        );
        assert_eq!(
            access(&["-w", "-o", "ro", "-o", "rw", "i"]),
            ImageAccess::Writable,
            "-w stays what it says once read-write wins again"
        );
        for taken_back in [
            &["-w", "-r", "-o", "rw", "i"][..],
            &["--rw", "--read-only", "-o", "rw", "i"],
        ] {
            assert_eq!(
                access(taken_back),
                ImageAccess::WritableWherePossible,
                "{taken_back:?}: a later -r takes -w back"
            );
        }
    }

    /// The set holding only `fstype`.
    fn only(fstype: VolumeFsType) -> Option<VolumeFsTypes> {
        VolumeFsTypes::of([fstype])
    }

    fn expects(list: &str) -> Option<VolumeFsTypes> {
        match parse(&["-t", list, "i"]) {
            Ok(Command::Mount(request)) => request.expects,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_generation_of_a_format_asks_for_its_family() {
        assert_eq!(expects("ext2"), only(VolumeFsType::Ext4));
        assert_eq!(expects("adfs"), only(VolumeFsType::Adfs));
        assert_eq!(expects("fat32"), only(VolumeFsType::Fat32));
        assert_eq!(expects("auto"), None, "whatever it holds");
    }

    /// util-linux's `mount -t udf,iso9660` mounts what the image holds of
    /// those, and `-t noudf` anything else; a type no image holds is passed
    /// over, and a list naming none is an unknown type.
    #[test]
    fn a_mount_takes_a_util_linux_type_list() {
        use VolumeFsType::{
            Adfs, Affs, ArcFs, Bzip2, Ext4, Fat32, Gzip, Iso9660, Rar, Spark, Tar, Udf, Zip, ARXFS,
        };
        assert_eq!(expects("udf,iso9660"), VolumeFsTypes::of([Udf, Iso9660]));
        // Every type a disk image or an archive holds but UDF.
        assert_eq!(
            expects("noudf"),
            VolumeFsTypes::of([
                ARXFS, Ext4, Fat32, Iso9660, Adfs, Affs, Zip, Tar, Gzip, Bzip2, Spark, ArcFs, Rar,
            ])
        );
        assert_eq!(
            expects("tar.gz,gzip"),
            only(Gzip),
            "a compressed tar is its stream's family"
        );
        assert_eq!(expects("rar"), only(Rar));
        assert_eq!(expects("udf,noiso9660"), only(Udf));
        assert_eq!(expects("vfat,iso9660"), only(Iso9660));
        for none in ["vfat,smb3", "ISO9660", "Udf,AFFS"] {
            assert_eq!(
                parse(&["-t", none, "i"]),
                Err(MountError::UnknownType {
                    image: String::from("i"),
                    name: String::from(none),
                }),
                "{none}: as spelled, as the kernel names a type"
            );
        }
        assert_eq!(
            parse(&["-t", "udf,,iso9660", "i"]),
            Err(MountError::BadOption)
        );
    }

    #[test]
    fn help_wins_immediately() {
        for help in [
            &["-h"][..],
            &["-?"],
            &["--help"],
            &["-r", "--help", "a", "b", "c"],
        ] {
            assert_eq!(parse(help), Ok(Command::Help), "{help:?}");
        }
    }

    #[test]
    fn options_with_nothing_to_mount_and_too_many_operands_are_usage() {
        assert_eq!(parse(&["-r"]), Err(MountError::Usage));
        assert_eq!(parse(&["-w", "-t", "affs"]), Err(MountError::Usage));
        assert_eq!(parse(&["-o", "ro"]), Err(MountError::Usage));
        assert_eq!(parse(&["a", "b", "c"]), Err(MountError::Usage));
    }

    #[test]
    fn two_different_names_are_usage_and_the_same_one_twice_is_not() {
        assert_eq!(
            parse(&["-o", "name=one", "i", "/Storage/two"]),
            Err(MountError::Usage)
        );
        assert_eq!(
            parse(&["-o", "name=one", "i", "/Storage/one"]),
            Ok(mounted(MountRequest {
                name: Some(String::from("one")),
                ..image("i")
            }))
        );
    }

    #[test]
    fn a_name_no_volume_may_take_is_refused() {
        for bad in ["Workbench", "-wb", "a/b", ""] {
            let asked = alloc::format!("name={bad}");
            assert_eq!(
                parse(&["-o", &asked, "i"]),
                Err(MountError::BadName(String::from(bad))),
                "{bad:?}"
            );
        }
        assert_eq!(
            parse(&["i", "/Storage/Work.bench"]),
            Err(MountError::BadName(String::from("Work.bench")))
        );
    }

    #[test]
    fn a_bad_option_value_is_bad_option() {
        for bad in ["", "weird", "ro,,noexec", "exec", "nosuid", "nodev", "name"] {
            assert_eq!(
                parse(&["-o", bad, "i"]),
                Err(MountError::BadOption),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_type_no_disk_image_holds_is_an_unknown_filesystem_type() {
        for bad in ["", "smb3", "vfat", "ISO9660"] {
            let unknown = Err(MountError::UnknownType {
                image: String::from("i"),
                name: String::from(bad),
            });
            assert_eq!(parse(&["-t", bad, "i"]), unknown, "{bad:?}");
            let long = alloc::format!("--types={bad}");
            assert_eq!(parse(&[&long, "i"]), unknown, "{bad:?}");
        }
        let unknown = parse(&["-t", "vfat", "stick.img"]).expect_err("refused");
        assert_eq!(
            unknown.to_string(),
            "stick.img: unknown filesystem type 'vfat'."
        );
        assert_eq!(unknown.status(), 32, "a mount that failed, as util-linux's");
        assert!(
            !unknown.misused(),
            "a sentence alone, with no hint to --help"
        );
        assert!(parse(&["-o", "weird", "i"]).expect_err("refused").misused());
    }

    /// GNU 9.4's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-x", "a"]), "invalid option -- 'x'");
        assert_eq!(refused(&["--frob", "a"]), "unrecognized option '--frob'");
        assert_eq!(refused(&["-t"]), "option requires an argument -- 't'");
        assert_eq!(refused(&["a", "-o"]), "option requires an argument -- 'o'");
        assert_eq!(
            refused(&["--types"]),
            "option '--types' requires an argument"
        );
        assert_eq!(
            refused(&["--read-only=1", "a"]),
            "option '--read-only' doesn't allow an argument"
        );
    }

    /// `getopt_long` reads `mount --read-o --ty=affs` as
    /// `mount --read-only --types=affs`, and `--r` as ambiguous.
    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(
            parse(&["--read-o", "--ty=affs", "i"]),
            Ok(mounted(MountRequest {
                expects: only(VolumeFsType::Affs),
                access: ImageAccess::ReadOnly,
                ..image("i")
            }))
        );
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
        assert!(refused(&["--r", "i"]).contains("ambiguous"));
    }

    /// `--list-held` lists only the volumes holding writes, by any
    /// unambiguous prefix, and takes no operand or mount option beside it.
    #[test]
    fn list_held_lists_only_the_volumes_holding_writes() {
        let held = Ok(Command::List(Listing {
            types: TypeFilter::default(),
            held: true,
        }));
        assert_eq!(parse(&["--list-held"]), held);
        assert_eq!(parse(&["--list", "-v"]), held, "by any unambiguous prefix");
        for usage in [
            &["--list-held", "i"][..],
            &["--list-held", "-r"],
            &["--list-held", "--replay-held", "usb1"],
        ] {
            assert_eq!(parse(usage), Err(MountError::Usage), "{usage:?}");
        }
    }

    /// `--replay-held` and `--discard-held` answer for one named volume, by
    /// any unambiguous prefix, and are refused beside a second answer, an
    /// operand, a mount option, a type list, or with an empty name.
    #[test]
    fn a_held_answer_names_one_volume_and_nothing_else() {
        let answered =
            |name: &str, answer: ReplayAnswer, verbose: bool| -> Result<Command, MountError> {
                Ok(Command::Answer(HeldAnswer {
                    name: String::from(name),
                    answer,
                    verbose,
                }))
            };
        assert_eq!(
            parse(&["--replay-held", "usb1"]),
            answered("usb1", ReplayAnswer::Write, false)
        );
        assert_eq!(
            parse(&["--discard-held=/Storage/usb1", "-v"]),
            answered("/Storage/usb1", ReplayAnswer::Discard, true)
        );
        assert_eq!(
            parse(&["--rep", "usb1"]),
            answered("usb1", ReplayAnswer::Write, false)
        );
        assert_eq!(
            parse(&["--d", "usb1"]),
            answered("usb1", ReplayAnswer::Discard, false)
        );
        for usage in [
            &["--replay-held", "usb1", "--discard-held", "usb1"][..],
            &["--replay-held", "a", "--replay-held", "b"],
            &["--replay-held", "usb1", "image.adf"],
            &["--replay-held", "usb1", "-r"],
            &["--discard-held", "usb1", "-t", "fat32"],
            &["--discard-held", ""],
        ] {
            assert_eq!(parse(usage), Err(MountError::Usage), "{usage:?}");
        }
        assert_eq!(
            refused(&["--replay-held"]),
            "option '--replay-held' requires an argument"
        );
        assert!(refused(&["--re", "usb1"]).contains("ambiguous"));
    }

    #[test]
    fn double_dash_protects_a_dash_named_operand() {
        assert_eq!(
            parse(&["--", "-weird.adf"]),
            Ok(mounted(image("-weird.adf")))
        );
    }
}
