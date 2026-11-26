//! DeviceFS held to what RISC OS 5's DeviceFS 0.73 did, recorded on RPCEmu:
//! a driver in BASIC's assembler logs every entry it is called at and
//! every `UpCall`, and each line of the log is genuine's, the addresses that
//! differ between the machines shown as what they are.

mod common;

use common::programs::run;

/// The driver and the routines around it, assembled in two passes: the
/// entry logs its reason, R2 to R7 and its mode, answers `Initialise` with
/// &5300 for a stream that receives and &5301 for one that sends, takes
/// every byte it is woken to send while `txtake` is set, gives `rxcount`
/// bytes from `rxbyte` up when woken to receive, answers `EndOfData` with
/// `eodanswer`, a buffer's handle with `bufsugg` unless it is -2, fails its
/// monitoring while `monfail` is set and refuses every reason whose bit
/// `refuse` sets, as it fails its monitoring. `upcall` logs `UpCall`s 7, 10 and
/// 11 and counts 6; `tick` gives a byte, sets Escape or drains, as `tmode`
/// says; `esch` counts Escapes so BASIC never sees them.
const DRIVER: &[&str] = &[
    "DIM code% 4096, log% 8192, blk% 512, sent% 256, nm% 64:dh2%=0",
    "FOR pass%=0 TO 2 STEP 2",
    "P%=code%",
    "[OPT pass%",
    ".logp EQUD 0",
    ".logend EQUD 0",
    ".srx EQUD 0",
    ".stx EQUD 0",
    ".txtake EQUD 1",
    ".txanswer EQUD -1",
    ".rxcount EQUD 1",
    ".rxbyte EQUD 65",
    ".eodanswer EQUD -1",
    ".bufsugg EQUD -2",
    ".monfail EQUD 0",
    ".refuse EQUD 0",
    ".sentp EQUD 0",
    ".n6 EQUD 0",
    ".tmode EQUD 0",
    ".escapes EQUD 0",
    ".drv STMFD R13!,{R0-R11,R14}:LDR R11,logp:LDR R10,logend:CMP R11,R10:BHS dsel",
    "STMIA R11!,{R0-R7}:MRS R10,CPSR:STR R10,[R11],#4:STR R10,[R11],#4:STR R11,logp",
    ".dsel LDR R10,refuse:MOV R9,#1:TST R10,R9,LSL R0:BNE dfail:CMP R0,#0:BEQ dinit:CMP R0,#2:BEQ dwtx:CMP R0,#3:BEQ dwrx:CMP R0,#6:BEQ dbuf:CMP R0,#7:BEQ dbuf",
    "CMP R0,#10:BEQ deod:CMP R0,#12:BEQ dmon:CMP R0,#13:BEQ dmon",
    ".dout LDMFD R13!,{R0-R11,R14}:CMP R0,R0:MOV PC,R14",
    ".dinit TST R3,#1:STRNE R2,stx:STREQ R2,srx:MOV R10,#&5300:ADDNE R10,R10,#1:STR R10,[R13,#8]:B dout",
    ".dwtx LDR R10,txtake:CMP R10,#0:BEQ dwtxa",
    ".dwtxl LDR R1,stx:SWI \"XDeviceFS_TransmitCharacter\":BCS dwtxa",
    "LDR R11,sentp:STRB R0,[R11],#1:STR R11,sentp:B dwtxl",
    ".dwtxa LDR R10,txanswer:CMN R10,#1:STRNE R10,[R13]:B dout",
    ".dwrx LDR R9,rxcount",
    ".dwrxl SUBS R9,R9,#1:BMI dout:LDR R0,rxbyte:ADD R10,R0,#1:STR R10,rxbyte:LDR R1,srx",
    "SWI \"XDeviceFS_ReceivedCharacter\":B dwrxl",
    ".dbuf LDR R10,bufsugg:CMN R10,#2:STRNE R10,[R13,#20]:B dout",
    ".deod LDR R10,eodanswer:STR R10,[R13,#12]:B dout",
    ".dmon LDR R10,monfail:CMP R10,#0:BEQ dout",
    ".dfail LDMFD R13!,{R0-R11,R14}",
    "ADR R0,monerr:MSR CPSR_f,#&10000000:MOV PC,R14",
    ".monerr EQUD &1234:EQUS \"Monitor\"+CHR$0:ALIGN",
    ".upcall STMFD R13!,{R0-R11,R14}:CMP R0,#6:LDREQ R10,n6:ADDEQ R10,R10,#1:STREQ R10,n6:BEQ upout",
    "CMP R0,#7:CMPNE R0,#10:CMPNE R0,#11:BNE upout",
    "LDR R11,logp:LDR R10,logend:CMP R11,R10:BHS upout",
    "ORR R10,R0,#&55000000:STR R10,[R11],#4:STMIA R11!,{R1-R7}:STR R10,[R11],#4:STR R10,[R11],#4:STR R11,logp",
    ".upout LDMFD R13!,{R0-R11,PC}",
    ".tick STMFD R13!,{R0-R4,R14}:MRS R4,CPSR:ORR R3,R4,#3:MSR CPSR_c,R3:MOV R0,R0:STMFD R13!,{R14}",
    "LDR R3,tmode:CMP R3,#1:BNE tick2:LDR R0,rxbyte:LDR R1,srx:SWI \"XDeviceFS_ReceivedCharacter\":B tickd",
    ".tick2 CMP R3,#2:BNE tick4:MOV R0,#125:SWI \"XOS_Byte\":B tickd",
    ".tick4 LDR R1,stx:SWI \"XDeviceFS_TransmitCharacter\":BCS tickd",
    "LDR R2,sentp:STRB R0,[R2],#1:STR R2,sentp:B tick4",
    ".tickd LDMFD R13!,{R14}:MSR CPSR_c,R4:MOV R0,R0:LDMFD R13!,{R0-R4,PC}",
    ".esch STMFD R13!,{R0}:LDR R0,escapes:ADD R0,R0,#1:STR R0,escapes:LDMFD R13!,{R0}:MOV PC,R14",
    ".devs EQUD n1-devs:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0",
    ".dev2 EQUD n2-dev2:EQUD 3:EQUD 0:EQUD 32:EQUD 0:EQUD 32:EQUD 0",
    "EQUD 0",
    ".n1 EQUS \"PrbU\"+CHR$0:.n2 EQUS \"PrbB\"+CHR$0:ALIGN",
    ".devs2 EQUD n3-devs2:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0",
    ".n3 EQUS \"PrbX\"+CHR$0:ALIGN",
    ".devs3 EQUD n4-devs3:EQUD 1:EQUD 8:EQUD 16:EQUD 0:EQUD 16:EQUD 0:EQUD 0",
    ".n4 EQUS \"PrbT\"+CHR$0:ALIGN",
    ".valid EQUS \"baud,b/Ndata/Nmike,dennis/S\"+CHR$0:ALIGN",
    "]",
    "NEXT pass%",
    "!logp=log%:!logend=log%+8000:!sentp=sent%:fh%=0:DIM regs$(14),uregs$(11)",
    "regs$()=\"23457\",\"25\",\"2\",\"2345\",\"2345\",\"\",\"234567\",\"234567\",\"2\",\"2\",\"23\",\"234\",\"2\",\"2\",\"123\"",
    "uregs$(7)=\"1\":uregs$(10)=\"12345\":uregs$(11)=\"123456\"",
    "SYS \"OS_Claim\",&1D,upcall,0",
    "SYS \"OS_ChangeEnvironment\",9,esch,0,0 TO ,oe1%,oe2%,oe3%",
];

/// What each test's program ends with: the claim let go and BASIC's escape
/// handler back.
const END: &[&str] = &[
    "SYS \"OS_Release\",&1D,upcall,0",
    "SYS \"OS_ChangeEnvironment\",9,oe1%,oe2%,oe3%",
];

/// The procedures the programs print the log with: an entry as `e<reason>`
/// with the registers its reason gives and whether interrupts were on, an
/// `UpCall` as `u<number>` with its; each register a token for what it is
/// where it is an address the machines lay out apart.
const PROCEDURES: &[&str] = &[
    "DEF PROClog:LOCAL p%:p%=log%:WHILE p%<!logp:PROCw(FNrec(p%)):p%+=40:ENDWHILE:!logp=log%:ENDPROC",
    "DEF FNrec(p%):LOCAL S$,R$,i%,n%,u%:n%=!p%:u%=(n% AND &FF000000)=&55000000",
    "IF u% THEN S$=\"u\"+STR$(n% AND &FF):R$=uregs$(n% AND &FF) ELSE IF n%>=0 AND n%<15 THEN S$=\"e\"+STR$n%:R$=regs$(n%) ELSE S$=\"c\"+STR$~n%:R$=\"1234567\"",
    "IF n%=5 THEN =S$+\" \"+FNz(p%!8)",
    "FOR i%=1 TO LEN R$:S$+=\" \"+FNt(p%!(4*VAL MID$(R$,i%,1))):NEXT",
    "IF u% THEN =S$",
    "IF p%!32 AND &80 THEN =S$+\" off\" ELSE =S$+\" on\"",
    "DEF FNt(v%):IF v%=!srx AND v%<>0 THEN =\"srx\"",
    "IF v%=!stx AND v%<>0 THEN =\"stx\"",
    "IF v%=!srx+&34 AND !srx<>0 THEN =\"rpoll\"",
    "IF v%=!stx+&34 AND !stx<>0 THEN =\"tpoll\"",
    "IF v%=dh% AND dh%<>0 THEN =\"drv\"",
    "IF v%=dh2% AND dh2%<>0 THEN =\"drv2\"",
    "IF v%=fh% AND fh%<>0 THEN =\"file\"",
    "IF v%=blk% THEN =\"blk\"",
    "IF v%=nm%+2 THEN =\"name\"",
    "=STR$~v%",
    "DEF FNz(a%):LOCAL z$:WHILE ?a%>=32 AND LEN z$<200:z$+=CHR$?a%:a%+=1:ENDWHILE:=z$",
    "DEF PROCentries(a%,n%):LOCAL i%:FOR i%=1 TO n%",
    "PROCw(\"entry \"+FNz(a%+20)+\" \"+STR$~(a%!0 AND &FFFFFF00)+\" \"+STR$(a%!8)+\" \"+STR$(a%!12)+\" \"+STR$(a%!16))",
    "a%=(a%+20+LEN FNz(a%+20)+4) AND -4:NEXT:ENDPROC",
    "DEF FNs(a%,n%):LOCAL i%,S$:FOR i%=0 TO n%-1:S$+=CHR$(a%?i%):NEXT:=S$",
    "DEF FNused:LOCAL f%,s%:SYS \"OS_Module\",5 TO ,,,f%:SYS \"OS_ReadDynamicArea\",1 TO ,s%:=s%-f%",
];

/// The program `body` makes with the driver before it and the end after.
fn program(body: &[&str]) -> Vec<String> {
    let lines: Vec<&str> = DRIVER.iter().chain(body).chain(END).copied().collect();
    run(&lines, PROCEDURES, b"")
}

#[test]
fn registered_devices_are_catalogued_with_their_variables() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%;f%:PROCw(\"register \"+FNerr(dh%,f%))",
        "SYS \"XDeviceFS_Register\",3,devs,drv,0,0,0,1,1 TO r%;f%:PROCw(\"block \"+FNerr(r%,f%))",
        "SYS \"XOS_File\",5,\"devices:$\" TO t%,,,,,a%:PROCw(\"root \"+STR$t%+\" \"+STR$a%)",
        "SYS \"XOS_File\",5,\"devices:$.PrbU\" TO t%,,l%,,n%,a%:PROCw(\"PrbU \"+STR$t%+\" \"+STR$~(l% AND &FFFFFF00)+\" \"+STR$n%+\" \"+STR$a%)",
        "SYS \"XOS_File\",5,\"devices:$.prbb\" TO t%:PROCw(\"prbb \"+STR$t%)",
        "SYS \"XOS_File\",5,\"devices:$.None\" TO t%:PROCw(\"None \"+STR$t%)",
        "SYS \"XOS_ReadVarVal\",\"DeviceFS$PrbU$Options\",blk%,200,0,3 TO ,,n%,,k%:PROCw(\"options \"+STR$n%+\" \"+STR$k%)",
        "SYS \"XOS_ReadVarVal\",\"PrbB$Path\",blk%,200,0,3 TO ,,n%,,k%:PROCw(\"path \"+FNs(blk%,n%)+\" \"+STR$k%)",
        "SYS \"XOS_ReadVarVal\",\"PrbU$Path\",blk%,200,0,3 TO r0%;f%:PROCw(\"no path \"+FNerr(r0%,f%))",
        "SYS \"XOS_GBPB\",10,\"devices:$\",blk%,8,0,200,0 TO ,,,n%,nx%:PROCw(\"listed \"+STR$n%+\" \"+STR$nx%)",
        "PROCentries(blk%,n%):PROClog",
    ]);
    assert_eq!(
        out,
        [
            "register ok",
            "block err &13509",
            "root 2 3",
            "PrbU 1 FFFFCC00 0 3",
            "prbb 1",
            "None 0",
            "options 0 0",
            "path devices#:$.PrbB. 2",
            "no path err &124",
            "listed 2 2",
            "entry PrbU FFFFCC00 0 3 1",
            "entry PrbB FFFFCC00 0 3 1",
            "e5 $",
        ]
    );
}

#[test]
fn an_unbuffered_stream_reads_a_byte_its_driver_gives_when_woken() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO fh%:PROClog",
        "SYS \"XOS_BGet\",,fh% TO b%:PROCw(\"byte \"+STR$b%):PROClog",
        "SYS \"XOS_Args\",254,fh% TO s%:SYS \"XOS_Args\",2,fh% TO ,,e%:PROCw(\"status \"+STR$~s%+\" ext \"+STR$e%)",
        "SYS \"XOS_Find\",0,fh%:PROClog",
    ]);
    assert_eq!(
        out,
        [
            "e0 srx 80000000 file 1 4F on",
            "e11 5300 FFFFFFFF file on",
            "u10 drv 0 file 5300 FFFFFFFF",
            "byte 65",
            "e3 5300 1 0 FFFFFFFF on",
            "e4 5300 1 0 FFFFFFFF on",
            "status 440 ext 0",
            "u7 rpoll",
            "e1 5300 80000000 on",
            "u11 drv 0 file 5300 80000000 FFFFFFFF",
        ]
    );
}

#[test]
fn an_unbuffered_byte_written_wakes_the_driver_which_takes_it() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&8F,\"devices:$.PrbU\" TO fh%:PROClog",
        "SYS \"XOS_Args\",254,fh% TO s%:SYS \"XOS_Args\",2,fh% TO ,,e%:PROCw(\"status \"+STR$~s%+\" ext \"+STR$e%)",
        "SYS \"XOS_BPut\",&51,fh%:PROClog:PROCw(\"sent \"+FNs(sent%,!sentp-sent%))",
        "SYS \"XOS_Find\",0,fh%:PROClog",
    ]);
    assert_eq!(
        out,
        [
            "e0 stx 80000001 file 1 8F on",
            "e11 5301 FFFFFFFF file on",
            "u10 drv FFFFFFFF file 5301 FFFFFFFF",
            "status 580 ext 1",
            "e2 5301 on",
            "sent Q",
            "e10 5301 FFFFFFFF on",
            "u7 tpoll",
            "e1 5301 80000001 on",
            "u11 drv FFFFFFFF file 5301 80000001 FFFFFFFF",
        ]
    );
}

#[test]
fn a_buffered_stream_reads_what_its_driver_puts_in_its_buffer() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%:!rxcount=3",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbB\" TO fh%:PROClog",
        "SYS \"XOS_BGet\",,fh% TO b%:PROCw(\"byte \"+STR$b%):PROClog",
        "SYS \"XOS_Args\",254,fh% TO s%:SYS \"XOS_Args\",2,fh% TO ,,e%:PROCw(\"status \"+STR$~s%+\" ext \"+STR$e%)",
        "SYS \"XOS_GBPB\",4,fh%,blk%,2 TO ,,,l%:PROCw(\"gbpb left \"+STR$l%+\" \"+FNs(blk%,2)):PROClog",
        "SYS \"XOS_Find\",0,fh%:PROClog",
    ]);
    assert_eq!(
        out,
        [
            "e0 srx 80000000 file 1 4F on",
            "e7 5300 0 20 FFFFFFFF FFFFFFFF 4F on",
            "e11 5300 100 file on",
            "u10 drv 0 file 5300 100",
            "byte 65",
            "e3 5300 1 0 100 on",
            "e4 5300 0 1 100 on",
            "status 450 ext 2",
            "gbpb left 0 BC",
            "e3 5300 2 0 100 on",
            "e4 5300 0 2 100 on",
            "u7 rpoll",
            "e1 5300 C0000000 on",
            "u11 drv 0 file 5300 C0000000 100",
        ]
    );
}

#[test]
fn a_buffered_stream_written_wakes_its_dormant_driver_with_interrupts_off() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&8F,\"devices:$.PrbB\" TO fh%:PROClog",
        "SYS \"XOS_Args\",254,fh% TO s%:SYS \"XOS_Args\",2,fh% TO ,,e%:PROCw(\"status \"+STR$~s%+\" ext \"+STR$e%)",
        "$blk%=\"abcdefgh\":SYS \"XOS_GBPB\",2,fh%,blk%,8 TO ,,,l%:PROCw(\"gbpb left \"+STR$l%):PROClog",
        "SYS \"XOS_BPut\",&51,fh%:PROClog:PROCw(\"sent \"+FNs(sent%,!sentp-sent%))",
        "SYS \"XOS_Find\",0,fh%:PROClog",
    ]);
    assert_eq!(
        out,
        [
            "e0 stx 80000001 file 1 8F on",
            "e6 5301 0 20 FFFFFFFF FFFFFFFF 8F on",
            "e11 5301 100 file on",
            "u10 drv FFFFFFFF file 5301 100",
            "status 590 ext 32",
            "gbpb left 0",
            "e2 5301 off",
            "e2 5301 off",
            "sent abcdefghQ",
            "e10 5301 FFFFFFFF on",
            "u7 tpoll",
            "e1 5301 C0000001 on",
            "u11 drv FFFFFFFF file 5301 C0000001 100",
        ]
    );
}

/// A procedure printing the special field block the last `Initialise` was
/// given: its three words, the text it points at for a driver with no
/// validation string, or none.
const BLOCK: &str = "DEF PROCblock(n%):LOCAL b%:b%=log%!24:IF b%=0 THEN PROCw(\"block none\") ELSE IF n%=0 THEN PROCw(\"text \"+FNz(b%)) ELSE PROCw(\"block \"+STR$~!b%+\" \"+STR$~b%!4+\" \"+STR$~b%!8)";

fn with_block(body: &[&str]) -> Vec<String> {
    let lines: Vec<&str> = DRIVER.iter().chain(body).chain(END).copied().collect();
    let procedures: Vec<&str> = PROCEDURES
        .iter()
        .copied()
        .chain([BLOCK, "ENDPROC"])
        .collect();
    run(&lines, &procedures, b"")
}

#[test]
fn a_special_field_is_read_by_the_validation_string_then_the_device_s_options() {
    let out = with_block(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,valid,1,1 TO dh%",
        "SYS \"XOS_Find\",&4F,\"devices#baud9600;data7;mike:$.PrbU\" TO fh%:PROCblock(3):SYS \"XOS_Find\",0,fh%:!logp=log%",
        "SYS \"XOS_Find\",&4F,\"devices#Dennis:$.PrbU\" TO fh%:PROCblock(3):SYS \"XOS_Find\",0,fh%:!logp=log%",
        "SYS \"XOS_Find\",&4F,\"devices#baud:$.PrbU\" TO r0%;f%:PROCw(\"no number \"+FNerr(r0%,f%)):!logp=log%",
        "SYS \"XOS_SetVarVal\",\"DeviceFS$PrbU$Options\",\"baud110;dennis\",14,0,0",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO fh%:PROCblock(3):SYS \"XOS_Find\",0,fh%:!logp=log%",
        "SYS \"XOS_Find\",&4F,\"devices#baud300:$.PrbU\" TO fh%:PROCblock(3):SYS \"XOS_Find\",0,fh%:!logp=log%",
        "SYS \"XDeviceFS_Deregister\",dh%:!logp=log%",
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&4F,\"devices#raw;text:$.PrbU\" TO fh%:PROCblock(0):SYS \"XOS_Find\",0,fh%:!logp=log%",
    ]);
    assert_eq!(
        out,
        [
            "block 2580 7 0",
            "block DEADDEAD DEADDEAD 1",
            "no number err &16B",
            "block 6E DEADDEAD 1",
            "block 12C DEADDEAD 1",
            "text raw;text",
        ]
    );
}

#[test]
fn a_buffer_named_in_the_special_field_is_used_and_kept_one_made_is_removed() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&4F,\"devices#buffer5:$.PrbB\" TO fh%:PROClog",
        "SYS \"XBuffer_GetInfo\",5 TO r0%;f%:PROCw(\"open \"+FNerr(r0%,f%)):SYS \"XOS_Find\",0,fh%:!logp=log%",
        "SYS \"XBuffer_GetInfo\",5 TO r0%;f%:PROCw(\"closed \"+FNerr(r0%,f%))",
        "SYS \"Buffer_Create\",0,64,&200 TO bh%",
        "SYS \"XOS_Find\",&4F,\"devices#buffer512:$.PrbB\" TO fh%:SYS \"XOS_Find\",0,fh%:!logp=log%",
        "SYS \"XBuffer_GetInfo\",bh% TO r0%;f%:PROCw(\"kept \"+FNerr(r0%,f%))",
        "SYS \"XOS_Find\",&4F,\"devices#buffer:$.PrbB\" TO r0%;f%:PROCw(\"no number \"+FNerr(r0%,f%)):!logp=log%",
        "!bufsugg=&300:SYS \"XOS_Find\",&4F,\"devices:$.PrbB\" TO fh%:PROClog:SYS \"XOS_Find\",0,fh%:!logp=log%",
    ]);
    assert_eq!(
        out,
        [
            "e0 srx 80000000 file 1 4F on",
            "e7 5300 0 20 5 FFFFFFFF 4F on",
            "e11 5300 5 file on",
            "u10 drv 0 file 5300 5",
            "open ok",
            "closed err &20702",
            "kept ok",
            "no number err &16B",
            "e0 srx 80000000 file 1 4F on",
            "e7 5300 0 20 FFFFFFFF FFFFFFFF 4F on",
            "e11 5300 300 file on",
            "u10 drv 0 file 5300 300",
        ]
    );
}

#[test]
fn a_stream_past_the_driver_s_limits_or_against_half_duplex_is_refused() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&8F,\"devices:$.PrbU\" TO fh%:!logp=log%",
        "SYS \"XOS_Find\",&8F,\"devices:$.PrbU\" TO r0%;f%:PROCw(\"second out \"+FNerr(r0%,f%)):PROClog",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO h%;f%:PROCw(\"in while out \"+FNerr(h%,f%)):!logp=log%",
        "IF (f% AND 1)=0 THEN SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",0,fh%:!logp=log%:SYS \"XDeviceFS_Deregister\",dh%:!logp=log%",
        "SYS \"XDeviceFS_Register\",0,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&8F,\"devices:$.PrbU\" TO fh%:!logp=log%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO r0%;f%:PROCw(\"half duplex \"+FNerr(r0%,f%)):PROClog",
        "SYS \"XOS_Find\",0,fh%:!logp=log%:SYS \"XDeviceFS_Deregister\",dh%:!logp=log%",
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,0,1 TO dh%",
        "SYS \"XOS_File\",5,\"devices:$.PrbU\" TO ,,,,,a%:PROCw(\"no rx attributes \"+STR$a%)",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO r0%;f%:PROCw(\"no rx \"+FNerr(r0%,f%))",
        "SYS \"XOS_Find\",&40,\"devices:$.PrbU\" TO r0%:PROCw(\"no rx, no error bit \"+STR$r0%)",
    ]);
    assert_eq!(
        out,
        [
            "second out err &D6",
            "u11 drv 0 FD 0 0 1",
            "in while out ok",
            "half duplex err &D6",
            "u11 drv 0 FD 0 0 0",
            "no rx attributes 2",
            "no rx err &D6",
            "no rx, no error bit 0",
        ]
    );
}

#[test]
fn the_catalogue_refuses_changes_but_its_information_and_a_save() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_File\",6,\"devices:$.PrbU\" TO r0%;f%:PROCw(\"delete \"+FNerr(r0%,f%))",
        "SYS \"XOS_FSControl\",25,\"devices:$.PrbU\",\"devices:$.PrbZ\" TO r0%;f%:PROCw(\"rename \"+FNerr(r0%,f%))",
        "SYS \"XOS_File\",8,\"devices:$.Dir\" TO r0%;f%:PROCw(\"cdir \"+FNerr(r0%,f%))",
        "SYS \"XOS_File\",18,\"devices:$.PrbU\",&123 TO r0%;f%:PROCw(\"settype \"+FNerr(r0%,f%))",
        "SYS \"XOS_File\",4,\"devices:$.PrbU\",,,,3 TO r0%;f%:PROCw(\"attributes \"+FNerr(r0%,f%))",
        "SYS \"XOS_File\",16,\"devices:$.PrbU\",blk%,0 TO r0%;f%:PROCw(\"load \"+FNerr(r0%,f%))",
        "SYS \"XOS_Find\",&4F,\"devices:$\" TO r0%;f%:PROCw(\"open root \"+FNerr(r0%,f%))",
        "SYS \"XOS_Find\",&4F,\"devices:\" TO r0%;f%:PROCw(\"open nothing \"+FNerr(r0%,f%))",
        "SYS \"XOS_Find\",&4F,\"devices:$.None\" TO r0%;f%:PROCw(\"open absent \"+FNerr(r0%,f%))",
        "SYS \"XOS_Find\",&8F,\"devices:$.None\" TO r0%;f%:PROCw(\"create absent \"+FNerr(r0%,f%))",
        "SYS \"XOS_Find\",&4F,\"devices:PrbU\" TO r0%;f%:PROCw(\"relative \"+FNerr(r0%,f%)):IF (f% AND 1)=0 THEN SYS \"XOS_Find\",0,r0%",
        "SYS \"XOS_GBPB\",10,\"devices:$.PrbU\",blk%,8,0,200,0 TO r0%;f%:PROCw(\"list a device \"+FNerr(r0%,f%))",
        "!logp=log%:$blk%=\"saved\":SYS \"XOS_File\",10,\"devices:$.PrbU\",&FFF,,blk%,blk%+5 TO r0%;f%:PROCw(\"save \"+FNerr(r0%,f%))",
        "PROCw(\"sent \"+FNs(sent%,!sentp-sent%))",
    ]);
    assert_eq!(
        out,
        [
            "delete err &13501",
            "rename err &13501",
            "cdir err &13501",
            "settype ok",
            "attributes ok",
            "load err &13501",
            "open root err &A8",
            "open nothing err &CC",
            "open absent err &D6",
            "create absent err &D6",
            "relative ok",
            "list a device err &41C",
            "save ok",
            "sent saved",
        ]
    );
}

#[test]
fn call_device_reaches_a_driver_by_handle_by_path_or_every_driver() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XDeviceFS_Register\",2,devs2,drv,&BBBB,&CCCC,0,1,1 TO dh2%",
        "SYS \"XDeviceFS_CallDevice\",&80000001,dh%,1,2,3,4,5,6 TO a%,b%,c%:PROCw(\"answered \"+STR$~a%+\" \"+FNt(b%)+\" \"+STR$c%):PROClog",
        "$nm%=\"$.PrbU\":SYS \"XDeviceFS_CallDevice\",&80000002,nm%:PROClog",
        "$nm%=\"devices:$.PrbU\":SYS \"XDeviceFS_CallDevice\",&80000003,nm% TO r0%;f%:PROCw(\"prefixed \"+FNerr(r0%,f%))",
        "$nm%=\"$.None\":SYS \"XDeviceFS_CallDevice\",&80000003,nm% TO r0%;f%:PROCw(\"absent \"+FNerr(r0%,f%))",
        "SYS \"XDeviceFS_CallDevice\",&80000004,0,7:PROClog",
    ]);
    assert_eq!(
        out,
        [
            "answered 80000001 drv 1",
            "c80000001 drv 1 2 3 4 5 6 on",
            "c80000002 name 0 0 0 0 0 0 on",
            "prefixed err &13505",
            "absent err &13505",
            "c80000004 drv 7 0 0 0 0 0 on",
            "c80000004 drv2 7 0 0 0 0 0 on",
        ]
    );
}

#[test]
fn a_buffered_stream_s_threshold_is_its_buffer_s_and_halts_and_resumes_its_driver() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbB\" TO fh%:!logp=log%",
        "SYS \"XDeviceFS_Threshold\",,!srx,-1 TO ,,t%:PROCw(\"read \"+STR$t%)",
        "SYS \"XDeviceFS_Threshold\",,!srx,5 TO ,,t%:PROCw(\"set \"+STR$t%)",
        "SYS \"XDeviceFS_Threshold\",,!srx,-1 TO ,,t%:PROCw(\"read \"+STR$t%)",
        "SYS \"Buffer_Threshold\",&100,-1 TO ,t%:PROCw(\"buffer \"+STR$t%)",
        "SYS \"XOS_Find\",0,fh%:!logp=log%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO fh%:SYS \"XDeviceFS_Threshold\",,!srx,5 TO r0%;f%",
        "PROCw(\"unbuffered \"+FNerr(r0%,f%)):SYS \"XOS_Find\",0,fh%:!logp=log%",
        "SYS \"XDeviceFS_Register\",2,devs3,drv,&BBBB,&CCCC,0,1,1 TO dh2%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbT\" TO fh%:!logp=log%:SYS \"DeviceFS_Threshold\",,!srx,8",
        "!rxcount=10:SYS \"XOS_BGet\",,fh%:PROClog",
        "!rxcount=0:SYS \"XOS_GBPB\",4,fh%,blk%,5:PROClog",
        "SYS \"XOS_Find\",0,fh%:!logp=log%",
    ]);
    assert_eq!(
        out,
        [
            "read 0",
            "set 0",
            "read 5",
            "buffer 5",
            "unbuffered err &13501",
            "e3 5300 1 0 100 on",
            "e8 5300 on",
            "e4 5300 0 1 100 on",
            "e3 5300 5 0 100 on",
            "e9 5300 on",
            "e4 5300 0 5 100 on",
        ]
    );
}

#[test]
fn a_read_waits_for_a_byte_a_ticker_hands_over_and_escape_ends_it() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO fh%:!logp=log%:!rxcount=0",
        "!tmode=1:!rxbyte=&5A:SYS \"OS_CallAfter\",20,tick,0",
        "SYS \"XOS_BGet\",,fh% TO b%:PROCw(\"later \"+STR$b%+\" escapes \"+STR$!escapes+\" upcall6 \"+STR$!n6):PROClog",
        "!tmode=2:SYS \"OS_CallAfter\",20,tick,0",
        "SYS \"XOS_BGet\",,fh% TO r0%;f%:PROCw(\"escape \"+FNerr(r0%,f%)+\" escapes \"+STR$!escapes)",
        "SYS \"XOS_Byte\",126 TO ,a%:PROCw(\"acknowledged \"+STR$a%):PROClog",
        "SYS \"XOS_Find\",0,fh%:!logp=log%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbB\" TO fh%:!logp=log%:!escapes=0",
        "!tmode=2:SYS \"OS_CallAfter\",20,tick,0",
        "SYS \"XOS_BGet\",,fh% TO r0%;f%:PROCw(\"buffered escape \"+FNerr(r0%,f%)+\" escapes \"+STR$!escapes):PROClog",
        "SYS \"XOS_Find\",0,fh%:!logp=log%",
    ]);
    assert_eq!(
        out,
        [
            "later 90 escapes 0 upcall6 0",
            "e3 5300 1 0 FFFFFFFF on",
            "e4 5300 1 0 FFFFFFFF on",
            "escape err &11 escapes 2",
            "acknowledged 0",
            "e3 5300 1 0 FFFFFFFF on",
            "e4 5300 1 0 FFFFFFFF on",
            "buffered escape err &11 escapes 2",
            "e3 5300 1 0 100 on",
            "e4 5300 1 0 100 on",
        ]
    );
}

#[test]
fn a_full_buffer_written_sleeps_until_drained_and_escape_empties_it() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&8F,\"devices:$.PrbB\" TO fh%:!logp=log%:!txtake=0",
        "FOR i%=0 TO 39:blk%?i%=65+(i% MOD 26):NEXT",
        "!tmode=4:SYS \"OS_CallAfter\",20,tick,0",
        "SYS \"XOS_GBPB\",2,fh%,blk%,40 TO ,,,l%:PROCw(\"written left \"+STR$l%+\" slept \"+STR$(!n6>0)+\" sent \"+STR$(!sentp-sent%)):PROClog",
        "REPEAT:SYS \"XDeviceFS_TransmitCharacter\",,!stx TO ;f%:UNTIL f% AND 2",
        "!tmode=2:SYS \"OS_CallAfter\",20,tick,0",
        "SYS \"XOS_GBPB\",2,fh%,blk%,40 TO r0%,,,l%;f%:PROCw(\"escaped \"+FNerr(r0%,f%)+\" escapes \"+STR$!escapes)",
        "SYS \"XDeviceFS_TransmitCharacter\",,!stx TO ;f%:PROCw(\"emptied \"+STR$((f% AND 2)<>0)):!logp=log%",
        "!txtake=1:SYS \"XOS_Find\",0,fh%:!logp=log%",
    ]);
    assert_eq!(
        out,
        [
            "written left 0 slept -1 sent 31",
            "e2 5301 off",
            "e2 5301 off",
            "escaped err &11 escapes 2",
            "emptied -1",
        ]
    );
}

#[test]
fn io_controls_set_how_a_stream_waits_or_go_to_its_driver() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO fh%:!logp=log%:!rxcount=0",
        "blk%!0=&80000001:blk%!4=9600:SYS \"XOS_Args\",9,fh%,blk%:PROClog",
        "blk%!0=&C0FF0001:blk%!4=1:SYS \"XOS_Args\",9,fh%,blk%:PROCw(\"non-blocking \"+STR$blk%!4)",
        "blk%!0=&40FF0001:blk%!4=99:SYS \"XOS_Args\",9,fh%,blk%:PROCw(\"read \"+STR$blk%!4)",
        "SYS \"XOS_BGet\",,fh% TO ;f%:PROCw(\"none \"+STR$((f% AND 2)<>0)):!logp=log%",
        "SYS \"XOS_Find\",0,fh%:!logp=log%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO fh%:!logp=log%",
        "blk%!0=&80FF0004:blk%!4=1:SYS \"XOS_Args\",9,fh%,blk%",
        "blk%!0=&80FF0005:blk%!4=30:SYS \"XOS_Args\",9,fh%,blk%:!n6=0",
        "SYS \"XOS_BGet\",,fh% TO ;f%:PROCw(\"timed out \"+STR$((f% AND 2)<>0)+\" slept \"+STR$(!n6>0)):!logp=log%",
        "blk%!0=&C0FF0009:SYS \"XOS_Args\",9,fh%,blk% TO r0%;f%:PROCw(\"unknown \"+FNerr(r0%,f%))",
        "SYS \"XOS_Find\",0,fh%:!logp=log%",
    ]);
    assert_eq!(
        out,
        [
            "e14 file 5300 blk on",
            "non-blocking 1",
            "read 1",
            "none -1",
            "timed out -1 slept -1",
            "unknown err &13501",
        ]
    );
}

#[test]
fn a_driver_that_monitors_its_streams_is_called_while_they_wait_and_may_end_it() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",6,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO fh%:!logp=log%:!rxcount=0",
        "!tmode=1:SYS \"OS_CallAfter\",20,tick,0",
        "SYS \"XOS_BGet\",,fh% TO b%:PROCw(\"byte \"+STR$b%):PROClog",
        "!monfail=1:SYS \"XOS_BGet\",,fh% TO r0%;f%:PROCw(\"monitored \"+FNerr(r0%,f%)):PROClog",
        "!monfail=0:SYS \"XOS_Find\",0,fh%:!logp=log%",
    ]);
    assert_eq!(
        out,
        [
            "byte 65",
            "e3 5300 1 0 FFFFFFFF on",
            "e4 5300 1 0 FFFFFFFF on",
            "e13 5300 on",
            "monitored err &1234",
            "e3 5300 1 0 FFFFFFFF on",
            "e4 5300 1 0 FFFFFFFF on",
            "e13 5300 on",
        ]
    );
}

#[test]
fn a_driver_deregistered_closes_its_streams_and_unsets_all_but_its_options() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO fh%:!logp=log%",
        "SYS \"XDeviceFS_Deregister\",dh%:PROClog",
        "SYS \"XOS_File\",5,\"devices:$.PrbU\" TO t%:PROCw(\"gone \"+STR$t%)",
        "SYS \"XOS_ReadVarVal\",\"DeviceFS$PrbU$Options\",blk%,200,0,3 TO r0%;f%:PROCw(\"options \"+FNerr(r0%,f%))",
        "SYS \"XOS_ReadVarVal\",\"PrbB$Path\",blk%,200,0,3 TO r0%;f%:PROCw(\"path \"+FNerr(r0%,f%))",
        "SYS \"XOS_BGet\",,fh% TO r0%;f%:PROCw(\"its stream \"+FNerr(r0%,f%))",
        "SYS \"XOS_Find\",0,fh% TO r0%;f%:PROCw(\"closed \"+FNerr(r0%,f%)):!logp=log%",
        "SYS \"XDeviceFS_Deregister\",dh% TO r0%;f%:PROCw(\"again \"+FNerr(r0%,f%))",
    ]);
    assert_eq!(
        out,
        [
            "u7 rpoll",
            "e1 5300 80000000 on",
            "u11 drv 0 file 5300 80000000 FFFFFFFF",
            "e1 0 0 on",
            "gone 0",
            "options ok",
            "path err &124",
            "its stream err &13505",
            "closed ok",
            "again err &13505",
        ]
    );
}

#[test]
fn a_written_stream_whose_driver_went_still_closes() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&8F,\"devices:$.PrbU\" TO fh%:SYS \"XDeviceFS_Deregister\",dh%:!logp=log%",
        "SYS \"XOS_Find\",0,fh% TO r0%;f%:SYS \"XOS_Args\",254,fh% TO s%",
        "PROCw(\"closed \"+FNerr(r0%,f%)+\" \"+STR$~s%):!logp=log%",
    ]);
    assert_eq!(out, ["closed ok 800"]);
}

#[test]
fn a_device_s_whole_name_opens_it_and_a_longer_path_names_no_device() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "DIM l% 300,m% 300,p% 300:FOR i%=0 TO 28 STEP 4:l%!i%=0:m%!i%=0:NEXT:l%!0=32:m%!0=32",
        "FOR i%=32 TO 286:l%?i%=97:m%?i%=97:NEXT:l%?287=0:m%?286=0",
        "SYS \"XDeviceFS_RegisterObjects\",dh%,l%",
        "$p%=\"devices:$.\":FOR i%=10 TO 264:p%?i%=97:NEXT:p%?265=0",
        "SYS \"XOS_Find\",&4F,p% TO fh%;f%:PROCw(\"255 \"+FNerr(fh%,f%)):IF (f% AND 1)=0 THEN SYS \"XOS_Find\",0,fh%",
        "SYS \"XDeviceFS_RegisterObjects\",dh%,m%",
        "p%?265=97:p%?266=0:SYS \"XOS_Find\",&4F,p% TO fh%;f%:PROCw(\"256 \"+FNerr(fh%,f%))",
        "l%?287=97:l%?288=0:SYS \"XDeviceFS_RegisterObjects\",dh%,l% TO r%;f%:PROCw(\"register 256 \"+FNerr(r%,f%))",
        "SYS \"XDeviceFS_Deregister\",dh%",
    ]);
    assert_eq!(out, ["255 ok", "256 err &D6", "register 256 err &1E4"]);
}

#[test]
fn a_stream_its_device_left_keeps_its_handle_from_the_next_stream_until_closed() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO fh%:x%=!srx",
        "$nm%=\"PrbU\":SYS \"XDeviceFS_DeregisterObjects\",dh%,nm%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbB\" TO fh2%:PROCw(\"another handle \"+STR$(!srx<>x%)):!logp=log%",
        "SYS \"XOS_BGet\",,fh% TO r0%;f%:PROCw(\"stale \"+FNerr(r0%,f%)):PROClog",
        "SYS \"XOS_Find\",0,fh% TO r0%;f%:PROCw(\"stale closed \"+FNerr(r0%,f%)):PROClog",
        "SYS \"XOS_Find\",0,fh2%:SYS \"XDeviceFS_Deregister\",dh%",
    ]);
    assert_eq!(
        out,
        ["another handle -1", "stale err &13505", "stale closed ok"]
    );
}

#[test]
fn an_open_its_driver_refuses_lets_its_stream_go_whatever_finalise_answers() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "!refuse=1<<11 OR 1<<1:SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO r0%;f%:PROCw(\"refused \"+FNerr(r0%,f%))",
        "!refuse=0:SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO fh%;f%:PROCw(\"opened again \"+FNerr(fh%,f%))",
        "SYS \"XOS_Find\",0,fh%:SYS \"XDeviceFS_Deregister\",dh%",
    ]);
    assert_eq!(out, ["refused err &1234", "opened again ok"]);
}

#[test]
fn a_stream_is_at_its_end_as_its_driver_says_unless_a_byte_waits() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO fh%:!logp=log%",
        "SYS \"XOS_Args\",5,fh% TO ,,e%:PROCw(\"end \"+STR$e%):PROClog",
        "!eodanswer=0:SYS \"XOS_Args\",5,fh% TO ,,e%:PROCw(\"more \"+STR$e%):PROClog",
        "SYS \"XDeviceFS_ReceivedCharacter\",&41,!srx:SYS \"XOS_Args\",5,fh% TO ,,e%:PROCw(\"waiting \"+STR$e%):PROClog",
        "SYS \"XOS_Find\",0,fh%:!logp=log%",
    ]);
    assert_eq!(
        out,
        [
            "end -1",
            "e10 5300 FFFFFFFF on",
            "more 0",
            "e10 5300 FFFFFFFF on",
            "waiting 0",
        ]
    );
}

#[test]
fn devices_are_added_and_removed_and_a_name_registered_again_replaces_its_device() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XDeviceFS_RegisterObjects\",dh%,devs2:SYS \"XOS_File\",5,\"devices:$.PrbX\" TO t%:PROCw(\"added \"+STR$t%)",
        "$nm%=\"PrbX\":SYS \"XDeviceFS_DeregisterObjects\",dh%,nm%:SYS \"XOS_File\",5,\"devices:$.PrbX\" TO t%:PROCw(\"removed \"+STR$t%)",
        "SYS \"XDeviceFS_DeregisterObjects\",dh%,nm% TO r0%;f%:PROCw(\"again \"+FNerr(r0%,f%)):!logp=log%",
        "SYS \"XOS_Find\",&4F,\"devices:$.PrbU\" TO fh%:!logp=log%",
        "SYS \"XDeviceFS_Register\",2,devs,drv,1,2,0,1,1 TO dh2%:PROClog",
        "SYS \"XOS_BGet\",,fh% TO r0%;f%:PROCw(\"old stream \"+FNerr(r0%,f%)):SYS \"XOS_Find\",0,fh%:!logp=log%",
        "SYS \"XOS_GBPB\",10,\"devices:$\",blk%,8,0,200,0 TO ,,,n%:PROCw(\"listed \"+STR$n%):!logp=log%",
        "SYS \"XDeviceFS_Deregister\",dh% TO r0%;f%:PROCw(\"first driver \"+FNerr(r0%,f%)):PROClog",
        "SYS \"XOS_File\",5,\"devices:$.PrbU\" TO t%:PROCw(\"kept \"+STR$t%)",
    ]);
    assert_eq!(
        out,
        [
            "added 1",
            "removed 0",
            "again err &13505",
            "u7 rpoll",
            "e1 5300 80000000 on",
            "u11 drv 0 file 5300 80000000 FFFFFFFF",
            "old stream err &13505",
            "listed 2",
            "first driver ok",
            "e1 0 0 on",
            "kept 1",
        ]
    );
}

#[test]
fn a_byte_written_while_the_last_waits_untaken_sleeps_until_it_goes() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,0,1,1 TO dh%",
        "SYS \"XOS_Find\",&8F,\"devices:$.PrbU\" TO fh%:!logp=log%:!txtake=0",
        "SYS \"XOS_BPut\",&51,fh%:!n6=0:!tmode=4:SYS \"OS_CallAfter\",20,tick,0",
        "SYS \"XOS_BPut\",&52,fh%:PROCw(\"slept \"+STR$(!n6>0)+\" sent \"+FNs(sent%,!sentp-sent%)):PROClog",
        "SYS \"XDeviceFS_TransmitCharacter\",,!stx TO b%:PROCw(\"waiting \"+CHR$b%)",
        "!txtake=1:SYS \"XOS_Find\",0,fh%:!logp=log%",
    ]);
    assert_eq!(
        out,
        ["slept -1 sent Q", "e2 5301 on", "e2 5301 on", "waiting R"]
    );
}

#[test]
fn an_open_refused_for_its_buffer_keyword_gives_back_its_special_block() {
    let out = program(&[
        "SYS \"XDeviceFS_Register\",2,devs,drv,&BBBB,&CCCC,valid,1,1 TO dh%",
        "SYS \"XOS_Find\",&4F,\"devices#baud9600;buffer:$.PrbU\" TO r%;f%:PROCw(\"first \"+FNerr(r%,f%))",
        "u%=FNused:SYS \"XOS_Find\",&4F,\"devices#baud9600;buffer:$.PrbU\" TO r%;f%",
        "PROCw(\"again \"+FNerr(r%,f%)+\" held \"+STR$(FNused-u%)):!logp=log%",
        "SYS \"XDeviceFS_Deregister\",dh%:!logp=log%",
    ]);
    assert_eq!(out, ["first err &16B", "again err &16B held 0"]);
}

/// A driver of three devices whose stream handles are their files', which
/// logs each `Finalise` and refuses the one for the file `refuse` holds.
const REFUSING: &[&str] = &[
    "DIM code% 1024, log% 1024",
    "FOR pass%=0 TO 2 STEP 2",
    "P%=code%",
    "[OPT pass%",
    ".logp EQUD 0",
    ".refuse EQUD -1",
    ".drv STMFD R13!,{R0-R4,R14}:CMP R0,#0:BNE dnot0:LDR R4,[R13,#16]:STR R4,[R13,#8]:B dout",
    ".dnot0 CMP R0,#1:BNE dout:LDR R4,logp:STR R2,[R4],#4:STR R4,logp:LDR R3,refuse:CMP R2,R3:BNE dout",
    "LDMFD R13!,{R0-R4,R14}:ADR R0,derr:CMP R0,#&80000000:CMNVC R0,#&80000000:MOV PC,R14",
    ".dout LDMFD R13!,{R0-R4,R14}:CMP R0,R0:MOV PC,R14",
    ".derr EQUD &1234:EQUS \"Refused\"+CHR$0:ALIGN",
    ".devs EQUD n1-devs:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0",
    ".d2 EQUD n2-d2:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0",
    ".d3 EQUD n3-d3:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0:EQUD 0",
    "EQUD 0",
    ".n1 EQUS \"PA\"+CHR$0:.n2 EQUS \"PB\"+CHR$0:.n3 EQUS \"PC\"+CHR$0:ALIGN",
    "]",
    "NEXT pass%",
    "!logp=log%",
];

#[test]
fn shut_closes_from_the_highest_handle_and_a_refused_close_leaves_its_file_open() {
    let body: Vec<&str> = REFUSING
        .iter()
        .copied()
        .chain([
            "SYS \"XDeviceFS_Register\",2,devs,drv,0,0,0,9,9 TO dh%",
            "SYS \"XOS_Find\",&8F,\"devices:$.PA\" TO a%:SYS \"XOS_Find\",&8F,\"devices:$.PB\" TO b%:SYS \"XOS_Find\",&8F,\"devices:$.PC\" TO c%",
            "CLOSE#F%:SYS \"XOS_CLI\",\"Shut\" TO r%;f%:F%=OPENUP\"HostFS:$.Out\":PTR#F%=EXT#F%:PROCw(\"shut \"+FNerr(r%,f%)):PROCfinal",
            "SYS \"XOS_Find\",&8F,\"devices:$.PA\" TO a2%:SYS \"XOS_Find\",&8F,\"devices:$.PB\" TO b2%:SYS \"XOS_Find\",&8F,\"devices:$.PC\" TO c2%",
            "PROCw(\"same handles \"+STR$(a2%=a% AND b2%=b% AND c2%=c%))",
            "CLOSE#F%:!refuse=b%:SYS \"XOS_CLI\",\"Shut\" TO r%;f%:!refuse=-1:F%=OPENUP\"HostFS:$.Out\":PTR#F%=EXT#F%",
            "PROCw(\"refused \"+FNerr(r%,f%)):PROCfinal",
            "SYS \"XOS_Args\",254,a% TO s1%:SYS \"XOS_Args\",254,b% TO s2%:SYS \"XOS_Args\",254,c% TO s3%",
            "PROCw(\"left \"+STR$~s1%+\" \"+STR$~s2%+\" \"+STR$~s3%)",
            "!refuse=b%:SYS \"XOS_Find\",0,b% TO r%;f%:SYS \"XOS_Args\",254,b% TO s2%:PROCw(\"closed \"+FNerr(r%,f%)+\" \"+STR$~s2%)",
            "!refuse=-1:SYS \"XOS_Find\",0,b% TO r%;f%:PROCw(\"closed \"+FNerr(r%,f%)):PROCfinal",
            "SYS \"XDeviceFS_Deregister\",dh%",
        ])
        .collect();
    let procedures = [
        "DEF PROCfinal:LOCAL p%,S$:p%=log%:S$=\"finalised\":WHILE p%<!logp:IF !p%=a% THEN S$+=\" a\" ELSE IF !p%=b% THEN S$+=\" b\" ELSE S$+=\" c\"",
        "p%+=4:ENDWHILE:PROCw(S$):!logp=log%:ENDPROC",
    ];
    let out = run(&body, &procedures, b"");
    assert_eq!(
        out,
        [
            "shut ok",
            "finalised a b c",
            "same handles -1",
            "refused err &1234",
            "finalised a b c",
            "left 800 580 800",
            "closed err &1234 580",
            "closed ok",
            "finalised b b",
        ]
    );
}
