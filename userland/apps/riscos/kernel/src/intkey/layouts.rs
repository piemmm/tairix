//! TAIRiX's keyboard layouts for InternationalKeyboard, each drawn from its
//! country's standard layout: a key's characters plain, with Shift, with
//! Alt, and with Alt and Shift, Alt being the layout's AltGr. A combining
//! mark is a dead key for that accent; `∅` is no character; a key with one
//! character gives it with Shift too.
//!
//! The rows run left to right across the keyboard: the number row's thirteen
//! keys, the top letter row's twelve, the home row's twelve — the last the
//! key beside Return — and the bottom row's eleven, the first the key beside
//! the left Shift. What a key gives with Ctrl, and with Caps Lock, follows
//! from these by rule.

/// A layer of a layout: its four rows.
pub(crate) type Layer = [&'static str; 4];

/// A layout: the keyboards it serves, its layers — a second a script's,
/// switched to by the Kana key or, where it says so, by the left Alt key
/// with Shift, which is then no Alt key — the keys a country's keyboards
/// have beyond the rows, each one's characters in every layer, and what the
/// keypad's decimal key gives.
#[derive(Clone, Copy)]
pub(crate) struct Layout {
    pub(crate) keyboards: &'static [u32],
    pub(crate) layers: &'static [Layer],
    pub(crate) left_alt_switches: bool,
    pub(crate) extra: &'static [(u32, &'static [&'static str])],
    pub(crate) decimal: u8,
}

impl Layout {
    /// The characters of `key` in `layer`, as its row or its extra keys have
    /// them: none for a key the layout does not give.
    pub(crate) fn token(&self, layer: usize, key: u32) -> Option<&'static str> {
        let rows = self.layers.get(layer)?;
        for (row, keys) in rows.iter().zip(ROWS) {
            if let Some(column) = keys.iter().position(|&k| k == key) {
                return row.split(' ').nth(column);
            }
        }
        let (_, layers) = self.extra.iter().find(|(k, _)| *k == key)?;
        layers.get(layer).copied()
    }
}

/// The low-level key numbers of each row's keys.
pub(crate) const ROWS: [&[u32]; 4] = [
    &[
        0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C,
    ],
    &[
        0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x30, 0x31, 0x32,
    ],
    &[
        0x3C, 0x3D, 0x3E, 0x3F, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x33,
    ],
    &[
        0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57,
    ],
];

/// The key left of Backspace and the key left of the right Shift, which
/// Japanese keyboards have.
const YEN_KEY: u32 = 0x1D;
const RO_KEY: u32 = 0x6E;

const fn layout(keyboards: &'static [u32], layers: &'static [Layer]) -> Layout {
    Layout {
        keyboards,
        layers,
        left_alt_switches: false,
        extra: &[],
        decimal: b'.',
    }
}

/// A layout whose keypad gives a decimal comma.
const fn with_comma(layout: Layout) -> Layout {
    Layout {
        decimal: b',',
        ..layout
    }
}

/// The United Kingdom's, BS 4822's with an Alt layer of TAIRiX's: the
/// vowels' acute forms, the euro, the broken bar, and the accents dead.
const UK: Layer = [
    r#"`¬¦ 1! 2"∅̈ 3£ 4$€ 5% 6^∅̂ 7& 8* 9( 0) -_ =+"#,
    "qQ wWẃẂ eEéÉ rR tT yYýÝ uUúÚ iIíÍ oOóÓ pP [{ ]}",
    "aAáÁ sS dD fF gG hH jJ kK lL ;: '@́ #~̃",
    r"\|̀ zZ xX cCçÇ vV bB nN mM ,< .> /?",
];

/// The United States', ANSI: the key beside Return is above it, and the
/// bottom row's first is the same key's twin.
const USA: Layer = [
    "`~ 1! 2@ 3# 4$ 5% 6^ 7& 8* 9( 0) -_ =+",
    "qQ wW eE rR tT yY uU iI oO pP [{ ]}",
    r#"aA sS dD fF gG hH jJ kK lL ;: '" \|"#,
    r"\| zZ xX cC vV bB nN mM ,< .> /?",
];

/// Germany's, DIN 2137-1: QWERTZ, the circumflex, acute and grave dead.
const GERMANY: Layer = [
    r#"̂° 1! 2"² 3§³ 4$ 5% 6& 7/{ 8([ 9)] 0=} ß?\ ́̀"#,
    "qQ@ wW eE€ rR tT zZ uU iI oO pP üÜ +*~",
    "aA sS dD fF gG hH jJ kK lL öÖ äÄ #'",
    "<>| yY xX cC vV bB nN mMµ ,; .: -_",
];

/// Switzerland's, SN 074021: QWERTZ with the French letters, the circumflex,
/// grave, diaeresis, acute and tilde dead.
const SWITZERLAND: Layer = [
    r#"§° 1+¦ 2"@ 3*# 4ç 5% 6&¬ 7/| 8(¢ 9) 0= '?́ ̂̀̃"#,
    "qQ wW eE€ rR tT zZ uU iI oO pP üè[ ̈!]",
    "aA sS dD fF gG hH jJ kK lL éö àä{ $£}",
    r"<>\ yY xX cC vV bB nN mM ,; .: -_",
];

/// France's traditional AZERTY: the circumflex and diaeresis dead.
const FRANCE: Layer = [
    r#"²∅ &1 é2̃ "3# '4{ (5[ -6| è7̀ _8\ ç9^ à0@ )°] =+}"#,
    "aA zZ eE€ rR tT yY uU iI oO pP ̂̈ $£¤",
    "qQ sS dD fF gG hH jJ kK lL mM ù% *µ",
    "<> wW xX cC vV bB nN ,? ;. :/ !§",
];

/// Belgium's AZERTY: France's, the accents' keys and the Alt layer its own.
const BELGIUM: Layer = [
    r#"²³ &1| é2@ "3# '4 (5 §6^ è7 !8 ç9{ à0} )° -_"#,
    "aA zZ eE€ rR tT yY uU iI oO pP ̂̈[ $*]",
    "qQ sS dD fF gG hH jJ kK lL mM ù%́ µ£̀",
    r"<>\ wW xX cC vV bB nN ,? ;. :/ =+̃",
];

/// Spain's: the grave, circumflex, acute and diaeresis dead.
const SPAIN: Layer = [
    r#"ºª\ 1!| 2"@ 3·# 4$~ 5%€ 6&¬ 7/ 8( 9) 0= '? ¡¿"#,
    "qQ wW eE€ rR tT yY uU iI oO pP ̀̂[ +*]",
    "aA sS dD fF gG hH jJ kK lL ñÑ ́̈{ çÇ}",
    "<> zZ xX cC vV bB nN mM ,; .: -_",
];

/// Latin America's: Spain's letters, the acute and diaeresis dead.
const LATIN_AMERICA: Layer = [
    r#"|°¬ 1! 2" 3# 4$ 5% 6& 7/ 8( 9) 0= '?\ ¿¡"#,
    "qQ@ wW eE rR tT yY uU iI oO pP ́̈ +*~",
    "aA sS dD fF gG hH jJ kK lL ñÑ {[̂ }]̀",
    "<> zZ xX cC vV bB nN mM ,; .: -_",
];

/// Italy's: the accented vowels on keys of their own.
const ITALY: Layer = [
    r#"\| 1! 2" 3£ 4$€ 5% 6& 7/ 8( 9) 0= '? ì^"#,
    "qQ wW eE€ rR tT yY uU iI oO pP èé[{ +*]}",
    "aA sS dD fF gG hH jJ kK lL òç@ à°# ù§",
    "<> zZ xX cC vV bB nN mM ,; .: -_",
];

/// Sweden's and Finland's, SS 66 22 41: the diaeresis, circumflex, tilde,
/// acute and grave dead.
const SWEDEN_FINLAND: Layer = [
    r#"§½ 1! 2"@ 3#£ 4¤$ 5%€ 6& 7/{ 8([ 9)] 0=} +?\ ́̀"#,
    "qQ wW eE€ rR tT yY uU iI oO pP åÅ ̈̂~̃",
    "aA sS dD fF gG hH jJ kK lL öÖ äÄ '*",
    "<>| zZ xX cC vV bB nN mMµ ,; .: -_",
];

/// Denmark's: Sweden's but for æ and ø.
const DENMARK: Layer = [
    r#"½§ 1! 2"@ 3#£ 4¤$ 5%€ 6& 7/{ 8([ 9)] 0=} +? ́̀|"#,
    "qQ wW eE€ rR tT yY uU iI oO pP åÅ ̈̂~̃",
    "aA sS dD fF gG hH jJ kK lL æÆ øØ '*",
    r"<>\ zZ xX cC vV bB nN mMµ ,; .: -_",
];

/// Norway's, NS 4180: ø and æ the other way about from Denmark's.
const NORWAY: Layer = [
    r#"|§ 1! 2"@ 3#£ 4¤$ 5%€ 6& 7/{ 8([ 9)] 0=} +? \̀́"#,
    "qQ wW eE€ rR tT yY uU iI oO pP åÅ ̈̂~̃",
    "aA sS dD fF gG hH jJ kK lL øØ æÆ '*",
    "<> zZ xX cC vV bB nN mMµ ,; .: -_",
];

/// Iceland's: ð, þ and æ, the ring and the acute dead.
const ICELAND: Layer = [
    r#"̊̈ 1! 2" 3# 4$ 5% 6& 7/{ 8([ 9)] 0=} öÖ\ -_"#,
    "qQ@ wW eE€ rR tT yY uU iI oO pP ðÐ '?~",
    "aA sS dD fF gG hH jJ kK lL æÆ ́̈^ +*`",
    "<>| zZ xX cC vV bB nN mMµ ,; .: þÞ",
];

/// The Netherlands': the diaeresis and the acute dead.
const NETHERLANDS: Layer = [
    r#"@§¬ 1!¹ 2"² 3#³ 4$¼ 5%½ 6&¾ 7_£ 8({ 9)} 0' /?\ °̃¸"#,
    "qQ wW eE€ rR tT yY uU iI oO pP ̈̂ *|",
    "aA sSß dD fF gG hH jJ kK lL +± ́̀ <>",
    "][¦ zZ xX cC¢ vV bB nN mMµ ,; .· -=",
];

/// Turkey's Q layout: the dotted and dotless i on keys of their own, the
/// circumflex dead.
const TURKEY: Layer = [
    r#""é< 1!> 2'£ 3̂# 4+$ 5%½ 6& 7/{ 8([ 9)] 0=} *?\ -_|"#,
    "qQ@ wW eE€ rR tT yY uU ıI oO pP ğĞ üÜ~",
    "aAæ sSß dD fF gG hH jJ kK lL şŞ iİ ,;`",
    "<>| zZ xX cC vV bB nN mM öÖ çÇ .:",
];

/// Greece's: the Greek letters, the tonos and dialytika dead; Latin by the
/// Alt layer.
const GREECE: Layer = [
    "`~ 1! 2@² 3#³ 4$£€ 5%§ 6^¶ 7& 8*¤ 9(¦ 0)° -_± =+½",
    ";:qQ ςΣwW εΕeE ρΡrR τΤtT υΥyY θΘuU ιΙiI οΟoO πΠpP [{« ]}»",
    r#"αΑaA σΣsS δΔdD φΦfF γΓgG ηΗhH ξΞjJ κΚkK λΛlL ́̈ '" \|¬"#,
    r"<>\ ζΖzZ χΧxX ψΨcC ωΩvV βΒbB νΝnN μΜmM ,< .> /?",
];

/// Russia's, ГОСТ 6431: QWERTY first, ЙЦУКЕН the second layer, which the
/// left Alt key with Shift switches between.
const RUSSIA: [Layer; 2] = [
    USA,
    [
        r#"ёЁ 1! 2" 3№ 4; 5% 6: 7? 8* 9( 0) -_ =+"#,
        "йЙ цЦ уУ кК еЕ нН гГ шШ щЩ зЗ хХ ъЪ",
        r"фФ ыЫ вВ аА пП рР оО лЛ дД жЖ эЭ \/",
        r"\/ яЯ чЧ сС мМ иИ тТ ьЬ бБ юЮ .,",
    ],
];

/// Israel's, SI 1452: Hebrew, Latin by Shift.
const ISRAEL: Layer = [
    ";~ 1! 2@ 3# 4$₪ 5% 6^ 7& 8* 9) 0( -_ =+",
    "/Q 'W קE€ רR אT טY וU ןI םO פP ]} [{",
    r#"שA דS גD כF עG יH חJ לK ךL ף: ," \|"#,
    r"\| זZ סX בC הV נB מN צM ת> ץ< .?",
];

/// Esperanto's: the UK's rows, the six letters with their accents in place
/// of the keys Esperanto does not use, which Alt gives.
const ESPERANTO: Layer = [
    r#"`¬ 1! 2" 3£ 4$€ 5% 6^ 7& 8* 9( 0) -_ =+"#,
    "ŝŜqQ ĝĜwW eE rR tT ŭŬyY uU iI oO pP ĵĴ[{ ĥĤ]}",
    "aA sS dD fF gG hH jJ kK lL ;: '@ ĉĈ#~",
    r"\| zZ xX cC vV bB nN mM ,< .> /?",
];

/// Wales's: the UK's, with ŵ and ŷ and the circumflexed vowels by Alt.
const WALES: Layer = [
    r#"`¬¦ 1! 2" 3£ 4$€ 5% 6^ 7& 8* 9( 0) -_ =+"#,
    "qQ wWŵŴ eEêÊ rR tT yYŷŶ uUûÛ iIîÎ oOôÔ pP [{ ]}",
    "aAâÂ sS dD fF gG hH jJ kK lL ;: '@ #~",
    r"\|̂ zZ xX cC vV bB nN mM ,< .> /?",
];

/// Dvorak's, ANSI X4.22, the UK's symbols where the UK has them.
const DVORAK_UK: Layer = [
    r#"`¬¦ 1! 2" 3£ 4$€ 5% 6^ 7& 8* 9( 0) [{ ]}"#,
    "'@ ,< .> pP yY fF gG cC rR lL /? =+",
    "aA oO eE uU iI dD hH tT nN sS -_ #~",
    r"\| ;: qQ jJ kK xX bB mM wW vV zZ",
];

/// Dvorak's for the United States.
const DVORAK_USA: Layer = [
    "`~ 1! 2@ 3# 4$ 5% 6^ 7& 8* 9( 0) [{ ]}",
    r#"'" ,< .> pP yY fF gG cC rR lL /? =+"#,
    r"aA oO eE uU iI dD hH tT nN sS -_ \|",
    r"\| ;: qQ jJ kK xX bB mM wW vV zZ",
];

/// Colemak's, on the UK's symbols.
const COLEMAK_UK: Layer = [
    r#"`¬¦ 1! 2" 3£ 4$€ 5% 6^ 7& 8* 9( 0) -_ =+"#,
    "qQ wW fF pP gG jJ lL uU yY ;: [{ ]}",
    "aA rR sS tT dD hH nN eE iI oO '@ #~",
    r"\| zZ xX cC vV bB kK mM ,< .> /?",
];

/// Japan's, JIS X 6002: QWERTY with JIS's symbols, then the kana layer the
/// Kana key switches to.
const JAPAN: [Layer; 2] = [
    [
        r#"∅∅ 1! 2" 3# 4$ 5% 6& 7' 8( 9) 0∅ -= ^~"#,
        "qQ wW eE rR tT yY uU iI oO pP @` [{",
        "aA sS dD fF gG hH jJ kK lL ;+ :* ]}",
        "∅∅ zZ xX cC vV bB nN mM ,< .> /?",
    ],
    [
        "∅∅ ぬ ふ あぁ うぅ えぇ おぉ やゃ ゆゅ よょ わを ほ へ",
        "た て いぃ す か ん な に ら せ ゛ ゜「",
        "ち と し は き く ま の り れ け む」",
        "∅∅ つっ さ そ ひ こ み も ね、 る。 め・",
    ],
];

/// Japan's keys beyond the rows: the yen key and the ro key.
const JAPAN_EXTRA: [(u32, &[&str]); 2] = [(YEN_KEY, &[r"\|", "ー"]), (RO_KEY, &[r"\_", "ろ"])];

/// Korea's, KS X 5002: QWERTY, then the two-set Hangul jamo layer.
const KOREA: [Layer; 2] = [
    USA,
    [
        "`~ 1! 2@ 3# 4$ 5% 6^ 7& 8* 9( 0) -_ =+",
        "ㅂㅃ ㅈㅉ ㄷㄸ ㄱㄲ ㅅㅆ ㅛ ㅕ ㅑ ㅐㅒ ㅔㅖ [{ ]}",
        r#"ㅁ ㄴ ㅇ ㄹ ㅎ ㅗ ㅓ ㅏ ㅣ ;: '" \|"#,
        r"\| ㅋ ㅌ ㅊ ㅍ ㅠ ㅜ ㅡ ,< .> /?",
    ],
];

/// Taiwan's: QWERTY, then the Zhuyin layer of its standard keyboard.
const TAIWAN: [Layer; 2] = [
    USA,
    [
        "`~ ㄅ! ㄉ@ ˇ# ˋ$ ㄓ% ˊ^ ˙& ㄚ* ㄞ( ㄢ) ㄦ_ =+",
        "ㄆ ㄊ ㄍ ㄐ ㄔ ㄗ ㄧ ㄛ ㄟ ㄣ [{ ]}",
        r#"ㄇ ㄋ ㄎ ㄑ ㄕ ㄘ ㄨ ㄜ ㄠ ㄤ '" \|"#,
        r"\| ㄈ ㄌ ㄏ ㄒ ㄖ ㄙ ㄩ ㄝ< ㄡ> ㄥ?",
    ],
];

/// Every layout, and the keyboards each serves; any other keyboard is the
/// UK's.
pub(crate) static LAYOUTS: [Layout; 26] = [
    layout(&[1], &[UK]),
    layout(&[4], &[ITALY]),
    layout(&[5], &[SPAIN]),
    layout(&[6], &[FRANCE]),
    with_comma(layout(&[7], &[GERMANY])),
    layout(&[9], &[ESPERANTO]),
    layout(&[10], &[GREECE]),
    with_comma(layout(&[11, 12], &[SWEDEN_FINLAND])),
    with_comma(layout(&[14], &[DENMARK])),
    with_comma(layout(&[15], &[NORWAY])),
    with_comma(layout(&[16], &[ICELAND])),
    layout(&[20], &[TURKEY]),
    Layout {
        left_alt_switches: true,
        ..layout(&[24], &RUSSIA)
    },
    layout(&[26], &[ISRAEL]),
    layout(&[28], &[LATIN_AMERICA]),
    layout(&[31], &[BELGIUM]),
    Layout {
        extra: &JAPAN_EXTRA,
        ..layout(&[32], &JAPAN)
    },
    layout(&[34], &[NETHERLANDS]),
    layout(&[35], &[SWITZERLAND]),
    layout(&[36, 49], &[WALES]),
    layout(&[48, 52], &[USA]),
    layout(&[53], &KOREA),
    layout(&[54], &TAIWAN),
    layout(&[70], &[DVORAK_UK]),
    layout(&[71], &[DVORAK_USA]),
    layout(&[72], &[COLEMAK_UK]),
];

/// The UK's layout, which serves any keyboard no other does.
pub(crate) const FALLBACK: usize = 0;
