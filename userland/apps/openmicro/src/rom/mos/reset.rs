//! Reset in its three forms, the sideways ROM protocol, language entry and
//! BRK.
//!
//! A power-on reset is recognised on the Model B by the System VIA's
//! interrupt enables reading clear, since BREAK does not reset that chip, and
//! on the Electron by the ULA's power-on bit; a BREAK with CTRL held, or one
//! *FX200 has asked to clear memory, is a hard reset. Power-on and hard
//! resets clear memory and restore every setting; a soft one keeps memory,
//! TIME, *TV, the user flag and the printer's ignored character.

use super::ws::{os, page2, page_d, var, vdu, vector};
use super::{page, svia, Held, VECTOR_DEFAULTS};
use crate::machine::bbc::via::reg;
use crate::machine::electron::{irq as ula_irq, sheila as ula, LOCKED_ROMS};
use crate::machine::Model;
use crate::machine::SIDEWAYS_BASE;
use crate::rom::asm::{abs, hi, imm, lo, zp, Acc, Addr, Asm};
use crate::rom::os::{header, rom_type, service};

/// The page the OS's high-water mark starts from.
const OSHWM_START: u8 = 0x0E;
/// The settings every reset restores after the vectors and the five
/// addresses at &236, which together fill &200 to &27F.
const SETTINGS: usize = 64;
const SETTINGS_START: u16 = 0x0240;
const _: () = assert!(SETTINGS_START as usize + SETTINGS == var::COUNTRY as usize);
const ADDRESSES: usize = 5;
const _: () = assert!(
    2 * (VECTOR_DEFAULTS.len() + ADDRESSES) + SETTINGS == (var::COUNTRY - vector::USER) as usize
);
/// The settings from &280 only a hard reset restores.
const HARD_SETTINGS: u8 = 15;
// The Electron's start-up options follow them.
const _: () = assert!(var::STARTUP_OPTIONS == var::COUNTRY + HARD_SETTINGS as u16);
/// An RTI, what an NMI finds at &D00 until a filing system claims it.
const RTI: u8 = 0x40;
/// The internal key numbers reset looks at.
const KEY_SHIFT: u8 = 0x00;
const KEY_CTRL: u8 = 0x01;
/// *FX200's request that BREAK clear memory.
const BREAK_CLEARS: u8 = 0x02;
/// The startup option inverting SHIFT-BREAK.
const BOOT_INVERTED: u8 = 0x08;
/// The keyboard buffer code for the BREAK key, whose soft key runs after any
/// reset.
const BREAK_KEY_CODE: u8 = 0xCA;

pub fn emit(a: &mut Asm, model: Model) {
    reset(a, model);
    clear_memory(a);
    rom_scan(a, model);
    service_call(a, model);
    boot_and_enter(a, model);
    enter_language(a, model);
    brk(a, model);
    defaults(a, model);
}

/// The settings from &280 a hard reset restores: the Electron's start-up
/// option byte with them, since it has no links to read it from.
const fn hard_settings(model: Model) -> u8 {
    match model {
        Model::ModelB => HARD_SETTINGS,
        Model::Electron => HARD_SETTINGS + 1,
    }
}

fn reset(a: &mut Asm, model: Model) {
    a.here("reset");
    a.lda(imm(RTI));
    a.sta(abs(page_d::NMI));
    a.sei();
    a.cld();
    a.ldx(imm(0xFF));
    a.txs();
    match model {
        Model::ModelB => {
            // The System VIA keeps its enables through BREAK.
            a.lda(abs(svia(reg::IER)));
            a.asl(Acc);
            a.ldx(imm(1));
            a.tay();
            a.beq("reset_typed");
        }
        Model::Electron => {
            // Set only by power coming on; this read clears it.
            a.lda(abs(ula::STATUS));
            a.and(imm(ula_irq::POWER_ON));
            a.ldx(imm(1));
            a.tay();
            a.bne("reset_typed");
        }
    }
    a.ldx(imm(KEY_CTRL));
    a.jsr("key_down");
    a.bmi("reset_hard");
    a.lda(abs(var::BREAK_EFFECT));
    a.and(imm(BREAK_CLEARS));
    a.bne("reset_hard");
    a.ldx(imm(0));
    a.beq("reset_typed");
    a.here("reset_hard");
    a.ldx(imm(2));
    a.here("reset_typed");
    a.txa();
    a.beq("reset_kept");
    a.jsr("clear_memory");
    a.here("reset_kept");
    // Every reset restores the vectors and the settings up to &27F; a hard
    // one restores the rest of the page too.
    a.ldy(imm(lo(var::COUNTRY - vector::USER - 1)));
    a.here("reset_vectors");
    a.lda(abs("vector_defaults").y());
    a.sta(abs(vector::USER).y());
    a.dey();
    a.bpl("reset_vectors");
    a.txa();
    a.beq("reset_settings_done");
    a.ldy(imm(hard_settings(model) - 1));
    a.here("reset_hard_settings");
    a.lda(abs("hard_defaults").y());
    a.sta(abs(var::COUNTRY).y());
    a.dey();
    a.bpl("reset_hard_settings");
    a.here("reset_settings_done");
    a.stx(abs(var::LAST_BREAK));
    a.jsr("hardware_init");
    if model == Model::ModelB {
        a.jsr("read_links");
    }
    a.jsr("rom_scan");
    a.jsr("claim_workspace");
    a.jsr("buffers_flush_all");
    a.jsr("cfs_init");
    a.jsr("sound_init");
    a.jsr("vdu_init");
    a.lda(imm(BREAK_KEY_CODE));
    a.ldx(imm(0));
    a.jsr("buffer_insert");
    a.cli();
    a.clc();
    a.jsr("break_intercept");
    a.jsr("print_banner");
    a.sec();
    a.jsr("break_intercept");
    a.jmp("boot_and_enter");
    banner(a, model);
}

/// The BREAK intercept *FX247 sets, and the machine's name.
fn banner(a: &mut Asm, model: Model) {
    // *FX247–249's code, if it is a JMP, is called before the banner with C
    // clear and after it with C set.
    a.here("break_intercept");
    a.lda(abs(var::BREAK_INTERCEPT));
    a.eor(imm(0x4C));
    a.bne("break_intercept_none");
    a.jmp(abs(var::BREAK_INTERCEPT));
    a.here("break_intercept_none");
    a.rts();

    // The machine's name, unless *FX215 has turned it off; after a hard reset
    // the Model B's memory too, and the bell. The Model B's backspace is the
    // original's.
    a.here("print_banner");
    a.bit(abs(var::STARTUP));
    a.bpl("print_banner_none");
    a.jsr("print_inline");
    match model {
        Model::ModelB => a.text("\nOpenMOS Micro "),
        Model::Electron => a.text("\nOpenMOS Electron"),
    }
    a.byte(0);
    a.lda(abs(var::LAST_BREAK));
    a.beq("print_banner_soft");
    a.jsr("print_inline");
    match model {
        Model::ModelB => a.text("32K\x07"),
        Model::Electron => a.text("\x07"),
    }
    a.byte(0);
    a.here("print_banner_soft");
    if model == Model::ModelB {
        a.lda(imm(0x08));
        a.jsr("oswrch");
    }
    a.jsr("osnewl");
    a.jmp("osnewl");
    a.here("print_banner_none");
    a.rts();
}

/// Zero every page of RAM but the stack, keeping X; then put back the NMI's
/// RTI and set the soft keys out empty.
fn clear_memory(a: &mut Asm) {
    a.here("clear_memory");
    a.lda(imm(0));
    a.tay();
    a.here("clear_low_pages");
    a.sta(abs(0x0000).y());
    a.sta(abs(0x0200).y());
    a.sta(abs(0x0300).y());
    a.iny();
    a.bne("clear_low_pages");
    a.lda(imm(0x04));
    a.sta(zp(0x01));
    a.lda(imm(0));
    a.here("clear_pages");
    a.sta(zp(0x00).ind_y());
    a.iny();
    a.bne("clear_pages");
    a.inc(zp(0x01));
    a.bit(zp(0x01));
    a.bpl("clear_pages");
    a.lda(imm(RTI));
    a.sta(abs(page_d::NMI));
    a.jmp("key_reset");
}

/// Build the ROM type table: a ROM is one whose copyright pointer finds
/// "\0(C)"; BASIC is the highest with no service entry.
fn rom_scan(a: &mut Asm, model: Model) {
    a.here("rom_scan");
    a.lda(imm(0xFF));
    a.sta(abs(var::BASIC_ROM));
    a.ldx(imm(15));
    a.here("rom_scan_next");
    page(a, model, Held::X);
    a.ldy(abs(header::COPYRIGHT));
    a.ldx(imm(0));
    a.here("rom_scan_copyright");
    a.lda(abs(SIDEWAYS_BASE).y());
    a.cmp(abs("copyright_signature").x());
    a.bne("rom_scan_none");
    a.iny();
    a.inx();
    a.cpx(imm(4));
    a.bne("rom_scan_copyright");
    a.ldx(zp(os::ROM));
    a.lda(abs(header::TYPE));
    a.sta(abs(page2::ROM_TYPES).x());
    a.bmi("rom_scan_done");
    a.ldy(abs(var::BASIC_ROM));
    a.bpl("rom_scan_done");
    a.stx(abs(var::BASIC_ROM));
    a.bmi("rom_scan_done");
    a.here("rom_scan_none");
    a.ldx(zp(os::ROM));
    a.lda(imm(0));
    a.sta(abs(page2::ROM_TYPES).x());
    a.here("rom_scan_done");
    a.dex();
    a.bpl("rom_scan_next");
    a.rts();
    a.here("copyright_signature");
    a.bytes(b"\0(C)");
}

/// Offer service call A, with Y its parameter, to each ROM with a service
/// entry from 15 down until one claims it by returning A zero. Returns A
/// zero if claimed, Y as the ROMs leave it; the ROM paged in is kept.
fn service_call(a: &mut Asm, model: Model) {
    a.here("service_call");
    a.tax();
    a.lda(zp(os::ROM));
    a.pha();
    a.txa();
    a.ldx(imm(15));
    a.here("service_next");
    a.pha();
    // A ROM's type has its top bit set for a service entry.
    a.lda(abs(page2::ROM_TYPES).x());
    a.bpl("service_skip");
    page(a, model, Held::X);
    a.pla();
    a.jsr(header::SERVICE);
    a.tax();
    a.beq("service_done");
    a.ldx(zp(os::ROM));
    a.pha();
    a.here("service_skip");
    a.pla();
    a.dex();
    a.bpl("service_next");
    a.here("service_done");
    a.tax();
    a.pla();
    page(a, model, Held::A);
    a.txa();
    a.rts();

    // Absolute then private workspace above &0E00, the high-water mark after.
    a.here("claim_workspace");
    a.lda(imm(service::ABSOLUTE_WORKSPACE));
    a.ldy(imm(OSHWM_START));
    a.jsr("service_call");
    a.lda(imm(service::PRIVATE_WORKSPACE));
    a.jsr("service_call");
    a.sty(abs(var::PRIMARY_OSHWM));
    a.sty(abs(var::OSHWM));
    a.rts();
}

/// Offer the boot, which a filing system ROM takes; unclaimed, SHIFT-BREAK
/// runs !BOOT from the ROM filing system. Then enter the language.
fn boot_and_enter(a: &mut Asm, model: Model) {
    a.here("boot_and_enter");
    a.ldx(imm(KEY_SHIFT));
    a.jsr("key_down");
    a.and(imm(0x80));
    a.sta(zp(os::TEMP));
    a.lda(abs(var::STARTUP_OPTIONS));
    a.and(imm(BOOT_INVERTED));
    for _ in 0..4 {
        a.asl(Acc);
    }
    a.eor(zp(os::TEMP));
    // Y is zero to boot.
    a.tay();
    a.lda(imm(service::AUTO_BOOT));
    a.jsr("service_call");
    a.beq("boot_done");
    a.tya();
    a.bne("boot_done");
    a.jsr("boot_from_rom");
    a.here("boot_done");
    a.lda(abs(var::LAST_BREAK));
    a.beq("enter_kept_language");
    a.ldx(imm(15));
    a.here("find_language");
    if model == Model::Electron {
        // A language in a cartridge or the expansion's slots comes before
        // BASIC.
        a.cpx(imm(*LOCKED_ROMS.end()));
        a.bne("find_language_kept");
        a.ldx(imm(*LOCKED_ROMS.start() - 1));
        a.here("find_language_kept");
    }
    a.lda(abs(page2::ROM_TYPES).x());
    a.and(imm(rom_type::LANGUAGE));
    a.bne("enter_found_language");
    a.dex();
    a.bpl("find_language");
    if model == Model::Electron {
        a.ldx(abs(var::BASIC_ROM));
        a.bpl("enter_found_language");
    }
    a.brk();
    a.byte(0);
    a.text("Language?");
    a.byte(0);
    a.here("enter_found_language");
    a.stx(abs(var::LANGUAGE));
    a.here("enter_kept_language");
    a.ldx(abs(var::LANGUAGE));
    a.fall_into("enter_language");
}

/// OSBYTE 142's work: page in language ROM X, print its title, and enter it
/// with A = 1.
fn enter_language(a: &mut Asm, model: Model) {
    a.here("enter_language");
    a.stx(abs(var::LANGUAGE));
    page(a, model, Held::X);
    // The error pointer left at the language's copyright, so REPORT before
    // any error gives it, as MOS 1.20 does.
    a.lda(abs(header::COPYRIGHT));
    a.sta(zp(os::ERROR));
    a.lda(imm(hi(SIDEWAYS_BASE)));
    a.sta(zp(os::ERROR + 1));
    a.ldy(imm(0));
    a.here("enter_title");
    a.lda(abs(header::TITLE).y());
    a.beq("enter_titled");
    a.jsr("oswrch");
    a.iny();
    a.bne("enter_title");
    a.here("enter_titled");
    a.jsr("osnewl");
    a.jsr("osnewl");
    a.ldx(imm(0xFF));
    a.txs();
    a.cli();
    a.lda(imm(1));
    a.jmp(abs(header::LANGUAGE));
}

/// BRK: point &FD at the error number, note the ROM it happened in, offer
/// service call 6, then call BRKV with the language paged in.
fn brk(a: &mut Asm, model: Model) {
    a.here("brk_entry");
    a.txa();
    a.pha();
    a.tsx();
    a.lda(abs(0x0103).x());
    a.sec();
    a.sbc(imm(1));
    a.sta(zp(os::ERROR));
    a.lda(abs(0x0104).x());
    a.sbc(imm(0));
    a.sta(zp(os::ERROR + 1));
    a.lda(zp(os::ROM));
    a.sta(abs(var::BRK_ROM));
    a.ldy(imm(0));
    a.lda(imm(service::BRK));
    a.jsr("service_call");
    a.ldx(abs(var::LANGUAGE));
    page(a, model, Held::X);
    a.pla();
    a.tax();
    a.lda(zp(os::IRQ_A));
    a.cli();
    a.jmp(abs(vector::BRK).ind());

    // With no language to take it, an error is printed and the machine stops.
    a.here("brk_default");
    a.jsr("error_text");
    a.here("brk_default_halt");
    a.jmp("brk_default_halt");

    // The last error's message, then a newline.
    a.here("error_text");
    a.ldy(imm(1));
    a.here("error_text_next");
    a.lda(zp(os::ERROR).ind_y());
    a.beq("error_text_end");
    a.jsr("oswrch");
    a.iny();
    a.bne("error_text_next");
    a.here("error_text_end");
    a.jmp("osnewl");
}

/// The vectors' defaults, then the OSBYTE settings to &27F; `hard_defaults`
/// holds &280 on, restored by hard resets alone.
fn defaults(a: &mut Asm, model: Model) {
    a.here("vector_defaults");
    for routine in VECTOR_DEFAULTS {
        a.word(routine);
    }
    // &236: the addresses of the variables, the extended vectors, the ROM
    // table, the keyboard table and the VDU variables.
    let addresses: [Addr; ADDRESSES] = [
        var::BASE.into(),
        page_d::EXTENDED_VECTORS.into(),
        page2::ROM_TYPES.into(),
        "key_translation".into(),
        vdu::GRAPHICS_WINDOW.into(),
    ];
    for address in addresses {
        a.word(address);
    }
    a.bytes(&settings(model));
    a.here("hard_defaults");
    a.bytes(&hard(model));
}

/// The OSBYTE settings from &240 every reset restores.
fn settings(model: Model) -> [u8; SETTINGS] {
    let mut settings: [u8; SETTINGS] = [
        // &240: CFS timeout, input, keyboard semaphore, OSHWMs, RS423 mode,
        // fonts, CFS/RFS, the ULA copies, BRK ROM, BASIC, ADC channel,
        // highest channel, conversion type, RS423 use.
        0x00,
        0x00,
        0x00,
        OSHWM_START,
        OSHWM_START,
        0x01,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0xFF,
        0x04,
        0x04,
        0x00,
        0xFF,
        // &250: 6850 control, flash count and durations, repeat delay and
        // period, EXEC and SPOOL, BREAK effect, Econet, keyboard status,
        // handshake extent, suppression, cassette/RS423, Econet calls.
        0x56,
        0x19,
        0x19,
        0x19,
        0x32,
        0x08,
        0x00,
        0x00,
        0x00,
        0x00,
        0x20,
        0x09,
        0x00,
        0x00,
        0x00,
        0x00,
        // &260: Econet OSWRCH, speech, sound, the BELL, startup options,
        // soft key length, paged lines, VDU queue, TAB, ESCAPE, the input
        // code zones.
        0x00,
        0x50,
        0x00,
        0x03,
        0x90,
        0x64,
        0x06,
        0x81,
        0x00,
        0x00,
        0x00,
        0x09,
        0x1B,
        0x01,
        0xD0,
        0xE0,
        // &270: the last input zone, the function key zones, ESCAPE action
        // and effects, the IRQ masks, Tube, speech, output streams, cursor
        // editing.
        0xF0,
        0x01,
        0x80,
        0x90,
        0x00,
        0x00,
        0x00,
        0xFF,
        0xFF,
        0xFF,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
        0x00,
    ];
    if model == Model::Electron {
        for (variable, value) in [
            // The keyboard is scanned every tick while this is set.
            (var::KEYBOARD_SEMAPHORE, 0xFF),
            (var::ULA_ENABLES, ula_irq::RTC | ula_irq::DISPLAY_END),
            // CAPS LOCK on, FUNC, SHIFT and CTRL up.
            (var::KEYBOARD_STATUS, 0x00),
            (var::BELL_PITCH, 0x65),
            (var::EXTERNAL_SOUND, 0x00),
            // FUNC with a letter types its keyword, as FUNC with a digit its
            // soft key.
            (var::KEY_ZONES + 1, 0x01),
            (var::KEY_ZONES + 2, 0x01),
        ] {
            settings[usize::from(variable - SETTINGS_START)] = value;
        }
    }
    settings
}

/// The settings from &280 a hard reset alone restores: country, user flag,
/// serial ULA, timer switch, soft key consistency, printer, printer ignore,
/// BREAK intercept, two spare, language, last BREAK and RAM; on the Electron,
/// `&FE07`'s copy in the serial ULA's place, the counter listening to the
/// cassette, mode 0, the motor off and the light dark, and the start-up
/// options after them.
fn hard(model: Model) -> alloc::vec::Vec<u8> {
    const HARD: [u8; HARD_SETTINGS as usize] = [
        0x00, 0x00, 0x64, 0x05, 0x00, 0x01, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80,
    ];
    let mut hard = HARD.to_vec();
    if model == Model::Electron {
        hard[usize::from(var::ULA_CONTROL_COPY - var::COUNTRY)] = 0x00;
        hard.push(0xFF);
    }
    hard
}
