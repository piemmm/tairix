//! `cargo xtask unicode` implementation.
//!
//! `lib/unicode` answers the character properties the display-safety policy
//! needs — `Script`, `Script_Extensions`, the nonspacing marks and
//! `Default_Ignorable_Code_Point` — normalization's combining classes,
//! decompositions and compositions, and stringprep's tables (RFC 3454), from
//! tables generated here out of the Unicode Character Database files and the
//! RFC committed in `lib/unicode/assets/`. Each is byte-identical to its
//! upstream release and pinned by SHA-256, so anyone can check the snapshot
//! against upstream and nothing reaches the tables unreviewed.
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
const EXCLUSIONS: Pinned = Pinned {
    file: "CompositionExclusions.txt",
    sha256: "c759b100e9ae8960ae6b50fc5765950a7581fd9f30d4ef95f68f25a6d97e818e",
};
const CORRECTIONS: Pinned = Pinned {
    file: "NormalizationCorrections.txt",
    sha256: "b4f46d241e924c3803cb18c6a9224cadcee365bb24a2e1fbf33c2f4de955cd5d",
};
/// Read by `lib/unicode`'s conformance test alone; pinned here so the test
/// holds the crate to the release its tables are.
const NORMALIZATION_TEST: Pinned = Pinned {
    file: "NormalizationTest.txt",
    sha256: "25a50d816764b04abfb4a646d3eb2b2a803284c3873d9a06757b94fe4513dde3",
};
/// RFC 3454, stringprep: its tables are Unicode 3.2's, which stringprep pins.
const STRINGPREP: Pinned = Pinned {
    file: "rfc3454.txt",
    sha256: "eb722fa698fb7e8823b835d9fd263e4cdb8f1c7b0d234edf7f0e3bd2ccbb2c79",
};

/// The Unicode release stringprep is defined over.
const STRINGPREP_VERSION: (u8, u8, u8) = (3, 2, 0);

/// The RFC 3454 tables SASLprep prohibits in its output (RFC 4013 §2.3).
const SASLPREP_PROHIBITED: [&str; 10] = [
    "C.1.2", "C.2.1", "C.2.2", "C.3", "C.4", "C.5", "C.6", "C.7", "C.8", "C.9",
];

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
        noteln!("xtask: [unicode --write] {TABLES}");
        return std::fs::write(&path, generated)
            .map_err(|e| format!("unicode: cannot write {}: {e}", path.display()));
    }
    noteln!("xtask: [unicode] {TABLES}");
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

/// What `UnicodeData.txt` contributes.
struct Data {
    /// Each canonical decomposition mapping (UAX #15): a sixth field with no
    /// `<tag>`.
    canonical: BTreeMap<u32, Vec<u32>>,
    /// Each compatibility decomposition mapping: a sixth field with a `<tag>`.
    compatibility: BTreeMap<u32, Vec<u32>>,
    /// Each nonzero canonical combining class.
    classes: BTreeMap<u32, u8>,
    /// The code points it files as `Mn` or `Me`, as merged ranges.
    marks: Vec<(u32, u32)>,
    /// The Hangul syllables, which decompose by arithmetic, not by mapping.
    syllables: (u32, u32),
}

impl Data {
    fn read(text: &str) -> Result<Self, String> {
        let mut canonical = BTreeMap::new();
        let mut compatibility = BTreeMap::new();
        let mut classes = BTreeMap::new();
        let mut marks = Vec::new();
        let (mut first_syllable, mut last_syllable) = (None, None);
        for line in text.lines() {
            let fields: Vec<&str> = line.split(';').collect();
            let [code, name, category, class, _, mapping, ..] = fields[..] else {
                return Err(format!("unicode: short UnicodeData line `{line}`"));
            };
            let code = code_point(code)?;
            match name {
                "<Hangul Syllable, First>" => first_syllable = Some(code),
                "<Hangul Syllable, Last>" => last_syllable = Some(code),
                _ => {}
            }
            if matches!(category, "Mn" | "Me") {
                marks.push((code, code));
            }
            let class: u8 = class
                .parse()
                .map_err(|_| format!("unicode: bad combining class for U+{code:04X}"))?;
            if class != 0 {
                classes.insert(code, class);
            }
            if mapping.is_empty() {
                continue;
            }
            let tagged = mapping.starts_with('<');
            let parts = mapping
                .split_whitespace()
                .filter(|part| !part.starts_with('<'))
                .map(code_point)
                .collect::<Result<Vec<_>, _>>()?;
            if tagged {
                compatibility.insert(code, parts);
            } else {
                if !(1..=2).contains(&parts.len()) {
                    return Err(format!(
                        "unicode: U+{code:04X}'s canonical mapping is not one or two characters"
                    ));
                }
                canonical.insert(code, parts);
            }
        }
        let (Some(first), Some(last)) = (first_syllable, last_syllable) else {
            return Err("unicode: UnicodeData lists no Hangul syllables".to_string());
        };
        let data = Self {
            canonical,
            compatibility,
            classes,
            marks: merged(marks),
            syllables: (first, last),
        };
        // The runtime decomposes a syllable by arithmetic before any mapping
        // is looked up, so no mapping may reach one.
        let mapped = data.canonical.values().chain(data.compatibility.values());
        if mapped.flatten().any(|&cp| (first..=last).contains(&cp)) {
            return Err("unicode: a decomposition mapping holds a Hangul syllable".to_string());
        }
        Ok(data)
    }

    fn class(&self, code: u32) -> u8 {
        self.classes.get(&code).copied().unwrap_or(0)
    }

    /// `code`'s full decomposition (UAX #15 D68), its compatibility mappings
    /// applied too when `compatibility`.
    fn expand(
        &self,
        code: u32,
        compatibility: bool,
        out: &mut Vec<u32>,
        depth: usize,
    ) -> Result<(), String> {
        // A path through an acyclic mapping visits each code point once.
        if depth > self.canonical.len() + self.compatibility.len() {
            return Err(format!("unicode: decomposition cycle through U+{code:04X}"));
        }
        let mapping = self.canonical.get(&code).or_else(|| {
            compatibility
                .then(|| self.compatibility.get(&code))
                .flatten()
        });
        match mapping {
            Some(parts) => {
                for &part in parts {
                    self.expand(part, compatibility, out, depth + 1)?;
                }
            }
            None => out.push(code),
        }
        Ok(())
    }

    /// Every decomposable character's full decompositions.
    fn decompositions(&self) -> Result<Vec<Decomposition>, String> {
        let codes: std::collections::BTreeSet<u32> = self
            .canonical
            .keys()
            .chain(self.compatibility.keys())
            .copied()
            .collect();
        let mut out = Vec::with_capacity(codes.len());
        for code in codes {
            let canonical = if self.canonical.contains_key(&code) {
                let mut full = Vec::new();
                self.expand(code, false, &mut full, 0)?;
                Some(full)
            } else {
                None
            };
            let mut compatibility = Vec::new();
            self.expand(code, true, &mut compatibility, 0)?;
            out.push(Decomposition {
                code,
                canonical,
                compatibility,
            });
        }
        Ok(out)
    }

    /// Every primary composite (UAX #15 D114) as `(first, second, composite)`
    /// sorted by the pair: each canonical pair whose character is not in
    /// `excluded` and not a non-starter decomposition, its own class or its
    /// first character's being nonzero. A singleton maps to no pair.
    fn compositions(
        &self,
        excluded: &std::collections::BTreeSet<u32>,
    ) -> Result<Vec<(u32, u32, u32)>, String> {
        let mut out: Vec<(u32, u32, u32)> = self
            .canonical
            .iter()
            .filter_map(|(&code, parts)| match parts[..] {
                [first, second]
                    if !excluded.contains(&code)
                        && self.class(code) == 0
                        && self.class(first) == 0 =>
                {
                    Some((first, second, code))
                }
                _ => None,
            })
            .collect();
        out.sort_unstable();
        if out
            .windows(2)
            .any(|pair| (pair[0].0, pair[0].1) == (pair[1].0, pair[1].1))
        {
            return Err("unicode: two composites share a pair".to_string());
        }
        Ok(out)
    }

    /// Every nonzero combining class as `(first, last, class)`, neighbours of
    /// one class merged.
    fn class_ranges(&self) -> Vec<(u32, u32, u8)> {
        let mut out: Vec<(u32, u32, u8)> = Vec::new();
        for (&code, &class) in &self.classes {
            match out.last_mut() {
                Some(prev) if prev.1 + 1 == code && prev.2 == class => prev.1 = code,
                _ => out.push((code, code, class)),
            }
        }
        out
    }
}

/// A decomposable character's full decompositions: the canonical one, which
/// a character with only a compatibility mapping lacks, and the compatibility
/// one.
struct Decomposition {
    code: u32,
    canonical: Option<Vec<u32>>,
    compatibility: Vec<u32>,
}

/// The code points `CompositionExclusions.txt` lists.
fn exclusions(text: &str) -> Result<std::collections::BTreeSet<u32>, String> {
    let mut out = std::collections::BTreeSet::new();
    for record in records(text) {
        let (first, last) = code_points(record[0])?;
        out.extend(first..=last);
    }
    Ok(out)
}

/// Each decomposition `NormalizationCorrections.txt` records as corrected
/// after stringprep's release, as `(code point, its decomposition there)`:
/// one character each, which `data` maps to the correction and which maps to
/// nothing further.
fn decomposed_in_stringprep(text: &str, data: &Data) -> Result<Vec<(u32, u32)>, String> {
    let mut out = Vec::new();
    for record in records(text) {
        let [code, original, corrected, version] = record[..] else {
            return Err(format!("unicode: bad correction `{}`", record.join(";")));
        };
        let code = code_point(code)?;
        let release = version
            .split('.')
            .map(str::parse::<u8>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| format!("unicode: bad correction version `{version}`"))?;
        let [major, minor, update] = release[..] else {
            return Err(format!("unicode: bad correction version `{version}`"));
        };
        let corrected = code_point(corrected)?;
        if data.canonical.get(&code).map(Vec::as_slice) != Some(&[corrected]) {
            return Err(format!(
                "unicode: U+{code:04X}'s correction is not the mapping UnicodeData holds"
            ));
        }
        if (major, minor, update) <= STRINGPREP_VERSION {
            continue;
        }
        let original = code_point(original)?;
        if data.canonical.contains_key(&original) || data.compatibility.contains_key(&original) {
            return Err(format!(
                "unicode: U+{code:04X}'s original decomposition decomposes further"
            ));
        }
        out.push((code, original));
    }
    out.sort_unstable();
    Ok(out)
}

/// The full decompositions with each distinct sequence held once.
struct Pooled {
    /// `(code point, canonical first, canonical len, compatibility first,
    /// compatibility len)` into `pool`.
    entries: Vec<(u32, u16, u8, u16, u8)>,
    pool: Vec<u32>,
}

fn pooled(decompositions: &[Decomposition]) -> Result<Pooled, String> {
    let mut pool = Vec::new();
    let mut placed = BTreeMap::new();
    let mut entries = Vec::with_capacity(decompositions.len());
    for decomposition in decompositions {
        let (canonical_at, canonical_len) = match &decomposition.canonical {
            Some(parts) => place(parts, &mut pool, &mut placed)?,
            None => (0, 0),
        };
        let (compatibility_at, compatibility_len) =
            place(&decomposition.compatibility, &mut pool, &mut placed)?;
        entries.push((
            decomposition.code,
            canonical_at,
            canonical_len,
            compatibility_at,
            compatibility_len,
        ));
    }
    Ok(Pooled { entries, pool })
}

/// Where `parts` stands in `pool`, appended unless `placed` says it already
/// stands there, and its length.
fn place<'a>(
    parts: &'a [u32],
    pool: &mut Vec<u32>,
    placed: &mut BTreeMap<&'a [u32], u16>,
) -> Result<(u16, u8), String> {
    let len = u8::try_from(parts.len())
        .map_err(|_| "unicode: a decomposition is longer than a byte counts".to_string())?;
    if let Some(&at) = placed.get(parts) {
        return Ok((at, len));
    }
    let at = u16::try_from(pool.len())
        .map_err(|_| "unicode: the decompositions overflow their pool".to_string())?;
    pool.extend_from_slice(parts);
    placed.insert(parts, at);
    Ok((at, len))
}

/// One line of an RFC 3454 table: a code point or a `first-last` range, then
/// its `;` fields.
struct Row<'a> {
    range: (u32, u32),
    fields: Vec<&'a str>,
}

/// RFC 3454's table `name`, the RFC's page footers and headers between its
/// lines passed over.
fn rfc_table<'a>(text: &'a str, name: &str) -> Result<Vec<Row<'a>>, String> {
    let start = format!("----- Start Table {name} -----");
    let end = format!("----- End Table {name} -----");
    let mut lines = text.lines().skip_while(|line| line.trim() != start);
    if lines.next().is_none() {
        return Err(format!("unicode: RFC 3454 has no table {name}"));
    }
    let mut rows = Vec::new();
    for line in lines {
        let data = line.trim();
        if data == end {
            return Ok(rows);
        }
        if data.is_empty() || line.starts_with("Hoffman & Blanchet") || line.starts_with("RFC 3454")
        {
            continue;
        }
        let mut fields = data.split(';').map(str::trim);
        let span = fields.next().unwrap_or_default();
        let range = if let Some((first, last)) = span.split_once('-') {
            (code_point(first)?, code_point(last)?)
        } else {
            let cp = code_point(span)?;
            (cp, cp)
        };
        if range.0 > range.1 {
            return Err(format!("unicode: reversed range `{span}` in table {name}"));
        }
        rows.push(Row {
            range,
            fields: fields.collect(),
        });
    }
    Err(format!("unicode: RFC 3454's table {name} never ends"))
}

/// The code points of RFC 3454's table `name`, as merged ranges.
fn rfc_ranges(text: &str, name: &str) -> Result<Vec<(u32, u32)>, String> {
    Ok(merged(
        rfc_table(text, name)?
            .into_iter()
            .map(|row| row.range)
            .collect(),
    ))
}

/// What stringprep's tables contribute (RFC 3454 appendices A to D), as
/// SASLprep reads them (RFC 4013).
struct Stringprep {
    unassigned: Vec<(u32, u32)>,
    mapped_to_nothing: Vec<(u32, u32)>,
    spaces: Vec<(u32, u32)>,
    prohibited: Vec<(u32, u32)>,
    right_to_left: Vec<(u32, u32)>,
    left_to_right: Vec<(u32, u32)>,
}

impl Stringprep {
    fn read(text: &str) -> Result<Self, String> {
        let mapping = rfc_table(text, "B.1")?;
        if let Some(Row {
            range: (code, _), ..
        }) = mapping.iter().find(|row| row.fields.first() != Some(&""))
        {
            return Err(format!(
                "unicode: table B.1 maps U+{code:04X} to something; SASLprep maps it to nothing"
            ));
        }
        let mut prohibited = Vec::new();
        for table in SASLPREP_PROHIBITED {
            prohibited.extend(rfc_ranges(text, table)?);
        }
        Ok(Self {
            unassigned: rfc_ranges(text, "A.1")?,
            mapped_to_nothing: merged(mapping.into_iter().map(|row| row.range).collect()),
            spaces: rfc_ranges(text, "C.1.2")?,
            prohibited: merged(prohibited),
            right_to_left: rfc_ranges(text, "D.1")?,
            left_to_right: rfc_ranges(text, "D.2")?,
        })
    }
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

    let (major, minor, update) = VERSION;
    let mut out = String::new();
    let _ = write!(
        out,
        "// GENERATED FILE — DO NOT EDIT.\n\
         //\n\
         // Emitted by `cargo xtask unicode --write` from the Unicode \
         {major}.{minor}.{update}\n\
         // character database in `lib/unicode/assets/` (Unicode License V3; see\n\
         // `LICENSE` there), the Recommended Scripts of UAX #31 revision 45, and\n\
         // the tables of RFC 3454 (stringprep) in `rfc3454.txt` there.\n\
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
    emit_normalization(&mut out, root, &data)?;
    emit_stringprep(&mut out, root, &data)?;
    Ok(out)
}

/// Emit the normalization tables (UAX #15): the combining classes, the full
/// decompositions and the primary composites `data` and the composition
/// exclusions make.
fn emit_normalization(out: &mut String, root: &Path, data: &Data) -> Result<(), String> {
    let decompositions = data.decompositions()?;
    let pooled = pooled(&decompositions)?;
    let longest = decompositions
        .iter()
        .map(|decomposition| decomposition.compatibility.len())
        .max()
        .unwrap_or(1);
    let compositions = data.compositions(&exclusions(&read(root, &EXCLUSIONS)?)?)?;
    let least_composing = compositions
        .iter()
        .map(|&(_, second, _)| second)
        .min()
        .ok_or_else(|| "unicode: no character composes".to_string())?;
    let classes = data.class_ranges();
    let least_combining = classes
        .first()
        .map(|&(first, _, _)| first)
        .ok_or_else(|| "unicode: no character combines".to_string())?;
    read(root, &NORMALIZATION_TEST)?;
    let (syllable_first, syllable_last) = data.syllables;
    let _ = write!(
        out,
        "\n/// The most characters one character fully decomposes to.\n\
         pub const MAX_DECOMPOSITION: usize = {longest};\n\
         \n\
         /// The Hangul syllables, which decompose and compose by arithmetic.\n\
         pub(crate) const SYLLABLES: (u32, u32) = ({}, {});\n\
         \n\
         /// The least code point of a nonzero canonical combining class.\n\
         pub(crate) const LEAST_COMBINING: u32 = {};\n\
         \n\
         /// The nonzero canonical combining classes, as `(first, last, class)`.\n\
         pub(crate) const COMBINING_CLASSES: &[(u32, u32, u8)] = &[\n",
        literal(u64::from(syllable_first), 4),
        literal(u64::from(syllable_last), 4),
        literal(u64::from(least_combining), 4)
    );
    for (first, last, class) in &classes {
        let _ = writeln!(
            out,
            "    ({}, {}, {class}),",
            literal(u64::from(*first), 4),
            literal(u64::from(*last), 4)
        );
    }
    out.push_str(
        "];\n\n\
         /// Every character with a decomposition mapping but the Hangul syllables, as\n\
         /// `(code point, canonical first, canonical len, compatibility first,\n\
         /// compatibility len)` into [`DECOMPOSITION_POOL`], where its full\n\
         /// decompositions stand: a canonical len of 0 for one whose mapping is a\n\
         /// compatibility mapping alone.\n\
         pub(crate) const DECOMPOSITIONS: &[(u32, u16, u8, u16, u8)] = &[\n",
    );
    for (code, canonical_at, canonical_len, compatibility_at, compatibility_len) in &pooled.entries
    {
        let _ = writeln!(
            out,
            "    ({}, {canonical_at}, {canonical_len}, {compatibility_at}, {compatibility_len}),",
            literal(u64::from(*code), 4)
        );
    }
    out.push_str(
        "];\n\n/// The full decompositions [`DECOMPOSITIONS`] points into.\n\
         pub(crate) const DECOMPOSITION_POOL: &[char] = &[\n",
    );
    for row in pooled.pool.chunks(8) {
        let chars: Vec<String> = row.iter().map(|cp| format!("'\\u{{{cp:04X}}}'")).collect();
        let _ = writeln!(out, "    {},", chars.join(", "));
    }
    let _ = write!(
        out,
        "];\n\n\
         /// The least character to follow another in a primary composite's pair.\n\
         pub(crate) const LEAST_COMPOSING: u32 = {};\n\
         \n\
         /// Every primary composite, as `(first, second, composite)` sorted by the\n\
         /// pair.\n\
         pub(crate) const COMPOSITIONS: &[(u32, u32, u32)] = &[\n",
        literal(u64::from(least_composing), 4)
    );
    for (first, second, composite) in &compositions {
        let _ = writeln!(
            out,
            "    ({}, {}, {}),",
            literal(u64::from(*first), 4),
            literal(u64::from(*second), 4),
            literal(u64::from(*composite), 4)
        );
    }
    Ok(())
}

/// Emit stringprep's tables (RFC 3454) as SASLprep reads them, and the
/// decompositions Unicode 3.2 held that `data` holds corrected.
fn emit_stringprep(out: &mut String, root: &Path, data: &Data) -> Result<(), String> {
    let corrected = decomposed_in_stringprep(&read(root, &CORRECTIONS)?, data)?;
    let stringprep = Stringprep::read(&read(root, &STRINGPREP)?)?;
    let (major, minor, update) = STRINGPREP_VERSION;
    let _ = write!(
        out,
        "];\n\n\
         /// The decompositions Unicode {major}.{minor}.{update} held that a later release\n\
         /// corrected (`NormalizationCorrections.txt`), as `(code point, decomposition)`.\n\
         pub(crate) const DECOMPOSED_IN_3_2: &[(u32, u32)] = &[\n"
    );
    for (code, original) in &corrected {
        let _ = writeln!(
            out,
            "    ({}, {}),",
            literal(u64::from(*code), 4),
            literal(u64::from(*original), 4)
        );
    }
    out.push_str("];\n\n/// RFC 3454 table A.1: the code points Unicode 3.2 does not assign.\n");
    push_ranges(out, "UNASSIGNED_3_2", &stringprep.unassigned);
    out.push_str("\n/// RFC 3454 table B.1: what stringprep maps to nothing.\n");
    push_ranges(out, "MAPPED_TO_NOTHING", &stringprep.mapped_to_nothing);
    out.push_str("\n/// RFC 3454 table C.1.2: the non-ASCII spaces.\n");
    push_ranges(out, "NON_ASCII_SPACES", &stringprep.spaces);
    out.push_str(
        "\n/// What SASLprep prohibits in what it prepares (RFC 4013 \u{a7}2.3): RFC 3454\n\
         /// tables C.1.2, C.2.1, C.2.2 and C.3 to C.9.\n",
    );
    push_ranges(out, "SASLPREP_PROHIBITED", &stringprep.prohibited);
    out.push_str("\n/// RFC 3454 table D.1: the characters of bidirectional category R or AL.\n");
    push_ranges(out, "RIGHT_TO_LEFT", &stringprep.right_to_left);
    out.push_str("\n/// RFC 3454 table D.2: the characters of bidirectional category L.\n");
    push_ranges(out, "LEFT_TO_RIGHT", &stringprep.left_to_right);
    Ok(())
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

    /// A `UnicodeData.txt` fragment: `e`, two marks, a character canonically mapped
    /// through another, a compatibility mapping, a singleton and the Hangul
    /// syllables' bounds.
    const FRAGMENT: &str = "0065;LATIN SMALL LETTER E;Ll;0;L;;;;;N;;;0045;;0045\n\
        0302;COMBINING CIRCUMFLEX ACCENT;Mn;230;NSM;;;;;N;;;;;\n\
        0323;COMBINING DOT BELOW;Mn;220;NSM;;;;;N;;;;;\n\
        0344;COMBINING GREEK DIALYTIKA TONOS;Mn;230;NSM;0302 0323;;;;N;;;;;\n\
        1EB9;E WITH DOT BELOW;Ll;0;L;0065 0323;;;;N;;;;;\n\
        1EC7;E WITH CIRCUMFLEX AND DOT BELOW;Ll;0;L;1EB9 0302;;;;N;;;;;\n\
        212B;ANGSTROM SIGN;Lu;0;L;1EB9;;;;N;;;;;\n\
        3392;SQUARE MHZ;So;0;L;<square> 1EC7 0048;;;;N;;;;;\n\
        AC00;<Hangul Syllable, First>;Lo;0;L;;;;;N;;;;;\n\
        D7A3;<Hangul Syllable, Last>;Lo;0;L;;;;;N;;;;;\n";

    #[test]
    fn decompositions_expand_fully_each_way() {
        let data = Data::read(FRAGMENT).expect("well-formed");
        assert_eq!(data.marks, [(0x302, 0x302), (0x323, 0x323), (0x344, 0x344)]);
        assert_eq!(data.syllables, (0xac00, 0xd7a3));
        let decompositions = data.decompositions().expect("acyclic");
        let of = |code: u32| {
            decompositions
                .iter()
                .find(|decomposition| decomposition.code == code)
                .map(|decomposition| {
                    (
                        decomposition.canonical.clone(),
                        decomposition.compatibility.clone(),
                    )
                })
        };
        assert_eq!(
            of(0x1ec7),
            Some((Some(vec![0x65, 0x323, 0x302]), vec![0x65, 0x323, 0x302]))
        );
        assert_eq!(
            of(0x3392),
            Some((None, vec![0x65, 0x323, 0x302, 0x48])),
            "a compatibility mapping is no canonical one, yet expands through them"
        );
        assert_eq!(of(0x65), None);
        let pooled = pooled(&decompositions).expect("fits");
        let at = pooled
            .entries
            .iter()
            .find(|entry| entry.0 == 0x1ec7)
            .expect("listed");
        assert_eq!((at.1, at.2), (at.3, at.4), "one sequence is pooled once");
    }

    #[test]
    fn composites_are_the_canonical_pairs_nothing_excludes() {
        let data = Data::read(FRAGMENT).expect("well-formed");
        let none = std::collections::BTreeSet::new();
        assert_eq!(
            data.compositions(&none),
            Ok(vec![(0x65, 0x323, 0x1eb9), (0x1eb9, 0x302, 0x1ec7)]),
            "the singleton and the non-starter decomposition compose nothing"
        );
        let excluded = std::collections::BTreeSet::from([0x1ec7]);
        assert_eq!(
            data.compositions(&excluded),
            Ok(vec![(0x65, 0x323, 0x1eb9)])
        );
        assert_eq!(
            data.class_ranges(),
            [
                (0x302, 0x302, 230),
                (0x323, 0x323, 220),
                (0x344, 0x344, 230)
            ]
        );
    }

    #[test]
    fn a_decomposition_cycle_is_refused() {
        let text = "0041;A;Lu;0;L;0042;;;;N;;;;;\n0042;B;Lu;0;L;0041;;;;N;;;;;\n\
                    AC00;<Hangul Syllable, First>;Lo;0;L;;;;;N;;;;;\n\
                    D7A3;<Hangul Syllable, Last>;Lo;0;L;;;;;N;;;;;\n";
        let data = Data::read(text).expect("well-formed");
        assert!(data.decompositions().is_err());
    }

    #[test]
    fn a_mapping_the_runtime_could_not_follow_is_refused() {
        let syllables = "AC00;<Hangul Syllable, First>;Lo;0;L;;;;;N;;;;;\n\
                         D7A3;<Hangul Syllable, Last>;Lo;0;L;;;;;N;;;;;\n";
        let into_a_syllable = format!("3200;X;So;0;L;<circle> AC00;;;;N;;;;;\n{syllables}");
        assert!(Data::read(&into_a_syllable).is_err());
        let three = format!("00C0;X;Lu;0;L;0041 0300 0301;;;;N;;;;;\n{syllables}");
        assert!(Data::read(&three).is_err());
        assert!(
            Data::read("0041;A;Lu;0;L;;;;;N;;;;;\n").is_err(),
            "no syllables"
        );
    }

    #[test]
    fn only_corrections_made_after_unicode_3_2_are_undone() {
        let data = Data::read(
            "F951;X;Lo;0;L;964B;;;;N;;;;;\n2F868;Y;Lo;0;L;36FC;;;;N;;;;;\n\
             AC00;<Hangul Syllable, First>;Lo;0;L;;;;;N;;;;;\n\
             D7A3;<Hangul Syllable, Last>;Lo;0;L;;;;;N;;;;;\n",
        )
        .expect("well-formed");
        let corrections = "F951;96FB;964B;3.2.0 # Corrigendum 3\n\
                           2F868;2136A;36FC;4.0.0 # Corrigendum 4\n";
        assert_eq!(
            decomposed_in_stringprep(corrections, &data),
            Ok(vec![(0x2f868, 0x2136a)])
        );
        assert!(
            decomposed_in_stringprep("2F868;2136A;1234;4.0.0\n", &data).is_err(),
            "a correction must be the mapping the release holds"
        );
    }

    #[test]
    fn rfc_tables_are_read_past_their_page_furniture() {
        let text = "   ----- Start Table X.1 -----\n   0221\n   0234-024F; NAME\n\n\n\n\
                    Hoffman & Blanchet          Standards Track                    [Page 23]\n\
                    \u{c}\nRFC 3454        Preparation of Internationalized Strings   December 2002\n\n\
                    \n   1D173-1D17A; [MUSICAL]\n   ----- End Table X.1 -----\n";
        assert_eq!(
            rfc_ranges(text, "X.1"),
            Ok(vec![(0x221, 0x221), (0x234, 0x24f), (0x1d173, 0x1d17a)])
        );
        assert!(rfc_ranges(text, "X.2").is_err(), "a table it lacks");
        let stray =
            "   ----- Start Table X.1 -----\n   0221\n   oops\n   ----- End Table X.1 -----\n";
        assert!(
            rfc_ranges(stray, "X.1").is_err(),
            "a line that is no code point"
        );
        let open = "   ----- Start Table X.1 -----\n   0221\n";
        assert!(rfc_ranges(open, "X.1").is_err(), "a table that never ends");
    }

    #[test]
    fn neighbouring_ranges_merge() {
        assert_eq!(
            merged(vec![(5, 6), (0, 2), (3, 4), (8, 9)]),
            [(0, 6), (8, 9)]
        );
    }
}
