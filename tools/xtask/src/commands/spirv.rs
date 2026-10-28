//! `cargo xtask spirv` implementation.
//!
//! `lib/spirv` decodes shader modules against the SPIR-V grammar, which this
//! command generates into `lib/spirv/src/tables.rs` from the Khronos
//! machine-readable grammars committed in `lib/spirv/assets/`. Each asset is
//! byte-identical to its SPIRV-Headers release and pinned by SHA-256, so the
//! snapshot can be checked against upstream and nothing reaches the tables
//! unreviewed.
//!
//! - `cargo xtask spirv` (part of `cargo xtask ci`) regenerates the tables in
//!   memory and fails closed if the committed file has drifted.
//! - `cargo xtask spirv --write` rewrites it.
//!
//! The grammar recognised is the core and what the Khronos multi-vendor
//! extensions (`SPV_KHR_*`, `SPV_EXT_*`) add: an instruction or enumerant
//! that is provisional, that only a single vendor's extension enables, or
//! whose every capability is itself unrecognised is left out, so a module
//! using it is refused as unrecognised. A new grammar is taken by replacing
//! the assets and their pins together, then regenerating; a grammar whose
//! shape the decoder does not handle fails generation rather than producing
//! tables it would misread.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::Path;

use super::json::{self, Json};
use crate::Context;

const ASSETS: &str = "lib/spirv/assets";
const TABLES: &str = "lib/spirv/src/tables.rs";

/// The SPIRV-Headers release the assets are.
const RELEASE: &str = "vulkan-sdk-1.4.363.0";

struct Pinned {
    file: &'static str,
    sha256: &'static str,
}

const CORE: Pinned = Pinned {
    file: "spirv.core.grammar.json",
    sha256: "c125a0fbf6730fde4625fef2618c4f7611ab42edf9b2ad5bb9291002b10bfad3",
};
const GLSL: Pinned = Pinned {
    file: "extinst.glsl.std.450.grammar.json",
    sha256: "5d72e48247569f9b77a17a849db624350abc9d26f7f72b153bbcf15c37238239",
};
const LICENSE: Pinned = Pinned {
    file: "LICENSE",
    sha256: "ea43b1de38a6f90c488800d66dec1ed671e68cda530266bc96951fb5b6307613",
};

/// The literal kinds the decoder reads, each by its own rule.
const LITERALS: [&str; 6] = [
    "LiteralInteger",
    "LiteralString",
    "LiteralFloat",
    "LiteralContextDependentNumber",
    "LiteralExtInstInteger",
    "LiteralSpecConstantOpInteger",
];

/// The id kinds: one word naming an id.
const IDS: [&str; 5] = [
    "IdResultType",
    "IdResult",
    "IdRef",
    "IdScope",
    "IdMemorySemantics",
];

/// The composite kinds and the bases the decoder reads them as.
const COMPOSITES: [(&str, [&str; 2]); 3] = [
    ("PairLiteralIntegerIdRef", ["LiteralInteger", "IdRef"]),
    ("PairIdRefLiteralInteger", ["IdRef", "LiteralInteger"]),
    ("PairIdRefIdRef", ["IdRef", "IdRef"]),
];

/// The enumerated kinds no operand names, whose values `IdScope` and
/// `IdMemorySemantics` operands carry as constants.
const CARRIED: [&str; 2] = ["Scope", "MemorySemantics"];

pub fn run(ctx: &Context, args: &[std::ffi::OsString]) -> Result<(), String> {
    let write = match args {
        [] => false,
        [flag] if flag == "--write" => true,
        _ => return Err("spirv: usage: cargo xtask spirv [--write]".to_string()),
    };
    let generated = generate(&ctx.workspace_root)?;
    let path = ctx.workspace_root.join(TABLES);
    if write {
        eprintln!("xtask: [spirv --write] {TABLES}");
        return std::fs::write(&path, generated)
            .map_err(|e| format!("spirv: cannot write {}: {e}", path.display()));
    }
    eprintln!("xtask: [spirv] {TABLES}");
    match std::fs::read(&path) {
        Ok(committed) if committed == generated.as_bytes() => Ok(()),
        _ => Err(format!(
            "spirv: `{TABLES}` is out of sync with `{ASSETS}`; \
             run `cargo xtask spirv --write` and commit the result."
        )),
    }
}

/// The asset `pinned`, once it hashes to its pin.
fn read(root: &Path, pinned: &Pinned) -> Result<String, String> {
    let path = root.join(ASSETS).join(pinned.file);
    let bytes =
        std::fs::read(&path).map_err(|e| format!("spirv: cannot read {}: {e}", path.display()))?;
    let mut expected = [0; tairix_crypto::SHA256_OUTPUT_LEN];
    if !tairix_util::hex::decode_into(pinned.sha256, &mut expected)
        || tairix_crypto::sha256(&bytes) != expected
    {
        return Err(format!(
            "spirv: {} does not hash to its pinned SHA-256",
            path.display()
        ));
    }
    String::from_utf8(bytes).map_err(|_| format!("spirv: {} is not UTF-8", path.display()))
}

fn field<'j>(value: &'j Json, key: &str, what: &str) -> Result<&'j Json, String> {
    value
        .get(key)
        .ok_or_else(|| format!("spirv: {what} has no `{key}`"))
}

fn text<'j>(value: &'j Json, key: &str, what: &str) -> Result<&'j str, String> {
    field(value, key, what)?
        .as_str()
        .ok_or_else(|| format!("spirv: {what}'s `{key}` is not a string"))
}

fn list<'j>(value: &'j Json, key: &str) -> &'j [Json] {
    value.get(key).and_then(Json::as_array).unwrap_or(&[])
}

fn names<'j>(value: &'j Json, key: &str) -> Vec<&'j str> {
    list(value, key).iter().filter_map(Json::as_str).collect()
}

/// An enumerant's value: a decimal number, or a hexadecimal string.
fn value_of(value: &Json, what: &str) -> Result<u32, String> {
    let parsed = match value {
        Json::Number(_) => value.as_u64(),
        Json::String(hex) => hex
            .strip_prefix("0x")
            .and_then(|digits| u64::from_str_radix(digits, 16).ok()),
        _ => None,
    };
    parsed
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| format!("spirv: {what} has no 32-bit value"))
}

/// The capabilities the grammar recognises, each name it goes by mapped to
/// the one it is declared under.
struct Capabilities(BTreeMap<String, String>);

impl Capabilities {
    fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

    fn canonical(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
}

/// Whether the grammar recognises `item` given the capabilities it does: not
/// provisional, part of the core from some version or else enabled by a
/// Khronos extension, and reachable through a recognised capability where it
/// names any. What a vendor's extension brought into the core is the core's.
fn recognised(item: &Json, capabilities: &Capabilities) -> bool {
    if item.get("provisional").and_then(Json::as_bool) == Some(true) {
        return false;
    }
    let core = item.get("version").and_then(Json::as_str) != Some("None");
    let extensions = names(item, "extensions");
    let khronos = core
        || extensions.is_empty()
        || extensions
            .iter()
            .any(|e| e.starts_with("SPV_KHR_") || e.starts_with("SPV_EXT_"));
    let needs = names(item, "capabilities");
    khronos && (needs.is_empty() || needs.iter().any(|c| capabilities.contains(c)))
}

/// The capabilities the grammar recognises: each reachable from the core
/// through capabilities recognised before it, under every name it goes by.
fn recognised_capabilities(kinds: &[Json]) -> Result<Capabilities, String> {
    let capability = kinds
        .iter()
        .find(|k| k.get("kind").and_then(Json::as_str) == Some("Capability"))
        .ok_or("spirv: the grammar has no Capability kind")?;
    let enumerants = list(capability, "enumerants");
    let mut kept = Capabilities(BTreeMap::new());
    loop {
        let before = kept.0.len();
        for enumerant in enumerants {
            let name = text(enumerant, "enumerant", "a capability")?;
            if !kept.contains(name) && recognised(enumerant, &kept) {
                for alias in names(enumerant, "aliases").into_iter().chain([name]) {
                    kept.0.insert(alias.to_string(), name.to_string());
                }
            }
        }
        if kept.0.len() == before {
            return Ok(kept);
        }
    }
}

/// `value` in hexadecimal, its digits grouped in fours.
fn hex(value: u32) -> String {
    let digits = format!("{value:x}");
    let mut grouped = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 4 == 0 {
            grouped.push('_');
        }
        grouped.push(digit);
    }
    format!("0x{grouped}")
}

/// `name`, one of the grammar's camel-case names, in upper snake case.
fn upper_snake(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if i > 0 && c.is_ascii_uppercase() {
            let before = chars[i - 1];
            let lower_after = chars.get(i + 1).is_some_and(char::is_ascii_lowercase);
            let last = i + 1 == chars.len();
            if before.is_ascii_lowercase()
                || (before.is_ascii_digit() && !last)
                || (before.is_ascii_uppercase() && lower_after)
            {
                out.push('_');
            }
        }
        out.push(c.to_ascii_uppercase());
    }
    out
}

/// One operand of an instruction, as the tables spell it.
struct Operand {
    kind: String,
    quantity: &'static str,
}

/// What enables an instruction or enumerant: any one of `capabilities` (for
/// a capability, those it implicitly declares), and from `version` on — or,
/// before it, any one of `extensions`; `version` is `None` where only an
/// extension does. `last` is the newest version it exists in.
struct Gate {
    capabilities: Vec<String>,
    extensions: Vec<String>,
    version: Option<(u8, u8)>,
    last: Option<(u8, u8)>,
}

struct Instruction {
    opcode: u32,
    name: String,
    operands: Vec<Operand>,
    gate: Gate,
}

struct Enumerant {
    value: u32,
    name: String,
    parameters: Vec<String>,
    gate: Gate,
}

struct ExtInst {
    number: u32,
    operands: usize,
    capabilities: Vec<String>,
}

struct Grammar {
    version: (u64, u64, u64),
    instructions: Vec<Instruction>,
    kinds: BTreeMap<String, (String, Vec<Enumerant>)>,
    glsl: Vec<ExtInst>,
}

/// A `major.minor` version as the grammar spells it.
fn version_of(spelled: &str, what: &str) -> Result<(u8, u8), String> {
    spelled
        .split_once('.')
        .and_then(|(major, minor)| Some((major.parse().ok()?, minor.parse().ok()?)))
        .ok_or_else(|| format!("spirv: {what} has version `{spelled}`"))
}

/// The gate of `item`, keeping the capabilities and extensions the grammar
/// recognises.
fn gate_of(item: &Json, capabilities: &Capabilities, what: &str) -> Result<Gate, String> {
    let version = match item.get("version").and_then(Json::as_str) {
        None => Some((1, 0)),
        Some("None") => None,
        Some(spelled) => Some(version_of(spelled, what)?),
    };
    let last = item
        .get("lastVersion")
        .and_then(Json::as_str)
        .map(|spelled| version_of(spelled, what))
        .transpose()?;
    Ok(Gate {
        capabilities: names(item, "capabilities")
            .into_iter()
            .filter_map(|c| capabilities.canonical(c))
            .map(str::to_string)
            .collect(),
        extensions: names(item, "extensions")
            .into_iter()
            .filter(|e| e.starts_with("SPV_KHR_") || e.starts_with("SPV_EXT_"))
            .map(str::to_string)
            .collect(),
        version,
        last,
    })
}

fn operand(value: &Json, what: &str) -> Result<Operand, String> {
    let quantity = match value.get("quantifier").and_then(Json::as_str) {
        None => "One",
        Some("?") => "Optional",
        Some("*") => "Many",
        Some(other) => return Err(format!("spirv: {what} has quantifier `{other}`")),
    };
    Ok(Operand {
        kind: text(value, "kind", what)?.to_string(),
        quantity,
    })
}

/// Refuse an operand list the decoder would misread: every optional or
/// repeated operand is trailing, a repeated one is last, and a result type and
/// result lead it as the grammar always places them.
fn check_operands(name: &str, operands: &[Operand]) -> Result<(), String> {
    let mut trailing = false;
    for (index, operand) in operands.iter().enumerate() {
        if trailing && operand.quantity == "One" {
            return Err(format!(
                "spirv: {name} has a required operand after an optional one"
            ));
        }
        if operand.quantity == "Many" && index + 1 != operands.len() {
            return Err(format!(
                "spirv: {name} repeats an operand that is not its last"
            ));
        }
        trailing |= operand.quantity != "One";
        let placed = match operand.kind.as_str() {
            "IdResultType" => index == 0,
            "IdResult" => index == 0 || (index == 1 && operands[0].kind == "IdResultType"),
            _ => true,
        };
        if !placed || (operand.kind.starts_with("IdResult") && operand.quantity != "One") {
            return Err(format!("spirv: {name} places its result unexpectedly"));
        }
    }
    Ok(())
}

fn parse(core: &str, glsl: &str) -> Result<Grammar, String> {
    let core = json::parse(core)?;
    let number = |key: &str| {
        field(&core, key, "the grammar")?
            .as_u64()
            .ok_or_else(|| format!("spirv: the grammar's `{key}` is not a number"))
    };
    let version = (
        number("major_version")?,
        number("minor_version")?,
        number("revision")?,
    );
    let kinds = list(&core, "operand_kinds");
    let capabilities = recognised_capabilities(kinds)?;
    let instructions = instructions_of(&core, &capabilities)?;
    let kinds = kinds_of(kinds, &instructions, &capabilities)?;
    Ok(Grammar {
        version,
        instructions,
        kinds,
        glsl: glsl_of(&json::parse(glsl)?, &capabilities)?,
    })
}

/// Every instruction recognised, by ascending opcode.
fn instructions_of(core: &Json, capabilities: &Capabilities) -> Result<Vec<Instruction>, String> {
    let mut instructions = Vec::new();
    for item in list(core, "instructions") {
        if !recognised(item, capabilities) {
            continue;
        }
        let name = text(item, "opname", "an instruction")?.to_string();
        let opcode = value_of(field(item, "opcode", &name)?, &name)?;
        let operands = list(item, "operands")
            .iter()
            .map(|o| operand(o, &name))
            .collect::<Result<Vec<_>, _>>()?;
        check_operands(&name, &operands)?;
        let gate = gate_of(item, capabilities, &name)?;
        instructions.push(Instruction {
            opcode,
            name,
            operands,
            gate,
        });
    }
    instructions.sort_by_key(|i| i.opcode);
    if instructions.windows(2).any(|w| w[0].opcode == w[1].opcode) {
        return Err("spirv: two instructions share an opcode".to_string());
    }
    Ok(instructions)
}

/// Every kind an instruction or a recognised enumerant's parameter names,
/// with its category and recognised enumerants by ascending value.
fn kinds_of(
    kinds: &[Json],
    instructions: &[Instruction],
    capabilities: &Capabilities,
) -> Result<BTreeMap<String, (String, Vec<Enumerant>)>, String> {
    let mut used: BTreeSet<String> = instructions
        .iter()
        .flat_map(|i| i.operands.iter().map(|o| o.kind.clone()))
        .chain(CARRIED.iter().map(|k| (*k).to_string()))
        .collect();
    let by_name: BTreeMap<&str, &Json> = kinds
        .iter()
        .filter_map(|k| k.get("kind").and_then(Json::as_str).map(|n| (n, k)))
        .collect();
    let mut found = BTreeMap::new();
    let mut pending: Vec<String> = used.iter().cloned().collect();
    while let Some(kind) = pending.pop() {
        let json = by_name
            .get(kind.as_str())
            .ok_or_else(|| format!("spirv: operand kind {kind} is not in the grammar"))?;
        let category = text(json, "category", &kind)?.to_string();
        let enumerants = enumerants_of(json, &kind, &category, capabilities)?;
        for enumerant in &enumerants {
            for parameter in &enumerant.parameters {
                if used.insert(parameter.clone()) {
                    pending.push(parameter.clone());
                }
            }
        }
        found.insert(kind, (category, enumerants));
    }
    for (kind, (category, _)) in &found {
        let known = match category.as_str() {
            "Id" => IDS.contains(&kind.as_str()),
            "Literal" => LITERALS.contains(&kind.as_str()),
            "Composite" => COMPOSITES.iter().any(|(name, _)| name == kind),
            "ValueEnum" | "BitEnum" => true,
            _ => false,
        };
        if !known {
            return Err(format!(
                "spirv: the decoder does not read {category} kind {kind}"
            ));
        }
    }
    for (name, bases) in COMPOSITES {
        if by_name
            .get(name)
            .is_some_and(|json| names(json, "bases") != bases)
        {
            return Err(format!("spirv: composite {name} changed its bases"));
        }
    }
    Ok(found)
}

/// The recognised enumerants of kind `kind`, by ascending value.
fn enumerants_of(
    json: &Json,
    kind: &str,
    category: &str,
    capabilities: &Capabilities,
) -> Result<Vec<Enumerant>, String> {
    let mut enumerants = Vec::new();
    for item in list(json, "enumerants") {
        if !recognised(item, capabilities) {
            continue;
        }
        let name = text(item, "enumerant", kind)?;
        let value = value_of(field(item, "value", name)?, name)?;
        if category == "BitEnum" && value & value.wrapping_sub(1) != 0 {
            return Err(format!("spirv: {kind} {name} sets more than one bit"));
        }
        let mut parameters = Vec::new();
        for parameter in list(item, "parameters") {
            let parameter = operand(parameter, name)?;
            if parameter.quantity != "One" {
                return Err(format!("spirv: {kind} {name} has a quantified parameter"));
            }
            parameters.push(parameter.kind);
        }
        enumerants.push(Enumerant {
            value,
            name: name.to_string(),
            parameters,
            gate: gate_of(item, capabilities, name)?,
        });
    }
    enumerants.sort_by_key(|e| e.value);
    if enumerants.windows(2).any(|w| w[0].value == w[1].value) {
        return Err(format!("spirv: two {kind} enumerants share a value"));
    }
    Ok(enumerants)
}

/// Each `GLSL.std.450` instruction's number, how many ids it takes, and the
/// capabilities any one of which it needs.
fn glsl_of(glsl: &Json, capabilities: &Capabilities) -> Result<Vec<ExtInst>, String> {
    let mut found = Vec::new();
    for item in list(glsl, "instructions") {
        let name = text(item, "opname", "a GLSL.std.450 instruction")?;
        let number = value_of(field(item, "opcode", name)?, name)?;
        let operands = list(item, "operands");
        if operands.iter().any(|o| {
            o.get("kind").and_then(Json::as_str) != Some("IdRef") || o.get("quantifier").is_some()
        }) {
            return Err(format!(
                "spirv: GLSL.std.450 {name} takes more than plain ids"
            ));
        }
        let needs = names(item, "capabilities")
            .into_iter()
            .map(|c| capabilities.canonical(c).map(str::to_string))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                format!("spirv: GLSL.std.450 {name} needs an unrecognised capability")
            })?;
        found.push(ExtInst {
            number,
            operands: operands.len(),
            capabilities: needs,
        });
    }
    found.sort_by_key(|inst| inst.number);
    Ok(found)
}

/// The decoder's reading of a kind.
fn shape(kind: &str, category: &str) -> String {
    match (category, kind) {
        ("Id", _) => "Shape::Id".to_string(),
        (_, "LiteralString") => "Shape::Text".to_string(),
        (_, "LiteralContextDependentNumber") => "Shape::Number".to_string(),
        (_, "LiteralSpecConstantOpInteger") => "Shape::SpecOp".to_string(),
        ("Literal", _) => "Shape::Word".to_string(),
        (_, "PairLiteralIntegerIdRef") => "Shape::SwitchTarget".to_string(),
        ("Composite", _) => {
            let bases = COMPOSITES
                .iter()
                .find(|(name, _)| *name == kind)
                .map_or(["", ""], |(_, b)| *b);
            format!("Shape::Pair(Kind::{}, Kind::{})", bases[0], bases[1])
        }
        ("BitEnum", _) => "Shape::Bits".to_string(),
        _ => "Shape::Enum".to_string(),
    }
}

fn generate(root: &Path) -> Result<String, String> {
    read(root, &LICENSE)?;
    let grammar = parse(&read(root, &CORE)?, &read(root, &GLSL)?)?;
    let (major, minor, revision) = grammar.version;
    let names = constant_names(&grammar.kinds)?;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "// GENERATED FILE — DO NOT EDIT.\n\
         //\n\
         // Emitted by `cargo xtask spirv --write` from the Khronos SPIR-V grammar\n\
         // in `lib/spirv/assets/` (SPIRV-Headers {RELEASE}; MIT, see `LICENSE`\n\
         // there). `cargo xtask spirv` (run by `ci`) fails closed if this file\n\
         // drifts from a fresh generation.\n\n\
         use crate::grammar::{{Enumerant, ExtInst, Gate, Quantity, Shape, Spec}};\n\
         use crate::{{Op, Version}};\n\
         use enumerant::Capability;\n\n\
         /// The grammar's SPIR-V version (revision {revision}), the newest a module may\n\
         /// declare.\n\
         pub(crate) const VERSION: (u8, u8) = ({major}, {minor});\n"
    );
    emit_versions(&mut out, &grammar);
    emit_ops(&mut out, &grammar.instructions)?;
    emit_enumerants(&mut out, &grammar.kinds, &names)?;
    emit_kinds(&mut out, &grammar.kinds, &names)?;
    emit_tables(&mut out, &grammar, &names)?;
    Ok(out)
}

/// The gates of every instruction and enumerant.
fn gates(grammar: &Grammar) -> impl Iterator<Item = &Gate> {
    grammar.instructions.iter().map(|i| &i.gate).chain(
        grammar
            .kinds
            .values()
            .flat_map(|(_, enumerants)| enumerants.iter().map(|e| &e.gate)),
    )
}

/// A constant per version a gate names, and the gate most entries share.
fn emit_versions(w: &mut String, grammar: &Grammar) {
    let used: BTreeSet<(u8, u8)> = gates(grammar)
        .flat_map(|g| g.version.into_iter().chain(g.last))
        .chain([(1, 0)])
        .collect();
    for (major, minor) in used {
        let _ = writeln!(
            w,
            "const V{major}_{minor}: Option<Version> = \
             Some(Version {{ major: {major}, minor: {minor} }});"
        );
    }
    let _ = writeln!(
        w,
        "\n/// Core from the first version, needing nothing.\n\
         const CORE: Gate = Gate {{ capabilities: &[], extensions: &[], version: V1_0, last: None }};\n"
    );
}

/// `gate` as the tables write it.
fn gate(gate: &Gate, names: &Names) -> Result<String, String> {
    let version = |v: Option<(u8, u8)>| v.map_or("None".to_string(), |(a, b)| format!("V{a}_{b}"));
    if gate.capabilities.is_empty()
        && gate.extensions.is_empty()
        && gate.version == Some((1, 0))
        && gate.last.is_none()
    {
        return Ok("CORE".to_string());
    }
    let capabilities = gate
        .capabilities
        .iter()
        .map(|c| capability(c, names))
        .collect::<Result<Vec<_>, _>>()?;
    let extensions: Vec<String> = gate.extensions.iter().map(|e| format!("\"{e}\"")).collect();
    Ok(format!(
        "Gate {{ capabilities: &[{}], extensions: &[{}], version: {}, last: {} }}",
        capabilities.join(", "),
        extensions.join(", "),
        version(gate.version),
        version(gate.last)
    ))
}

/// The constant naming the capability `name`.
fn capability(name: &str, names: &Names) -> Result<String, String> {
    names
        .capabilities
        .get(name)
        .map(|constant| format!("Capability::{constant}"))
        .ok_or_else(|| format!("spirv: a gate names unknown capability {name}"))
}

/// Each enumerant's constant name within its kind, keyed by kind and value,
/// and each capability's by its name.
struct Names {
    by_value: BTreeMap<(String, u32), String>,
    capabilities: BTreeMap<String, String>,
}

fn constant_names(kinds: &BTreeMap<String, (String, Vec<Enumerant>)>) -> Result<Names, String> {
    let mut names = Names {
        by_value: BTreeMap::new(),
        capabilities: BTreeMap::new(),
    };
    for (kind, (_, enumerants)) in kinds {
        let mut seen = BTreeSet::new();
        for enumerant in enumerants {
            let constant = if enumerant.name.starts_with(|c: char| c.is_ascii_digit()) {
                format!(
                    "{}_{}",
                    upper_snake(kind),
                    enumerant.name.to_ascii_uppercase()
                )
            } else {
                upper_snake(&enumerant.name)
            };
            if !seen.insert(constant.clone()) {
                return Err(format!("spirv: two {kind} enumerants are both {constant}"));
            }
            if kind == "Capability" {
                names
                    .capabilities
                    .insert(enumerant.name.clone(), constant.clone());
            }
            names
                .by_value
                .insert((kind.clone(), enumerant.value), constant);
        }
    }
    Ok(names)
}

/// An associated constant on `Op` for each instruction.
fn emit_ops(w: &mut String, instructions: &[Instruction]) -> Result<(), String> {
    let _ = writeln!(w, "impl Op {{");
    let mut seen = BTreeSet::new();
    for instruction in instructions {
        let constant = upper_snake(
            instruction
                .name
                .strip_prefix("Op")
                .unwrap_or(&instruction.name),
        );
        if !seen.insert(constant.clone()) {
            return Err(format!("spirv: two instructions are both `Op::{constant}`"));
        }
        let _ = writeln!(
            w,
            "    /// `{}`.\n    pub const {constant}: Self = Self({});",
            instruction.name, instruction.opcode
        );
    }
    let _ = writeln!(w, "}}\n");
    Ok(())
}

/// The `enumerant` module: a value type per enumerated kind, with a constant
/// per recognised enumerant.
fn emit_enumerants(
    w: &mut String,
    kinds: &BTreeMap<String, (String, Vec<Enumerant>)>,
    names: &Names,
) -> Result<(), String> {
    let _ = writeln!(
        w,
        "pub mod enumerant {{\n    \
         //! The values of the grammar's enumerated kinds, by name: a type per kind,\n    \
         //! holding a value or, for a set of bits, a mask.\n"
    );
    for (kind, (category, enumerants)) in kinds {
        if enumerants.is_empty() {
            continue;
        }
        let what = if category == "BitEnum" {
            "set of bits"
        } else {
            "value"
        };
        let _ = writeln!(
            w,
            "    /// A `{kind}` {what}.\n    \
             #[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]\n    \
             pub struct {kind}(pub u32);\n\n    \
             impl {kind} {{"
        );
        for enumerant in enumerants {
            let constant = names
                .by_value
                .get(&(kind.clone(), enumerant.value))
                .ok_or_else(|| format!("spirv: {kind} {} has no constant", enumerant.name))?;
            let _ = writeln!(
                w,
                "        /// `{}`.\n        pub const {constant}: Self = Self({});",
                enumerant.name,
                hex(enumerant.value)
            );
        }
        let _ = writeln!(w, "    }}\n");
    }
    let _ = writeln!(w, "}}\n");
    Ok(())
}

/// The `Kind` enumeration, how the decoder reads each, and its enumerants.
fn emit_kinds(
    w: &mut String,
    kinds: &BTreeMap<String, (String, Vec<Enumerant>)>,
    names: &Names,
) -> Result<(), String> {
    let _ = writeln!(
        w,
        "/// A kind of operand the grammar names.\n\
         #[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]\n\
         pub enum Kind {{"
    );
    for kind in kinds.keys() {
        let _ = writeln!(w, "    /// `{kind}`.\n    {kind},");
    }
    let _ = writeln!(
        w,
        "}}\n\nimpl Kind {{\n    /// How the decoder reads it.\n    \
         pub(crate) const fn shape(self) -> Shape {{\n        match self {{"
    );
    let mut by_shape: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (kind, (category, _)) in kinds {
        by_shape
            .entry(shape(kind, category))
            .or_default()
            .push(kind);
    }
    for (shape, members) in &by_shape {
        let arms: Vec<String> = members.iter().map(|k| format!("Self::{k}")).collect();
        let _ = writeln!(w, "            {} => {shape},", arms.join(" | "));
    }
    let _ = writeln!(
        w,
        "        }}\n    }}\n\n    \
         /// Its enumerants, by ascending value; empty for a kind that has none.\n    \
         pub(crate) const fn enumerants(self) -> &'static [Enumerant] {{\n        match self {{"
    );
    for (kind, (_, enumerants)) in kinds {
        if !enumerants.is_empty() {
            let _ = writeln!(w, "            Self::{kind} => &{},", upper_snake(kind));
        }
    }
    let _ = writeln!(w, "            _ => &[],\n        }}\n    }}\n}}\n");
    for (kind, (_, enumerants)) in kinds {
        if enumerants.is_empty() {
            continue;
        }
        let _ = writeln!(
            w,
            "static {}: [Enumerant; {}] = [",
            upper_snake(kind),
            enumerants.len()
        );
        for enumerant in enumerants {
            let parameters: Vec<String> = enumerant
                .parameters
                .iter()
                .map(|p| format!("Kind::{p}"))
                .collect();
            let _ = writeln!(
                w,
                "    Enumerant {{ value: {}, parameters: &[{}], gate: {} }},",
                hex(enumerant.value),
                parameters.join(", "),
                gate(&enumerant.gate, names)?
            );
        }
        let _ = writeln!(w, "];\n");
    }
    Ok(())
}

/// The instruction and `GLSL.std.450` tables.
fn emit_tables(w: &mut String, grammar: &Grammar, names: &Names) -> Result<(), String> {
    let _ = writeln!(
        w,
        "/// Every instruction recognised, by ascending opcode.\n\
         pub(crate) static INSTRUCTIONS: [Spec; {}] = [",
        grammar.instructions.len()
    );
    for instruction in &grammar.instructions {
        let operands: Vec<String> = instruction
            .operands
            .iter()
            .map(|o| format!("(Kind::{}, Quantity::{})", o.kind, o.quantity))
            .collect();
        let _ = writeln!(
            w,
            "    Spec {{ op: Op({}), name: \"{}\", operands: &[{}], gate: {} }},",
            instruction.opcode,
            instruction.name,
            operands.join(", "),
            gate(&instruction.gate, names)?
        );
    }
    let _ = writeln!(
        w,
        "];\n\n/// The `GLSL.std.450` extended instructions, by ascending number.\n\
         pub(crate) static GLSL_STD_450: [ExtInst; {}] = [",
        grammar.glsl.len()
    );
    for inst in &grammar.glsl {
        let capabilities = inst
            .capabilities
            .iter()
            .map(|c| capability(c, names))
            .collect::<Result<Vec<_>, _>>()?;
        let _ = writeln!(
            w,
            "    ExtInst {{ number: {}, operands: {}, capabilities: &[{}] }},",
            inst.number,
            inst.operands,
            capabilities.join(", ")
        );
    }
    let _ = writeln!(w, "];");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{check_operands, upper_snake, Operand};

    #[test]
    fn a_grammar_name_takes_the_constant_spelling_its_words_give() {
        for (name, constant) in [
            ("TypeInt", "TYPE_INT"),
            ("IAdd", "I_ADD"),
            ("SDotKHR", "S_DOT_KHR"),
            ("FOrdLessThan", "F_ORD_LESS_THAN"),
            ("ConvertUToF", "CONVERT_U_TO_F"),
            ("GroupNonUniformIAdd", "GROUP_NON_UNIFORM_I_ADD"),
            (
                "ImageSampleDrefImplicitLod",
                "IMAGE_SAMPLE_DREF_IMPLICIT_LOD",
            ),
            ("FPFastMathMode", "FP_FAST_MATH_MODE"),
            ("Int64Atomics", "INT64_ATOMICS"),
            ("StorageBuffer16BitAccess", "STORAGE_BUFFER16_BIT_ACCESS"),
            ("Sampled1D", "SAMPLED1D"),
            ("Rgb10A2", "RGB10_A2"),
            ("Float8E4M3EXT", "FLOAT8_E4_M3_EXT"),
            ("Rgba8Snorm", "RGBA8_SNORM"),
        ] {
            assert_eq!(upper_snake(name), constant, "{name}");
        }
    }

    #[test]
    fn a_long_value_is_written_with_its_digits_grouped() {
        assert_eq!(super::hex(0x1), "0x1");
        assert_eq!(super::hex(0xffff), "0xffff");
        assert_eq!(super::hex(0x1_0000), "0x1_0000");
        assert_eq!(super::hex(0x4000_0000), "0x4000_0000");
    }

    fn operands(spec: &[(&str, &'static str)]) -> Vec<Operand> {
        spec.iter()
            .map(|&(kind, quantity)| Operand {
                kind: kind.to_string(),
                quantity,
            })
            .collect()
    }

    #[test]
    fn an_operand_list_the_decoder_would_misread_fails_generation() {
        let fine = operands(&[
            ("IdResultType", "One"),
            ("IdResult", "One"),
            ("IdRef", "Many"),
        ]);
        assert!(check_operands("fine", &fine).is_ok());
        for bad in [
            operands(&[("IdRef", "Optional"), ("IdRef", "One")]),
            operands(&[("IdRef", "Many"), ("LiteralInteger", "Optional")]),
            operands(&[("IdRef", "One"), ("IdResult", "One")]),
            operands(&[("IdResult", "Optional")]),
        ] {
            assert!(check_operands("bad", &bad).is_err());
        }
    }
}
