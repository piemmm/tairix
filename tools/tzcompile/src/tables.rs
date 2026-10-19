//! The two tab-separated tables beside the rules: tzdb's zone table, which
//! places each zone in a region, and the regions' own conventions.

use tairix_tz::write::{PlaceInput, RegionInput};
use tairix_tz::Weekday;

/// Why a table line was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TableError {
    /// The 1-based line.
    pub line: usize,
    /// What is wrong with it.
    pub reason: &'static str,
}

/// The rows of `text` that are not comments, each split at its tabs, with
/// their line numbers.
fn rows(text: &str) -> impl Iterator<Item = (usize, Vec<&str>)> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.is_empty() && !line.starts_with('#'))
        .map(|(index, line)| (index + 1, line.split('\t').collect()))
}

/// tzdb's `zone.tab`: per row a region code, coordinates, a zone name, and
/// an optional comment telling the region's zones apart.
///
/// # Errors
///
/// The first row that is not three or four fields.
pub fn places(text: &str) -> Result<Vec<PlaceInput>, TableError> {
    rows(text)
        .map(|(line, fields)| {
            let (region, zone, comment) = match fields[..] {
                [region, _, zone] => (region, zone, ""),
                [region, _, zone, comment] => (region, zone, comment),
                _ => {
                    return Err(TableError {
                        line,
                        reason: "a zone table row is a code, coordinates, a zone and a comment",
                    })
                }
            };
            Ok(PlaceInput {
                zone: String::from(zone),
                region: String::from(region),
                comment: String::from(comment),
            })
        })
        .collect()
}

/// `regions.tsv`: per row a region code, its English name, its numeric date
/// pattern, its time-of-day pattern, and the day its week begins on, spelled
/// as CLDR spells it (`mon` … `sun`).
///
/// # Errors
///
/// The first row that is not five fields, or whose week begins on no day.
pub fn regions(text: &str) -> Result<Vec<RegionInput>, TableError> {
    rows(text)
        .map(|(line, fields)| match fields[..] {
            [code, name, date, time, week] => Ok(RegionInput {
                code: String::from(code),
                name: String::from(name),
                first_weekday: weekday(week).ok_or(TableError {
                    line,
                    reason: "a week begins on mon, tue, wed, thu, fri, sat or sun",
                })?,
                date: String::from(date),
                time: String::from(time),
            }),
            _ => Err(TableError {
                line,
                reason: "a region row is a code, a name, a date, a time and a week",
            }),
        })
        .collect()
}

/// The day CLDR spells `day`.
fn weekday(day: &str) -> Option<Weekday> {
    let days = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
    let index = days.iter().position(|spelled| *spelled == day)?;
    Weekday::ALL.get(index).copied()
}

#[cfg(test)]
#[path = "tables_tests.rs"]
mod tests;
