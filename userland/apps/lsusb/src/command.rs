//! The `lsusb` command shape and its parser.
//!
//! The option surface follows `usbutils` `lsusb`: `-v` adds each listed
//! interface's class/subclass/protocol names, `-t` renders the bus
//! topology as a tree, and `-d [<vendor>]:[<product>]` /
//! `-s [[<bus>]:][<devnum>]` filter the listing. `-?` and `--help` answer
//! with the bundle's own short help. `lsusb` takes no positional
//! operands; one is a usage error, never silently ignored.
//!
//! TAIRiX has no Linux bus/devnum registry — a discovered USB interface
//! is a hardware-tree node with a stable node id under its controller's
//! node — so `-s` selects those node ids: the bus half names the
//! controller (parent) node id and the devnum half the interface node id
//! (the same deliberate divergence `lspci`'s `-s` makes; the Help
//! document states it).

use alloc::string::{String, ToString};
use core::fmt;
use tairix_util::getopt::{Arg, Opt, Order, Refusal, Scan, Takes};
use tairix_util::quote::quote;

/// What a parsed argument vector asks `lsusb` to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// `-?` / `--help`: render the bundle's own short help and exit
    /// successfully. Wins over every other option, as in the GNU tools.
    Help,
    /// List the discovered USB devices with the given options.
    List(Options),
}

/// A `-d [<vendor>]:[<product>]` filter: an omitted half is a wildcard,
/// exactly as in `usbutils`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceFilter {
    /// Match only this vendor id, when present.
    pub vendor: Option<u16>,
    /// Match only this product id, when present.
    pub product: Option<u16>,
}

/// A `-s [[<bus>]:][<devnum>]` filter: `usbutils` grammar, an omitted
/// half a wildcard. Without a colon the value is a device number alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SlotFilter {
    /// Match only devices under this controller (bus) node id, when
    /// present.
    pub bus: Option<u32>,
    /// Match only the device with this interface node id, when present.
    pub device: Option<u32>,
}

/// The parsed listing options.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Options {
    /// `-v`: append each interface's class/subclass/protocol names.
    pub verbose: bool,
    /// `-t`: render the bus topology as a tree.
    pub tree: bool,
    /// `-d`: keep only devices matching the vendor/product filter.
    pub device: Option<DeviceFilter>,
    /// `-s`: keep only devices matching the bus/devnum filter.
    pub slot: Option<SlotFilter>,
}

/// Why an argument vector did not parse.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ParseError {
    /// An option `getopt` refuses, worded as it words it.
    Option(Refusal),
    /// A `-d` value that is not `[<vendor>]:[<product>]` in hex.
    BadDeviceFilter(String),
    /// A `-s` value that is not `[[<bus>]:][<devnum>]` in decimal
    /// hardware-tree node ids.
    BadSlot(String),
    /// A positional operand; `lsusb` takes none.
    UnexpectedOperand(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Option(refusal) => write!(f, "{refusal}"),
            Self::BadDeviceFilter(value) => {
                write!(
                    f,
                    "-d expects [<vendor>]:[<product>] in hex, got {value}",
                    value = quote(value)
                )
            }
            Self::BadSlot(value) => {
                write!(
                    f,
                    "-s expects [[<bus>]:][<devnum>] as decimal hardware-tree node ids, got {value}", value = quote(value)
                )
            }
            Self::UnexpectedOperand(arg) => write!(f, "unexpected operand {arg}", arg = quote(arg)),
        }
    }
}

/// What each of `lsusb`'s options means.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flag {
    Verbose,
    Tree,
    Device,
    Slot,
    Help,
}

/// `lsusb`'s options, as `usbutils` spells them, and the reserved help
/// switches.
const OPTIONS: &[Opt<Flag>] = &[
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
/// answer at once.
///
/// # Errors
///
/// * [`ParseError::Option`] — an option `getopt` refuses, in its words.
/// * [`ParseError::BadDeviceFilter`] — a `-d` value that is not
///   `[<vendor>]:[<product>]` in hex.
/// * [`ParseError::BadSlot`] — a `-s` value that is not `[[<bus>]:][<devnum>]`
///   in decimal hardware-tree node ids.
/// * [`ParseError::UnexpectedOperand`] — any operand, before or after `--`:
///   `lsusb` takes none.
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
            Flag::Verbose => self.verbose = true,
            Flag::Tree => self.tree = true,
            Flag::Device => self.device = Some(parse_device_filter(value)?),
            Flag::Slot => self.slot = Some(parse_slot_filter(value)?),
            Flag::Help => {}
        }
        Ok(())
    }
}

/// Parse a `-d` value: `[<vendor>]:[<product>]`, each half up to four hex
/// digits, an empty half a wildcard. Anything else fails closed.
fn parse_device_filter(value: &str) -> Result<DeviceFilter, ParseError> {
    let bad = || ParseError::BadDeviceFilter(value.to_string());
    let (vendor, product) = value.split_once(':').ok_or_else(bad)?;
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
        product: half(product)?,
    })
}

/// Parse a `-s` value: `[[<bus>]:][<devnum>]` in decimal node ids, an
/// omitted half a wildcard. A value with no colon is a device number
/// alone, as in `usbutils`. A fully empty value fails closed.
fn parse_slot_filter(value: &str) -> Result<SlotFilter, ParseError> {
    let bad = || ParseError::BadSlot(value.to_string());
    let half = |text: &str| -> Result<Option<u32>, ParseError> {
        if text.is_empty() {
            return Ok(None);
        }
        text.parse::<u32>().map(Some).map_err(|_| bad())
    };
    let (bus, device) = match value.split_once(':') {
        Some((bus, device)) => (half(bus)?, half(device)?),
        None => (None, half(value)?),
    };
    if bus.is_none() && device.is_none() {
        return Err(bad());
    }
    Ok(SlotFilter { bus, device })
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
    fn defaults_list_everything() {
        let options = list(&[]);
        assert_eq!(options, Options::default());
        assert!(!options.verbose);
        assert!(!options.tree);
    }

    #[test]
    fn help_switches_win() {
        assert_eq!(parse(&["--help"]), Ok(Command::Help));
        assert_eq!(parse(&["-?"]), Ok(Command::Help));
        assert_eq!(parse(&["-v", "-?"]), Ok(Command::Help));
    }

    #[test]
    fn flags_cluster_and_values_attach() {
        let options = list(&["-vt", "-d046d:c534", "-s", "2:7"]);
        assert!(options.verbose);
        assert!(options.tree);
        assert_eq!(
            options.device,
            Some(DeviceFilter {
                vendor: Some(0x046d),
                product: Some(0xc534),
            })
        );
        assert_eq!(
            options.slot,
            Some(SlotFilter {
                bus: Some(2),
                device: Some(7),
            })
        );
    }

    #[test]
    fn device_filter_halves_are_wildcards() {
        assert_eq!(
            list(&["-d", "046d:"]).device,
            Some(DeviceFilter {
                vendor: Some(0x046d),
                product: None,
            })
        );
        assert_eq!(
            list(&["-d", ":c534"]).device,
            Some(DeviceFilter {
                vendor: None,
                product: Some(0xc534),
            })
        );
    }

    #[test]
    fn slot_filter_speaks_the_usbutils_grammar() {
        // A bare value is a device number.
        assert_eq!(
            list(&["-s", "7"]).slot,
            Some(SlotFilter {
                bus: None,
                device: Some(7),
            })
        );
        // `bus:` selects a whole bus.
        assert_eq!(
            list(&["-s", "2:"]).slot,
            Some(SlotFilter {
                bus: Some(2),
                device: None,
            })
        );
        // `:devnum` is a device number with an explicit wildcard bus.
        assert_eq!(
            list(&["-s", ":7"]).slot,
            Some(SlotFilter {
                bus: None,
                device: Some(7),
            })
        );
    }

    #[test]
    fn malformed_values_fail_closed() {
        assert!(matches!(
            parse(&["-d", "046d"]),
            Err(ParseError::BadDeviceFilter(_))
        ));
        assert!(matches!(
            parse(&["-d", "046d7:1"]),
            Err(ParseError::BadDeviceFilter(_))
        ));
        assert!(matches!(
            parse(&["-d", "04g6:1"]),
            Err(ParseError::BadDeviceFilter(_))
        ));
        assert!(matches!(parse(&["-s", "0x7"]), Err(ParseError::BadSlot(_))));
        assert!(matches!(parse(&["-s", ""]), Err(ParseError::BadSlot(_))));
        assert!(matches!(parse(&["-s", ":"]), Err(ParseError::BadSlot(_))));
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
