//! `cargo xtask devids` implementation.
//!
//! The `lspci`/`lsusb` command apps name devices, and the network map names
//! the organisation behind a MAC address, through `lib/devids`, whose data is
//! vetted snapshots of the public PCI and USB ID databases (plans/DEVICES.md
//! DEVICE1) and of the IEEE MA-L registry (plans/NETMAP.md NM-F). This command
//! owns the import pipeline; the grammar, vetting filter, and table codec live
//! in `lib/devids` itself (one definition, shared with the runtime consumers).
//!
//! - `cargo xtask devids` (no arguments; part of `cargo xtask ci`) re-runs
//!   the converter over the committed snapshots in `lib/devids/assets/` and
//!   fails closed on any drift against the committed tables (each inside its
//!   consuming bundle's `Resources/` —
//!   `userland/apps/lspci/Resources/pci.ids.bin`,
//!   `userland/apps/lsusb/Resources/usb.ids.bin`,
//!   `userland/apps/netmap/Resources/oui.ids.bin`), exactly like `c-header`
//!   and `font-atlas`.
//! - `cargo xtask devids --write` regenerates the committed tables from the
//!   committed snapshots.
//! - `cargo xtask devids --fetch [<database>]` is **developer-run only** —
//!   builds stay offline and reproducible, so neither CI nor any build step
//!   ever touches the network. It downloads every database, or the one named
//!   (`pci.ids`, `usb.ids`, `oui.ids`), runs the `lib/devids` vetting filter,
//!   rewrites the committed snapshots with a provenance header (upstream
//!   URL/version/date, fetch date, SHA-256 of the raw download, licence
//!   statement, transport), and regenerates the tables so the tree stays
//!   self-consistent. The refresh diff is human-reviewed like any other
//!   change.
//!
//! The IEEE publishes its registry as CSV with each organisation's postal
//! address. The fetch keeps the assignment and the organisation's name and
//! nothing else, in the shared `ids` grammar, so the snapshot is reviewed and
//! vetted exactly like the other two.
//!
//! Transport honesty: `pci.ids` is fetched over verified TLS. The `usb.ids`
//! upstream (`linux-usb.org` and its mirrors) publishes no valid TLS
//! endpoint, so it is fetched over the canonical HTTP URL upstream
//! documents; integrity comes from the recorded SHA-256 and the human
//! review of the snapshot diff, and the runtime never refetches (databases
//! update only through the signed system-update path).
//!
//! Encoding honesty: today's `usb.ids` carries the odd stray ISO-8859-1
//! byte in an otherwise UTF-8 file. The fetch repairs exactly the byte
//! sequences that are not valid UTF-8 by promoting each byte to its
//! Latin-1 code point (valid UTF-8 elsewhere is left untouched), records
//! the repair in the provenance header, and then vets the repaired text
//! strictly.

use std::collections::btree_map::{BTreeMap, Entry};
use std::fmt::Write as _;
use std::path::Path;
use std::process::Command as Process;
use std::time::{SystemTime, UNIX_EPOCH};

use tairix_devids::{textdb, DbKind, MAX_TABLE_BYTES, TABLE_SUFFIX};
use tairix_visible::is_hidden;

use crate::Context;

/// The form a database is published in upstream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Upstream {
    /// The shared `ids` grammar, with `Version:` and `Date:` header comments.
    Ids,
    /// The IEEE registry's CSV export, converted on import.
    IeeeCsv,
}

/// One database's import pinning: where it comes from, which bundle ships
/// it, and what the provenance header records about it.
struct Database {
    kind: DbKind,
    upstream: Upstream,
    /// Fetch URL (see the module docs for the transport rationale).
    url: &'static str,
    /// Provenance-header transport statement.
    transport: &'static str,
    /// Provenance-header licence statement.
    licence: &'static str,
    /// The `userland/apps/` bundle whose `Resources/` holds the compiled
    /// table, so it ships inside that self-contained bundle with no second
    /// copy in the tree.
    bundle: &'static str,
}

impl Database {
    /// The name messages, the provenance header and `--fetch` use.
    fn name(&self) -> &'static str {
        self.kind.name()
    }

    /// The workspace-relative committed snapshot.
    fn snapshot(&self) -> String {
        format!("lib/devids/assets/{}", self.kind.name())
    }

    /// The workspace-relative committed compact table.
    fn table(&self) -> String {
        format!(
            "userland/apps/{}/Resources/{}{TABLE_SUFFIX}",
            self.bundle,
            self.kind.name()
        )
    }
}

/// The imported databases.
const DATABASES: &[Database] = &[
    Database {
        kind: DbKind::Pci,
        upstream: Upstream::Ids,
        url: "https://pci-ids.ucw.cz/v2.2/pci.ids",
        transport: "HTTPS (TLS verified)",
        licence: "dual GPL-2.0-or-later / BSD-3-Clause (upstream header below)",
        bundle: "lspci",
    },
    Database {
        kind: DbKind::Usb,
        upstream: Upstream::Ids,
        url: "http://www.linux-usb.org/usb.ids",
        transport: "HTTP (upstream publishes no valid TLS endpoint; integrity \
                    is the SHA-256 below plus human review of this diff)",
        licence: "GPL-2.0-or-later (as distributed with usbutils; the upstream \
                  file carries no licence header)",
        bundle: "lsusb",
    },
    Database {
        kind: DbKind::Oui,
        upstream: Upstream::IeeeCsv,
        url: "https://standards-oui.ieee.org/oui/oui.csv",
        transport: "HTTPS (TLS verified)",
        licence: "IEEE Registration Authority public MA-L listing; the export \
                  carries no licence statement, and only the factual \
                  assignment-to-organisation mapping is redistributed",
        bundle: "netmap",
    },
];

/// The usage line every argument refusal states.
const USAGE: &str = "usage: cargo xtask devids [--fetch [pci.ids | usb.ids | oui.ids] | --write]";

/// Entry point for `cargo xtask devids [--fetch [<database>] | --write]`.
pub fn run(ctx: &Context, args: &[std::ffi::OsString]) -> Result<(), String> {
    let mut fetch: Option<Vec<&Database>> = None;
    let mut write = false;
    let mut args = args.iter().peekable();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--fetch") => {
                let named =
                    args.next_if(|next| next.to_str().is_some_and(|word| !word.starts_with("--")));
                fetch = Some(match named {
                    Some(name) => {
                        let db = DATABASES
                            .iter()
                            .find(|db| Some(db.name()) == name.to_str())
                            .ok_or_else(|| {
                                format!("devids: unknown database {}; {USAGE}", name.display())
                            })?;
                        Vec::from([db])
                    }
                    None => DATABASES.iter().collect(),
                });
            }
            Some("--write") => write = true,
            _ => {
                return Err(format!(
                    "devids: unexpected argument {}; {USAGE}",
                    arg.display()
                ));
            }
        }
    }
    if let Some(databases) = fetch {
        eprintln!("xtask: [devids --fetch] importing upstream databases");
        return run_fetch(ctx, &databases);
    }
    if write {
        eprintln!("xtask: [devids --write] regenerating the compact tables");
        return run_write(ctx);
    }
    eprintln!("xtask: [devids] verifying lib/devids snapshot/table sync");
    run_verify(ctx)
}

/// Parse one committed snapshot and encode its table.
fn compile(root: &Path, db: &Database) -> Result<Vec<u8>, String> {
    let path = root.join(db.snapshot());
    let bytes =
        std::fs::read(&path).map_err(|e| format!("devids: cannot read {}: {e}", path.display()))?;
    let parsed = textdb::parse(db.kind, &bytes)
        .map_err(|e| format!("devids: {} fails vetting: {e}", path.display()))?;
    let table = parsed.encode();
    if table.len() > MAX_TABLE_BYTES {
        return Err(format!(
            "devids: {} compiles to {} bytes, over the {MAX_TABLE_BYTES}-byte bound \
             its consumer reads",
            path.display(),
            table.len()
        ));
    }
    Ok(table)
}

/// Regenerate the committed tables from the committed snapshots (`--write`).
fn run_write(ctx: &Context) -> Result<(), String> {
    for db in DATABASES {
        let encoded = compile(&ctx.workspace_root, db)?;
        let path = ctx.workspace_root.join(db.table());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("devids: cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, &encoded)
            .map_err(|e| format!("devids: cannot write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Verify the committed tables match a fresh compile of the committed
/// snapshots (the `ci` drift guard). Fails closed with the regeneration
/// command on any mismatch.
fn run_verify(ctx: &Context) -> Result<(), String> {
    for db in DATABASES {
        let encoded = compile(&ctx.workspace_root, db)?;
        let path = ctx.workspace_root.join(db.table());
        let committed = std::fs::read(&path).map_err(|_| drifted(db))?;
        if committed != encoded {
            return Err(drifted(db));
        }
    }
    Ok(())
}

fn drifted(db: &Database) -> String {
    format!(
        "devids: `{}` is out of sync with `{}`; \
         run `cargo xtask devids --write` and commit the result.",
        db.table(),
        db.snapshot(),
    )
}

/// Import upstream databases (`--fetch`): download, repair encoding,
/// convert, vet, rewrite the snapshots with provenance headers, and
/// regenerate the tables. Developer-run only; never CI or the build.
fn run_fetch(ctx: &Context, databases: &[&Database]) -> Result<(), String> {
    let today = utc_date_today()?;
    for db in databases {
        let raw = download(db)?;
        let digest = tairix_crypto::sha256(&raw);
        let (downloaded, repaired) = promote_latin1(&raw);
        let (text, published, conversion) = match db.upstream {
            Upstream::Ids => {
                let version = header_field(&downloaded, "Version:").ok_or_else(|| {
                    format!("devids: fetched {} has no `Version:` header", db.name())
                })?;
                let date = header_field(&downloaded, "Date:").ok_or_else(|| {
                    format!("devids: fetched {} has no `Date:` header", db.name())
                })?;
                (downloaded, Some((version, date)), None)
            }
            Upstream::IeeeCsv => {
                let (converted, conversion) = ids_from_ieee_csv(&downloaded)
                    .map_err(|e| format!("devids: fetched {}: {e}", db.name()))?;
                (converted, None, Some(conversion))
            }
        };
        let parsed = textdb::parse(db.kind, text.as_bytes())
            .map_err(|e| format!("devids: fetched {} fails vetting: {e}", db.name()))?;
        let counts = parsed.counts();
        let encoding = if repaired == 0 {
            "as downloaded (valid UTF-8)".to_string()
        } else {
            format!("{repaired} invalid byte(s) promoted from ISO-8859-1 to UTF-8")
        };
        let mut snapshot = String::new();
        let _ = write!(
            snapshot,
            "# TAIRiX vetted snapshot of the public {name} database.\n\
             # Imported by `cargo xtask devids --fetch`; do not hand-edit — refetch\n\
             # and re-review instead. The compact lookup table is generated from\n\
             # this file by\n\
             # `cargo xtask devids --write` and drift-checked by `cargo xtask ci`.\n\
             # Upstream URL: {url}\n",
            name = db.name(),
            url = db.url,
        );
        if let Some((version, date)) = &published {
            let _ = write!(
                snapshot,
                "# Upstream version: {version}\n# Upstream date: {date}\n"
            );
        }
        let _ = write!(
            snapshot,
            "# Fetch date: {today}\n\
             # Transport: {transport}\n\
             # Raw download SHA-256: {sha}\n\
             # Encoding: {encoding}\n\
             # Licence: {licence}\n",
            transport = db.transport,
            sha = hex(&digest),
            licence = db.licence,
        );
        if let Some(conversion) = &conversion {
            let _ = write!(snapshot, "{conversion}");
        }
        snapshot.push('\n');
        snapshot.push_str(&text);
        // The final snapshot (header included) must itself pass vetting:
        // this is exactly what `--write` and the CI gate will parse.
        textdb::parse(db.kind, snapshot.as_bytes())
            .map_err(|e| format!("devids: {} snapshot fails vetting: {e}", db.name()))?;
        let path = ctx.workspace_root.join(db.snapshot());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("devids: cannot create {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, snapshot.as_bytes())
            .map_err(|e| format!("devids: cannot write {}: {e}", path.display()))?;
        let version = published
            .as_ref()
            .map_or("unversioned", |(version, _)| version.as_str());
        eprintln!(
            "xtask: [devids --fetch] {}: {version}, {} vendors, {} devices, \
             {} class names ({encoding})",
            db.name(),
            counts.vendors,
            counts.devices,
            counts.classes,
        );
    }
    run_write(ctx)
}

/// The header row of the IEEE registry's CSV export.
const IEEE_CSV_HEADER: [&str; 4] = [
    "Registry",
    "Assignment",
    "Organization Name",
    "Organization Address",
];

/// What converting the IEEE export changed, recorded in the snapshot's
/// provenance so the review of a refresh sees it.
#[derive(Debug, Default, Eq, PartialEq)]
struct Conversion {
    /// Rows whose name's spacing was collapsed.
    respaced: usize,
    /// Rows whose name lost characters that hide or reorder text.
    stripped: usize,
    /// Assignments omitted because the registry names several organisations
    /// for them.
    ambiguous: usize,
    /// Assignments omitted because their name was empty once normalised.
    unnamed: usize,
    /// Assignments omitted because the registry holds them to sub-allocate
    /// (its MA-M and MA-S blocks), so their name is not the device's maker.
    registry_held: usize,
}

/// The organisation the registry lists for the MA-L blocks it sub-allocates.
const REGISTRY: &str = "IEEE Registration Authority";

impl std::fmt::Display for Conversion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "# Conversion: each MA-L assignment and its organisation's name; \
             addresses dropped."
        )?;
        writeln!(f, "#   Names respaced: {}", self.respaced)?;
        writeln!(
            f,
            "#   Names stripped of invisible characters: {}",
            self.stripped
        )?;
        writeln!(
            f,
            "#   Assignments omitted as naming several organisations: {}",
            self.ambiguous
        )?;
        writeln!(
            f,
            "#   Assignments omitted as naming none: {}",
            self.unnamed
        )?;
        writeln!(
            f,
            "#   Assignments omitted as held by the registry to sub-allocate: {}",
            self.registry_held
        )
    }
}

/// Convert the IEEE MA-L registry's CSV export into the shared `ids` grammar:
/// one `xxxxxx  Organisation` line per assignment, sorted, with the postal
/// address dropped, the name's spacing collapsed, and every character that
/// hides or reorders text removed from it. An assignment the registry gives
/// to more than one organisation names none of them, and one it holds to
/// sub-allocate names none of the makers it was divided between, so both are
/// omitted. A header or registry other than the one this was written for
/// stops the import for review rather than being guessed at.
fn ids_from_ieee_csv(text: &str) -> Result<(String, Conversion), String> {
    let mut records = csv_records(text);
    let header = records
        .next()
        .ok_or_else(|| String::from("the export is empty"))??;
    if header.iter().map(String::as_str).ne(IEEE_CSV_HEADER) {
        return Err(format!("unexpected header {header:?}"));
    }
    let mut conversion = Conversion::default();
    let mut assignments = BTreeMap::<u32, Option<String>>::new();
    for (row, record) in records.enumerate() {
        let record = record?;
        let [registry, assignment, name, _address] = record.as_slice() else {
            return Err(format!("row {} has {} fields", row + 2, record.len()));
        };
        if registry != "MA-L" {
            return Err(format!("row {} is in registry {registry:?}", row + 2));
        }
        let oui = u32::from_str_radix(assignment, 16)
            .ok()
            .filter(|_| assignment.len() == 6)
            .ok_or_else(|| format!("row {} has assignment {assignment:?}", row + 2))?;
        let name = normalised_name(name, &mut conversion);
        match assignments.entry(oui) {
            Entry::Vacant(slot) => {
                slot.insert(Some(name));
            }
            Entry::Occupied(mut slot) => {
                if slot.get().as_ref().is_some_and(|first| *first != name) {
                    slot.insert(None);
                    conversion.ambiguous += 1;
                }
            }
        }
    }
    let mut out = String::new();
    for (oui, name) in assignments {
        match name {
            Some(name) if name.is_empty() => conversion.unnamed += 1,
            Some(name) if name == REGISTRY => conversion.registry_held += 1,
            Some(name) => {
                let _ = writeln!(out, "{oui:06x}  {name}");
            }
            None => {}
        }
    }
    Ok((out, conversion))
}

/// `name` with every character that hides or reorders text removed and its
/// spacing collapsed to single spaces, counting each change in `conversion`.
fn normalised_name(name: &str, conversion: &mut Conversion) -> String {
    let shown: String = name
        .chars()
        .filter(|&c| c.is_whitespace() || !is_hidden(c))
        .collect();
    if shown.len() != name.len() {
        conversion.stripped += 1;
    }
    let spaced = shown.split_whitespace().collect::<Vec<_>>().join(" ");
    if spaced != shown {
        conversion.respaced += 1;
    }
    spaced
}

/// The records of RFC 4180 CSV `text`: fields split on commas, a quoted
/// field may hold commas, newlines and doubled quotes, and a record ends at a
/// line break outside quotes. An unterminated quote fails the record.
fn csv_records(text: &str) -> impl Iterator<Item = Result<Vec<String>, String>> + '_ {
    let mut chars = text.chars().peekable();
    std::iter::from_fn(move || {
        chars.peek()?;
        let mut record = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        loop {
            match (chars.next(), quoted) {
                (None, true) => return Some(Err(String::from("an unterminated quoted field"))),
                (None | Some('\n'), false) => break,
                (Some('"'), true) if chars.peek() == Some(&'"') => {
                    chars.next();
                    field.push('"');
                }
                (Some('"'), true) => quoted = false,
                (Some('"'), false) if field.is_empty() => quoted = true,
                (Some(','), false) => record.push(std::mem::take(&mut field)),
                (Some('\r'), false) if chars.peek() == Some(&'\n') => {}
                (Some(c), _) => field.push(c),
            }
        }
        record.push(field);
        Some(Ok(record))
    })
}

/// Download `db` with the system `curl`, failing closed on any transport or
/// HTTP error. An external process like the QEMU/toolchain wrappers; used
/// only by the developer-run `--fetch`.
fn download(db: &Database) -> Result<Vec<u8>, String> {
    let output = Process::new("curl")
        .args(["--fail", "--silent", "--show-error", "--location"])
        .args(["--max-time", "120", "--output", "-"])
        .arg(db.url)
        .output()
        .map_err(|e| format!("devids: cannot run curl: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "devids: fetching {} failed: {}",
            db.url,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    if output.stdout.is_empty() {
        return Err(format!("devids: fetching {} returned no data", db.url));
    }
    Ok(output.stdout)
}

/// Decode `raw` as UTF-8, promoting each byte of any *invalid* sequence to
/// its ISO-8859-1 code point. Valid UTF-8 (including multi-byte sequences)
/// is left untouched; the returned count is the number of promoted bytes.
fn promote_latin1(raw: &[u8]) -> (String, usize) {
    let mut out = String::with_capacity(raw.len());
    let mut promoted = 0usize;
    let mut rest = raw;
    loop {
        match std::str::from_utf8(rest) {
            Ok(tail) => {
                out.push_str(tail);
                return (out, promoted);
            }
            Err(e) => {
                let valid = e.valid_up_to();
                // The prefix was just validated.
                out.push_str(std::str::from_utf8(&rest[..valid]).unwrap_or(""));
                let bad = e.error_len().unwrap_or(rest.len() - valid);
                for &b in &rest[valid..valid + bad] {
                    out.push(char::from(b));
                    promoted += 1;
                }
                rest = &rest[valid + bad..];
            }
        }
    }
}

/// The value of the first `# ... <key> <value>` comment line, e.g.
/// `#\tVersion: 2026.07.09` (pci.ids) or `# Version: 2026.06.26` (usb.ids).
fn header_field(text: &str, key: &str) -> Option<String> {
    text.lines()
        .take_while(|l| l.is_empty() || l.starts_with('#'))
        .find_map(|l| {
            let (_, value) = l.split_once(key)?;
            let value = value.trim();
            (!value.is_empty()).then(|| value.to_string())
        })
}

/// Lowercase-hex rendering of a digest.
fn hex(digest: &[u8]) -> String {
    digest.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Today's UTC date as `YYYY-MM-DD`, from the system clock (the fetch is a
/// developer action; the date is provenance, not build input).
fn utc_date_today() -> Result<String, String> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("devids: system clock is before the epoch: {e}"))?
        .as_secs();
    let days =
        i64::try_from(secs / 86_400).map_err(|e| format!("devids: system clock overflow: {e}"))?;
    let (y, m, d) = tairix_abi::time::civil_from_days(days);
    Ok(format!("{y:04}-{m:02}-{d:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn promote_latin1_leaves_valid_utf8_untouched() {
        let (text, promoted) = promote_latin1("héllo — ✓".as_bytes());
        assert_eq!(text, "héllo — ✓");
        assert_eq!(promoted, 0);
    }

    #[test]
    fn promote_latin1_promotes_only_the_invalid_bytes() {
        // The real-world shape: one stray ISO-8859-1 acute accent inside an
        // otherwise UTF-8 file that also holds a valid multi-byte sequence.
        let raw = b"a\xb4b \xc3\xa9";
        let (text, promoted) = promote_latin1(raw);
        assert_eq!(text, "a´b é");
        assert_eq!(promoted, 1);
    }

    #[test]
    fn header_field_reads_both_upstream_header_shapes() {
        let pci = "#\n#\tVersion: 2026.07.09\n#\tDate:    2026-07-09 03:15:02\n";
        assert_eq!(header_field(pci, "Version:").as_deref(), Some("2026.07.09"));
        let usb = "#\n# Version: 2026.06.26\n# Date:    2026-06-26 20:34:02\n";
        assert_eq!(
            header_field(usb, "Date:").as_deref(),
            Some("2026-06-26 20:34:02")
        );
        // The scan stops at the first entry line: a smuggled later comment
        // cannot claim the version.
        let late = "0001  V\n# Version: 9\n";
        assert_eq!(header_field(late, "Version:"), None);
    }

    #[test]
    fn the_ieee_export_becomes_names_alone_in_the_shared_grammar() {
        let csv = "Registry,Assignment,Organization Name,Organization Address\r\n\
                   MA-L,B827EB,Raspberry Pi Foundation,Mitchell Wood House Caldecote GB CB23 7NU \r\n\
                   MA-L,00000C,\"Cisco Systems,\u{a0}  Inc\t\",\"170 West Tasman Dr.\nSan Jose CA US 95134 \"\r\n\
                   MA-L,00000C,\"Cisco Systems, Inc\",Elsewhere\r\n\
                   MA-L,48BCA6,\"\u{200b}ASUNG TECHNO CO.,Ltd\",Seoul\r\n\
                   MA-L,ABCDEF,\"Say \"\"Hi\"\"\",Nowhere\r\n";
        let (ids, conversion) = ids_from_ieee_csv(csv).expect("converts");
        assert_eq!(
            ids,
            "00000c  Cisco Systems, Inc\n48bca6  ASUNG TECHNO CO.,Ltd\n\
             abcdef  Say \"Hi\"\nb827eb  Raspberry Pi Foundation\n"
        );
        assert_eq!(
            conversion,
            Conversion {
                respaced: 1,
                stripped: 1,
                ambiguous: 0,
                unnamed: 0,
                registry_held: 0,
            },
            "a row repeating its assignment under the same name is no conflict"
        );
        assert!(textdb::parse(DbKind::Oui, ids.as_bytes()).is_ok());
    }

    #[test]
    fn an_assignment_no_one_maker_holds_names_none() {
        let csv = "Registry,Assignment,Organization Name,Organization Address\r\n\
                   MA-L,080030,NETWORK RESEARCH CORPORATION,A\r\n\
                   MA-L,080030,ROYAL MELBOURNE INST OF TECH,B\r\n\
                   MA-L,080030,CERN,C\r\n\
                   MA-L,000001,\"\u{200b} \u{202e}\",D\r\n\
                   MA-L,70B3D5,IEEE Registration Authority,F\r\n\
                   MA-L,B827EB,Raspberry Pi Foundation,E\r\n";
        let (ids, conversion) = ids_from_ieee_csv(csv).expect("converts");
        assert_eq!(ids, "b827eb  Raspberry Pi Foundation\n");
        assert_eq!(conversion.ambiguous, 1);
        assert_eq!(conversion.unnamed, 1);
        assert_eq!(conversion.registry_held, 1);
    }

    #[test]
    fn an_ieee_export_of_another_shape_stops_the_import() {
        assert!(ids_from_ieee_csv("Registry,Assignment,Name\r\n").is_err());
        let header = "Registry,Assignment,Organization Name,Organization Address\r\n";
        assert!(ids_from_ieee_csv(&format!("{header}MA-M,B827EB0,X,Y\r\n")).is_err());
        assert!(ids_from_ieee_csv(&format!("{header}MA-L,B827E,X,Y\r\n")).is_err());
        assert!(ids_from_ieee_csv(&format!("{header}MA-L,B827EB,\"open,Y\r\n")).is_err());
    }

    #[test]
    fn committed_snapshots_stay_inside_the_crate_assets() {
        for db in DATABASES {
            assert!(db.snapshot().starts_with("lib/devids/assets/"));
        }
        // Each compiled table lives inside its consuming bundle's
        // `Resources/` directory — the self-contained-bundle home, with no
        // second copy anywhere in the tree.
        let tables: Vec<String> = DATABASES.iter().map(Database::table).collect();
        assert_eq!(
            tables,
            [
                "userland/apps/lspci/Resources/pci.ids.bin",
                "userland/apps/lsusb/Resources/usb.ids.bin",
                "userland/apps/netmap/Resources/oui.ids.bin",
            ]
        );
    }
}
