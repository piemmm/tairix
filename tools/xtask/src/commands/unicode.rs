//! `cargo xtask unicode` implementation.
//!
//! `lib/unicode` answers the character properties the display-safety policy
//! needs — `Script`, `Script_Extensions`, the nonspacing marks, the marks each
//! canonical decomposition carries and `Default_Ignorable_Code_Point` — from
//! tables generated here out of the Unicode Character Database files committed
//! in `lib/unicode/assets/`. Each is
//! byte-identical to its unicode.org release and pinned by SHA-256, so anyone
//! can check the snapshot against upstream and nothing reaches the tables
//! unreviewed.
//!
//! - `cargo xtask unicode` (part of `cargo xtask ci`) regenerates the tables in
//!   memory and fails closed if `lib/unicode/src/tables.rs` has drifted.
//! - `cargo xtask unicode --write` rewrites it.
//!
//! A new Unicode release is taken by replacing the assets, their pins,
//! [`VERSION`] and [`RECOMMENDED`] together, then regenerating; the build never
//! touches the network.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use crate::Context;

const ASSETS: &str = "lib/unicode/assets";
const TABLES: &str = "lib/unicode/src/tables.rs";

/// The Unicode version the assets are.
const VERSION: (u8, u8, u8) = (18, 0, 0);

/// One committed asset and the SHA-256 it must hash to.
struct Pinned {
    file: &'static str,
    sha256: &'static str,
}

const SCRIPTS: Pinned = Pinned {
    file: "Scripts.txt",
    sha256: "0071fd81b6aeae25f6e8bce8efec3066a6476a91b49bdb2f52dc76e817862a6a",
};
const EXTENSIONS: Pinned = Pinned {
    file: "ScriptExtensions.txt",
    sha256: "5c9d34a922f687726f2a8bcf57d49f905987e51f1b21b58c95a00fbe255cec23",
};
const ALIASES: Pinned = Pinned {
    file: "PropertyValueAliases.txt",
    sha256: "06c4c8eaf7b0bf34abe73b113da1215bd784ac254d4c223600b90267caa4bbbd",
};
const CATEGORIES: Pinned = Pinned {
    file: "DerivedGeneralCategory.txt",
    sha256: "d6b151d2d40ee9b1876d26f417980f45ffae47b6055ccf7203cb31f07a030f94",
};
const CORE: Pinned = Pinned {
    file: "DerivedCoreProperties.txt",
    sha256: "09c928886a178fcafd93c29e4bd59073a058e5a100b716d425cb563ab50f68c9",
};
const DATA: Pinned = Pinned {
    file: "UnicodeData.txt",
    sha256: "0736451de439ae7baf1425136617da495e09ee5afbe6e394374db7009ea08950",
};

/// UAX #31 revision 45, Table 5: the Recommended Scripts.
const RECOMMENDED: [&str; 30] = [
    "Zyyy", "Zinh", "Arab", "Armn", "Beng", "Cyrl", "Deva", "Ethi", "Geor", "Grek", "Gujr", "Guru",
    "Hang", "Hani", "Hebr", "Hira", "Kana", "Knda", "Khmr", "Laoo", "Latn", "Mlym", "Mymr", "Orya",
    "Sinh", "Taml", "Telu", "Thaa", "Thai", "Tibt",
];

/// The writing systems UTS #39 augments a script set with, which are no
/// Script value of their own.
const AUGMENTED: [&str; 4] = ["Hanb", "Hntl", "Jpan", "Kore"];

/// The scripts the lookups in `lib/unicode` name.
const NAMED: [&str; 15] = [
    "Zyyy", "Zinh", "Zzzz", "Latn", "Cyrl", "Grek", "Hani", "Hira", "Kana", "Hang", "Bopo", "Hanb",
    "Hntl", "Jpan", "Kore",
];

/// The words a script set takes.
const SET_WORDS: usize = 3;

type Set = [u64; SET_WORDS];

/// The highest code point.
const LAST: u32 = 0x10_ffff;

pub fn run(ctx: &Context, args: &[std::ffi::OsString]) -> Result<(), String> {
    let write = match args {
        [] => false,
        [flag] if flag == "--write" => true,
        _ => return Err("unicode: usage: cargo xtask unicode [--write]".to_string()),
    };
    let generated = generate(&ctx.workspace_root)?;
    let path = ctx.workspace_root.join(TABLES);
    if write {
        eprintln!("xtask: [unicode --write] {TABLES}");
        return std::fs::write(&path, generated)
            .map_err(|e| format!("unicode: cannot write {}: {e}", path.display()));
    }
    eprintln!("xtask: [unicode] {TABLES}");
    match std::fs::read(&path) {
        Ok(committed) if committed == generated.as_bytes() => Ok(()),
        _ => Err(format!(
            "unicode: `{TABLES}` is out of sync with `{ASSETS}`; \
             run `cargo xtask unicode --write` and commit the result."
        )),
    }
}

/// The asset `pinned`, once it hashes to its pin.
fn read(root: &Path, pinned: &Pinned) -> Result<String, String> {
    let path = root.join(ASSETS).join(pinned.file);
    let bytes = std::fs::read(&path)
        .map_err(|e| format!("unicode: cannot read {}: {e}", path.display()))?;
    let mut expected = [0; tairix_crypto::SHA256_OUTPUT_LEN];
    if !tairix_util::hex::decode_into(pinned.sha256, &mut expected)
        || tairix_crypto::sha256(&bytes) != expected
    {
        return Err(format!(
            "unicode: {} does not hash to its pinned SHA-256",
            path.display()
        ));
    }
    String::from_utf8(bytes).map_err(|_| format!("unicode: {} is not UTF-8", path.display()))
}

/// The data lines of a UCD file: comments stripped, blank lines skipped, each
/// split into its trimmed `;` fields.
fn records(text: &str) -> impl Iterator<Item = Vec<&str>> {
    text.lines().filter_map(|line| {
        let data = line.split('#').next().unwrap_or("").trim();
        (!data.is_empty()).then(|| data.split(';').map(str::trim).collect())
    })
}

fn code_point(hex: &str) -> Result<u32, String> {
    u32::from_str_radix(hex, 16)
        .ok()
        .filter(|&cp| cp <= LAST)
        .ok_or_else(|| format!("unicode: bad code point `{hex}`"))
}

/// A UCD code point field: one code point, or `first..last`.
fn code_points(field: &str) -> Result<(u32, u32), String> {
    let (first, last) = if let Some((first, last)) = field.split_once("..") {
        (code_point(first)?, code_point(last)?)
    } else {
        let cp = code_point(field)?;
        (cp, cp)
    };
    if first > last {
        return Err(format!("unicode: reversed range `{field}`"));
    }
    Ok((first, last))
}

/// The `(first, last)` ranges of `text` whose second field is `value`, sorted
/// and merged.
fn ranges_of(text: &str, value: &str) -> Result<Vec<(u32, u32)>, String> {
    let mut ranges = Vec::new();
    for record in records(text) {
        if record.get(1) == Some(&value) {
            ranges.push(code_points(record[0])?);
        }
    }
    Ok(merged(ranges))
}

fn merged(mut ranges: Vec<(u32, u32)>) -> Vec<(u32, u32)> {
    ranges.sort_unstable();
    let mut out: Vec<(u32, u32)> = Vec::with_capacity(ranges.len());
    for (first, last) in ranges {
        match out.last_mut() {
            Some(prev) if first <= prev.1.saturating_add(1) => prev.1 = prev.1.max(last),
            _ => out.push((first, last)),
        }
    }
    out
}

/// Every script, by short code: the Script values of `PropertyValueAliases`,
/// then [`AUGMENTED`], indexed in that order.
struct Scripts {
    codes: Vec<String>,
    by_code: BTreeMap<String, u8>,
    by_name: BTreeMap<String, u8>,
}

impl Scripts {
    fn read(aliases: &str) -> Result<Self, String> {
        let mut values: Vec<(String, String)> = records(aliases)
            .filter(|record| record.first() == Some(&"sc") && record.len() >= 3)
            .map(|record| (record[1].to_string(), record[2].to_string()))
            .collect();
        values.sort();
        let mut scripts = Self {
            codes: Vec::new(),
            by_code: BTreeMap::new(),
            by_name: BTreeMap::new(),
        };
        for (code, name) in values {
            let index = scripts.add(&code)?;
            scripts.by_name.insert(name, index);
        }
        for code in AUGMENTED {
            if scripts.by_code.contains_key(code) {
                return Err(format!("unicode: {code} became a Script value"));
            }
            scripts.add(code)?;
        }
        if scripts.codes.len() > SET_WORDS * 64 {
            return Err("unicode: more scripts than a script set holds".to_string());
        }
        Ok(scripts)
    }

    fn add(&mut self, code: &str) -> Result<u8, String> {
        let index = u8::try_from(self.codes.len())
            .map_err(|_| "unicode: more scripts than a byte numbers".to_string())?;
        self.codes.push(code.to_string());
        self.by_code.insert(code.to_string(), index);
        Ok(index)
    }

    fn code(&self, code: &str) -> Result<u8, String> {
        self.by_code
            .get(code)
            .copied()
            .ok_or_else(|| format!("unicode: unknown script code `{code}`"))
    }

    fn set_of<'a>(&self, codes: impl IntoIterator<Item = &'a str>) -> Result<Set, String> {
        let mut set = [0u64; SET_WORDS];
        for code in codes {
            let index = usize::from(self.code(code)?);
            set[index / 64] |= 1 << (index % 64);
        }
        Ok(set)
    }
}

/// Every code point's script as `(first, script)` entries, each running to
/// the next one's first, the gaps Unknown.
fn script_entries(text: &str, scripts: &Scripts) -> Result<Vec<(u32, u8)>, String> {
    let unknown = scripts.code("Zzzz")?;
    let mut ranges = Vec::new();
    for record in records(text) {
        let (first, last) = code_points(record[0])?;
        let name = record
            .get(1)
            .ok_or_else(|| format!("unicode: no script for `{}`", record[0]))?;
        let script = scripts
            .by_name
            .get(*name)
            .copied()
            .ok_or_else(|| format!("unicode: unknown script `{name}`"))?;
        ranges.push((first, last, script));
    }
    ranges.sort_unstable();
    let mut entries: Vec<(u32, u8)> = Vec::new();
    let mut next = 0u32;
    for (first, last, script) in ranges {
        if first < next {
            return Err(format!(
                "unicode: overlapping script range at U+{first:04X}"
            ));
        }
        if first > next {
            push_entry(&mut entries, next, unknown);
        }
        push_entry(&mut entries, first, script);
        next = last + 1;
    }
    if next <= LAST {
        push_entry(&mut entries, next, unknown);
    }
    Ok(entries)
}

fn push_entry(entries: &mut Vec<(u32, u8)>, first: u32, script: u8) {
    if entries.last().map(|&(_, prev)| prev) != Some(script) {
        entries.push((first, script));
    }
}

/// The code points whose `Script_Extensions` are explicit.
struct Extensions {
    /// `(first, last, set)`, merged where neighbours share a set.
    entries: Vec<(u32, u32, u8)>,
    /// The distinct sets.
    sets: Vec<Set>,
}

fn extension_entries(text: &str, scripts: &Scripts) -> Result<Extensions, String> {
    let mut sets: Vec<Set> = Vec::new();
    let mut ranges = Vec::new();
    for record in records(text) {
        let (first, last) = code_points(record[0])?;
        let codes = record
            .get(1)
            .ok_or_else(|| format!("unicode: no extensions for `{}`", record[0]))?;
        let set = scripts.set_of(codes.split_whitespace())?;
        if set == [0; SET_WORDS] {
            return Err(format!("unicode: empty extensions for `{}`", record[0]));
        }
        let index = if let Some(index) = sets.iter().position(|known| *known == set) {
            index
        } else {
            sets.push(set);
            sets.len() - 1
        };
        let index = u8::try_from(index)
            .map_err(|_| "unicode: more extension sets than a byte numbers".to_string())?;
        ranges.push((first, last, index));
    }
    ranges.sort_unstable();
    let mut entries: Vec<(u32, u32, u8)> = Vec::new();
    for (first, last, set) in ranges {
        match entries.last_mut() {
            Some(prev) if first <= prev.1 => {
                return Err(format!("unicode: overlapping extensions at U+{first:04X}"));
            }
            Some(prev) if first == prev.1 + 1 && prev.2 == set => prev.1 = last,
            _ => entries.push((first, last, set)),
        }
    }
    Ok(Extensions { entries, sets })
}

fn contains(ranges: &[(u32, u32)], cp: u32) -> bool {
    let after = ranges.partition_point(|&(first, _)| first <= cp);
    after
        .checked_sub(1)
        .is_some_and(|index| cp <= ranges[index].1)
}

/// What `UnicodeData.txt` contributes.
struct Data {
    /// Each canonical decomposition mapping (UAX #15): a sixth field with no
    /// `<tag>`.
    mappings: BTreeMap<u32, Vec<u32>>,
    /// The code points it files as `Mn` or `Me`, as merged ranges.
    marks: Vec<(u32, u32)>,
}

impl Data {
    fn read(text: &str) -> Result<Self, String> {
        let mut mappings = BTreeMap::new();
        let mut marks = Vec::new();
        for line in text.lines() {
            let fields: Vec<&str> = line.split(';').collect();
            let [code, _, category, _, _, mapping, ..] = fields[..] else {
                return Err(format!("unicode: short UnicodeData line `{line}`"));
            };
            let code = code_point(code)?;
            if matches!(category, "Mn" | "Me") {
                marks.push((code, code));
            }
            if mapping.is_empty() || mapping.starts_with('<') {
                continue;
            }
            let parts = mapping
                .split_whitespace()
                .map(code_point)
                .collect::<Result<Vec<_>, _>>()?;
            mappings.insert(code, parts);
        }
        Ok(Self {
            mappings,
            marks: merged(marks),
        })
    }

    /// The nonspacing marks of every full canonical decomposition carrying
    /// any, in decomposition order.
    fn decomposed_marks(&self) -> Result<Vec<(u32, Vec<u32>)>, String> {
        let mut out = Vec::new();
        for &code in self.mappings.keys() {
            let mut full = Vec::new();
            self.expand(code, &mut full, 0)?;
            let carried: Vec<u32> = full
                .into_iter()
                .filter(|&cp| contains(&self.marks, cp))
                .collect();
            if !carried.is_empty() {
                out.push((code, carried));
            }
        }
        Ok(out)
    }

    fn expand(&self, code: u32, out: &mut Vec<u32>, depth: usize) -> Result<(), String> {
        // A path through an acyclic mapping visits each code point once.
        if depth > self.mappings.len() {
            return Err(format!("unicode: decomposition cycle through U+{code:04X}"));
        }
        match self.mappings.get(&code) {
            Some(parts) => {
                for &part in parts {
                    self.expand(part, out, depth + 1)?;
                }
            }
            None => out.push(code),
        }
        Ok(())
    }
}

/// The decomposed marks with each distinct sequence held once.
struct Pooled {
    /// `(code point, first, len)` into `pool`.
    entries: Vec<(u32, u16, u8)>,
    pool: Vec<u32>,
}

fn pooled(decomposed: &[(u32, Vec<u32>)]) -> Result<Pooled, String> {
    let mut pool = Vec::new();
    let mut placed: BTreeMap<&[u32], u16> = BTreeMap::new();
    let mut entries = Vec::with_capacity(decomposed.len());
    for (code, marks) in decomposed {
        let at = if let Some(&at) = placed.get(marks.as_slice()) {
            at
        } else {
            let at = u16::try_from(pool.len())
                .map_err(|_| "unicode: decomposed marks overflow their pool".to_string())?;
            pool.extend_from_slice(marks);
            placed.insert(marks, at);
            at
        };
        let len = u8::try_from(marks.len())
            .map_err(|_| format!("unicode: U+{code:04X} carries too many marks"))?;
        entries.push((*code, at, len));
    }
    Ok(Pooled { entries, pool })
}

fn generate(root: &Path) -> Result<String, String> {
    let scripts = Scripts::read(&read(root, &ALIASES)?)?;
    let script_table = script_entries(&read(root, &SCRIPTS)?, &scripts)?;
    let extensions = extension_entries(&read(root, &EXTENSIONS)?, &scripts)?;
    let categories = read(root, &CATEGORIES)?;
    let marks = merged(
        ranges_of(&categories, "Mn")?
            .into_iter()
            .chain(ranges_of(&categories, "Me")?)
            .collect(),
    );
    let ignorable = ranges_of(&read(root, &CORE)?, "Default_Ignorable_Code_Point")?;
    let recommended = scripts.set_of(RECOMMENDED)?;
    let data = Data::read(&read(root, &DATA)?)?;
    if data.marks != marks {
        return Err(format!(
            "unicode: {} and {} disagree on the marks; the assets are not one release",
            DATA.file, CATEGORIES.file
        ));
    }
    let decomposed = pooled(&data.decomposed_marks()?)?;

    let (major, minor, update) = VERSION;
    let mut out = String::new();
    let _ = write!(
        out,
        "// GENERATED FILE — DO NOT EDIT.\n\
         //\n\
         // Emitted by `cargo xtask unicode --write` from the Unicode \
         {major}.{minor}.{update}\n\
         // character database in `lib/unicode/assets/` (Unicode License V3; see\n\
         // `LICENSE` there) and the Recommended Scripts of UAX #31 revision 45.\n\
         // `cargo xtask unicode` (run by `ci`) fails closed if this file drifts\n\
         // from a fresh generation.\n\
         \n\
         //! The generated property tables, pure data read by the lookups in\n\
         //! `lib.rs`.\n\
         \n\
         /// The Unicode version the tables describe.\n\
         pub const VERSION: (u8, u8, u8) = ({major}, {minor}, {update});\n\
         \n\
         /// The words of a script set.\n\
         pub(crate) const SET_WORDS: usize = {SET_WORDS};\n\
         \n"
    );
    for code in NAMED {
        let _ = writeln!(
            out,
            "pub(crate) const {}: u8 = {};",
            code.to_uppercase(),
            scripts.code(code)?
        );
    }
    let _ = write!(
        out,
        "\n/// UAX #31's Recommended Scripts.\n\
         pub(crate) const RECOMMENDED: [u64; SET_WORDS] =\n    {};\n\
         \n\
         /// Every code point's script, as `(first, script)`: each entry runs to the\n\
         /// next one's first.\n\
         pub(crate) const SCRIPTS: &[(u32, u8)] = &[\n",
        set_literal(&recommended)
    );
    for (first, script) in &script_table {
        let _ = writeln!(out, "    ({}, {script}),", literal(u64::from(*first), 4));
    }
    out.push_str(
        "];\n\n\
         /// The code points whose `Script_Extensions` are not their script alone, as\n\
         /// `(first, last, set)` into [`EXTENSION_SETS`].\n\
         pub(crate) const EXTENSIONS: &[(u32, u32, u8)] = &[\n",
    );
    for (first, last, set) in &extensions.entries {
        let _ = writeln!(
            out,
            "    ({}, {}, {set}),",
            literal(u64::from(*first), 4),
            literal(u64::from(*last), 4)
        );
    }
    out.push_str("];\n\n/// The distinct explicit `Script_Extensions`.\n");
    out.push_str("pub(crate) const EXTENSION_SETS: &[[u64; SET_WORDS]] = &[\n");
    for set in &extensions.sets {
        let _ = writeln!(out, "    {},", set_literal(set));
    }
    out.push_str("];\n\n/// The nonspacing marks: `General_Category` `Mn` or `Me`.\n");
    push_ranges(&mut out, "NONSPACING_MARKS", &marks);
    out.push_str("\n/// The `Default_Ignorable_Code_Point` code points.\n");
    push_ranges(&mut out, "DEFAULT_IGNORABLE", &ignorable);
    out.push_str(
        "\n/// The nonspacing marks of each full canonical decomposition carrying any, as\n\
         /// `(code point, first, len)` into [`DECOMPOSED_MARKS`].\n\
         pub(crate) const DECOMPOSED: &[(u32, u16, u8)] = &[\n",
    );
    for (code, at, len) in &decomposed.entries {
        let _ = writeln!(out, "    ({}, {at}, {len}),", literal(u64::from(*code), 4));
    }
    out.push_str(
        "];\n\n/// The mark sequences [`DECOMPOSED`] points into.\n\
         pub(crate) const DECOMPOSED_MARKS: &[char] = &[\n",
    );
    for mark in &decomposed.pool {
        let _ = writeln!(out, "    '\\u{{{mark:04X}}}',");
    }
    out.push_str("];\n");
    Ok(out)
}

fn set_literal(set: &Set) -> String {
    let words: Vec<String> = set.iter().map(|&word| literal(word, 16)).collect();
    format!("[{}]", words.join(", "))
}

/// `value` as a hex literal of at least `digits` digits, grouped in fours
/// from the right once it is longer than one group.
fn literal(value: u64, digits: usize) -> String {
    let hex = format!("{value:0digits$X}");
    let mut grouped = String::new();
    for (index, digit) in hex.chars().enumerate() {
        if index > 0 && (hex.len() - index) % 4 == 0 {
            grouped.push('_');
        }
        grouped.push(digit);
    }
    format!("0x{grouped}")
}

fn push_ranges(out: &mut String, name: &str, ranges: &[(u32, u32)]) {
    let _ = writeln!(out, "pub(crate) const {name}: &[(u32, u32)] = &[");
    for (first, last) in ranges {
        let _ = writeln!(
            out,
            "    ({}, {}),",
            literal(u64::from(*first), 4),
            literal(u64::from(*last), 4)
        );
    }
    out.push_str("];\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_strip_comments_and_split_fields() {
        let text = "# header\n\n0041..005A    ; Latin # L&  [26] ...\n00AD ; X\n";
        let rows: Vec<Vec<&str>> = records(text).collect();
        assert_eq!(rows, [vec!["0041..005A", "Latin"], vec!["00AD", "X"]]);
    }

    #[test]
    fn code_points_read_one_or_a_range_and_refuse_the_rest() {
        assert_eq!(code_points("0041"), Ok((0x41, 0x41)));
        assert_eq!(code_points("0041..005A"), Ok((0x41, 0x5a)));
        assert!(code_points("005A..0041").is_err());
        assert!(code_points("110000").is_err());
        assert!(code_points("zz").is_err());
    }

    #[test]
    fn literals_group_their_digits_in_fours() {
        assert_eq!(literal(0x41, 4), "0x0041");
        assert_eq!(literal(0x1_f300, 4), "0x1_F300");
        assert_eq!(literal(0x10_ffff, 4), "0x10_FFFF");
        assert_eq!(literal(0x67_60a0_3000_0828, 16), "0x0067_60A0_3000_0828");
    }

    #[test]
    fn decompositions_expand_fully_and_keep_only_their_marks() {
        // U+1EC7 maps to U+1EB9 + U+0302, and U+1EB9 to e + U+0323; U+00C5's
        // compatibility mapping is no canonical one.
        let text = "0065;LATIN SMALL LETTER E;Ll;0;L;;;;;N;;;0045;;0045\n\
                    0302;COMBINING CIRCUMFLEX ACCENT;Mn;230;NSM;;;;;N;;;;;\n\
                    0323;COMBINING DOT BELOW;Mn;220;NSM;;;;;N;;;;;\n\
                    1EB9;E WITH DOT BELOW;Ll;0;L;0065 0323;;;;N;;;;;\n\
                    1EC7;E WITH CIRCUMFLEX AND DOT BELOW;Ll;0;L;1EB9 0302;;;;N;;;;;\n\
                    3392;SQUARE MHZ;So;0;L;<square> 004D 0048 007A;;;;N;;;;;\n";
        let data = Data::read(text).expect("well-formed");
        assert_eq!(data.marks, [(0x302, 0x302), (0x323, 0x323)]);
        assert_eq!(
            data.decomposed_marks(),
            Ok(vec![(0x1eb9, vec![0x323]), (0x1ec7, vec![0x323, 0x302])])
        );
        let pooled = pooled(&[(1, vec![7]), (2, vec![7]), (3, vec![7, 8])]).expect("fits");
        assert_eq!(pooled.entries, [(1, 0, 1), (2, 0, 1), (3, 1, 2)]);
        assert_eq!(pooled.pool, [7, 7, 8]);
    }

    #[test]
    fn a_decomposition_cycle_is_refused() {
        let text = "0041;A;Lu;0;L;0042;;;;N;;;;;\n0042;B;Lu;0;L;0041;;;;N;;;;;\n";
        let data = Data::read(text).expect("well-formed");
        assert!(data.decomposed_marks().is_err());
    }

    #[test]
    fn neighbouring_ranges_merge() {
        assert_eq!(
            merged(vec![(5, 6), (0, 2), (3, 4), (8, 9)]),
            [(0, 6), (8, 9)]
        );
    }
}
