//! SWI names: the kernel's own, and the forms `OS_SWINumberToString` builds
//! and `OS_SWINumberFromString` reads, as RISC OS 5 has them.
//!
//! A name is `OS_` and the SWI's name below 512; past that, a module's
//! prefix, `_` and the name its decoding table gives the offset, or the
//! offset in decimal where the table has none. `X` before either marks the
//! form that returns its errors. Names match exactly, case included, and
//! end at a space or a control character.

use alloc::vec::Vec;

use tairix_obey::number::{read_unsigned, Reading};

use super::{CHUNK, WRITE_I, X};

/// The OS SWIs' names, from 0.
pub const OS: [&str; 128] = [
    "WriteC",
    "WriteS",
    "Write0",
    "NewLine",
    "ReadC",
    "CLI",
    "Byte",
    "Word",
    "File",
    "Args",
    "BGet",
    "BPut",
    "GBPB",
    "Find",
    "ReadLine",
    "Control",
    "GetEnv",
    "Exit",
    "SetEnv",
    "IntOn",
    "IntOff",
    "CallBack",
    "EnterOS",
    "BreakPt",
    "BreakCtrl",
    "UnusedSWI",
    "UpdateMEMC",
    "SetCallBack",
    "Mouse",
    "Heap",
    "Module",
    "Claim",
    "Release",
    "ReadUnsigned",
    "GenerateEvent",
    "ReadVarVal",
    "SetVarVal",
    "GSInit",
    "GSRead",
    "GSTrans",
    "BinaryToDecimal",
    "FSControl",
    "ChangeDynamicArea",
    "GenerateError",
    "ReadEscapeState",
    "EvaluateExpression",
    "SpriteOp",
    "ReadPalette",
    "ServiceCall",
    "ReadVduVariables",
    "ReadPoint",
    "UpCall",
    "CallAVector",
    "ReadModeVariable",
    "RemoveCursors",
    "RestoreCursors",
    "SWINumberToString",
    "SWINumberFromString",
    "ValidateAddress",
    "CallAfter",
    "CallEvery",
    "RemoveTickerEvent",
    "InstallKeyHandler",
    "CheckModeValid",
    "ChangeEnvironment",
    "ClaimScreenMemory",
    "ReadMonotonicTime",
    "SubstituteArgs",
    "PrettyPrint",
    "Plot",
    "WriteN",
    "AddToVector",
    "WriteEnv",
    "ReadArgs",
    "ReadRAMFsLimits",
    "ClaimDeviceVector",
    "ReleaseDeviceVector",
    "DelinkApplication",
    "RelinkApplication",
    "HeapSort",
    "ExitAndDie",
    "ReadMemMapInfo",
    "ReadMemMapEntries",
    "SetMemMapEntries",
    "AddCallBack",
    "ReadDefaultHandler",
    "SetECFOrigin",
    "SerialOp",
    "ReadSysInfo",
    "Confirm",
    "ChangedBox",
    "CRC",
    "ReadDynamicArea",
    "PrintChar",
    "ChangeRedirection",
    "RemoveCallBack",
    "FindMemMapEntries",
    "SetColour",
    "ClaimSWI",
    "ReleaseSWI",
    "Pointer",
    "ScreenMode",
    "DynamicArea",
    "AbortTrap",
    "Memory",
    "ClaimProcessorVector",
    "Reset",
    "MMUControl",
    "ResyncTime",
    "PlatformFeatures",
    "SynchroniseCodeAreas",
    "CallASWI",
    "AMBControl",
    "CallASWIR12",
    "SpecialControl",
    "EnterUSR32",
    "EnterUSR26",
    "VIDCDivider",
    "NVMemory",
    "ClaimOSSWI",
    "TaskControl",
    "DeviceDriver",
    "Hardware",
    "IICOp",
    "LeaveOS",
    "ReadLine32",
    "SubstituteArgs32",
    "HeapSort32",
];

/// `OS_ConvertStandardDateAndTime`, the first of the conversions' SWIs.
pub const CONVERT_STANDARD_DATE_AND_TIME: u32 = 0xC0;
/// `OS_ConvertDateAndTime`.
pub const CONVERT_DATE_AND_TIME: u32 = 0xC1;
/// `OS_ConvertHex1`, the first of the number conversions.
pub const CONVERT_HEX1: u32 = 0xD0;

/// The number conversions' names, from [`CONVERT_HEX1`].
pub const CONVERSIONS: [&str; 30] = [
    "ConvertHex1",
    "ConvertHex2",
    "ConvertHex4",
    "ConvertHex6",
    "ConvertHex8",
    "ConvertCardinal1",
    "ConvertCardinal2",
    "ConvertCardinal3",
    "ConvertCardinal4",
    "ConvertInteger1",
    "ConvertInteger2",
    "ConvertInteger3",
    "ConvertInteger4",
    "ConvertBinary1",
    "ConvertBinary2",
    "ConvertBinary3",
    "ConvertBinary4",
    "ConvertSpacedCardinal1",
    "ConvertSpacedCardinal2",
    "ConvertSpacedCardinal3",
    "ConvertSpacedCardinal4",
    "ConvertSpacedInteger1",
    "ConvertSpacedInteger2",
    "ConvertSpacedInteger3",
    "ConvertSpacedInteger4",
    "ConvertFixedNetStation",
    "ConvertNetStation",
    "ConvertFixedFileSize",
    "ConvertFileSize",
    "ConvertVariform",
];

/// The OS name of SWI `number` (below 512, X clear), after `OS_`: its own,
/// or `Undefined` for a number the kernel gives none.
#[must_use]
pub fn os_name(number: u32) -> &'static str {
    let index = |base: u32| (number.wrapping_sub(base)) as usize;
    if let Some(name) = OS.get(number as usize) {
        return name;
    }
    if let Some(name) = CONVERSIONS.get(index(CONVERT_HEX1)) {
        return name;
    }
    match number {
        CONVERT_STANDARD_DATE_AND_TIME => "ConvertStandardDateAndTime",
        CONVERT_DATE_AND_TIME => "ConvertDateAndTime",
        _ => "Undefined",
    }
}

/// How a module names the SWIs of its chunk.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Naming<'a> {
    /// A decoding table as a module holds one: the prefix, then the names
    /// from offset 0, each terminated by a zero, an empty name ending them.
    Table(&'a [u8]),
    /// No table: the module's title, which names an offset as a number.
    Title(&'a [u8]),
}

/// A decoding table's prefix, and its names in offset order.
fn split_table(table: &[u8]) -> (&[u8], impl Iterator<Item = &[u8]>) {
    let mut parts = table.split(|&b| b == 0);
    let prefix = parts.next().unwrap_or_default();
    (prefix, parts.take_while(|name| !name.is_empty()))
}

/// The name of SWI `number` as `OS_SWINumberToString` builds it, without
/// its terminator; `naming` is how the module providing its chunk names
/// it, if one does.
#[must_use]
pub fn to_string(number: u32, naming: Option<Naming<'_>>) -> Vec<u8> {
    let mut text = Vec::new();
    let mut put = |bytes: &[u8]| {
        if text.try_reserve(bytes.len()).is_ok() {
            text.extend_from_slice(bytes);
        }
    };
    if number & X != 0 {
        put(b"X");
    }
    let base = number & !X;
    if base < 2 * WRITE_I {
        put(b"OS_");
        if base >= WRITE_I {
            put(b"WriteI+");
            let c = (base & 0xFF) as u8;
            if (b' '..0x7F).contains(&c) {
                put(&[b'"', c, b'"']);
            } else {
                put(crate::convert::Decimal::of(i32::from(c)).as_bytes());
            }
        } else {
            put(os_name(base).as_bytes());
        }
        return text;
    }
    let offset = base & (CHUNK - 1);
    match naming {
        None => put(b"User"),
        Some(Naming::Table(table)) => {
            let (prefix, mut names) = split_table(table);
            put(prefix);
            put(b"_");
            match names.nth(offset as usize) {
                Some(name) => put(name),
                None => put(decimal(offset).as_bytes()),
            }
        }
        Some(Naming::Title(title)) => {
            put(title);
            put(b"_");
            put(decimal(offset).as_bytes());
        }
    }
    text
}

fn decimal(n: u32) -> crate::convert::Decimal {
    crate::convert::Decimal::of(i32::try_from(n).unwrap_or(i32::MAX))
}

/// The bytes of a SWI's name as given: to its first space or control
/// character.
#[must_use]
pub fn name_of(text: &[u8]) -> &[u8] {
    &text[..text.iter().position(|&b| b <= b' ').unwrap_or(text.len())]
}

/// The kernel's SWI `name` names, with its `X` bit; none if no OS SWI is
/// called that. Only exact names match: OS SWIs are never numbered.
#[must_use]
pub fn os_from_string(name: &[u8]) -> Option<u32> {
    let (x, name) = match name.strip_prefix(b"X") {
        Some(rest) => (X, rest),
        None => (0, name),
    };
    let rest = name.strip_prefix(b"OS_")?;
    let number = if rest == b"WriteI" {
        WRITE_I
    } else if let Some(n) = index_of(&OS, rest) {
        n
    } else if rest == b"ConvertStandardDateAndTime" {
        CONVERT_STANDARD_DATE_AND_TIME
    } else if rest == b"ConvertDateAndTime" {
        CONVERT_DATE_AND_TIME
    } else {
        CONVERT_HEX1 + index_of(&CONVERSIONS, rest)?
    };
    Some(number | x)
}

/// The offset `name` (without its `X`) gives in a chunk named `naming`:
/// `prefix_name` or `prefix_number` for a table, `title_number` for a
/// title.
#[must_use]
pub fn offset_from_string(name: &[u8], naming: Naming<'_>) -> Option<u32> {
    let (prefix, found) = match naming {
        Naming::Table(table) => {
            let (prefix, mut names) = split_table(table);
            let rest = name
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix(b"_"));
            (prefix, rest.and_then(|rest| names.position(|n| n == rest)))
        }
        Naming::Title(title) => (title, None),
    };
    let rest = name.strip_prefix(prefix)?.strip_prefix(b"_")?;
    if let Some(n) = found {
        return u32::try_from(n).ok();
    }
    let reading = Reading {
        base: 10,
        limit: u64::from(CHUNK - 1),
        wide: false,
        terminated: false,
    };
    read_unsigned(rest, reading)
        .ok()
        .and_then(|(n, _)| u32::try_from(n).ok())
}

/// Where `name` is in `names`.
fn index_of(names: &[&str], name: &[u8]) -> Option<u32> {
    (0..)
        .zip(names)
        .find_map(|(n, candidate)| (candidate.as_bytes() == name).then_some(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_numbers_name_themselves_and_their_gaps() {
        assert_eq!(to_string(0x00, None), b"OS_WriteC");
        assert_eq!(to_string(0x2_0006, None), b"XOS_Byte");
        assert_eq!(to_string(0x7F, None), b"OS_HeapSort32");
        assert_eq!(to_string(0x80, None), b"OS_Undefined");
        assert_eq!(to_string(0xC1, None), b"OS_ConvertDateAndTime");
        assert_eq!(to_string(0xED, None), b"OS_ConvertVariform");
        assert_eq!(to_string(0xEE, None), b"OS_Undefined");
        assert_eq!(to_string(0x141, None), b"OS_WriteI+\"A\"");
        assert_eq!(to_string(0x10A, None), b"OS_WriteI+10");
        assert_eq!(to_string(0x17F, None), b"OS_WriteI+127");
        assert_eq!(to_string(0x4_0000, None), b"User");
    }

    #[test]
    fn module_numbers_are_named_by_table_or_title() {
        let table = Naming::Table(b"Wimp\0Initialise\0CreateWindow\0\0");
        assert_eq!(to_string(0x400C1, Some(table)), b"Wimp_CreateWindow");
        assert_eq!(to_string(0x600C5, Some(table)), b"XWimp_5");
        assert_eq!(
            to_string(0x400C7, Some(Naming::Title(b"Thing"))),
            b"Thing_7"
        );
    }

    #[test]
    fn names_read_back_to_their_numbers() {
        assert_eq!(os_from_string(b"OS_WriteC"), Some(0));
        assert_eq!(os_from_string(b"XOS_HeapSort32"), Some(0x2_007F));
        assert_eq!(os_from_string(b"OS_WriteI"), Some(0x100));
        assert_eq!(os_from_string(b"OS_ConvertHex8"), Some(0xD4));
        assert_eq!(os_from_string(b"OS_ConvertStandardDateAndTime"), Some(0xC0));
        assert_eq!(os_from_string(b"OS_5"), None, "OS SWIs are never numbered");
        assert_eq!(os_from_string(b"os_WriteC"), None);
        assert_eq!(name_of(b"OS_Byte  rest"), b"OS_Byte");
        let table = Naming::Table(b"Wimp\0Initialise\0CreateWindow\0\0");
        assert_eq!(offset_from_string(b"Wimp_CreateWindow", table), Some(1));
        assert_eq!(offset_from_string(b"Wimp_12", table), Some(12));
        assert_eq!(offset_from_string(b"Wimp_64", table), None);
        assert_eq!(offset_from_string(b"Wimp", table), None);
        assert_eq!(
            offset_from_string(b"Thing_3", Naming::Title(b"Thing")),
            Some(3)
        );
        assert_eq!(
            offset_from_string(b"Thing_Name", Naming::Title(b"Thing")),
            None
        );
    }
}
