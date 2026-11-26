//! MimeMap held to the interface RISC OS 5's was recorded keeping, answered
//! from TAIRiX's registry: the four ways of naming a type, each read as RISC
//! OS 5 reads it, a string answer zero-terminated at R3 and a number in R3,
//! the refusals' numbers, and `*MimeMap`'s one mapping or all of them.

mod common;

use common::{call, command, info, load, ram_disc, read, text};
use tairix_riscos_kernel::os::{Fail, Kernel, Os};
use tairix_riscos_kernel::swi::mime_map::TRANSLATE;

/// MimeMap's refusals: a form it does not know, and nothing mapped.
const UNKNOWN_FORM: u32 = 0xB0_0001;
const NO_MAPPING: u32 = 0xB0_0002;

const TYPE: u32 = 0;
const NAME: u32 = 1;
const MIME: u32 = 2;
const EXTENSION: u32 = 3;

/// `MimeMap_Translate` of `value` in form `from`, answered in form `to` in a
/// buffer of 64 bytes: R3, and the buffer up to its first zero.
fn translate(os: &mut Os<'_>, from: u32, value: u32, to: u32) -> Result<(u32, Vec<u8>), u32> {
    let buffer = os.rma_claim(64).expect("room");
    os.write_bytes(buffer, &[0xEE; 64]).expect("written");
    let regs = call(os, TRANSLATE, &[from, value, to, buffer]).map_err(|(number, _)| number)?;
    assert_eq!(regs.r[..3], [from, value, to], "R0 to R2 kept");
    let answer = read(os, buffer, 64);
    let end = answer.iter().position(|&b| b == 0).unwrap_or(answer.len());
    Ok((regs.r[3], answer[..end].to_vec()))
}

/// The string `bytes` named as `from` names it, translated into `to`.
fn translated(os: &mut Os<'_>, from: u32, bytes: &[u8], to: u32) -> Result<Vec<u8>, u32> {
    let at = common::block(os, bytes);
    translate(os, from, at, to).map(|(_, answer)| answer)
}

#[test]
fn a_type_number_translates_into_every_form() {
    let mut kernel = common::kernel();
    let mut os = kernel.os();
    let (r3, _) = translate(&mut os, TYPE, 0xFFF, TYPE).expect("mapped");
    assert_eq!(r3, 0xFFF);
    for (to, answer) in [
        (NAME, &b"Text"[..]),
        (MIME, b"text/plain"),
        (EXTENSION, b"txt"),
    ] {
        assert_eq!(
            translate(&mut os, TYPE, 0xFFF, to).expect("mapped").1,
            answer
        );
    }
    // A type with no name of its own is named by its number.
    assert_eq!(
        translate(&mut os, TYPE, 0xA91, NAME).expect("mapped").1,
        b"&A91"
    );
    assert_eq!(
        translate(&mut os, TYPE, 0xA91, MIME).expect("mapped").1,
        b"application/zip"
    );
    // A number past a type's twelve bits, or one the registry has nothing
    // for, maps nothing.
    for unmapped in [0x1000, u32::MAX, 0x123] {
        assert_eq!(translate(&mut os, TYPE, unmapped, MIME), Err(NO_MAPPING));
    }
}

#[test]
fn a_mime_type_ends_as_a_name_does_and_is_matched_without_case() {
    let mut kernel = common::kernel();
    let mut os = kernel.os();
    for spelt in [&b"text/plain\0"[..], b"TEXT/PLAIN\r", b"Text/Plain extra\0"] {
        let at = common::block(&mut os, spelt);
        assert_eq!(translate(&mut os, MIME, at, TYPE).expect("mapped").0, 0xFFF);
    }
    assert_eq!(
        translated(&mut os, MIME, b"text/html\0", EXTENSION),
        Ok(b"html".to_vec())
    );
    assert_eq!(
        translated(&mut os, MIME, b"no/such\0", TYPE),
        Err(NO_MAPPING)
    );
}

#[test]
fn an_extension_ends_only_at_a_zero_its_dot_optional_its_case_ignored() {
    let mut kernel = common::kernel();
    let mut os = kernel.os();
    for spelt in [&b"txt\0"[..], b".txt\0", b"TXT\0"] {
        assert_eq!(
            translated(&mut os, EXTENSION, spelt, MIME),
            Ok(b"text/plain".to_vec())
        );
    }
    for unmapped in [&b"txt\r\0"[..], b"txt \0", b"zzz\0"] {
        assert_eq!(
            translated(&mut os, EXTENSION, unmapped, MIME),
            Err(NO_MAPPING)
        );
    }
}

#[test]
fn a_type_name_is_the_system_s_and_its_case_is_ignored() {
    let mut kernel = common::kernel();
    let mut os = kernel.os();
    for name in [&b"Text\0"[..], b"text\0", b"&FFF\0"] {
        assert_eq!(
            translated(&mut os, NAME, name, MIME),
            Ok(b"text/plain".to_vec())
        );
    }
    assert_eq!(
        translated(&mut os, NAME, b"NoSuchType\0", MIME),
        Err(NO_MAPPING)
    );
    // A name a program gives a type is its name to MimeMap too.
    command(&mut os, b"Set File$Type_A91 Zipped").expect("set");
    assert_eq!(
        translated(&mut os, NAME, b"zipped\0", MIME),
        Ok(b"application/zip".to_vec())
    );
    assert_eq!(
        translate(&mut os, TYPE, 0xA91, NAME).expect("mapped").1,
        b"Zipped"
    );
}

#[test]
fn a_form_it_does_not_know_is_refused_on_the_way_in_and_ignored_on_the_way_out() {
    let mut kernel = common::kernel();
    let mut os = kernel.os();
    for form in [4, u32::MAX] {
        assert_eq!(translate(&mut os, form, 0xFFF, MIME), Err(UNKNOWN_FORM));
    }
    let (r3, answer) = translate(&mut os, TYPE, 0xFFF, 4).expect("answered");
    assert_ne!(r3, 0xFFF);
    assert!(answer.iter().all(|&b| b == 0xEE), "nothing written");
    let (number, message) = call(&mut os, TRANSLATE, &[TYPE, 0x123, MIME, 0]).expect_err("none");
    assert_eq!(
        (number, message.as_str()),
        (NO_MAPPING, "MimeMap knows no mapping for that")
    );
}

/// `line` run with its output in the RAM disc's `Out`: the output, or the
/// line's refusal's number.
fn shown(kernel: &mut Kernel, line: &str) -> Result<String, u32> {
    let mut os = kernel.os();
    let redirected = format!("{line} {{ > RAM:$.Out }}");
    let at = text(&mut os, redirected.as_bytes());
    match common::call(&mut os, tairix_riscos_kernel::swi::os::CLI, &[at]) {
        Ok(_) => {}
        Err((number, _)) => return Err(number),
    }
    let length = info(&mut os, b"RAM:$.Out").expect("written").3 as usize;
    let bytes = load(&mut os, b"RAM:$.Out", length).expect("read");
    Ok(String::from_utf8(bytes).expect("text"))
}

#[test]
fn mimemap_shows_one_mapping_by_any_of_its_names_or_all_of_them() {
    let mut kernel = common::kernel();
    ram_disc(&mut kernel.os());
    let text_line = "text/plain &FFF Text .txt .rst .log .ini .cfg .conf\n\r";
    for line in [
        "MimeMap &FFF",
        "MimeMap &fff",
        "MimeMap .TXT",
        "MimeMap text/plain",
        "MimeMap Text",
    ] {
        assert_eq!(shown(&mut kernel, line).as_deref(), Ok(text_line), "{line}");
    }
    for unmapped in ["MimeMap txt", "MimeMap &123", "MimeMap no/such"] {
        assert_eq!(shown(&mut kernel, unmapped), Err(NO_MAPPING), "{unmapped}");
    }
    assert_eq!(shown(&mut kernel, "MimeMap &FFF more"), Err(0xDC));
    let all = shown(&mut kernel, "MimeMap").expect("listed");
    assert!(all.lines().count() > 20);
    assert!(all.contains(text_line));
    assert!(all.contains("application/zip &A91 &A91 .zip"));
    assert!(matches!(command(&mut kernel.os(), b"ReadMimeMap"), Ok(())));
}

#[test]
fn the_swi_and_its_commands_are_named_as_mimemap_names_them() {
    let mut kernel = common::kernel();
    let mut os = kernel.os();
    let buffer = os.rma_claim(64).expect("room");
    let regs = call(
        &mut os,
        tairix_riscos_kernel::swi::os::SWI_NUMBER_TO_STRING,
        &[TRANSLATE, buffer, 64],
    )
    .expect("named");
    assert_eq!(
        read(&mut os, buffer, regs.r[2] as usize - 1),
        b"MimeMap_Translate"
    );
    let failed = command(&mut os, b"MimeMap &FFF &FFF");
    assert!(matches!(failed, Err(Fail::Error(_))));
}
