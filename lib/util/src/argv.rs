//! Word arguments: an option's value named by one of a set of words
//! (`--total=auto`), resolved and refused as GNU resolves them.
//!
//! A word is a choice spelled exactly or the only one it prefixes: the one
//! prefix rule, which the [`getopt`](crate::getopt) scanner reads an
//! abbreviated long option by too.

use crate::quote::quote;

/// Why a word argument (`--total=auto`) named no one choice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WordMiss {
    /// It is a prefix of more than one choice, the empty word included.
    Ambiguous,
    /// It is no choice and no prefix of one.
    Unknown,
}

/// The one candidate `prefixed` holds, or the one they all are by `same`: the
/// rule GNU reads a prefix by, for a word argument and an abbreviated long
/// option alike, so a prefix two synonyms share is either.
pub(crate) fn resolve<C>(
    mut prefixed: impl Iterator<Item = C>,
    same: impl Fn(&C, &C) -> bool,
) -> Result<C, WordMiss> {
    let Some(first) = prefixed.next() else {
        return Err(WordMiss::Unknown);
    };
    if prefixed.all(|other| same(&first, &other)) {
        Ok(first)
    } else {
        Err(WordMiss::Ambiguous)
    }
}

/// The choice `word` names among `choices`: one spelled exactly so, or else
/// the only one it is a prefix of, as GNU resolves a word argument (`au`
/// names `auto` among `auto` and `always`; `a` names neither).
///
/// # Errors
///
/// [`WordMiss`] when `word` names none or several.
pub fn choose<T: Copy + PartialEq>(choices: &[(&str, T)], word: &str) -> Result<T, WordMiss> {
    if let Some(&(_, exact)) = choices.iter().find(|(name, _)| *name == word) {
        return Ok(exact);
    }
    resolve(
        choices
            .iter()
            .filter(|(name, _)| name.starts_with(word))
            .map(|&(_, choice)| choice),
        |a, b| a == b,
    )
}

/// GNU's refusal of a word argument: `invalid argument 'x' for '--total'` or
/// `ambiguous argument 'a' for '--total'`, then `Valid arguments are:` with
/// each choice on a line of its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BadWord<'a, T> {
    /// How the word failed.
    pub miss: WordMiss,
    /// The word given.
    pub word: &'a str,
    /// The option it was given to, spelled `--name`.
    pub option: &'a str,
    /// Every choice by its word, in the order the tool lists them — the
    /// table [`choose`] read.
    pub choices: &'a [(&'a str, T)],
}

impl<T> core::fmt::Display for BadWord<'_, T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let miss = match self.miss {
            WordMiss::Ambiguous => "ambiguous",
            WordMiss::Unknown => "invalid",
        };
        write!(
            f,
            "{miss} argument {} for {}\nValid arguments are:",
            quote(self.word),
            quote(self.option)
        )?;
        for (choice, _) in self.choices {
            write!(f, "\n  - {}", quote(choice))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{choose, BadWord, WordMiss};
    use alloc::string::ToString;

    #[test]
    fn a_word_names_the_choice_it_spells_or_alone_prefixes() {
        const MODES: [(&str, u8); 4] = [
            ("warn", 1),
            ("warn-nopipe", 2),
            ("exit", 3),
            ("exit-nopipe", 4),
        ];
        assert_eq!(
            choose(&MODES, "warn"),
            Ok(1),
            "exact, though it prefixes another"
        );
        assert_eq!(choose(&MODES, "warn-"), Ok(2));
        assert_eq!(choose(&MODES, "exit-n"), Ok(4));
        assert_eq!(choose(&MODES, "w"), Err(WordMiss::Ambiguous));
        assert_eq!(choose(&MODES, "e"), Err(WordMiss::Ambiguous));
        assert_eq!(choose(&MODES, ""), Err(WordMiss::Ambiguous));
        assert_eq!(choose(&MODES, "bogus"), Err(WordMiss::Unknown));
        assert_eq!(choose(&MODES, "warnings"), Err(WordMiss::Unknown));
        assert_eq!(
            choose(&[("only", 7)], ""),
            Ok(7),
            "the empty word prefixes the one choice"
        );
    }

    #[test]
    fn a_word_two_synonyms_share_is_either() {
        assert_eq!(
            choose(&[("always", 1), ("ever", 1), ("never", 2)], "a"),
            Ok(1)
        );
        assert_eq!(
            choose(&[("always", 1), ("ever", 1), ("never", 2)], "e"),
            Ok(1),
            "`ever` alone, as `never` does not begin with it"
        );
        assert_eq!(
            choose(&[("north", 1), ("nowhere", 2)], "no"),
            Err(WordMiss::Ambiguous)
        );
    }

    #[test]
    fn a_refused_word_is_reported_with_the_valid_ones() {
        let bad = BadWord {
            miss: WordMiss::Ambiguous,
            word: "a",
            option: "--total",
            choices: &[("auto", ()), ("always", ())],
        };
        assert_eq!(
            bad.to_string(),
            "ambiguous argument 'a' for '--total'\nValid arguments are:\n  - 'auto'\n  - 'always'"
        );
        let bad = BadWord {
            miss: WordMiss::Unknown,
            word: "x",
            ..bad
        };
        assert!(bad
            .to_string()
            .starts_with("invalid argument 'x' for '--total'\n"));
    }
}
