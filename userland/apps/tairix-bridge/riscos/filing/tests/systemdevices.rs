//! SystemDevices held to what RISC OS 5's SystemDevices 1.34 did, recorded
//! on RPCEmu: each device's streams, refusals and files, `vdu:`'s listing as
//! a `WrchV` claimant saw it, with what `OS_Byte` 236 read inside it, and
//! `kbd:`'s lines and their echo. `printer:` follows the PRM's routing,
//! which RISC OS 5 on RPCEmu does not.

mod common;

use common::programs::run;

/// The routines around the program: `wrch` logs each character with the
/// output streams `OS_Byte` 236 reads as it goes; `esc` sets Escape and
/// `typed` types `ktarget` `x`s then Return, one a tick, from a ticker; and
/// `esch` counts Escapes so BASIC never sees them.
const ROUTINES: &[&str] = &[
    "DIM code% 2048, log% 8192, blk% 1024",
    "FOR pass%=0 TO 2 STEP 2",
    "P%=code%",
    "[OPT pass%",
    ".logp EQUD 0",
    ".logend EQUD 0",
    ".escapes EQUD 0",
    ".kcount EQUD 0",
    ".ktarget EQUD 0",
    ".wrch STMFD R13!,{R0-R4,R14}:LDR R4,logp:LDR R3,logend:CMP R4,R3:BHS wrout",
    "STR R0,[R4],#4:MOV R0,#236:MOV R1,#0:MOV R2,#255:SWI \"XOS_Byte\":STR R1,[R4],#4:STR R4,logp",
    ".wrout LDMFD R13!,{R0-R4,PC}",
    ".esch STMFD R13!,{R0}:LDR R0,escapes:ADD R0,R0,#1:STR R0,escapes:LDMFD R13!,{R0}:MOV PC,R14",
    ".esc STMFD R13!,{R0-R4,R14}:MRS R4,CPSR:ORR R3,R4,#3:MSR CPSR_c,R3:MOV R0,R0:STMFD R13!,{R14}",
    "MOV R0,#125:SWI \"XOS_Byte\"",
    "LDMFD R13!,{R14}:MSR CPSR_c,R4:MOV R0,R0:LDMFD R13!,{R0-R4,PC}",
    ".typed STMFD R13!,{R0-R4,R14}:MRS R4,CPSR:ORR R3,R4,#3:MSR CPSR_c,R3:MOV R0,R0:STMFD R13!,{R14}",
    "LDR R3,kcount:LDR R1,ktarget:CMP R3,R1:BHI typedx:MOVLO R2,#120:MOVEQ R2,#13:MOV R0,#138:MOV R1,#0:SWI \"XOS_Byte\"",
    "LDRCC R3,kcount:ADDCC R3,R3,#1:STRCC R3,kcount",
    ".typedx LDMFD R13!,{R14}:MSR CPSR_c,R4:MOV R0,R0:LDMFD R13!,{R0-R4,PC}",
    "]",
    "NEXT pass%",
    "!logp=log%:!logend=log%+8000",
    "SYS \"OS_ChangeEnvironment\",9,esch,0,0",
];

/// `PROClog` writes what `wrch` logged, each character and the output
/// streams, in hexadecimal as the probes wrote them; `PROCinfo` a stream's
/// pointer, extent, allocation, status and filing system's information
/// word; `PROCkeys` types its string; `PROCrd` reads a stream to its Return
/// or its end, Escape set 60 centiseconds on.
const PROCEDURES: &[&str] = &[
    "DEF PROClog(l$):LOCAL p%,S$:S$=l$+\" |\":p%=log%:WHILE p%<!logp:S$+=\" \"+STR$~(p%!0)+\"/\"+STR$~(p%!4):p%+=8:ENDWHILE:PROCw(S$):!logp=log%:ENDPROC",
    "DEF PROCinfo(h%,l$):LOCAL p%,e%,a%,s%,f%:SYS \"XOS_Args\",0,h% TO ,,p%:SYS \"XOS_Args\",2,h% TO ,,e%:SYS \"XOS_Args\",4,h% TO ,,a%",
    "SYS \"XOS_Args\",254,h% TO s%,,f%:PROCw(l$+\" \"+STR$p%+\" \"+STR$e%+\" \"+STR$a%+\" \"+STR$~s%+\" \"+STR$~f%):ENDPROC",
    "DEF PROCkeys(k$):LOCAL i%:FOR i%=1 TO LEN k$:SYS \"OS_Byte\",138,0,ASC MID$(k$,i%,1):NEXT:ENDPROC",
    "DEF PROCrd(h%,l$):LOCAL n%,b%,f%,S$:REPEAT:SYS \"OS_CallAfter\",60,esc,0:SYS \"XOS_BGet\",,h% TO b%;f%:SYS \"OS_RemoveTickerEvent\",esc,0:n%+=1",
    "IF f% AND 1 THEN S$+=\" err &\"+STR$~!b% ELSE IF f% AND 2 THEN S$+=\" end\" ELSE S$+=\" \"+STR$~b%",
    "UNTIL b%=13 OR (f% AND 3) OR n%>=20:PROClog(l$+S$):ENDPROC",
    "DEF FNs(a%,n%):LOCAL i%,S$:FOR i%=0 TO n%-1:IF a%?i%<32 THEN S$+=\"|\"+CHR$(a%?i%+64) ELSE S$+=CHR$(a%?i%)",
    "NEXT:=S$",
    "DEF FNfile(n$):LOCAL t%,l%:SYS \"XOS_File\",5,n$ TO t%,,,,l%:IF t%<>1 THEN =\"none\"",
    "SYS \"XOS_File\",16,n$,blk%,0:=FNs(blk%,l%)",
];

/// The program `body` makes with the routines before it, `procedures` its
/// own besides the shared ones.
fn program_with(body: &[&str], procedures: &[&str]) -> Vec<String> {
    let lines: Vec<&str> = ROUTINES.iter().chain(body).copied().collect();
    let procedures: Vec<&str> = PROCEDURES.iter().chain(procedures).copied().collect();
    run(&lines, &procedures, b"")
}

fn program(body: &[&str]) -> Vec<String> {
    program_with(body, &[])
}

#[test]
fn each_device_is_a_filing_system_whose_streams_say_what_they_may_do() {
    let out = program(&[
        "SYS \"XOS_Find\",&4F,\"null:\" TO h%:PROCinfo(h%,\"null in\"):SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",&8F,\"null:\" TO h%:SYS \"XOS_BPut\",65,h%:PROCinfo(h%,\"null out\"):SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",&C0,\"null:\" TO h%:PROCinfo(h%,\"null up\"):SYS \"XOS_Find\",0,h%",
        "SYS \"OS_Claim\",3,wrch,0",
        "SYS \"XOS_Find\",&8F,\"vdu:\" TO h%:SYS \"XOS_BPut\",65,h%:PROCinfo(h%,\"vdu out\"):SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",&C0,\"rawvdu:\" TO h%:PROCinfo(h%,\"rawvdu up\"):SYS \"XOS_Find\",0,h%",
        "SYS \"OS_Release\",3,wrch,0:!logp=log%",
        "SYS \"XOS_Find\",&4F,\"kbd:\" TO h%:PROCinfo(h%,\"kbd in\"):SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",&4F,\"rawkbd:\" TO h%:PROCinfo(h%,\"rawkbd in\"):SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",&8F,\"printer#8:\" TO h%:SYS \"XOS_GBPB\",2,h%,blk%,4:PROCinfo(h%,\"printer out\"):SYS \"XOS_Find\",0,h%",
    ]);
    assert_eq!(
        out,
        [
            "null in 0 0 0 450 7000000D",
            "null out 0 0 0 5D0 7000000D",
            "null up 0 0 0 4D0 7000000D",
            "vdu out 0 0 0 580 70000011",
            "rawvdu up 0 0 0 480 70000012",
            "kbd in 0 0 0 448 70000013",
            "rawkbd in 0 0 0 448 70000014",
            "printer out 0 0 0 590 F000000E",
        ]
    );
}

#[test]
fn a_device_refuses_what_it_cannot_do_and_has_no_files() {
    let out = program(&[
        "FOR i%=1 TO 6:READ n$,m%:SYS \"XOS_Find\",m%,n$ TO h%;f%:PROCw(n$+\" \"+STR$~m%+\" \"+FNerr(h%,f%)):NEXT",
        "DATA vdu:,&4F,rawvdu:,&4F,kbd:,&8F,kbd:,&C0,rawkbd:,&8F,printer:,&4F",
        "SYS \"XOS_Find\",&8F,\"null#x:\" TO h%;f%:PROCw(\"special null \"+FNerr(h%,f%))",
        "SYS \"XOS_File\",5,\"vdu:anything\" TO t%:SYS \"XOS_File\",5,\"printer#bad:\" TO u%:PROCw(\"objects \"+STR$t%+\" \"+STR$u%)",
        "SYS \"XOS_File\",255,\"null:\",blk%,0 TO r%;f%:PROCw(\"null load \"+FNerr(r%,f%))",
        "SYS \"XOS_File\",10,\"kbd:\",&FFF,,blk%,blk%+2 TO r%;f%:PROCw(\"kbd save \"+FNerr(r%,f%))",
        "SYS \"XOS_File\",10,\"null:\",&FFF,,blk%,blk%+2 TO r%;f%:PROCw(\"null save \"+FNerr(r%,f%))",
        "SYS \"XOS_File\",6,\"kbd:\" TO r%;f%:PROCw(\"kbd delete \"+FNerr(r%,f%)+\" \"+STR$r%)",
        "SYS \"XOS_File\",8,\"null:x\" TO r%;f%:PROCw(\"null cdir \"+FNerr(r%,f%))",
        "SYS \"XOS_File\",4,\"null:\",,,,3 TO r%;f%:PROCw(\"null attributes \"+FNerr(r%,f%))",
        "SYS \"XOS_File\",18,\"null:\",&FFF TO r%;f%:PROCw(\"null settype \"+FNerr(r%,f%))",
        "SYS \"XOS_FSControl\",25,\"null:\",\"null:a\" TO r%;f%:PROCw(\"null rename \"+FNerr(r%,f%))",
        "SYS \"XOS_GBPB\",10,\"null:\",blk%,8,0,200,0 TO ,,,n%,x%:SYS \"XOS_GBPB\",10,\"vdu:\",blk%,8,0,200,0 TO ,,,m%,y%",
        "PROCw(\"listed \"+STR$n%+\" \"+STR$x%+\" \"+STR$m%+\" \"+STR$y%)",
    ]);
    assert_eq!(
        out,
        [
            "vdu: 4F err &112",
            "rawvdu: 4F err &112",
            "kbd: 8F err &112",
            "kbd: C0 err &112",
            "rawkbd: 8F err &112",
            "printer: 4F err &112",
            "special null err &40F",
            "objects 0 0",
            "null load err &D6",
            "kbd save err &112",
            "null save ok",
            "kbd delete ok 0",
            "null cdir ok",
            "null attributes ok",
            "null settype err &D6",
            "null rename err &D6",
            "listed 0 -1 0 -1",
        ]
    );
}

#[test]
fn null_gives_nothing_and_takes_everything() {
    let out = program(&[
        "SYS \"XOS_Find\",&4F,\"null:\" TO h%:SYS \"XOS_GBPB\",4,h%,blk%,10 TO ,,,l%;f%:PROCw(\"read left \"+STR$l%+\" \"+STR$((f% AND 2)<>0))",
        "SYS \"XOS_BGet\",,h% TO b%;f%:PROCw(\"bget \"+STR$(b%=h%)+\" \"+STR$((f% AND 2)<>0))",
        "SYS \"XOS_BGet\",,h% TO b%;f%:PROCw(\"again \"+FNerr(b%,f%))",
        "SYS \"XOS_Args\",5,h% TO ,,e%:PROCw(\"eof \"+STR$e%):SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",&8F,\"null:\" TO h%:SYS \"XOS_GBPB\",2,h%,blk%,5 TO ,,,l%:SYS \"XOS_BPut\",65,h%:PROCw(\"written left \"+STR$l%)",
        "SYS \"XOS_Args\",3,h%,100:SYS \"XOS_Args\",1,h%,5:SYS \"XOS_Args\",6,h%,50 TO r%;f%:PROCw(\"set \"+FNerr(r%,f%))",
        "PROCinfo(h%,\"after\"):SYS \"XOS_Find\",0,h%",
    ]);
    assert_eq!(
        out,
        [
            "read left 10 -1",
            "bget -1 -1",
            "again err &DF",
            "eof -1",
            "written left 0",
            "set ok",
            "after 0 0 0 5D0 7000000D",
        ]
    );
}

#[test]
fn vdu_shows_what_it_is_given_as_type_does_on_the_screen_alone() {
    let out = program(&[
        "SYS \"OS_Claim\",3,wrch,0",
        "RESTORE +1:FOR i%=0 TO 17:READ blk%?i%:NEXT",
        "DATA 65,0,7,9,10,13,26,27,31,48,124,34,60,127,128,160,255,66",
        "SYS \"XOS_Find\",&8F,\"vdu:\" TO h%:SYS \"XOS_GBPB\",2,h%,blk%,18:SYS \"XOS_Find\",0,h%:PROClog(\"vdu\")",
        "SYS \"XOS_Find\",&8F,\"rawvdu:\" TO h%:SYS \"XOS_GBPB\",2,h%,blk%,18:SYS \"XOS_Find\",0,h%:PROClog(\"rawvdu\")",
        "RESTORE +1:FOR i%=0 TO 12:READ blk%?i%:NEXT",
        "DATA 97,13,10,98,10,13,99,13,13,100,10,10,101",
        "SYS \"XOS_Find\",&8F,\"vdu:\" TO h%:FOR i%=0 TO 12:SYS \"XOS_BPut\",blk%?i%,h%:NEXT:SYS \"XOS_Find\",0,h%:PROClog(\"line ends\")",
        "SYS \"XOS_Find\",&8F,\"vdu:\" TO h%:SYS \"XOS_BPut\",13,h%:SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",&8F,\"vdu:\" TO h%:SYS \"XOS_BPut\",10,h%:SYS \"XOS_Find\",0,h%:PROClog(\"CR then LF on two opens\")",
        "$blk%=\"xy\":SYS \"XOS_File\",10,\"vdu:\",&FFF,,blk%,blk%+2:PROClog(\"vdu save\")",
        "$blk%=\"zw\":SYS \"XOS_File\",10,\"rawvdu:\",&FFF,,blk%,blk%+2:PROClog(\"rawvdu save\")",
        "SYS \"OS_Byte\",3,2 TO ,o%:SYS \"XOS_Find\",&8F,\"vdu:\" TO h%:SYS \"XOS_BPut\",65,h%:SYS \"XOS_Find\",0,h%",
        "SYS \"OS_Byte\",3,o% TO ,n%:PROClog(\"from 2, back to \"+STR$n%)",
        "SYS \"XOS_CLI\",\"Spool HostFS:$.Spooled\":SYS \"XOS_Find\",&8F,\"vdu:\" TO h%:SYS \"XOS_BPut\",86,h%:SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",&8F,\"rawvdu:\" TO h%:SYS \"XOS_BPut\",82,h%:SYS \"XOS_Find\",0,h%:SYS \"OS_WriteC\",79:SYS \"XOS_CLI\",\"Spool\"",
        "SYS \"OS_Release\",3,wrch,0:PROCw(\"spooled \"+FNfile(\"HostFS:$.Spooled\"))",
        "SYS \"XOS_Find\",&8F,\"vdu:\" TO h%:SYS \"XOS_Args\",5,h% TO ,,e%:SYS \"XOS_BGet\",,h% TO r%;f%",
        "PROCw(\"eof \"+STR$e%+\" bget \"+FNerr(r%,f%)):SYS \"XOS_Find\",0,h%",
    ]);
    assert_eq!(
        out,
        [
            "vdu | 41/14 7C/14 40/14 7C/14 47/14 7C/14 49/14 A/14 D/14 7C/14 5A/14 7C/14 5B/14 7C/14 5F/14 30/14 7C/14 7C/14 22/14 3C/14 7C/14 3F/14 80/14 A0/14 FF/14 42/14",
            "rawvdu | 41/14 0/14 7/14 9/14 A/14 D/14 1A/14 1B/14 1F/14 30/14 7C/14 22/14 3C/14 7F/14 80/14 A0/14 FF/14 42/14",
            "line ends | 61/14 A/14 D/14 62/14 A/14 D/14 63/14 A/14 D/14 A/14 D/14 64/14 A/14 D/14 A/14 D/14 65/14",
            "CR then LF on two opens | A/14 D/14",
            "vdu save | 78/14 79/14",
            "rawvdu save | 7A/14 77/14",
            "from 2, back to 2 | 41/14",
            "spooled O",
            "eof -1 bget err &413",
        ]
    );
}

#[test]
fn vdu_reads_dump_format_when_system_devices_starts() {
    let out = program_with(
        &[
            "SYS \"OS_Claim\",3,wrch,0",
            "SYS \"XOS_CLI\",\"Configure DumpFormat 0\":SYS \"XOS_CLI\",\"RMReInit SystemDevices\":SYS \"XOS_CLI\",\"Configure DumpFormat 2\"",
            "PROCv(\"DF0 at its start, DF2 after\")",
            "SYS \"XOS_CLI\",\"RMReInit SystemDevices\":PROCv(\"DF2\")",
            "SYS \"XOS_CLI\",\"Configure DumpFormat 20\":SYS \"XOS_CLI\",\"RMReInit SystemDevices\":PROCv(\"DF20\")",
            "SYS \"OS_Release\",3,wrch,0",
        ],
        &[
            "DEF PROCv(l$):LOCAL h%:SYS \"XOS_Find\",&8F,\"vdu:\" TO h%:SYS \"XOS_BPut\",&80,h%:SYS \"XOS_BPut\",7,h%:SYS \"XOS_BPut\",9,h%",
            "SYS \"XOS_BPut\",&A3,h%:SYS \"XOS_Find\",0,h%:PROClog(l$):ENDPROC",
        ],
    );
    assert_eq!(
        out,
        [
            "DF0 at its start, DF2 after | 7C/14 21/14 7C/14 40/14 7C/14 47/14 7C/14 49/14 7C/14 21/14 23/14",
            "DF2 | 3C/14 31/14 32/14 38/14 3E/14 3C/14 37/14 3E/14 3C/14 39/14 3E/14 3C/14 31/14 36/14 33/14 3E/14",
            "DF20 | 80/14 7C/14 47/14 7C/14 49/14 A3/14",
        ]
    );
}

#[test]
fn kbd_reads_a_line_at_a_time_its_editing_and_echo_os_readline_s() {
    let out = program(&[
        "SYS \"OS_Claim\",3,wrch,0",
        "PROCkeys(\"ab\"+CHR$13):SYS \"XOS_Find\",&4F,\"kbd:\" TO h%:SYS \"XOS_GBPB\",4,h%,blk%,3 TO ,,,l%",
        "PROClog(\"read left \"+STR$l%+\" \"+STR$~blk%?0+\" \"+STR$~blk%?1+\" \"+STR$~blk%?2)",
        "SYS \"XOS_Args\",5,h% TO ,,e%:PROCw(\"eof \"+STR$e%):SYS \"XOS_Find\",0,h%",
        "PROCkeys(\"a\"+CHR$8+\"b\"+CHR$21+\"cd\"+CHR$127+\"e\"+CHR$13):SYS \"XOS_Find\",&4F,\"kbd:\" TO h%:PROCrd(h%,\"edited\"):SYS \"XOS_Find\",0,h%",
        "PROCkeys(\"p\"+CHR$10+\"q\"+CHR$13):SYS \"XOS_Find\",&4F,\"kbd:\" TO h%:PROCrd(h%,\"to LF\"):PROCrd(h%,\"after\"):SYS \"XOS_Find\",0,h%",
        "PROCkeys(\"x\"+CHR$26+\"y\"+CHR$13):SYS \"XOS_Find\",&4F,\"kbd:\" TO h%:PROCrd(h%,\"control\"):SYS \"XOS_Find\",0,h%",
        "PROCkeys(\"pq\"+CHR$13):SYS \"XOS_Find\",&4F,\"rawkbd:\" TO h%:PROCrd(h%,\"rawkbd\"):SYS \"XOS_Args\",5,h% TO ,,e%",
        "PROCw(\"rawkbd eof \"+STR$e%):SYS \"XOS_Find\",0,h%",
        "SYS \"OS_Release\",3,wrch,0",
    ]);
    assert_eq!(
        out,
        [
            "read left 0 61 62 D | 61/0 62/0 A/0 D/0",
            "eof 0",
            "edited 63 65 D | 61/0 7F/0 62/0 7F/0 63/0 64/0 7F/0 65/0 A/0 D/0",
            "to LF 70 D | 70/0 A/0 D/0",
            "after 71 D | 71/0 A/0 D/0",
            "control 78 1A 79 D | 78/0 1A/0 79/0 A/0 D/0",
            "rawkbd 70 71 D |",
            "rawkbd eof 0",
        ]
    );
}

#[test]
fn kbd_opens_once_ends_at_a_ctrl_d_and_drops_a_line_left_at_its_close() {
    let out = program(&[
        "SYS \"XOS_Find\",&4F,\"kbd:\" TO h%:SYS \"XOS_Args\",5,h% TO ,,e%:PROCw(\"fresh eof \"+STR$e%)",
        "SYS \"XOS_Find\",&4F,\"kbd:\" TO s%;f%:SYS \"XOS_Find\",&40,\"kbd:\" TO t%:PROCw(\"second \"+FNerr(s%,f%)+\" \"+STR$t%)",
        "SYS \"XOS_Find\",&4F,\"rawkbd:\" TO r%:PROCw(\"rawkbd too \"+STR$(r%<>0)):SYS \"XOS_Find\",0,r%",
        "PROCkeys(\"a\"+CHR$4+\"b\"+CHR$13):PROCrd(h%,\"ctrl-d\"):SYS \"XOS_Args\",5,h% TO ,,e%:PROCw(\"eof \"+STR$e%)",
        "PROCrd(h%,\"then\"):SYS \"XOS_Find\",0,h%",
        "PROCkeys(CHR$4+CHR$13):SYS \"XOS_Find\",&4F,\"kbd:\" TO h%:SYS \"XOS_Args\",5,h% TO ,,e%:PROCrd(h%,\"first, eof \"+STR$e%+\":\")",
        "SYS \"XOS_Find\",0,h%",
        "PROCkeys(\"xyz\"+CHR$13+\"uv\"+CHR$13):SYS \"XOS_Find\",&4F,\"kbd:\" TO h%:SYS \"XOS_BGet\",,h% TO b%:SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",&4F,\"kbd:\" TO h%:SYS \"XOS_BGet\",,h% TO c%:SYS \"XOS_Find\",0,h%:PROCw(\"across a close \"+CHR$b%+CHR$c%)",
    ]);
    assert_eq!(
        out,
        [
            "fresh eof 0",
            "second err &D6 0",
            "rawkbd too -1",
            "ctrl-d 61 end |",
            "eof -1",
            "then err &DF |",
            "first, eof 0: end |",
            "across a close xu",
        ]
    );
}

#[test]
fn kbd_and_rawkbd_wait_for_keys_until_escape_which_they_acknowledge() {
    let out = program(&[
        "PROCkeys(\"ab\"+CHR$13):SYS \"XOS_Find\",&4F,\"kbd:\" TO h%:SYS \"OS_CallAfter\",60,esc,0",
        "SYS \"XOS_GBPB\",4,h%,blk%,10 TO r%;f%:SYS \"XOS_Byte\",126 TO ,a%",
        "PROCw(\"kbd past its line \"+FNerr(r%,f%)+\" escapes \"+STR$!escapes+\" pending \"+STR$a%):SYS \"XOS_Find\",0,h%",
        "!escapes=0:PROCkeys(\"pq\"):SYS \"XOS_Find\",&4F,\"rawkbd:\" TO h%:SYS \"OS_CallAfter\",60,esc,0",
        "SYS \"XOS_GBPB\",4,h%,blk%,5 TO r%;f%:SYS \"XOS_Byte\",126 TO ,a%",
        "PROCw(\"rawkbd past its keys \"+FNerr(r%,f%)+\" escapes \"+STR$!escapes+\" pending \"+STR$a%)",
        "SYS \"XOS_Args\",5,h% TO ,,e%:PROCw(\"rawkbd eof \"+STR$e%):SYS \"XOS_Find\",0,h%",
    ]);
    assert_eq!(
        out,
        [
            "kbd past its line err &11 escapes 3 pending 0",
            "rawkbd past its keys err &11 escapes 3 pending 0",
            "rawkbd eof 0",
        ]
    );
}

#[test]
fn kbd_takes_255_characters_a_line_and_bells_the_rest() {
    let out = program(&[
        "SYS \"OS_Claim\",3,wrch,0:!kcount=0:!ktarget=300:SYS \"OS_CallEvery\",1,typed,0",
        "SYS \"XOS_Find\",&4F,\"kbd:\" TO h%:n%=0:REPEAT:SYS \"XOS_BGet\",,h% TO b%;f%:n%+=1:UNTIL b%=13 OR (f% AND 3)",
        "SYS \"OS_RemoveTickerEvent\",typed,0:SYS \"OS_Release\",3,wrch,0:SYS \"XOS_Find\",0,h%",
        "x%=0:g%=0:p%=log%:WHILE p%<!logp:IF !p%=120 THEN x%+=1 ELSE IF !p%=7 THEN g%+=1",
        "p%+=8:ENDWHILE:PROCw(\"read \"+STR$n%+\" echoed \"+STR$x%+\" bells \"+STR$g%)",
    ]);
    assert_eq!(out, ["read 256 echoed 255 bells 45"]);
}

#[test]
fn printer_writes_to_the_file_its_printer_type_names() {
    let out = program_with(
        &[
            "FOR i%=0 TO 7:v$=\"HostFS:$.Prn\"+STR$i%:SYS \"XOS_SetVarVal\",\"PrinterType$\"+STR$i%,v$,LEN v$,0,0:NEXT",
            "SYS \"XOS_SetVarVal\",\"PrinterType$9\",\"nosuchfs:x\",10,0,0:SYS \"XOS_SetVarVal\",\"PrinterType$16\",\"printer#16:\",11,0,0",
            "SYS \"OS_Byte\",245,0,255 TO ,c%:PROCw(\"configured \"+STR$c%)",
            "PROCpr(\"printer:\",\"A\"):PROCpr(\"printer#sink:\",\"B\"):PROCpr(\"printer#Centronics:\",\"C\"):PROCpr(\"printer#rs423:\",\"D\"+CHR$10+\"d\")",
            "PROCpr(\"printer#user:\",\"E\"):PROCpr(\"printer#05:\",\"F\"):PROCpr(\"printer#8:\",\"G\"):PROCpr(\"printer#9:\",\"H\")",
            "PROCpr(\"printer#16:\",\"I\"):PROCpr(\"printer#16_6:\",\"J\")",
            "FOR i%=1 TO 6:READ n$:SYS \"XOS_Find\",&8F,n$ TO h%;f%:PROCw(n$+\" \"+FNerr(h%,f%)):IF (f% AND 1)=0 THEN SYS \"XOS_Find\",0,h%",
            "NEXT",
            "DATA printer#256:,printer#2x:,printer#2_:,printer#&10:,printer#-1:,printer#sin:",
            "$blk%=\"save\":SYS \"XOS_File\",10,\"printer#7:\",&FFF,,blk%,blk%+4 TO r%;f%:PROCw(\"save \"+FNerr(r%,f%))",
            "FOR i%=0 TO 7:PROCw(STR$i%+\" \"+FNfile(\"HostFS:$.Prn\"+STR$i%)):NEXT",
        ],
        &[
            "DEF PROCpr(n$,d$):LOCAL h%,f%,i%:SYS \"XOS_Find\",&8F,n$ TO h%;f%:IF f% AND 1 THEN PROCw(n$+\" \"+FNerr(h%,f%)):ENDPROC",
            "FOR i%=1 TO LEN d$:SYS \"XOS_BPut\",ASC MID$(d$,i%,1),h%:NEXT:SYS \"XOS_Find\",0,h%:ENDPROC",
        ],
    );
    assert_eq!(
        out,
        [
            "configured 1",
            "printer#9: err &F8",
            "printer#16: err &D6",
            "printer#256: err &16C",
            "printer#2x: err &16B",
            "printer#2_: err &16B",
            "printer#&10: err &10E02",
            "printer#-1: err &10E02",
            "printer#sin: err &10E02",
            "save ok",
            "0 B",
            "1 C",
            "2 D|Jd",
            "3 E",
            "4 none",
            "5 F",
            "6 J",
            "7 save",
        ]
    );
}

#[test]
fn a_printer_stream_whose_file_was_closed_under_it_still_closes() {
    let out = program(&[
        "SYS \"XOS_SetVarVal\",\"PrinterType$3\",\"HostFS:$.Prn3\",13,0,0",
        "SYS \"XOS_Find\",&8F,\"printer#3:\" TO h%:SYS \"XOS_Args\",254,h%-1 TO s%:PROCw(\"target open \"+STR$((s% AND &800)=0))",
        "SYS \"XOS_Find\",0,h%-1:SYS \"XOS_Find\",0,h% TO r%;f%:SYS \"XOS_Args\",254,h% TO s%",
        "PROCw(\"closed \"+FNerr(r%,f%)+\" \"+STR$((s% AND &800)<>0))",
    ]);
    assert_eq!(out, ["target open -1", "closed ok -1"]);
}

#[test]
fn a_printer_whose_file_starts_system_devices_afresh_still_opens_or_refuses() {
    let out = program(&[
        "DIM drv% 256:FOR pass%=0 TO 2 STEP 2:P%=drv%",
        "[OPT pass%",
        ".entry STMFD R13!,{R0-R3,R14}:CMP R0,#0:BNE out:MOV R0,#3:ADR R1,module:SWI \"XOS_Module\"",
        ".out LDMFD R13!,{R0-R3,R14}:CMP R0,R0:MOV PC,R14",
        ".module EQUS \"SystemDevices\"+CHR$0:ALIGN",
        ".devices EQUD name-devices:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0",
        ".name EQUS \"Afresh\"+CHR$0:ALIGN",
        "]:NEXT",
        "SYS \"DeviceFS_Register\",0,devices,entry,0,0,0,1,1 TO dh%",
        "SYS \"XOS_SetVarVal\",\"PrinterType$5\",\"devices:$.Afresh\",16,0,0",
        "SYS \"XOS_Find\",&8F,\"printer#5:\" TO h%;f%:IF (f% AND 1)=0 AND h%<>0 THEN SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",&8F,\"null:\" TO n%;f%:PROCw(\"null \"+FNerr(n%,f%)):SYS \"XOS_Find\",0,n%",
        "SYS \"DeviceFS_Deregister\",dh%",
    ]);
    assert_eq!(out, ["null ok"]);
}
