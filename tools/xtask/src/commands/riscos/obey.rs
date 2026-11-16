//! The reference harness's part for `lib/obey`: each case in [`CASES`] run
//! on genuine RISC OS 5, in order on one machine, and its answer recorded
//! beside it in [`ANSWERS`], which `lib/obey`'s replay holds the crate to.
//!
//! A case is a line: its kind, then its fields, each a quoted string of
//! printable ASCII but for `\xHH`. A text field reaches the call followed by
//! a CR, a number field is a BASIC expression, and an Obey file is written as
//! it is. The answer follows `= ` on the next line, its bytes quoted the same
//! way. An `out` is the buffer the call was given, each byte it did not write
//! still `&EE`, shown to a byte past what the call reports where it fits:
//!
//! - `gstrans TEXT FLAGS ROOM`: `carry C next N stored R2 out B`, `N` where R0
//!   is in the text and `elsewhere` where it is not;
//! - `gsread TEXT FLAGS`: `OS_GSInit`'s `first R1 empty Z at N`, then
//!   `reads` and each `OS_GSRead`'s R1 to `end R1 next N`;
//! - `evaluate TEXT ROOM`: `integer N`, or `string R2 out B`;
//! - `readargs KEYS TEXT ROOM`: `left R3 base &B out B`, to a byte past
//!   `ROOM`;
//! - `substitute ARGUMENTS TEMPLATE ROOM FLAGS`, `OS_SubstituteArgs32`:
//!   `length R2 out B`;
//! - `setvar NAME VALUE LENGTH TYPE`: `type R4`;
//! - `readvar NAME ROOM TYPE`: `length R2 type R4 out B`;
//! - `readunsigned FLAGS TEXT LIMIT WIDE`, R4 the `WIDE` selector where WIDE
//!   is not zero: `value R2 high R3 next N`;
//! - `cli COMMAND`, and `obey FILE ARGUMENTS` run by `*Obey`: `done` or
//!   `error &E`, then `said B`, what was spooled;
//! - `sweep PREFIX SUFFIX FROM TO PATTERN`: `set` and a digit for each byte
//!   from `FROM` to `TO`, whether running the prefix, the byte and the suffix
//!   left a variable the pattern matches, which is then unset.
//!
//! An error answers `error &E`, by number, never its text; `gstrans` goes on
//! `stored R2 out B` and `readvar` `length R2`.

/// The cases, from the workspace's root.
pub(super) const CASES: &str = "lib/obey/tests/riscos5/cases.txt";
/// The answers the harness records.
pub(super) const ANSWERS: &str = "lib/obey/tests/riscos5/answers.txt";
/// The HostFS name the cases are staged under.
pub(super) const STAGED: &str = "Cases";
/// The HostFS name the answers come back as.
pub(super) const RECORDED: &str = "Answers";
/// Every file the probe leaves in HostFS, as its listing names them.
pub(super) const LEFT: [&str; 4] = [STAGED, RECORDED, "ObeyFile", "Spooled"];
/// What the probe fills each buffer with before a call, so that the bytes a
/// call writes stand apart from those it leaves.
const UNWRITTEN: u8 = 0xEE;

/// The statements the harness runs the cases by.
pub(super) fn run() -> String {
    format!("oe%=&{:08X}:PROCobey", u32::from_ne_bytes([UNWRITTEN; 4]))
}

/// The probe's lines, after the harness's own: its names stay clear of the
/// harness's registers, flags and kept values. BBC BASIC runs a `FOR` loop's
/// body at least once, so a count that may be zero is a `WHILE`.
pub(super) const PROBE: &str = r#"DEF PROCobey
LOCAL ou%,on%
DIM ol% 8192,os% 4352,ot% 4096,ou2% 2048
ou%=OPENIN"HostFS:$.Cases":ow%=OPENOUT"HostFS:$.Answers"
WHILE NOT EOF#ou%
on%=0:REPEAT:ol%?on%=BGET#ou%:on%+=1:UNTIL ol%?(on%-1)=10 OR EOF#ou% OR on%=8192
IF ol%?(on%-1)=10 THEN on%-=1
IF on%>0 THEN IF ?ol%<>35 THEN PROCobeycase(on%)
ENDWHILE
CLOSE#ou%:CLOSE#ow%
ENDPROC
DEF PROCobeycase(ln%)
LOCAL ok$,op%,ox%,oy%,of%,oa%,ob%,oc%,od%
FOR ox%=0 TO ln%-1:BPUT#ow%,ol%?ox%:NEXT:BPUT#ow%,10
WHILE op%<ln% AND ol%?op%<>32:ok$+=CHR$(ol%?op%):op%+=1:ENDWHILE
FOR ox%=0 TO 4348 STEP 4:os%!ox%=oe%:NEXT
BPUT#ow%,"= ";
LOCAL ERROR
ON ERROR LOCAL BPUT#ow%,"! "+STR$ERR+" at "+STR$ERL:ENDPROC
CASE ok$ OF
WHEN "gstrans":PROCobeygstrans
WHEN "gsread":PROCobeygsread
WHEN "evaluate":PROCobeyevaluate
WHEN "readargs":PROCobeyreadargs
WHEN "substitute":PROCobeysubstitute
WHEN "setvar":PROCobeysetvar
WHEN "readvar":PROCobeyreadvar
WHEN "readunsigned":PROCobeyreadunsigned
WHEN "cli":oa%=FNobeyfield(ot%):ot%?oa%=13:PROCobeycli
WHEN "obey":PROCobeyobey
WHEN "sweep":PROCobeysweep
OTHERWISE BPUT#ow%,"! unknown kind "+ok$;
ENDCASE
BPUT#ow%,10:SYS"OS_Args",255,ow%
ENDPROC
DEF PROCobeygstrans
oa%=FNobeyfield(ot%):ot%?oa%=13:ob%=FNobeynumber:oc%=FNobeynumber
SYS"XOS_GSTrans",ot%,os%,ob% OR oc% TO ox%,,oy%;of%
IF of% AND 1 THEN BPUT#ow%,"error &"+STR$~!ox%+" "; ELSE BPUT#ow%,"carry "+STR$(of% DIV 2 AND 1)+" next "+FNobeyat(ox%,oa%)+" ";
BPUT#ow%,"stored "+STR$oy%+" out ";:PROCobeyquote(os%,FNobeyshown(oy%+1,oc%))
ENDPROC
DEF PROCobeygsread
oa%=FNobeyfield(ot%):ot%?oa%=13:ob%=FNobeynumber
SYS"XOS_GSInit",ot%,0,ob% TO ox%,oy%,oc%;of%
IF of% AND 1 THEN PROCobeyerror(ox%):ENDPROC
BPUT#ow%,"first "+STR$oy%+" empty "+STR$(of% DIV 4 AND 1)+" at "+STR$(ox%-ot%)+" reads";
od%=0
REPEAT:SYS"XOS_GSRead",ox%,0,oc% TO ox%,oy%,oc%;of%:od%+=1
IF of% AND 1 THEN BPUT#ow%," error &"+STR$~!ox%; ELSE IF of% AND 2 THEN BPUT#ow%," end "+STR$oy%+" next "+FNobeyat(ox%,oa%); ELSE BPUT#ow%," "+STR$oy%;
UNTIL (of% AND 3)<>0 OR od%>=600
ENDPROC
DEF PROCobeyevaluate
oa%=FNobeyfield(ot%):ot%?oa%=13:ob%=FNobeynumber
SYS"XOS_EvaluateExpression",ot%,os%,ob% TO ox%,oy%,oc%;of%
IF of% AND 1 THEN PROCobeyerror(ox%):ENDPROC
IF oy%=0 THEN BPUT#ow%,"integer "+STR$oc%; ELSE BPUT#ow%,"string "+STR$oc%+" out ";:PROCobeyquote(os%,FNobeyshown(oc%+1,ob%))
ENDPROC
DEF PROCobeyreadargs
oa%=FNobeyfield(ot%):ot%?oa%=13:ob%=FNobeyfield(ot%+2048):ot%?(2048+ob%)=13:oc%=FNobeynumber
SYS"XOS_ReadArgs",ot%,ot%+2048,os%,oc% TO ox%,,,od%;of%
IF of% AND 1 THEN PROCobeyerror(ox%):ENDPROC
BPUT#ow%,"left "+STR$od%+" base &"+STR$~os%+" out ";:PROCobeyquote(os%,oc%+1)
ENDPROC
DEF PROCobeysubstitute
oa%=FNobeyfield(ot%):ot%?oa%=13:ob%=FNobeyfield(ot%+2048):ot%?(2048+ob%)=13:oc%=FNobeynumber:od%=FNobeynumber
SYS"XOS_SubstituteArgs32",ot%,os%,oc%,ot%+2048,ob%,od% TO ox%,,oy%;of%
IF of% AND 1 THEN PROCobeyerror(ox%):ENDPROC
BPUT#ow%,"length "+STR$oy%+" out ";:PROCobeyquote(os%,FNobeyshown(oy%+1,oc%+1))
ENDPROC
DEF PROCobeysetvar
oa%=FNobeyfield(ot%):ot%?oa%=13:ob%=FNobeyfield(ot%+2048):ot%?(2048+ob%)=13:oc%=FNobeynumber:od%=FNobeynumber
SYS"XOS_SetVarVal",ot%,ot%+2048,oc%,0,od% TO ox%,,,,oy%;of%
IF of% AND 1 THEN PROCobeyerror(ox%) ELSE BPUT#ow%,"type "+STR$oy%;
ENDPROC
DEF PROCobeyreadvar
oa%=FNobeyfield(ot%):ot%?oa%=13:ob%=FNobeynumber:oc%=FNobeynumber
SYS"XOS_ReadVarVal",ot%,os%,ob%,0,oc% TO ox%,,oy%,,oc%;of%
IF of% AND 1 THEN BPUT#ow%,"error &"+STR$~!ox%+" length "+STR$oy%;:ENDPROC
BPUT#ow%,"length "+STR$oy%+" type "+STR$oc%+" out ";:PROCobeyquote(os%,FNobeyshown(oy%+1,ob%))
ENDPROC
DEF PROCobeyreadunsigned
ob%=FNobeynumber:oa%=FNobeyfield(ot%):ot%?oa%=13:oc%=FNobeynumber:od%=FNobeynumber
IF od% THEN od%=&45444957
SYS"XOS_ReadUnsigned",ob%,ot%,oc%,0,od% TO ox%,oy%,oc%,od%;of%
IF of% AND 1 THEN PROCobeyerror(ox%):ENDPROC
BPUT#ow%,"value "+STR$oc%+" high "+STR$od%+" next "+STR$(oy%-ot%);
ENDPROC
DEF PROCobeyobey
LOCAL oo$
oa%=FNobeyfield(ot%+2048):ob%=OPENOUT"HostFS:$.ObeyFile":ox%=0
WHILE ox%<oa%:BPUT#ob%,ot%?(2048+ox%):ox%+=1:ENDWHILE:CLOSE#ob%
OSCLI"SetType HostFS:$.ObeyFile Obey"
oo$="Obey HostFS:$.ObeyFile ":$ot%=oo$:oc%=FNobeyfield(ot%+LEN oo$):ot%?(LEN oo$+oc%)=13
PROCobeycli
ENDPROC
DEF PROCobeycli
OSCLI"Spool HostFS:$.Spooled":SYS"XOS_CLI",ot% TO ox%;of%:OSCLI"Spool"
IF of% AND 1 THEN BPUT#ow%,"error &"+STR$~!ox%+" "; ELSE BPUT#ow%,"done ";
ob%=OPENIN"HostFS:$.Spooled":oc%=EXT#ob%:ox%=0
IF oc%>4096 THEN CLOSE#ob%:BPUT#ow%,"! spooled too much";:ENDPROC
WHILE ox%<oc%:os%?ox%=BGET#ob%:ox%+=1:ENDWHILE:CLOSE#ob%
BPUT#ow%,"said ";:PROCobeyquote(os%,oc%)
ENDPROC
DEF PROCobeysweep
LOCAL oz%,og%,oh%
oa%=FNobeyfield(ot%):ob%=FNobeyfield(ot%+2048):og%=FNobeynumber:oh%=FNobeynumber:oc%=FNobeyfield(ou2%):ou2%?oc%=13
BPUT#ow%,"set ";
FOR oz%=og% TO oh%
ox%=0:WHILE ox%<oa%:os%?ox%=ot%?ox%:ox%+=1:ENDWHILE:os%?oa%=oz%
ox%=0:WHILE ox%<ob%:os%?(oa%+1+ox%)=ot%?(2048+ox%):ox%+=1:ENDWHILE:os%?(oa%+1+ob%)=13
SYS"XOS_CLI",os%
SYS"XOS_ReadVarVal",ou2%,os%+4096,256,0,0 TO oy%;of%
IF of% AND 1 THEN BPUT#ow%,"0"; ELSE BPUT#ow%,"1";:$os%="Unset "+FNobeytext(ou2%):SYS"XOS_CLI",os%
NEXT
ENDPROC
DEF FNobeyfield(dt%)
LOCAL dn%,dz%,de%
WHILE op%<ln% AND ol%?op%=32:op%+=1:ENDWHILE
op%+=1
REPEAT:dz%=ol%?op%:op%+=1:de%=FALSE:IF dz%=92 THEN de%=TRUE:dz%=EVAL("&"+CHR$(ol%?(op%+1))+CHR$(ol%?(op%+2))):op%+=3
IF de% OR dz%<>34 THEN IF dn%<2040 THEN dt%?dn%=dz%:dn%+=1
UNTIL (dz%=34 AND NOT de%) OR op%>=ln%
=dn%
DEF FNobeynumber
LOCAL nn%,ns$,ni%
nn%=FNobeyfield(ou2%)
WHILE ni%<nn%:ns$+=CHR$(ou2%?ni%):ni%+=1:ENDWHILE
=EVAL(ns$)
DEF FNobeyshown(sn%,sr%):IF sn%<0 OR sn%>sr% THEN =sr% ELSE =sn%
DEF FNobeyat(pa%,pn%):IF pa%<ot% OR pa%>ot%+pn%+1 THEN ="elsewhere" ELSE =STR$(pa%-ot%)
DEF PROCobeyquote(qa%,qn%)
LOCAL qx%,qz%
BPUT#ow%,34
WHILE qx%<qn%:qz%=qa%?qx%:IF qz%<32 OR qz%>126 OR qz%=34 OR qz%=92 THEN BPUT#ow%,"\x"+RIGHT$("0"+STR$~qz%,2); ELSE BPUT#ow%,qz%
qx%+=1:ENDWHILE
BPUT#ow%,34
ENDPROC
DEF PROCobeyerror(ea%)
BPUT#ow%,"error &"+STR$~!ea%;
ENDPROC
DEF FNobeytext(ta%)
LOCAL ts$
WHILE ?ta%>=32 AND LEN ts$<250:ts$+=CHR$?ta%:ta%+=1:ENDWHILE
=ts$"#;

/// The answers file: the recording's provenance, then what it recorded.
pub(super) fn answers(provenance: &[String], recorded: &str) -> String {
    let mut text = String::new();
    for line in provenance {
        text.push_str("# ");
        text.push_str(line);
        text.push('\n');
    }
    text.push_str("# Never edit by hand.\n");
    text.push_str(recorded);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// The bytes the machine wrote into the buffers `answers` show: each quoted
/// string parted wherever a byte was left unwritten.
pub(super) fn written(answers: &str) -> Vec<Vec<u8>> {
    super::quoted(answers)
        .iter()
        .flat_map(|string| string.split(|&b| b == UNWRITTEN))
        .filter(|part| !part.is_empty())
        .map(<[u8]>::to_vec)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_was_written_is_what_lies_between_the_unwritten_bytes() {
        assert_eq!(
            written("= out \"ab\\xEE\\xEEc\\xEE\" carry 0\n"),
            [b"ab".to_vec(), b"c".to_vec()]
        );
        assert!(run().starts_with("oe%=&EEEEEEEE:"));
    }

    #[test]
    fn the_answers_open_with_their_provenance() {
        let provenance = ["from here,".to_string(), "this image.".to_string()];
        assert_eq!(
            answers(&provenance, "cli \"x\"\n= done said \"\""),
            "# from here,\n# this image.\n# Never edit by hand.\ncli \"x\"\n= done said \"\"\n"
        );
    }
}
