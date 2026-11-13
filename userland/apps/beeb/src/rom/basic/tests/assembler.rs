//! The inline assembler: its listing, every instruction class, the operand
//! errors, quiet first passes, offset assembly, and assembly at the prompt.

use super::assert_screen_from;

#[test]
fn the_assembler_lists_two_passes() {
    assert_screen_from(
        &[
            "10 DIM C% 100",
            "20 FOR P=0 TO 3 STEP 3",
            "30 P%=C%",
            "40 [OPT P",
            "50 .START LDA #65:JSR &FFEE",
            "60 LDX #3:.LOOP DEX:BNE LOOP",
            "70 STA &70,X:LDA (&80),Y:RTS:JMP (&FFFC)",
            "80 ASL A:EQUB 1:EQUW &1234",
            "90 EQUD &12345678:EQUS \"AB\"",
            "100 ]",
            "110 NEXT",
            "120 CALL START",
            "RUN",
        ],
        80,
        ">RUN",
        &[
            ">RUN",
            "0EEC          OPT P",
            "0EEC A9 41    .START LDA _65",
            "0EEE 20 EE FF JSR &FFEE",
            "0EF1 A2 03    LDX _3",
            "0EF3 CA       .LOOP DEX",
            "0EF4 D0 FD    BNE LOOP",
            "0EF6 95 70    STA &70,X",
            "0EF8 B1 80    LDA (&80),Y",
            "0EFA 60       RTS",
            "0EFB 6C FC FF JMP (&FFFC)",
            "0EFE 0A       ASL A",
            "0EFF 01       EQUB 1",
            "0F00 34 12    EQUW &1234",
            "0F02 78 56 34",
            "     12       EQUD &12345678",
            "0F06 41 42    EQUS \"AB\"",
            "A>",
        ],
    );
}

#[test]
fn every_class_of_instruction_assembles() {
    assert_screen_from(
        &[
            "10 DIM C% 300",
            "20 P%=C%",
            "30 [OPT 3",
            "40 STX &70,Y:ROR A:ROL &70:BIT &FE40:CMP #&FF:CPX #0:CPY &71",
            "50 INC &1000,X:DEC &70:JMP &1234:BRK:NOP:PHA:PLA:PHP:PLP:SEI:CLI",
            "60 ]",
            "RUN",
        ],
        80,
        ">RUN",
        &[
            ">RUN",
            "0EAA          OPT 3",
            "0EAA 96 70    STX &70,Y",
            "0EAC 6A       ROR A",
            "0EAD 26 70    ROL &70",
            "0EAF 2C 40 FE BIT &FE40",
            "0EB2 C9 FF    CMP _&FF",
            "0EB4 E0 00    CPX _0",
            "0EB6 C4 71    CPY &71",
            "0EB8 FE 00 10 INC &1000,X",
            "0EBB C6 70    DEC &70",
            "0EBD 4C 34 12 JMP &1234",
            "0EC0 00       BRK",
            "0EC1 EA       NOP",
            "0EC2 48       PHA",
            "0EC3 68       PLA",
            "0EC4 08       PHP",
            "0EC5 28       PLP",
            "0EC6 78       SEI",
            "0EC7 58       CLI",
            ">",
        ],
    );
}

#[test]
fn every_implied_instruction_and_the_indirect_modes() {
    assert_screen_from(
        &[
            "10 DIM C% 300",
            "20 P%=C%",
            "30 [OPT 3",
            "40 SED:CLD:SEC:CLC:CLV:TAX:TAY:TXA:TYA:TSX:TXS:INX:INY:DEY:DEX:RTI",
            "50 LDA (&70,X):STA (&71),Y:ADC &1234:SBC #1:LSR &70,X:ASL &2000,X",
            "60 ]",
            "RUN",
        ],
        80,
        ">RUN",
        &[
            ">RUN",
            "0EB1          OPT 3",
            "0EB1 F8       SED",
            "0EB2 D8       CLD",
            "0EB3 38       SEC",
            "0EB4 18       CLC",
            "0EB5 B8       CLV",
            "0EB6 AA       TAX",
            "0EB7 A8       TAY",
            "0EB8 8A       TXA",
            "0EB9 98       TYA",
            "0EBA BA       TSX",
            "0EBB 9A       TXS",
            "0EBC E8       INX",
            "0EBD C8       INY",
            "0EBE 88       DEY",
            "0EBF CA       DEX",
            "0EC0 40       RTI",
            "0EC1 A1 70    LDA (&70,X)",
            "0EC3 91 71    STA (&71),Y",
            "0EC5 6D 34 12 ADC &1234",
            "0EC8 E9 01    SBC _1",
            "0ECA 56 70    LSR &70,X",
            "0ECC 1E 00 20 ASL &2000,X",
            ">",
        ],
    );
}

#[test]
fn long_strings_list_three_bytes_a_line() {
    assert_screen_from(
        &[
            "10 DIM C% 300",
            "20 P%=C%",
            "30 [OPT 3",
            "40 EQUS \"HELLO WORLD\":EOR #3:AND &70:ORA &1234,X",
            "50 LDA &1234,Y:LDX &70,Y:LDY &80,X:STX &7000,Y",
            "60 ]",
            "RUN",
        ],
        80,
        ">RUN",
        &[
            ">RUN",
            "0E87          OPT 3",
            "0E87 48 45 4C",
            "     4C 4F 20",
            "     57 4F 52",
            "     4C 44    EQUS \"HELLO WORLD\"",
            "0E92 49 03    EOR _3",
            "0E94 25 70    AND &70",
            "0E96 1D 34 12 ORA &1234,X",
            "0E99 B9 34 12 LDA &1234,Y",
            "0E9C B6 70    LDX &70,Y",
            "0E9E B4 80    LDY &80,X",
            "",
            "Byte at line 50",
            ">",
        ],
    );
}

#[test]
fn operand_errors() {
    assert_screen_from(
        &[
            "10 DIM C% 300:A=5:P%=C%",
            "20 ON ERROR PRINT ERR;\" \";:REPORT:PRINT:GOTO ERL+10",
            "30 [OPT 2:LDA #300:]",
            "40 [OPT 2:LDA (&70),X:]",
            "50 [OPT 2:XYZ:]",
            "60 [OPT 2:BNE &2000:]",
            "70 [OPT 2:LDA ZZ:]",
            "80 [OPT 2:STA #5:]",
            "90 [OPT 2:LDX &70,X:]",
            "100 [OPT 2:STY &1234,X:]",
            "110 [OPT 2:EQUS 5:]",
            "120 [OPT 2:INC A:]",
            "130 [OPT 2:JMP (&70,X):]",
            "140 END",
            "RUN",
        ],
        80,
        ">RUN",
        &[
            ">RUN",
            "         2",
            "Byte",
            "         3",
            "Index",
            "        16",
            "Syntax error",
            "         1",
            "Out of range",
            "        26",
            "No such variable",
            "        26",
            "No such variable",
            "         3",
            "Index",
            "         2",
            "Byte",
            "         6",
            "Type mismatch",
            "         3",
            "Index",
            ">",
        ],
    );
}

#[test]
fn quiet_passes_take_absent_labels_as_p_percent() {
    assert_screen_from(
        &[
            "10 DIM C% 300",
            "20 FOR O=0 TO 1:P%=C%",
            "30 [OPT O",
            "40 LDA ZZ:STA YY:BNE &2000:NOP 5:LDA #3 4",
            "50 ]",
            "55 PRINT ~C%?0;\" \";~C%?1;\" \";~C%?2;\" \";~C%?6;\" \";~C%?7;\" \";~P%-C%",
            "60 NEXT",
            "RUN",
        ],
        80,
        ">RUN",
        &[
            ">RUN",
            "        AD A4 E D0 0 B",
            "0EA4          OPT O",
            "0EA4 AD A4 0E LDA ZZ",
            "0EA7 8D A7 0E STA YY",
            "0EAA D0 00    BNE &2000",
            "0EAC EA       NOP 5",
            "0EAD A9 03    LDA _3 4",
            "        AD A4 E D0 0 B",
            ">",
        ],
    );
}

#[test]
fn offset_assembly_and_the_default_opt() {
    assert_screen_from(
        &[
            "10 DIM C% 300,D% 300",
            "20 P%=C%:O%=D%",
            "30 [NOP:]",
            "40 [OPT 7:NOP:LDA #1:]",
            "50 [:]",
            "55 PRINT ~P%-C%;\" \";~O%-D%;\" \";~D%?0;\" \";~C%?0",
            "RUN",
        ],
        80,
        ">10 DIM C% 300,D% 300",
        &[
            ">10 DIM C% 300,D% 300",
            ">20 P%=C%:O%=D%",
            ">30 [NOP:]",
            ">40 [OPT 7:NOP:LDA _1:]",
            ">50 [:]",
            ">55 PRINT ~P%-C%;\" \";~O%-D%;\" \";~D%?0;\"",
            "\";~C%?0",
            ">RUN",
            "0E7D EA       NOP",
            "0E7E          OPT 7",
            "0E7E EA       NOP",
            "0E7F A9 01    LDA _1",
            "0E81",
            "         4 3 EA EA",
            ">",
        ],
    );
}

#[test]
fn assembly_typed_at_the_prompt_ends_with_the_line() {
    assert_screen_from(
        &["P%=&3000", "[NOP", "NOP", "]", "PRINT ~P%"],
        50,
        ">P%=&3000",
        &[
            ">P%=&3000",
            ">[NOP",
            "3000 EA       NOP",
            ">NOP",
            "",
            "Mistake",
            ">]",
            "",
            "Syntax error",
            ">PRINT ~P%",
            "      3001",
            ">",
        ],
    );
}

#[test]
fn operands_may_be_reals_and_quotients() {
    assert_screen_from(
        &[
            "10DIM C% 20:P%=C%",
            "20[OPT 3:LDA #2.5:LDX #&1234 DIV 256:LDY #7 MOD 4:EQUW 1.5E3:]",
            "RUN",
        ],
        80,
        ">RUN",
        &[
            ">RUN",
            "0E4F          OPT 3",
            "0E4F A9 02    LDA _2.5",
            "0E51 A2 12    LDX _&1234 DIV 256",
            "0E53 A0 03    LDY _7 MOD 4",
            "0E55 DC 05    EQUW 1.5E3",
            ">",
        ],
    );
}

#[test]
fn a_macro_assembles_through_the_fn_its_opt_calls() {
    assert_screen_from(
        &[
            "10FORQ=0TO3STEP3",
            "20P%=&3000",
            "30[OPTQ",
            "40LDA #1",
            "50OPT FNM(2)",
            "60RTS",
            "70]",
            "80NEXT",
            "90PRINT ~P%;\" \";~?&3000;\" \";~?&3001;\" \";~?&3002;\" \";~?&3003;\" \";~?&3004;\" \";~?&3005",
            "100END",
            "110DEFFNM(N)",
            "120[OPTQ",
            "130LDX #N",
            "140]",
            "150=Q",
            "RUN",
        ],
        200,
        ">RUN",
        &[
            ">RUN",
            "3000          OPTQ",
            "3000 A9 01    LDA _1",
            "3002          OPTQ",
            "3002 A2 02    LDX _N",
            "3004          OPT FNM(2)",
            "3004 60       RTS",
            "      3005 A9 1 A2 2 60 0",
            ">",
        ],
    );
}

#[test]
fn a_quiet_operand_s_fn_is_quiet_until_it_assembles() {
    assert_screen_from(
        &[
            "10P%=&3000",
            "20[OPT0",
            "30LDA FNA",
            "40]",
            "50PRINT ~P%;\" \";~?&3000;\" \";~?&3001;\" \";~?&3002",
            "60END",
            "70DEFFNA=ZZ+1",
            "RUN",
        ],
        100,
        ">RUN",
        &[">RUN", "      3003 AD 1 30", ">"],
    );
    assert_screen_from(
        &[
            "10P%=&3000",
            "20[OPT0",
            "30LDA FNB+ZZ",
            "40]",
            "60END",
            "70DEFFNB",
            "80[",
            "100]",
            "110=0",
            "RUN",
        ],
        100,
        ">RUN",
        &[">RUN", "3000", "", "No such variable at line 30", ">"],
    );
}

#[test]
fn an_instruction_whose_fn_assembles_follows_the_fn_s_code() {
    // BASIC II puts this LDA's bytes elsewhere; here they go where P% has
    // reached, after the FN's NOP.
    assert_screen_from(
        &[
            "10P%=&3000",
            "20[OPT3",
            "30LDA #FNA",
            "40]",
            "50PRINT ~P%;\" \";~?&3000;\" \";~?&3001;\" \";~?&3002",
            "60END",
            "70DEFFNA",
            "80[OPT3",
            "90NOP",
            "100]",
            "110=5",
            "RUN",
        ],
        100,
        ">RUN",
        &[
            ">RUN",
            "3000          OPT3",
            "3000          OPT3",
            "3000 EA       NOP",
            "3001 A9 05    LDA _FNA",
            "      3003 EA A9 5",
            ">",
        ],
    );
}
