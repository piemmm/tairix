//! PipeFS held to what RISC OS 5's PipeFS 0.25 did, recorded on RPCEmu:
//! pipes opened each way, what their pointers, extents and catalogue say as
//! bytes pass, the `UpCall`s each wait and close makes, saves, loads and
//! creations, dotted names, and `*PipeCopy`.

mod common;

use common::programs::run;

/// `up` logs `UpCall`s 6 and 7, R0, R1 and the word at R1, and as `act`
/// says, an `UpCall` 6 is claimed: 1 writes `Z` through `whandle` first, 2
/// reads a byte through `rhandle`, 3 claims `idle` times doing nothing; the
/// first claim's R1 is kept as `base`, the reader's poll word, which the
/// others are told apart by.
const ROUTINES: &[&str] = &[
    "DIM code% 2048, log% 8192, blk% 1024",
    "FOR pass%=0 TO 2 STEP 2",
    "P%=code%",
    "[OPT pass%",
    ".logp EQUD 0",
    ".logend EQUD 0",
    ".act EQUD 0",
    ".idle EQUD 0",
    ".base EQUD 0",
    ".whandle EQUD 0",
    ".rhandle EQUD 0",
    ".up STMFD R13!,{R0-R4,R14}:CMP R0,#6:CMPNE R0,#7:BNE uppass",
    "LDR R4,logp:LDR R3,logend:CMP R4,R3:BHS upact:STR R0,[R4],#4:STR R1,[R4],#4:LDR R2,[R1]:STR R2,[R4],#4:STR R4,logp",
    ".upact LDR R2,[R13]:CMP R2,#6:BNE uppass:LDR R3,act:CMP R3,#0:BEQ uppass:LDR R4,base:CMP R4,#0:STREQ R1,base",
    "CMP R3,#3:BEQ upidle",
    "MOV R4,#0:STR R4,act:CMP R3,#1:MOVEQ R0,#90:LDREQ R1,whandle:SWIEQ \"XOS_BPut\":CMP R3,#2:LDREQ R1,rhandle:SWIEQ \"XOS_BGet\"",
    ".upclaim LDR R1,[R13,#4]:LDR R2,[R1]:LDR R4,logp:MOV R0,#&AC:STR R0,[R4],#4:STR R1,[R4],#4:STR R2,[R4],#4:STR R4,logp",
    "MOV R0,#0:STR R0,[R13]:LDMFD R13!,{R0-R4,R14}:CMP R0,R0:LDMFD R13!,{PC}",
    ".upidle LDR R4,idle:SUBS R4,R4,#1:STR R4,idle:BPL upclaim",
    ".uppass LDMFD R13!,{R0-R4,PC}",
    "]",
    "NEXT pass%",
    "!logp=log%:!logend=log%+8000",
];

/// `PROClog` writes the `UpCall`s logged, each its number — `claimed` for
/// one taken — its poll word as `r`, `w` or `n` from the reader's, and the
/// word's value as nought or `+`; `PROCinfo` a stream's pointer, extent,
/// allocation, status and end; `PROCi5` what `OS_File` 5 gives; `PROClist`
/// a directory's entries.
const PROCEDURES: &[&str] = &[
    "DEF PROClog(l$):LOCAL p%,S$,d%:S$=l$+\" |\":p%=log%:WHILE p%<!logp:d%=p%!4-!base:IF !p%=&AC THEN S$+=\" claimed\" ELSE S$+=\" \"+STR$!p%",
    "IF d%=0 THEN S$+=\" r\" ELSE IF d%=4 THEN S$+=\" w\" ELSE IF d%=8 THEN S$+=\" n\" ELSE S$+=\" ?\"",
    "IF p%!8=0 THEN S$+=\" 0\" ELSE S$+=\" +\"",
    "p%+=12:ENDWHILE:PROCw(S$):!logp=log%:ENDPROC",
    "DEF PROCinfo(h%,l$):LOCAL p%,e%,a%,s%,f%:SYS \"XOS_Args\",0,h% TO ,,p%:SYS \"XOS_Args\",2,h% TO ,,e%:SYS \"XOS_Args\",4,h% TO ,,a%",
    "SYS \"XOS_Args\",254,h% TO s%:SYS \"XOS_Args\",5,h% TO ,,f%:PROCw(l$+\" \"+STR$p%+\" \"+STR$e%+\" \"+STR$a%+\" \"+STR$~s%+\" \"+STR$f%):ENDPROC",
    "DEF PROCi5(n$,l$):LOCAL t%,a%,b%,c%,d%:SYS \"XOS_File\",5,n$ TO t%,,a%,b%,c%,d%:PROCw(l$+\" \"+STR$t%+\" \"+FNstamp(a%,b%)+\" \"+STR$c%+\" \"+STR$d%):ENDPROC",
    "DEF PROClist(d$):LOCAL a%,n%,i%:SYS \"XOS_GBPB\",10,d$,blk%,8,0,400,0 TO ,,,n%:a%=blk%:FOR i%=1 TO n%",
    "PROCw(\"  \"+FNz(a%+20)+\" \"+STR$a%!16+\" \"+FNstamp(a%!0,a%!4)+\" \"+STR$a%!8+\" \"+STR$a%!12):a%=(a%+20+LEN FNz(a%+20)+4) AND -4",
    "NEXT:PROCw(\"listed \"+STR$n%):ENDPROC",
    "DEF FNstamp(l%,x%):IF (l% AND &FFF00000)=&FFF00000 THEN =STR$~(l% AND &FFFFFF00)+\" dated\"",
    "=STR$~l%+\" \"+STR$~x%",
    "DEF FNz(a%):LOCAL z$:WHILE ?a%>=32 AND LEN z$<200:z$+=CHR$?a%:a%+=1:ENDWHILE:=z$",
    "DEF FNs(a%,n%):LOCAL i%,S$:FOR i%=0 TO n%-1:S$+=CHR$(a%?i%):NEXT:=S$",
];

fn program(body: &[&str]) -> Vec<String> {
    let lines: Vec<&str> = ROUTINES.iter().chain(body).copied().collect();
    run(&lines, PROCEDURES, b"")
}

#[test]
fn a_pipe_opens_once_each_way_and_counts_what_passes_through_it() {
    let out = program(&[
        "SYS \"XOS_Find\",&8F,\"pipe:$.U\" TO wh%:SYS \"XOS_Find\",&4F,\"pipe:$.u\" TO rh%:PROCinfo(wh%,\"w\"):PROCinfo(rh%,\"r\")",
        "FOR i%=1 TO 3:SYS \"XOS_BPut\",64+i%,wh%:NEXT:SYS \"XOS_BGet\",,rh% TO b%:PROCw(\"read \"+STR$b%)",
        "PROCinfo(wh%,\"w\"):PROCinfo(rh%,\"r\")",
        "SYS \"XOS_Find\",&4F,\"pipe:$.U\" TO h%;f%:SYS \"XOS_Find\",&8F,\"pipe:$.U\" TO g%;e%:PROCw(\"again \"+FNerr(h%,f%)+\" \"+FNerr(g%,e%))",
        "PROClist(\"pipe:$\")",
        "SYS \"XOS_File\",5,\"pipe:$.U\" TO r%;f%:SYS \"XOS_File\",6,\"pipe:$.U\" TO s%;e%:PROCw(\"open \"+FNerr(r%,f%)+\" \"+FNerr(s%,e%))",
        "SYS \"XOS_Find\",0,rh%:SYS \"XOS_Find\",0,wh%:SYS \"XOS_File\",5,\"pipe:$.U\" TO t%,,l%,,n%,a%",
        "PROCw(\"closed \"+STR$t%+\" \"+STR$~(l% AND &FFFFFF00)+\" \"+STR$n%+\" \"+STR$a%):PROClist(\"pipe:$\")",
        "SYS \"XOS_Find\",&8F,\"pipe:$.U\" TO wh%:PROCinfo(wh%,\"rewritten\"):SYS \"XOS_BPut\",68,wh%",
        "SYS \"XOS_Find\",&4F,\"pipe:$.U\" TO rh%:PROCinfo(rh%,\"reread\"):SYS \"XOS_Find\",0,wh%:PROCinfo(rh%,\"no writer\")",
        "SYS \"XOS_GBPB\",4,rh%,blk%,10 TO ,,,l%:PROCw(\"rest \"+FNs(blk%,10-l%)+\" left \"+STR$l%):PROCinfo(rh%,\"drained\")",
        "SYS \"XOS_Find\",0,rh%:SYS \"XOS_File\",5,\"pipe:$.U\" TO t%:PROCw(\"gone \"+STR$t%)",
    ]);
    assert_eq!(
        out,
        [
            "w 0 0 256 588 -1",
            "r 0 0 256 448 0",
            "read 65",
            "w 3 3 256 588 -1",
            "r 1 0 256 448 0",
            "again err &D6 err &D6",
            "  U 1 DEADDEAD DEADDEAD 2 3",
            "listed 1",
            "open err &12FC2 err &12FC2",
            "closed 1 FFFFFD00 3 3",
            "  U 1 FFFFFD00 dated 2 3",
            "listed 1",
            "rewritten 3 3 256 588 -1",
            "reread 1 0 256 448 0",
            "no writer 1 3 256 448 0",
            "rest BCD left 7",
            "drained 4 0 256 448 -1",
            "gone 0",
        ]
    );
}

#[test]
fn without_a_task_window_a_read_of_nothing_fails_and_a_full_pipe_grows() {
    let out = program(&[
        "SYS \"XOS_Find\",&8F,\"pipe:$.W\" TO wh%:SYS \"XOS_Find\",&4F,\"pipe:$.W\" TO rh%",
        "SYS \"XOS_BGet\",,rh% TO r%;f%:SYS \"XOS_Args\",5,rh% TO ,,e%:PROCw(\"empty \"+FNerr(r%,f%)+\" eof \"+STR$e%)",
        "FOR i%=1 TO 255:SYS \"XOS_BPut\",i% AND 255,wh%:NEXT:PROCinfo(wh%,\"255\"):SYS \"XOS_BPut\",1,wh%:PROCinfo(wh%,\"256\")",
        "SYS \"XOS_Find\",0,rh%:SYS \"XOS_Find\",0,wh%:SYS \"XOS_File\",6,\"pipe:$.W\"",
        "SYS \"XOS_Find\",&8F,\"pipe:$.E\" TO wh%:SYS \"XOS_Args\",3,wh%,1000:PROCinfo(wh%,\"extended\"):SYS \"XOS_Find\",0,wh%",
        "SYS \"XOS_Find\",&4F,\"pipe:$.E\" TO rh%:SYS \"XOS_GBPB\",4,rh%,blk%,1000 TO ,,,l%:PROCw(\"zeros left \"+STR$l%+\" \"+STR$blk%?999)",
        "SYS \"XOS_Find\",0,rh%",
        "SYS \"XOS_Find\",&8F,\"pipe:$.R\" TO wh%:SYS \"XOS_Find\",&4F,\"pipe:$.R\" TO rh%",
        "FOR i%=1 TO 200:SYS \"XOS_BPut\",i%,wh%:NEXT:SYS \"XOS_GBPB\",4,rh%,blk%,150:FOR i%=1 TO 100:SYS \"XOS_BPut\",i%,wh%:NEXT",
        "PROCinfo(wh%,\"ring w\"):PROCinfo(rh%,\"ring r\"):SYS \"XOS_Find\",0,rh%:SYS \"XOS_Find\",0,wh%:SYS \"XOS_File\",6,\"pipe:$.R\"",
        "SYS \"XOS_Find\",&4F,\"pipe:$.N\" TO rh%:SYS \"XOS_BGet\",,rh% TO r%;f%:SYS \"XOS_Args\",5,rh% TO ,,e%",
        "PROCw(\"never written \"+FNerr(r%,f%)+\" eof \"+STR$e%):SYS \"XOS_Find\",&8F,\"pipe:$.N\" TO wh%:SYS \"XOS_Find\",0,wh%",
        "SYS \"XOS_Args\",5,rh% TO ,,e%:SYS \"XOS_BGet\",,rh% TO r%;f%:PROCw(\"writer gone eof \"+STR$e%+\" end \"+STR$(f% AND 2))",
        "SYS \"XOS_Find\",0,rh%:SYS \"XOS_File\",5,\"pipe:$.N\" TO t%:PROCw(\"n \"+STR$t%)",
    ]);
    assert_eq!(
        out,
        [
            "empty err &12F00 eof 0",
            "255 255 255 256 588 -1",
            "256 256 256 512 588 -1",
            "extended 1000 1000 1024 588 -1",
            "zeros left 0 0",
            "ring w 300 300 256 588 -1",
            "ring r 150 0 256 448 0",
            "never written err &12F00 eof 0",
            "writer gone eof -1 end 2",
            "n 0",
        ]
    );
}

#[test]
fn a_close_a_task_switcher_objects_to_closes_and_keeps_the_pipe_it_polls() {
    let out = program(&[
        "DIM obj% 128:FOR pass%=0 TO 2 STEP 2:P%=obj%",
        "[OPT pass%",
        ".object CMP R0,#7:MOVNE PC,R14:ADR R0,objerr:MSR CPSR_f,#&10000000:LDMFD R13!,{PC}",
        ".objerr EQUD &1717:EQUS \"Polled\"+CHR$0:ALIGN",
        "]",
        "NEXT pass%",
        "SYS \"XOS_Find\",&8F,\"pipe:$.O\" TO wh%:SYS \"XOS_Find\",&4F,\"pipe:$.O\" TO rh%",
        "SYS \"OS_Claim\",&1D,object,0",
        "SYS \"XOS_Find\",0,wh% TO r%;f%:PROCw(\"writer \"+FNerr(r%,f%))",
        "SYS \"XOS_Find\",0,rh% TO r%;f%:PROCw(\"reader \"+FNerr(r%,f%))",
        "SYS \"OS_Release\",&1D,object,0",
        "SYS \"XOS_Args\",254,wh% TO s%:SYS \"XOS_Args\",254,rh% TO t%",
        "PROCw(\"handles free \"+STR$((s% AND t% AND &800)<>0))",
        "SYS \"XOS_File\",5,\"pipe:$.O\" TO t%:PROCw(\"kept \"+STR$t%)",
        "SYS \"XOS_File\",6,\"pipe:$.O\":SYS \"XOS_File\",5,\"pipe:$.O\" TO t%:PROCw(\"deleted \"+STR$t%)",
    ]);
    assert_eq!(
        out,
        [
            "writer ok",
            "reader ok",
            "handles free -1",
            "kept 1",
            "deleted 0"
        ]
    );
}

#[test]
fn a_listing_window_reads_entries_in_order_each_directory_once_where_it_first_is() {
    let out = program(&[
        "SYS \"XOS_Find\",&8F,\"pipe:$.D1.x\" TO h%:SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",&8F,\"pipe:$.P\" TO h%:SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",&8F,\"pipe:$.d1.y\" TO h%:SYS \"XOS_Find\",0,h%",
        "SYS \"XOS_Find\",&8F,\"pipe:$.D2.x\" TO h%:SYS \"XOS_Find\",0,h%",
        "FOR o%=0 TO 3:SYS \"XOS_GBPB\",10,\"pipe:$\",blk%,1,o%,400,0 TO ,,,n%,x%:S$=STR$o%+\" \"+STR$n%",
        "IF n%>0 THEN S$+=\" \"+FNz(blk%+20)",
        "PROCw(S$+\" \"+STR$x%):NEXT",
        "SYS \"XOS_GBPB\",10,\"pipe:$\",blk%,2,1,400,0 TO ,,,n%,x%:PROCw(\"two \"+STR$n%+\" \"+STR$x%)",
        "SYS \"XOS_File\",6,\"pipe:$.D1.x\":SYS \"XOS_File\",6,\"pipe:$.P\"",
        "SYS \"XOS_File\",6,\"pipe:$.D1.y\":SYS \"XOS_File\",6,\"pipe:$.D2.x\"",
    ]);
    assert_eq!(
        out,
        ["0 1 D1 1", "1 1 P 2", "2 1 D2 3", "3 0 -1", "two 2 3"]
    );
}

#[test]
fn a_name_longer_than_a_pipe_s_names_no_pipe() {
    let out = program(&[
        "$blk%=\"pipe:$.\":FOR i%=7 TO 261:blk%?i%=97:NEXT:blk%?262=0",
        "SYS \"XOS_Find\",&8F,blk% TO wh%;f%:PROCw(\"255 \"+FNerr(wh%,f%))",
        "blk%?262=97:blk%?263=0:SYS \"XOS_Find\",&4F,blk% TO rh%;f%:PROCw(\"256 \"+FNerr(rh%,f%))",
        "SYS \"XOS_Find\",0,wh%:blk%?262=0:SYS \"XOS_File\",6,blk%",
    ]);
    assert_eq!(out, ["255 ok", "256 err &12FF8"]);
}

#[test]
fn an_extent_past_all_memory_grows_the_pipe_until_the_rma_refuses() {
    let out = program(&[
        "SYS \"XOS_Find\",&8F,\"pipe:$.H\" TO wh%:SYS \"XOS_Args\",3,wh%,&FFFFFFFF TO r%;f%",
        "SYS \"XOS_Args\",2,wh% TO ,,e%:PROCw(\"extended \"+FNerr(r%,f%)+\" \"+STR$(e%>1000000))",
        "SYS \"XOS_Find\",0,wh% TO r%;f%:PROCw(\"closed \"+FNerr(r%,f%)):SYS \"XOS_File\",6,\"pipe:$.H\"",
    ]);
    assert_eq!(out, ["extended err &101 -1", "closed ok"]);
}

#[test]
fn a_task_switcher_is_told_of_each_wait_and_close_and_may_take_a_wait() {
    let out = program(&[
        "SYS \"OS_Claim\",&1D,up,0",
        "SYS \"XOS_Find\",&8F,\"pipe:$.C\" TO wh%:SYS \"XOS_Find\",&4F,\"pipe:$.C\" TO rh%:!whandle=wh%:!rhandle=rh%",
        "!act=1:SYS \"XOS_BGet\",,rh% TO b%:PROClog(\"claimed read \"+STR$b%)",
        "FOR i%=1 TO 3:SYS \"XOS_BPut\",64+i%,wh%:NEXT:SYS \"XOS_BGet\",,rh% TO b%:PROClog(\"read \"+STR$b%)",
        "SYS \"XOS_Args\",2,rh% TO ,,e%:PROClog(\"reader extent \"+STR$e%)",
        "SYS \"XOS_Find\",&4F,\"pipe:$.C\" TO h%;f%:PROClog(\"second \"+FNerr(h%,f%))",
        "SYS \"XOS_File\",5,\"pipe:$.C\" TO r%;f%:PROClog(\"file5 \"+FNerr(r%,f%))",
        "SYS \"XOS_BGet\",,rh%:SYS \"XOS_BGet\",,rh%:!act=3:!idle=3:SYS \"XOS_BGet\",,rh% TO r%;f%:PROClog(\"idle \"+FNerr(r%,f%))",
        "FOR i%=1 TO 255:SYS \"XOS_BPut\",i% AND 255,wh%:NEXT:PROClog(\"255\")",
        "!act=2:SYS \"XOS_BPut\",1,wh%:PROClog(\"made room\"):PROCinfo(wh%,\"w\")",
        "SYS \"XOS_Find\",0,rh%:PROClog(\"reader closed\"):SYS \"XOS_Find\",0,wh%:PROClog(\"writer closed\")",
        "SYS \"XOS_File\",6,\"pipe:$.C\":PROClog(\"deleted\")",
        "SYS \"OS_Release\",&1D,up,0",
    ]);
    assert_eq!(
        out,
        [
            "claimed read 90 | 6 r 0 claimed r +",
            "read 65 |",
            "reader extent 0 | 6 n 0",
            "second err &D6 |",
            "file5 err &12FC2 | 6 n 0",
            "idle err &12F00 | 6 r 0 claimed r 0 6 r 0 claimed r 0 6 r 0 claimed r 0 6 r 0",
            "255 |",
            "made room | 6 w 0 claimed w +",
            "w 260 260 256 588 -1",
            "reader closed | 7 r + 7 n 0",
            "writer closed | 7 w +",
            "deleted | 7 n +",
        ]
    );
}

#[test]
fn a_save_makes_a_pipe_a_load_takes_it_and_a_creation_empties_it() {
    let out = program(&[
        "$blk%=\"abcd\":SYS \"XOS_File\",10,\"pipe:$.S1\",&FFF,,blk%,blk%+3:PROCi5(\"pipe:$.S1\",\"save\")",
        "SYS \"XOS_File\",0,\"pipe:$.S2\",&1234,&5678,blk%,blk%+4:PROCi5(\"pipe:$.S2\",\"save0\")",
        "SYS \"XOS_File\",7,\"pipe:$.S3\",&1111,&2222,0,10:PROCi5(\"pipe:$.S3\",\"create\")",
        "SYS \"XOS_Find\",&8F,\"pipe:$.S5\" TO h%:SYS \"XOS_Find\",0,h%:SYS \"XOS_File\",5,\"pipe:$.S5\" TO t%,,l%,,n%",
        "PROCw(\"unwritten \"+STR$t%+\" \"+STR$~(l% AND &FFFFFF00)+\" \"+STR$n%)",
        "$blk%=\"de\":SYS \"XOS_File\",10,\"pipe:$.S1\",&FFF,,blk%,blk%+2:PROCi5(\"pipe:$.S1\",\"saved again\")",
        "SYS \"XOS_File\",255,\"pipe:$.S1\",blk%+64,0 TO ,,,,n%:PROCw(\"loaded \"+FNs(blk%+64,n%)):PROCi5(\"pipe:$.S1\",\"taken\")",
        "$blk%=\"abc\":SYS \"XOS_File\",10,\"pipe:$.K\",&FFF,,blk%,blk%+3:SYS \"XOS_Find\",&4F,\"pipe:$.K\" TO rh%:SYS \"XOS_BGet\",,rh%",
        "SYS \"XOS_Find\",0,rh%:PROCi5(\"pipe:$.K\",\"one read\"):PROClist(\"pipe:$\")",
        "SYS \"XOS_Find\",&4F,\"pipe:$.K\" TO rh%:PROCinfo(rh%,\"reopened\"):SYS \"XOS_Find\",0,rh%",
        "$blk%=\"xyz\":SYS \"XOS_File\",10,\"pipe:$.L\",&FFF,,blk%,blk%+3:SYS \"XOS_Find\",&4F,\"pipe:$.L\" TO rh%:SYS \"XOS_BGet\",,rh%",
        "SYS \"XOS_Find\",0,rh%:$(blk%+64)=\"....\":SYS \"XOS_File\",255,\"pipe:$.L\",blk%+64,0 TO r%,,,,n%;f%",
        "PROCw(\"load of a part read \"+FNerr(r%,f%)+\" \"+STR$n%+\" \"+FNs(blk%+64,3)):PROCi5(\"pipe:$.L\",\"taken\")",
        "SYS \"XOS_Find\",&4F,\"pipe:$.Q\" TO rh%:PROCi5(\"pipe:$.Q\",\"reader only\")",
        "SYS \"XOS_File\",6,\"pipe:$.Q\" TO r%;f%:SYS \"XOS_File\",7,\"pipe:$.Q\",1,2,0,0 TO s%;e%:PROCw(\"delete \"+FNerr(r%,f%)+\" create \"+FNerr(s%,e%))",
        "SYS \"XOS_Find\",0,rh%:SYS \"XOS_Find\",&8F,\"pipe:$.P\" TO wh%:SYS \"XOS_BPut\",65,wh%",
        "SYS \"XOS_File\",7,\"pipe:$.P\",1,2,0,0 TO r%;f%:PROCw(\"create while written \"+FNerr(r%,f%))",
        "SYS \"XOS_File\",18,\"pipe:$.P\",&FFF:SYS \"XOS_Find\",&4F,\"pipe:$.P\" TO rh%:SYS \"XOS_Find\",0,rh%:SYS \"XOS_Find\",0,wh%",
        "SYS \"XOS_File\",5,\"pipe:$.P\" TO t%,,l%,,n%:PROCw(\"typed then read \"+STR$~(l% AND &FFFFFF00)+\" \"+STR$n%)",
    ]);
    assert_eq!(
        out,
        [
            "save 1 0 0 3 3",
            "save0 1 0 0 4 3",
            "create 1 1111 2222 0 3",
            "unwritten 1 FFFFFD00 0",
            "saved again 1 0 0 2 3",
            "loaded de",
            "taken 0 0 0 0 0",
            "one read 1 0 0 3 3",
            "  S2 1 0 0 4 3",
            "  S3 1 1111 2222 0 3",
            "  S5 1 FFFFFD00 dated 0 3",
            "  K 1 0 0 2 3",
            "listed 4",
            "reopened 1 2 256 448 0",
            "load of a part read ok 3 yz.",
            "taken 0 0 0 0 0",
            "reader only 1 DEADDEAD DEADDEAD 0 3",
            "delete err &12FC2 create ok",
            "create while written err &12FC2",
            "typed then read FFFFFD00 1",
        ]
    );
}

#[test]
fn dotted_names_make_directories_and_names_are_compared_without_case() {
    let out = program(&[
        "SYS \"XOS_Find\",&8F,\"pipe:$.Dir.In\" TO wh%:SYS \"XOS_File\",5,\"pipe:$.Dir\" TO t%:PROCw(\"dir \"+STR$t%)",
        "PROClist(\"pipe:$.Dir\"):SYS \"XOS_Find\",0,wh%",
        "SYS \"XOS_File\",8,\"pipe:$.X\" TO r%;f%:SYS \"XOS_File\",5,\"pipe:$.X\" TO t%:PROCw(\"cdir \"+FNerr(r%,f%)+\" \"+STR$t%)",
        "SYS \"XOS_Find\",&8F,\"pipe:Rel\" TO wh%:SYS \"XOS_Find\",0,wh%:SYS \"XOS_File\",5,\"pipe:$.Rel\" TO t%:PROCw(\"relative \"+STR$t%)",
        "SYS \"XOS_FSControl\",25,\"pipe:$.Rel\",\"pipe:$.Other\" TO r%;f%:PROCw(\"rename \"+FNerr(r%,f%))",
        "SYS \"XOS_File\",1,\"pipe:$.None\",1,2,,3 TO r%;f%:SYS \"XOS_File\",2,\"pipe:$.Rel\",&12345678 TO s%;e%",
        "SYS \"XOS_File\",3,\"pipe:$.Rel\",,&9ABC:PROCw(\"write info \"+FNerr(r%,f%)+\" \"+FNerr(s%,e%)):PROCi5(\"pipe:$.REL\",\"written\")",
        "SYS \"XOS_File\",6,\"pipe:$.Rel\":SYS \"XOS_File\",6,\"pipe:$.Dir.In\":SYS \"XOS_File\",6,\"pipe:$.Gone\" TO r%:PROCw(\"absent \"+STR$r%)",
    ]);
    assert_eq!(
        out,
        [
            "dir 2",
            "  In 1 DEADDEAD DEADDEAD 0 3",
            "listed 1",
            "cdir ok 0",
            "relative 1",
            "rename err &12FF8",
            "write info ok ok",
            "written 1 12345678 9ABC 0 3",
            "absent 0",
        ]
    );
}

#[test]
fn pipecopy_copies_a_byte_at_a_time_to_one_or_two_destinations() {
    let out = program(&[
        "SYS \"XOS_CLI\",\"PipeCopy\" TO r%;f%:PROCw(\"none \"+FNerr(r%,f%))",
        "$blk%=\"0123456789\":SYS \"XOS_File\",10,\"pipe:$.Src\",&FFF,,blk%,blk%+10",
        "SYS \"XOS_CLI\",\"PipeCopy pipe:$.Src pipe:$.D1 pipe:$.D2\" TO r%;f%:PROCw(\"copied \"+FNerr(r%,f%))",
        "PROCi5(\"pipe:$.Src\",\"source\"):SYS \"XOS_File\",255,\"pipe:$.D2\",blk%+64,0 TO ,,,,n%:PROCw(\"d2 \"+FNs(blk%+64,n%))",
        "SYS \"XOS_File\",5,\"pipe:$.D1\" TO ,,,,n%:PROCw(\"d1 \"+STR$n%)",
        "SYS \"XOS_CLI\",\"PipeCopy null: null:\" TO r%;f%:PROCw(\"null \"+FNerr(r%,f%))",
        "SYS \"XOS_CLI\",\"PipeCopy HostFS:$.Absent null:\" TO r%;f%:PROCw(\"absent \"+FNerr(r%,f%))",
    ]);
    assert_eq!(
        out,
        [
            "none err &DC",
            "copied ok",
            "source 0 0 0 0 0",
            "d2 0123456789",
            "d1 10",
            "null ok",
            "absent err &D6",
        ]
    );
}
