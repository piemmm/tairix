//! The KERNAL's workspace where the Programmer's Reference Guide's memory map
//! documents it, under its names there: what software reads and writes
//! directly, at the addresses it uses.

/// Zero page, `$90–$FF`.
pub mod zp {
    use crate::machine::c64::zero_page;

    /// STATUS: the I/O status word READST returns.
    pub const STATUS: u16 = 0x90;
    /// STKEY: the keyboard row read with the STOP key's column selected.
    pub const STKEY: u16 = 0x91;
    /// SVXT: the tape's adjustment to a short pulse's length.
    pub const SVXT: u16 = 0x92;
    /// VERCK: 0 to load, 1 to verify.
    pub const VERCK: u16 = 0x93;
    /// C3PO: a byte waits to go out on the serial bus.
    pub const C3PO: u16 = 0x94;
    /// BSOUR: the byte waiting.
    pub const BSOUR: u16 = 0x95;
    /// SYNO: the tape block's countdown byte last read.
    pub const SYNO: u16 = 0x96;
    /// XSAV: X kept across a character's input or output.
    pub const XSAV: u16 = 0x97;
    /// LDTND: how many files are open.
    pub const LDTND: u16 = 0x98;
    /// DFLTN: the input device.
    pub const DFLTN: u16 = 0x99;
    /// DFLTO: the output device.
    pub const DFLTO: u16 = 0x9A;
    /// PRTY: the tape byte's parity.
    pub const PRTY: u16 = 0x9B;
    /// DPSW: a tape byte has been read.
    pub const DPSW: u16 = 0x9C;
    /// MSGFLG: bit 7 shows the control messages, bit 6 the error messages.
    pub const MSGFLG: u16 = 0x9D;
    /// MSGFLG's bit that shows the control messages, as BASIC sets it in
    /// direct mode.
    pub const CONTROL_MESSAGES: u8 = 0x80;
    /// PTR1: the tape's first pass's count of bad bytes.
    pub const PTR1: u16 = 0x9E;
    /// PTR2: the second pass's count of bad bytes it put right.
    pub const PTR2: u16 = 0x9F;
    /// TIME: the jiffy clock, its most significant byte first.
    pub const TIME: u16 = 0xA0;
    /// Two bytes of working storage the tape and the serial bus share.
    pub const TEMP: u16 = 0xA3;
    /// CNTDN: the tape's countdown to the next byte.
    pub const CNTDN: u16 = 0xA5;
    /// BUFPT: the offset into the tape buffer.
    pub const BUFPT: u16 = 0xA6;
    /// INBIT: RS-232's bit coming in.
    pub const INBIT: u16 = 0xA7;
    /// BITCI: RS-232's count of bits in.
    pub const BITCI: u16 = 0xA8;
    /// RIDATA: RS-232's byte coming in.
    pub const RIDATA: u16 = 0xAA;
    /// SAL: the address a load or save is at.
    pub const SAL: u16 = 0xAC;
    /// EAL: the address past its end.
    pub const EAL: u16 = zero_page::LOAD_END;
    /// CMP0: the tape's timing.
    pub const CMP0: u16 = 0xB0;
    /// TAPE1: the tape buffer's address.
    pub const TAPE1: u16 = 0xB2;
    /// BITTS: RS-232's count of bits out.
    pub const BITTS: u16 = 0xB4;
    /// NXTBIT: RS-232's next bit out.
    pub const NXTBIT: u16 = 0xB5;
    /// RODATA: RS-232's byte going out.
    pub const RODATA: u16 = 0xB6;
    /// FNLEN: the file name's length.
    pub const FNLEN: u16 = 0xB7;
    /// LA: the logical file.
    pub const LA: u16 = 0xB8;
    /// SA: the secondary address, `$FF` for none.
    pub const SA: u16 = 0xB9;
    /// FA: the device.
    pub const FA: u16 = 0xBA;
    /// FNADR: the file name's address.
    pub const FNADR: u16 = 0xBB;
    /// ROPRTY: RS-232's parity going out.
    pub const ROPRTY: u16 = 0xBD;
    /// FSBLK: the copies of a tape block left to read or write.
    pub const FSBLK: u16 = 0xBE;
    /// MYCH: the tape's byte being assembled.
    pub const MYCH: u16 = 0xBF;
    /// CAS1: the tape motor is the tape routines', not the switch's.
    pub const CAS1: u16 = 0xC0;
    /// STAL: the start of the memory a load or save covers.
    pub const STAL: u16 = 0xC1;
    /// MEMUSS: the address LOAD was given.
    pub const MEMUSS: u16 = 0xC3;
    /// LSTX: the key last scanned, 64 for none.
    pub const LSTX: u16 = 0xC5;
    /// NDX: the characters in the keyboard buffer.
    pub const NDX: u16 = 0xC6;
    /// RVS: characters print reversed.
    pub const RVS: u16 = 0xC7;
    /// INDX: the input line's last column.
    pub const INDX: u16 = 0xC8;
    /// LXSP: where input began, its row and then its column.
    pub const LXSP: u16 = 0xC9;
    /// SFDX: the key held now, 64 for none.
    pub const SFDX: u16 = 0xCB;
    /// BLNSW: the cursor flashes while 0.
    pub const BLNSW: u16 = 0xCC;
    /// BLNCT: jiffies to the cursor's next change.
    pub const BLNCT: u16 = 0xCD;
    /// GDBLN: the character under the cursor.
    pub const GDBLN: u16 = 0xCE;
    /// BLNON: the cursor is showing.
    pub const BLNON: u16 = 0xCF;
    /// CRSW: input comes from the screen line just entered.
    pub const CRSW: u16 = 0xD0;
    /// PNT: the current screen line's address.
    pub const PNT: u16 = 0xD1;
    /// PNTR: the cursor's column in its logical line.
    pub const PNTR: u16 = 0xD3;
    /// QTSW: quote mode.
    pub const QTSW: u16 = 0xD4;
    /// LNMX: the logical line's last column, 39 or 79.
    pub const LNMX: u16 = 0xD5;
    /// TBLX: the cursor's row.
    pub const TBLX: u16 = 0xD6;
    /// The character last printed or read.
    pub const DATA: u16 = 0xD7;
    /// INSRT: characters still to insert.
    pub const INSRT: u16 = 0xD8;
    /// LDTB1: for each row, its screen address's high byte, bit 7 set where
    /// it starts a logical line.
    pub const LDTB1: u16 = 0xD9;
    /// USER: the current line's address in colour RAM.
    pub const USER: u16 = 0xF3;
    /// KEYTAB: the keyboard table in use.
    pub const KEYTAB: u16 = 0xF5;
    /// RIBUF: RS-232's input buffer.
    pub const RIBUF: u16 = 0xF7;
    /// ROBUF: its output buffer.
    pub const ROBUF: u16 = 0xF9;
}

/// Pages 2 and 3.
pub mod page {
    /// LAT: each open file's number.
    pub const LAT: u16 = 0x0259;
    /// FAT: each open file's device.
    pub const FAT: u16 = 0x0263;
    /// SAT: each open file's secondary address.
    pub const SAT: u16 = 0x026D;
    /// KEYD: the keyboard buffer.
    pub const KEYD: u16 = 0x0277;
    /// MEMSTR: the bottom of memory.
    pub const MEMSTR: u16 = 0x0281;
    /// MEMSIZ: the top of memory.
    pub const MEMSIZ: u16 = 0x0283;
    /// TIMOUT: the IEEE timeout flag SETTMO sets.
    pub const TIMOUT: u16 = 0x0285;
    /// COLOR: the colour characters print in.
    pub const COLOR: u16 = 0x0286;
    /// GDCOL: the colour under the cursor.
    pub const GDCOL: u16 = 0x0287;
    /// HIBASE: the screen's page.
    pub const HIBASE: u16 = 0x0288;
    /// XMAX: the keyboard buffer's size.
    pub const XMAX: u16 = 0x0289;
    /// RPTFLG: `$80` every key repeats, `$40` none, else the cursor keys,
    /// INST DEL and the space bar.
    pub const RPTFLG: u16 = 0x028A;
    /// KOUNT: jiffies between repeats.
    pub const KOUNT: u16 = 0x028B;
    /// DELAY: jiffies before the first repeat.
    pub const DELAY: u16 = 0x028C;
    /// SHFLAG: SHIFT in bit 0, the Commodore key in bit 1, CTRL in bit 2.
    pub const SHFLAG: u16 = 0x028D;
    /// LSTSHF: the shift keys at the last scan.
    pub const LSTSHF: u16 = 0x028E;
    /// KEYLOG: the routine that picks a key's table and decodes it.
    pub const KEYLOG: u16 = 0x028F;
    /// MODE: SHIFT and the Commodore key change the character set while 0.
    pub const MODE: u16 = 0x0291;
    /// AUTODN: a line spilling into the next opens a line for it while 0.
    pub const AUTODN: u16 = 0x0292;
    /// M51CTR: RS-232's control register: speed, word length, stop bits.
    pub const M51CTR: u16 = 0x0293;
    /// M51CDR: its command register: handshake, duplex, parity.
    pub const M51CDR: u16 = 0x0294;
    /// M51AJB: the speed a control register's rate 0 asks for.
    pub const M51AJB: u16 = 0x0295;
    /// RSSTAT: RS-232's status.
    pub const RSSTAT: u16 = 0x0297;
    /// BITNUM: bits to a word, its stop bit included.
    pub const BITNUM: u16 = 0x0298;
    /// BAUDOF: a bit's length in cycles.
    pub const BAUDOF: u16 = 0x0299;
    /// RIDBE: the input buffer's end.
    pub const RIDBE: u16 = 0x029B;
    /// RIDBS: the input buffer's start.
    pub const RIDBS: u16 = 0x029C;
    /// RODBS: the output buffer's start.
    pub const RODBS: u16 = 0x029D;
    /// RODBE: the output buffer's end.
    pub const RODBE: u16 = 0x029E;
    /// ENABL: the CIA2 interrupts RS-232 has enabled.
    pub const ENABL: u16 = 0x02A1;
    /// TVSFLG: 1 on a PAL machine, 0 on NTSC.
    pub const TVSFLG: u16 = 0x02A6;
    /// TBUFFR: the tape buffer.
    pub const TBUFFR: u16 = 0x033C;
}

/// The vectors `$0314–$0333` the KERNAL calls through, in their order.
pub mod vector {
    /// CINV: the IRQ handler.
    pub const CINV: u16 = 0x0314;
    /// CBINV: the BRK handler.
    pub const CBINV: u16 = 0x0316;
    /// NMINV: the NMI handler.
    pub const NMINV: u16 = 0x0318;
    /// IOPEN.
    pub const IOPEN: u16 = 0x031A;
    /// ICLOSE.
    pub const ICLOSE: u16 = 0x031C;
    /// ICHKIN.
    pub const ICHKIN: u16 = 0x031E;
    /// ICKOUT.
    pub const ICKOUT: u16 = 0x0320;
    /// ICLRCH.
    pub const ICLRCH: u16 = 0x0322;
    /// IBASIN: CHRIN.
    pub const IBASIN: u16 = 0x0324;
    /// IBSOUT: CHROUT.
    pub const IBSOUT: u16 = 0x0326;
    /// ISTOP.
    pub const ISTOP: u16 = 0x0328;
    /// IGETIN.
    pub const IGETIN: u16 = 0x032A;
    /// ICLALL.
    pub const ICLALL: u16 = 0x032C;
    /// ILOAD.
    pub const ILOAD: u16 = 0x0330;
    /// ISAVE.
    pub const ISAVE: u16 = 0x0332;
    /// The vectors' bytes.
    pub const BYTES: u8 = 0x20;
}
