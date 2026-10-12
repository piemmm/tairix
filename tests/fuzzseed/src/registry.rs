//! Documents for a closed settings registry's fuzz harness: whole setting
//! lines drawn from a table, so most documents are accepted and a round trip
//! is genuinely exercised.

use std::fmt::Write as _;
use std::string::String;
use std::vec::Vec;

use crate::Prng;

/// One key of a registry's generator table: its name, values it accepts, and
/// values it refuses.
pub type RegistryRow = (
    &'static str,
    &'static [&'static str],
    &'static [&'static str],
);

/// One document in this many carries a value drawn to be refused.
pub const POISONED_ONE_IN: usize = 4;

/// A document of distinct keys from `keys`, with one line poisoned in one
/// document of every [`POISONED_ONE_IN`] by a value its key refuses or one of
/// `bad`. Poisoning a document rather than each line keeps the share accepted
/// whole from falling as the registry grows.
pub fn registry_document(rng: &mut Prng, keys: &[RegistryRow], bad: &[&'static str]) -> String {
    let mut out = String::new();
    let mut left: Vec<&RegistryRow> = keys.iter().collect();
    let lines = rng.below(left.len() + 1);
    let poisoned = (lines > 0 && rng.below(POISONED_ONE_IN) == 0).then(|| rng.below(lines));
    for line in 0..lines {
        let &(key, accepted, refused) = left.swap_remove(rng.below(left.len()));
        if rng.below(10) == 0 {
            out.push_str("# comment\n");
        }
        let value = if poisoned != Some(line) {
            *rng.pick(accepted)
        } else if refused.is_empty() || rng.below(2) == 0 {
            *rng.pick(bad)
        } else {
            *rng.pick(refused)
        };
        let _ = writeln!(out, "{key} = {value}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{registry_document, RegistryRow};
    use crate::Prng;

    const KEYS: &[RegistryRow] = &[("a", &["1"], &["x"]), ("b", &["2"], &[])];

    #[test]
    fn a_document_names_each_key_at_most_once_from_its_table() {
        let mut rng = Prng::new(7);
        for _ in 0..500 {
            let doc = registry_document(&mut rng, KEYS, &["bad"]);
            for key in ["a", "b"] {
                let lines = doc
                    .lines()
                    .filter(|line| line.starts_with(&format!("{key} =")))
                    .count();
                assert!(lines <= 1, "{doc}");
            }
            for line in doc.lines().filter(|line| !line.starts_with('#')) {
                let value = line.split(" = ").nth(1).expect("a setting line");
                assert!(["1", "2", "x", "bad"].contains(&value), "{line}");
            }
        }
    }
}
