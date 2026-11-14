//! Numbers as BASIC II gives them: the arithmetic and functions to the
//! bit, conversions, and every printed form.

use alloc::format;

use super::{assert_screen, bytes_after, from, typed, PROMPTED};
use crate::scenario::{mode7_text, press_break, type_text, wait};

/// Sixteen arguments from a seeded RND, and each function's packed result
/// stored from &3000.
const FUNCTIONS: [&str; 8] = [
    "10A=0:X=RND(-7):P%=&3000:L%=LOMEM",
    "20FORI%=1TO16:X=RND(1)*20-10",
    "30A=SQRABSX:PROCS:A=LNABSX:PROCS:A=LOGABSX:PROCS:A=EXPX:PROCS",
    "40A=SINX:PROCS:A=COSX:PROCS:A=TANX:PROCS:A=ATNX:PROCS",
    "50A=ASN(X/10):PROCS:A=ACS(X/10):PROCS:A=RADX:PROCS:A=DEGX:PROCS",
    "60A=ABSX^(X/3):PROCS:NEXT:END",
    "70DEFPROCS:FORJ%=0TO4:P%?J%=L%?(3+J%):NEXT:P%=P%+5:ENDPROC",
    "RUN",
];
const FUNCTION_NAMES: [&str; 13] = [
    "SQR", "LN", "LOG", "EXP", "SIN", "COS", "TAN", "ATN", "ASN", "ACS", "RAD", "DEG", "^",
];
/// What BASIC II stored for each argument.
const FUNCTION_RESULTS: [[&str; 13]; 16] = [
    [
        "8247857475",
        "82118A75B1",
        "807CD492EA",
        "8F01EB01A6",
        "7F9471F627",
        "80F5010C9F",
        "7F1B1B81FC",
        "813BF01D1C",
        "812AA4774C",
        "7E735B1AB1",
        "7E2DB2CDA3",
        "8A0B36A603",
        "8B45E61C2B",
    ],
    [
        "81366BD401",
        "8035653E74",
        "7F1D8EE0AC",
        "8373EB1110",
        "80655AF119",
        "7FE3707E4E",
        "82811404A2",
        "810E80574B",
        "7E51710B6D",
        "812EE1B934",
        "7C113351BD",
        "8768BF2061",
        "814ECD2CE9",
    ],
    [
        "824A117124",
        "82132A15E3",
        "807FA696FC",
        "72447CFE5B",
        "800476FD62",
        "80DB10683D",
        "809ACCA57A",
        "81BC43B441",
        "81BEEBE34F",
        "8243FDDEF8",
        "7EB22941CC",
        "8A8ECA4969",
        "757BE5E893",
    ],
    [
        "7E07128CF3",
        "8381A46B8C",
        "81E13630DD",
        "81023F21BC",
        "7B0E874CAF",
        "807FF61483",
        "7B0E8CD2C3",
        "7B0E857587",
        "77640EA789",
        "8148D6D6F8",
        "751F36C2E3",
        "807F358E0C",
        "807A0E100E",
    ],
    [
        "8207C4E3DD",
        "814087D41E",
        "80273ADD2D",
        "873416D3E3",
        "80FA439F9D",
        "7ED78CB58E",
        "83149D529A",
        "812D12FAEF",
        "7F6F00579E",
        "810D4FC4BB",
        "7D20DC460B",
        "8900EC9AFB",
        "8418C62156",
    ],
    [
        "8245DC9901",
        "821078C244",
        "807AF91B54",
        "8E5D31B33B",
        "7E87F05D7F",
        "80FDBBF3B9",
        "7E092720B9",
        "813BB7EC20",
        "8122DC21BD",
        "7F18CEE395",
        "7E2AD2238C",
        "8A08E83C99",
        "8B26151451",
    ],
    [
        "8244C73225",
        "820FC4CF52",
        "8079C080D9",
        "8E4743FC18",
        "7BEB6F4FFC",
        "80FFE4EDE0",
        "7B6B883807",
        "813B927991",
        "811E8CD5FA",
        "7F2A0C12A1",
        "7E28F47824",
        "8A0769668F",
        "8B144FC487",
    ],
    [
        "824A117147",
        "82132A15F9",
        "807FA69722",
        "8F26C4AD63",
        "8084770053",
        "80DB106675",
        "801ACCAA2D",
        "813C43B445",
        "813EEBE57D",
        "7D223F5252",
        "7E32294209",
        "8A0ECA499B",
        "8C02159A69",
    ],
    [
        "8235BF2CD3",
        "820598FCC4",
        "806815334B",
        "8C46B5521A",
        "807A5A2633",
        "7ED5E89471",
        "8395CEB095",
        "8139453685",
        "8070280DDF",
        "8021F7A765",
        "7E1020FA03",
        "8967075FA4",
        "8908C3508D",
    ],
    [
        "82072DBFD4",
        "813F6A3919",
        "802642CA47",
        "7A3D337261",
        "8077F8260E",
        "7EFE7884E2",
        "82F975A0AF",
        "81ACD64375",
        "7FECC3C0D6",
        "820220656C",
        "7D9F76E80C",
        "88FF9C6011",
        "7D5D94E215",
    ],
    [
        "813F3B8B7F",
        "804D8C3150",
        "7F32895190",
        "84151A6B1F",
        "804A0A84B7",
        "809D361907",
        "81A47FFED2",
        "811325CEE6",
        "7E6680ECB9",
        "812C3FBD0B",
        "7C1F90FA33",
        "877FC62A48",
        "81689F53D9",
    ],
    [
        "820F1EF8DF",
        "814E07D8DE",
        "8032F4B93A",
        "88148B53F5",
        "80F56B9FAB",
        "7F11AB1FEE",
        "82D7A71E8E",
        "812FCCB578",
        "8006114033",
        "8106073A89",
        "7D32C0D68E",
        "890F43C61F",
        "846A19E3F5",
    ],
    [
        "8139E5F453",
        "803F100AD0",
        "7F25F475C9",
        "7D787A7BA3",
        "80DBC66625",
        "8083482E0C",
        "815647E158",
        "8190650763",
        "7ED99F7D7C",
        "816443CA51",
        "7C96C9D6A1",
        "87F1B42707",
        "80177A331A",
    ],
    [
        "8178D19FF0",
        "812A2970FB",
        "8013CCFCB9",
        "862F0AF8AA",
        "80984B80C0",
        "80CDC5B7A8",
        "803D78183C",
        "8127F2818B",
        "7F466636F8",
        "8117764CE4",
        "7D07118625",
        "885881726C",
        "832AC048D5",
    ],
    [
        "8206443BC9",
        "813DAE7E99",
        "8024C15F59",
        "7A48E6C7D5",
        "8073B53D05",
        "7F9CBF4F1C",
        "82C7031D07",
        "81AC76FA33",
        "7FE9566A1D",
        "8201B2BA95",
        "7D9D51D40C",
        "88FC2C3D6B",
        "7D68E3F768",
    ],
    [
        "8245DCBD07",
        "821078D991",
        "807AF943CE",
        "7314222C59",
        "7E07FE29B1",
        "80FDBB7D6D",
        "7E89354C6E",
        "81BBB7F0F3",
        "81A2DCB93D",
        "8235F649F0",
        "7EAAD261BF",
        "8A88E86E73",
        "764549D127",
    ],
];

/// Twenty-four pairs of operands over sixty-four powers of two, and each
/// operation's packed result.
const ARITHMETIC: [&str; 5] = [
    "10A=0:B=RND(-11):C=0:P%=&3000:L%=LOMEM",
    "20FORI%=1TO24:B=(RND(1)-.5)*2^(RND(1)*64-32):C=(RND(1)-.5)*2^(RND(1)*64-32)",
    "30A=B+C:PROCS:A=B-C:PROCS:A=B*C:PROCS:A=B/C:PROCS:NEXT:END",
    "40DEFPROCS:FORJ%=0TO4:P%?J%=L%?(3+J%):NEXT:P%=P%+5:ENDPROC",
    "RUN",
];
const OPERATIONS: [&str; 4] = ["+", "-", "*", "/"];
const ARITHMETIC_RESULTS: [[&str; 4]; 24] = [
    ["808316948D", "80037DAEE9", "75D38160CD", "77C90A0C99"],
    ["9ED0620AE8", "9E6B98DF46", "B8BCCA37BC", "7CFB117284"],
    ["7293AC7E2D", "7293AD8501", "53979CD2E3", "918FD75B95"],
    ["6998686B6A", "6995027104", "4B000A51F7", "873163EE14"],
    ["7FB0C7A8AD", "7FB0DED60D", "7280129608", "8CF422D7D3"],
    ["8A2FBC64C1", "8A2FBC64C1", "69CD0ED3F5", "AB969B552A"],
    ["8714174DB3", "8714175315", "77C72E2845", "96DC365202"],
    ["6630FC8652", "66E6095D46", "49A8B1444F", "7E85770F9E"],
    ["901E8B7130", "901E8B7130", "72DABAFA37", "ADE5D6DD74"],
    ["7CEAA836E5", "7C6AA94BDF", "67FDE2B358", "709714FE58"],
    ["88EB4578ED", "885E9868A1", "8B355F7548", "7B62CB9B15"],
    ["9B26B631D7", "9B26B631D7", "7CECB71137", "B9EAD21210"],
    ["9AB94CAAB1", "9A394CAAB1", "7904AC297A", "457D3A7AB1"],
    ["720D98BD52", "7285C2D4C2", "5E06D96545", "7B691C90FE"],
    ["9D1C26BA02", "9D1C26BA02", "930A80CCCB", "A7300C5483"],
    ["9B911CC5E2", "9B911CC5E2", "8E0B2A6C2E", "A817502CC3"],
    ["7A0578D75E", "7A0578A2FA", "605A885BBB", "932309FF28"],
    ["99D52743EB", "9955273187", "9D74F4A49D", "6C30AA8CDB"],
    ["7099DF792D", "702310B625", "5AB61671DA", "7BED98DF5A"],
    ["6FA5B7EB37", "6CFF0595B8", "5B4E9CD1AB", "813CF9249B"],
    ["7F831D43E6", "7F831D41FA", "657C70D819", "980832AD45"],
    ["8EEB77CA12", "8EF0CAA8BE", "969E777C86", "87B2EB9032"],
    ["791C6E7A1E", "799C6E7E58", "5BA55723B0", "6ADD66B646"],
    ["7E3BF4AA98", "7DDC2DF1D5", "79354F362D", "7F05C3452E"],
];

#[test]
fn functions_have_basic_ii_bits() {
    bytes_after(&FUNCTIONS, 1500, 0x3000, &FUNCTION_NAMES, &FUNCTION_RESULTS);
}

#[test]
fn arithmetic_has_basic_ii_bits() {
    bytes_after(&ARITHMETIC, 600, 0x3000, &OPERATIONS, &ARITHMETIC_RESULTS);
}

#[test]
fn quotients_powers_and_pi_print_as_basic_ii_prints_them() {
    assert_screen(
        &[
            "PRINT 1/3",
            "PRINT PI",
            "A=1/3:PRINT A*3",
            "PRINT 2^0.5",
            "PRINT 3^20-3486784400, (-2)^3, 2^-2, 0^0",
            "PRINT 2^0.5, 10^0.5",
        ],
        50,
        &[
            ">PRINT 1/3",
            "0.333333333",
            ">PRINT PI",
            "3.14159265",
            ">A=1/3:PRINT A*3",
            "         1",
            ">PRINT 2^0.5",
            "1.41421356",
            ">PRINT 3^20-3486784400, (-2)^3, 2^-2, 0^",
            "0",
            "         1        -8      0.25         1",
            "",
            ">PRINT 2^0.5, 10^0.5",
            "1.414213563.16227766",
            ">",
        ],
    );
}

#[test]
fn integers_truncate_and_their_products_become_reals() {
    assert_screen(
        &[
            "A%=2.7:B%=-2.7:PRINT A%,B%",
            "PRINT 46341*46341",
            "PRINT 46340*46340",
            "PRINT -&80000000",
            "PRINT 2147483647, 999999999",
            "PRINT 1E9, 1234567890",
            "PRINT 3/2*2",
        ],
        60,
        &[
            ">A%=2.7:B%=-2.7:PRINT A%,B%",
            "         2        -2",
            ">PRINT 46341*46341",
            "2.14748828E9",
            ">PRINT 46340*46340",
            "2.1473956E9",
            ">PRINT -&80000000",
            "-2.14748365E9",
            ">PRINT 2147483647, 999999999",
            "2.14748365E9         999999999",
            ">PRINT 1E9, 1234567890",
            "       1E91.23456789E9",
            ">PRINT 3/2*2",
            "         3",
            ">",
        ],
    );
}

#[test]
fn numbers_print_in_nine_figures_and_exponent_form() {
    assert_screen(
        &[
            "PRINT 1E10, 123456789012, 1E-5",
            "PRINT 0.1, .5, -0.001",
            "PRINT 1+1=2",
            "PRINT \"A\"+\"B\"",
        ],
        50,
        &[
            ">PRINT 1E10, 123456789012, 1E-5",
            "      1E101.23456789E11             1E-5",
            "",
            ">PRINT 0.1, .5, -0.001",
            "       0.1       0.5     -1E-3",
            ">PRINT 1+1=2",
            "        -1",
            ">PRINT \"A\"+\"B\"",
            "AB",
            ">",
        ],
    );
}

#[test]
fn reals_compare_exactly() {
    assert_screen(
        &[
            "PRINT 1=1/3*3, 1/3*3=1",
            "A=1/3:B=A*3:PRINT B=1, A*3=1",
            "A%=0.99999999999:PRINT A%",
            "PRINT INT(0.99999999999), INT(-0.99999999999)",
            "A=0.99999999999:PRINT A=1",
            "PRINT 0.99999999999=1",
        ],
        50,
        &[
            ">PRINT 1=1/3*3, 1/3*3=1",
            "        -1        -1",
            ">A=1/3:B=A*3:PRINT B=1, A*3=1",
            "        -1        -1",
            ">A%=0.99999999999:PRINT A%",
            "         1",
            ">PRINT INT(0.99999999999), INT(-0.999999",
            "99999)",
            "         1        -1",
            ">A=0.99999999999:PRINT A=1",
            "        -1",
            ">PRINT 0.99999999999=1",
            "        -1",
            ">",
        ],
    );
}

#[test]
fn int_and_integer_assignment_at_their_limits() {
    assert_screen(
        &[
            "PRINT INT(3E9)",
            "PRINT INT(-2.5), INT(2^30)",
            "PRINT 2^127, 1.7E38",
            "A%=2147483647.4:PRINT A%",
            "A%=-2147483648.4:PRINT A%",
        ],
        50,
        &[
            ">PRINT INT(3E9)",
            "",
            "Too big",
            ">PRINT INT(-2.5), INT(2^30)",
            "        -31.07374182E9",
            ">PRINT 2^127, 1.7E38",
            "1.70141181E38           1.7E38",
            ">A%=2147483647.4:PRINT A%",
            "2.14748365E9",
            ">A%=-2147483648.4:PRINT A%",
            "",
            "Too big",
            ">",
        ],
    );
}

#[test]
fn str_rounds_to_nine_figures() {
    assert_screen(
        &[
            "PRINT STR$(9.99999999995)",
            "PRINT STR$(9.9999999999)",
            "PRINT STR$(0.99999999999)",
            "PRINT STR$(99999.999999)",
        ],
        40,
        &[
            ">PRINT STR$(9.99999999995)",
            "10",
            ">PRINT STR$(9.9999999999)",
            "10",
            ">PRINT STR$(0.99999999999)",
            "1",
            ">PRINT STR$(99999.999999)",
            "100000",
            ">",
        ],
    );
}

#[test]
fn at_percent_chooses_fixed_and_exponent_forms() {
    assert_screen(
        &[
            "@%=&1030A:PRINT 123.456,-0.00123,0,1E20",
            "@%=&1030A:PRINT 1E-20,-5,0.5",
            "@%=&20F0A:PRINT 1/4,100",
            "@%=&2050A:PRINT 1E10,1E-10,12345678.123",
            "@%=&10A:PRINT 1/3,2/3",
        ],
        50,
        &[
            ">@%=&1030A:PRINT 123.456,-0.00123,0,1E20",
            "",
            "  1.23E2   -1.23E-3   0.00E0    1.00E20",
            "",
            ">@%=&1030A:PRINT 1E-20,-5,0.5",
            "  1.00E-20 -5.00E0    5.00E-1",
            ">@%=&20F0A:PRINT 1/4,100",
            "0.2500000000               100",
            ">@%=&2050A:PRINT 1E10,1E-10,12345678.123",
            "",
            "      1E10   0.0000012345678.12",
            ">@%=&10A:PRINT 1/3,2/3",
            "       0.3       0.7",
            ">",
        ],
    );
}

#[test]
fn at_percent_sets_figures_and_field_width() {
    assert_screen(
        &[
            "@%=&20F0A:PRINT 1;\" \";10;\" \";100;\" \";5.5",
            "@%=&20F0A:PRINT 0.5;\" \";-100;\" \";1E10",
            "@%=&20B0A:PRINT 1;\" \";10;\" \";100;\" \";0.25",
            "@%=&2090A:PRINT 1;\" \";10;\" \";100;\" \";0.25",
            "@%=&2080A:PRINT 12;\" \";123;\" \";1234.5",
        ],
        50,
        &[
            ">@%=&20F0A:PRINT 1;\" \";10;\" \";100;\" \";5.",
            "5",
            "         1 10 100 5.5",
            ">@%=&20F0A:PRINT 0.5;\" \";-100;\" \";1E10",
            "0.5000000000 -100 1E10",
            ">@%=&20B0A:PRINT 1;\" \";10;\" \";100;\" \";0.",
            "25",
            "         1 10 100 0.2500000000",
            ">@%=&2090A:PRINT 1;\" \";10;\" \";100;\" \";0.",
            "25",
            "1.000000000 10 100 0.250000000",
            ">@%=&2080A:PRINT 12;\" \";123;\" \";1234.5",
            "12.00000000 123 1234.5",
            ">",
        ],
    );
}

#[test]
fn literals_beyond_the_range_and_bare_exponents() {
    assert_screen(
        &[
            "PRINT 1E39",
            "PRINT 1E-200",
            "PRINT 1E200",
            "PRINT 1E1000",
            "PRINT 1E-1000",
            "PRINT 1E",
            "PRINT 1E-",
            "PRINT 3E2X",
        ],
        50,
        &[
            ">PRINT 1E39",
            "",
            "Too big",
            ">PRINT 1E-200",
            "     1E-20         0",
            ">PRINT 1E200",
            "      1E20         0",
            ">PRINT 1E1000",
            "      1E10         0",
            ">PRINT 1E-1000",
            "     1E-10         0",
            ">PRINT 1E",
            "         1",
            ">PRINT 1E-",
            "         1",
            ">PRINT 3E2X",
            "       300",
            "No such variable",
            ">",
        ],
    );
}

#[test]
fn functions_refuse_arguments_out_of_their_range() {
    assert_screen(
        &[
            "PRINT EXP(89.5)",
            "PRINT EXP(-127),2^-1000",
            "PRINT SIN(8388608)",
            "PRINT ASN(2)",
            "PRINT LN(0)",
            "PRINT (-2)^0.5",
            "PRINT 0^-1",
        ],
        60,
        &[
            ">PRINT EXP(89.5)",
            "",
            "Exp range",
            ">PRINT EXP(-127),2^-1000",
            "         0         0",
            ">PRINT SIN(8388608)",
            "",
            "Accuracy lost",
            ">PRINT ASN(2)",
            "",
            "-ve root",
            ">PRINT LN(0)",
            "",
            "Log range",
            ">PRINT (-2)^0.5",
            "",
            "Log range",
            ">PRINT 0^-1",
            "",
            "Division by zero",
            ">",
        ],
    );
}

#[test]
fn the_smallest_reals_live_until_stored() {
    assert_screen(
        &[
            "@%=&1414",
            "PRINT 2^-128,2^-129,2^-130",
            "PRINT 1E-20*1E-19,1E-38/10,1.5E-39",
            "PRINT 2.94E-39/2,1.6E-39*2",
            "B=2^-129:PRINT B",
        ],
        50,
        &[
            ">@%=&1414",
            ">PRINT 2^-128,2^-129,2^-130",
            "     2.938735911E-39     1.469367953E-39",
            "                   0",
            ">PRINT 1E-20*1E-19,1E-38/10,1.5E-39",
            "                   0                   0",
            "             1.5E-39",
            ">PRINT 2.94E-39/2,1.6E-39*2",
            "            1.47E-39             3.2E-39",
            "",
            ">B=2^-129:PRINT B",
            "     1.469367953E-39",
            ">",
        ],
    );
}

/// The regression: RND began from a seed of zero, and so gave zero until
/// seeded, where BASIC II begins from a seed of its own.
#[test]
fn rnd_begins_from_basic_ii_s_seed() {
    assert_screen(
        &["P.RND(1)", "P.RND"],
        50,
        &[">P.RND(1)", "0.440400091", ">P.RND", "1.09686661E9", ">"],
    );
}

/// A seed survives BREAK, but BASIC starts again from its own one that
/// finds the seed all zero.
#[test]
fn break_keeps_rnd_s_seed_unless_it_is_zero() {
    const SEED: &str = "S$=\"\":FOR I%=13 TO 17:S$=S$+STR$~(I%?0)+\" \":NEXT:P.S$";
    for (line, seed) in [
        ("X=RND(1)", "70 BE F 75 41"),
        ("FOR I%=13 TO 17:I%?0=0:NEXT", "41 52 57 0 0"),
    ] {
        let mut machine = typed(&[line], 30);
        press_break(&mut machine, &[]);
        type_text(&mut machine, &format!("{SEED}\n")).expect("a key types each character");
        wait(&mut machine, 50);
        let first = format!(">{}", &SEED[..PROMPTED]);
        assert_eq!(
            from(&mode7_text(&machine), &first),
            [first.as_str(), "+\" \":NEXT:P.S$", seed, ">"],
            "{line}"
        );
    }
}

#[test]
fn rnd_steps_seeds_and_scales() {
    assert_screen(
        &[
            "X=RND(-7):A=RND(1):B=RND(0):PRINT A=B,A",
            "C=RND(10):D=RND(0):PRINT C,D,RND(0)=D",
            "E=RND(-1):PRINT E,RND(0)",
            "PRINT RND(-1.5),RND(3.7),RND(0)",
            "X=RND(-7):PRINT RND,RND(1)",
        ],
        60,
        &[
            ">X=RND(-7):A=RND(1):B=RND(0):PRINT A=B,A",
            "",
            "        -10.985946685",
            ">C=RND(10):D=RND(0):PRINT C,D,RND(0)=D",
            "         70.601554857                 -1",
            "",
            ">E=RND(-1):PRINT E,RND(0)",
            "        -1         1",
            ">PRINT RND(-1.5),RND(3.7),RND(0)",
            "        -1         30.996200591",
            ">X=RND(-7):PRINT RND,RND(1)",
            "-2.14745703E9       0.601554857",
            ">",
        ],
    );
}

#[test]
fn int_rounds_down_whatever_bits_its_shift_drops() {
    // The first's bits below the point are &FD in the bytes shifted out and
    // three ones among the bits.
    assert_screen(
        &[
            "C=-2587221021/2147483648:PRINT INT(C);\" \";INT(-C);\" \";INT(C*2);\" \";INT(-1.5)",
            "D=-&7FFFFF01/256:PRINT INT(D);\" \";D",
        ],
        50,
        &[
            ">C=-2587221021/2147483648:PRINT INT(C);\"",
            " \";INT(-C);\" \";INT(C*2);\" \";INT(-1.5)",
            "        -2 1 -3 -2",
            ">D=-&7FFFFF01/256:PRINT INT(D);\" \";D",
            "  -8388608 -8388607",
            ">",
        ],
    );
}
