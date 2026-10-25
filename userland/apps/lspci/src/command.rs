//! The `lspci` command shape and its parser.
//!
//! The option surface follows `pciutils` `lspci`: `-n` lists numeric ids,
//! `-nn` lists names *and* ids, `-v` adds each function's declared
//! resources, `-t` renders the bus topology as a tree, and `-d
//! [<vendor>]:[<device>]` / `-s <node>` filter the listing. `-?` and
//! `--help` answer with the bundle's own short help. `lspci` takes no
//! positional operands; one is a usage error, never silently ignored.
//!
//! TAIRiX's hardware model has no PCI bus/device/function address, so the
//! listing numbers each device with a small, stable sequence number
//! assigned in bus order (shown as `#<n>`) rather than the opaque
//! hardware-tree node id — which the bootstrap-floor probes and the
//! boot-display shim mint from a high reserved id space, producing huge
//! meaningless values. The `-s` selector names that displayed number (the
//! same deliberate divergence `lsusb`'s bus/device numbers make; the Help
//! document states it).

use alloc::string::{String, ToString};
use core::fmt;
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote;

/// What a parsed argument vector asks `lspci` to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// `-?` / `--help`: render the bundle's own short help and exit
    /// successfully. Wins over every other option, as in the GNU tools.
    Help,
    /// List the discovered PCI functions with the given options.
    List(Options),
}

/// How device identities are rendered.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NameMode {
    /// Human-readable names, the numeric form only where the database has
    /// no entry (the default).
    #[default]
    Names,
    /// `-n`: numeric ids only.
    Numeric,
    /// `-nn`: names followed by the numeric ids in brackets.
    Both,
}

/// A `-d [<vendor>]:[<device>]` filter: an omitted half is a wildcard,
/// exactly as in `pciutils`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceFilter {
    /// Match only this vendor id, when present.
    pub vendor: Option<u16>,
    /// Match only this device id, when present.
    pub device: Option<u16>,
}

/// The parsed listing options.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Options {
    /// How identities are rendered (`-n` / `-nn`).
    pub names: NameMode,
    /// `-v`: append each function's declared resources.
    pub verbose: bool,
    /// `-t`: render the bus topology as a tree.
    pub tree: bool,
    /// `-d`: keep only functions matching the vendor/device filter.
    pub device: Option<DeviceFilter>,
    /// `-s`: keep only the function with this displayed listing number
    /// (the small, bus-order `#<n>` the listing shows, not a node id).
    pub slot: Option<u32>,
}

/// Why an argument vector did not parse.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// A `-d` value that is not `[<vendor>]:[<device>]` in hex.
    BadDeviceFilter(String),
    /// A `-s` value that is not a decimal listing number.
    BadSlot(String),
    /// A positional operand; `lspci` takes none.
    UnexpectedOperand(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::BadDeviceFilter(value) => {
                write!(
                    f,
                    "-d expects [<vendor>]:[<device>] in hex, got {value}",
                    value = quote(value)
                )
            }
            Self::BadSlot(value) => {
                write!(
                    f,
                    "-s expects a decimal listing number, got {value}",
                    value = quote(value)
                )
            }
            Self::UnexpectedOperand(arg) => write!(f, "unexpected operand {arg}", arg = quote(arg)),
        }
    }
}

/// What each of `lspci`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Numeric,
    Verbose,
    Tree,
    Device,
    Slot,
    Help,
}

/// `lspci`'s options, as `pciutils` spells them, and the reserved help
/// switches.
const OPTIONS: &[Opt<Flag>] = &[
    Opt::letter('n', Takes::Nothing, Flag::Numeric),
    Opt::letter('v', Takes::Nothing, Flag::Verbose),
    Opt::letter('t', Takes::Nothing, Flag::Tree),
    Opt::letter('d', Takes::Value, Flag::Device),
    Opt::letter('s', Takes::Value, Flag::Slot),
    Opt::flag('?', &["help"], Flag::Help),
];

/// Parse an argument vector (without the program name).
///
/// Read as GNU's `getopt_long` reads it ([`Scan`]): options clustered, `-d`'s
/// and `-s`'s value attached or the next argument, a long option by any
/// unambiguous prefix, `--` ending them. The help switches (`-?`, `--help`)
/// answer at once. A second `n` upgrades `-n` to `-nn`, exactly as `pciutils`
/// treats `-nn`.
///
/// # Errors
///
/// * [`ParseError::Option`] — an option `getopt` refuses, in its words.
/// * [`ParseError::BadDeviceFilter`] — a `-d` value that is not
///   `[<vendor>]:[<device>]` in hex.
/// * [`ParseError::BadSlot`] — a `-s` value that is not a decimal listing
///   number.
/// * [`ParseError::UnexpectedOperand`] — any operand, before or after `--`:
///   `lspci` takes none.
///
/// The caller reports each, then where the help is, and exits `1`.
pub fn parse(args: &[&str]) -> Result<Command, ParseError> {
    let mut options = Options::default();
    for arg in Scan::new(OPTIONS, args, Order::Permute) {
        match arg.map_err(ParseError::Option)? {
            Arg::Operand(word) => return Err(ParseError::UnexpectedOperand(word.to_string())),
            Arg::Opt(Flag::Help, _) => return Ok(Command::Help),
            Arg::Opt(flag, value) => options.set(flag, value.unwrap_or_default())?,
        }
    }
    Ok(Command::List(options))
}

impl Options {
    /// Apply the option `flag` names, with the value it took.
    fn set(&mut self, flag: Flag, value: &str) -> Result<(), ParseError> {
        match flag {
            Flag::Numeric => {
                self.names = match self.names {
                    NameMode::Names => NameMode::Numeric,
                    NameMode::Numeric | NameMode::Both => NameMode::Both,
                };
            }
            Flag::Verbose => self.verbose = true,
            Flag::Tree => self.tree = true,
            Flag::Device => self.device = Some(parse_device_filter(value)?),
            Flag::Slot => {
                self.slot = Some(
                    value
                        .parse::<u32>()
                        .map_err(|_| ParseError::BadSlot(value.to_string()))?,
                );
            }
            Flag::Help => {}
        }
        Ok(())
    }
}

/// Parse a `-d` value: `[<vendor>]:[<device>]`, each half up to four hex
/// digits, an empty half a wildcard. Anything else fails closed.
fn parse_device_filter(value: &str) -> Result<DeviceFilter, ParseError> {
    let bad = || ParseError::BadDeviceFilter(value.to_string());
    let (vendor, device) = value.split_once(':').ok_or_else(bad)?;
    let half = |text: &str| -> Result<Option<u16>, ParseError> {
        if text.is_empty() {
            return Ok(None);
        }
        if text.len() > 4 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad());
        }
        u16::from_str_radix(text, 16).map(Some).map_err(|_| bad())
    };
    Ok(DeviceFilter {
        vendor: half(vendor)?,
        device: half(device)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GNU's words for each refusal.
    fn refused(args: &[&str]) -> String {
        parse(args).expect_err("refused").to_string()
    }

    #[test]
    fn a_refused_option_is_worded_as_getopt_words_it() {
        assert_eq!(refused(&["-Q"]), "invalid option -- 'Q'");
        assert_eq!(refused(&["-vQ"]), "invalid option -- 'Q'");
        assert_eq!(refused(&["--frob"]), "unrecognized option '--frob'");
        assert_eq!(refused(&["-d"]), "option requires an argument -- 'd'");
        assert_eq!(
            refused(&["--help=1"]),
            "option '--help' doesn't allow an argument"
        );
    }

    #[test]
    fn a_long_option_may_be_abbreviated() {
        assert_eq!(parse(&["--he"]), Ok(Command::Help));
        assert_eq!(parse(&["-v", "--h"]), Ok(Command::Help));
    }

    fn list(args: &[&str]) -> Options {
        match parse(args).expect("parses") {
            Command::List(options) => options,
            Command::Help => panic!("expected a listing"),
        }
    }

    #[test]
    fn defaults_render_names_only() {
        let options = list(&[]);
        assert_eq!(options, Options::default());
        assert_eq!(options.names, NameMode::Names);
    }

    #[test]
    fn help_switches_win() {
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["-v", "-?"]), Ok(Command::Help));
    }

    #[test]
    fn n_and_nn_select_the_pciutils_modes() {
        assert_eq!(list(&["-n"]).names, NameMode::Numeric);
        assert_eq!(list(&["-nn"]).names, NameMode::Both);
        // Split across arguments, as `pciutils` also accepts.
        assert_eq!(list(&["-n", "-n"]).names, NameMode::Both);
    }

    #[test]
    fn flags_cluster_and_values_attach() {
        let options = list(&["-vt", "-d8086:10d3", "-s", "7"]);
        assert!(options.verbose);
        assert!(options.tree);
        assert_eq!(
            options.device,
            Some(DeviceFilter {
                vendor: Some(0x8086),
                device: Some(0x10d3),
            })
        );
        assert_eq!(options.slot, Some(7));
    }

    #[test]
    fn device_filter_halves_are_wildcards() {
        assert_eq!(
            list(&["-d", "8086:"]).device,
            Some(DeviceFilter {
                vendor: Some(0x8086),
                device: None,
            })
        );
        assert_eq!(
            list(&["-d", ":10d3"]).device,
            Some(DeviceFilter {
                vendor: None,
                device: Some(0x10d3),
            })
        );
    }

    #[test]
    fn malformed_values_fail_closed() {
        assert!(matches!(
            parse(&["-d", "8086"]),
            Err(ParseError::BadDeviceFilter(_))
        ));
        assert!(matches!(
            parse(&["-d", "80867:1"]),
            Err(ParseError::BadDeviceFilter(_))
        ));
        assert!(matches!(
            parse(&["-d", "80g6:1"]),
            Err(ParseError::BadDeviceFilter(_))
        ));
        assert!(matches!(parse(&["-s", "0x7"]), Err(ParseError::BadSlot(_))));
        assert_eq!(
            parse(&["-s"]),
            Err(ParseError::Option(Refusal::MissingShortValue('s')))
        );
        assert_eq!(
            parse(&["-z"]),
            Err(ParseError::Option(Refusal::UnknownShort('z')))
        );
        assert!(matches!(
            parse(&["--verbose"]),
            Err(ParseError::Option(Refusal::UnknownLong(_)))
        ));
    }

    #[test]
    fn operands_are_refused() {
        assert!(matches!(
            parse(&["bus0"]),
            Err(ParseError::UnexpectedOperand(_))
        ));
        assert!(matches!(
            parse(&["--", "-v"]),
            Err(ParseError::UnexpectedOperand(_))
        ));
    }
}
