use alloc::vec;
use alloc::vec::Vec;

use super::*;

/// How a test word is coded.
#[derive(Clone, Copy)]
enum Code {
    Zero,
    Whole,
    /// A short-table entry by its index.
    Short(usize),
    /// A long-table entry by its index, above the word's low byte.
    Long(usize),
}

fn nibble(code: Code) -> u8 {
    match code {
        Code::Zero => 0,
        Code::Whole => 1,
        Code::Short(index) => 9 + u8::try_from(index >> 8).unwrap(),
        Code::Long(index) => 2 + u8::try_from(index >> 8).unwrap(),
    }
}

/// A table of `entries`, each spelled whole as a delta from the last.
fn whole_table(entries: &[u32], width: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut last = u32::MAX;
    for &entry in entries {
        out.push(0);
        out.extend_from_slice(&entry.wrapping_sub(last).to_le_bytes()[..width]);
        last = entry;
    }
    out
}

/// A squeezed image of `module`, whose words are coded as `codes` says
/// against `short` and `long`, `tables` their bytes: the header's first
/// `kept` bytes whole, the trailer on a word.
fn squeezed(
    module: &[u8],
    kept: usize,
    codes: &[Code],
    (short, long): (&[u32], &[u32]),
    tables: &[u8],
) -> Vec<u8> {
    let words: Vec<u32> = module
        .chunks(4)
        .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
        .collect();
    // In the order they are read, back from the tables: the last pair first.
    let mut read = Vec::new();
    for pair in (0..words.len() / 2).rev() {
        let (first, second) = (2 * pair, 2 * pair + 1);
        read.push(nibble(codes[first]) | nibble(codes[second]) << 4);
        for at in [first, second] {
            match codes[at] {
                Code::Zero => {}
                Code::Whole => read.extend_from_slice(&words[at].to_le_bytes()),
                Code::Short(index) => read.push(index.to_le_bytes()[0]),
                Code::Long(index) => {
                    read.push(index.to_le_bytes()[0]);
                    read.push(words[at].to_le_bytes()[0]);
                }
            }
        }
    }
    read.reverse();
    let mut tables = tables.to_vec();
    tables.resize(
        tables.len() + (4 - (kept + read.len() + tables.len()) % 4) % 4,
        0,
    );
    let mut image = module[..kept].to_vec();
    image.extend_from_slice(&read);
    image.extend_from_slice(&tables);
    for size in [
        module.len(),
        read.len(),
        tables.len(),
        short.len(),
        long.len(),
    ] {
        image.extend_from_slice(&u32::try_from(size).unwrap().to_le_bytes());
    }
    let length = u32::try_from(image.len()).unwrap() | SQUEEZED;
    image[4..8].copy_from_slice(&length.to_le_bytes());
    image
}

/// `module`, padded to a multiple of eight bytes, squeezed against no
/// tables with its whole self kept before the codes, as a squeezer keeps
/// the header a module is checked by.
pub(crate) fn squeeze(module: &[u8]) -> Vec<u8> {
    let mut padded = module.to_vec();
    padded.resize(module.len().next_multiple_of(8), 0);
    let codes: Vec<Code> = padded
        .chunks(4)
        .map(|w| if w == [0; 4] { Code::Zero } else { Code::Whole })
        .collect();
    squeezed(&padded, padded.len(), &codes, (&[], &[]), &[])
}

/// A module of `words` after a header of eight zero words.
fn module_of(words: &[u32]) -> Vec<u8> {
    let mut out = vec![0; 32];
    for word in words {
        out.extend_from_slice(&word.to_le_bytes());
    }
    out
}

fn all(codes: Code, module: &[u8]) -> Vec<Code> {
    vec![codes; module.len() / 4]
}

#[test]
fn whole_and_zero_words_expand_to_the_module() {
    let module = module_of(&[0x1122_3344, 0, 0x5566_7788, 0x99AA_BBCC]);
    let codes: Vec<Code> = module
        .chunks(4)
        .map(|w| if w == [0; 4] { Code::Zero } else { Code::Whole })
        .collect();
    let image = squeezed(&module, 32, &codes, (&[], &[]), &[]);
    assert!(is_squeezed(&image));
    assert_eq!(unsqueeze(&image, 1 << 20), Ok(module));
}

#[test]
fn table_entries_expand_by_their_index_bytes() {
    let short = [0x1000_0001, 0x2000_0002];
    let long = [0x00_AB_CD, 0x01_02_03];
    let module = module_of(&[0x2000_0002, 0x00AB_CD56, 0x1000_0001, 0x0102_0378]);
    let mut codes = all(Code::Zero, &module);
    codes[8..].copy_from_slice(&[Code::Short(1), Code::Long(0), Code::Short(0), Code::Long(1)]);
    let mut tables = whole_table(&short, 4);
    tables.extend(whole_table(&long, 3));
    let image = squeezed(&module, 32, &codes, (&short, &long), &tables);
    assert_eq!(unsqueeze(&image, 1 << 20), Ok(module));
}

#[test]
fn every_form_of_delta_builds_a_table() {
    let entries = [5, 6, 7, 0x40, 0x1240, 0x0035_1240, 0x7F00_1234];
    let mut tables = vec![0];
    tables.extend_from_slice(&6u32.to_le_bytes());
    tables.push(2);
    tables.push(10 + (0x40 - 7));
    tables.extend([92 + 0x12, 0]);
    tables.extend([174 + 0x35, 0, 0]);
    tables.push(0);
    tables.extend_from_slice(&(0x7F00_1234u32 - 0x0035_1240).to_le_bytes());
    let module = module_of(&[5, 6, 7, 0x40, 0x1240, 0x0035_1240, 0x7F00_1234, 0]);
    let mut codes = all(Code::Zero, &module);
    for (at, code) in codes[8..15].iter_mut().enumerate() {
        *code = Code::Short(at);
    }
    let image = squeezed(&module, 32, &codes, (&entries, &[]), &tables);
    assert_eq!(unsqueeze(&image, 1 << 20), Ok(module));
}

#[test]
fn a_run_stops_at_the_tables_count() {
    let module = module_of(&[5, 0]);
    let mut codes = all(Code::Zero, &module);
    codes[8] = Code::Short(0);
    let mut tables = vec![0];
    tables.extend_from_slice(&6u32.to_le_bytes());
    tables.push(9);
    let image = squeezed(&module, 32, &codes, (&[5, 6], &[]), &tables);
    assert_eq!(unsqueeze(&image, 1 << 20), Ok(module), "two of the nine");
}

#[test]
fn codes_that_stop_short_of_the_size_make_the_module_as_genuine_risc_os_does() {
    let module = module_of(&[1, 2]);
    let mut image = squeeze(&module);
    let at = image.len() - TRAILER;
    let larger = u32::try_from(module.len() + 8).unwrap();
    image[at..at + 4].copy_from_slice(&larger.to_le_bytes());
    assert_eq!(unsqueeze(&image, 1 << 20), Ok(module));
}

#[test]
fn a_length_past_the_module_given_is_refused_as_such() {
    let image = squeeze(&module_of(&[1, 2]));
    assert_eq!(
        unsqueeze(&image[..image.len() - 4], 1 << 20),
        Err(Unsqueezed::PastItsEnd)
    );
}

#[test]
fn what_genuine_risc_os_hangs_or_faults_on_is_refused() {
    let good = squeeze(&module_of(&[1, 2]));
    let trailer = good.len() - TRAILER;
    let with = |at: usize, value: u32| {
        let mut image = good.clone();
        image[at..at + 4].copy_from_slice(&value.to_le_bytes());
        image
    };
    let length = u32::try_from(good.len()).unwrap();
    for (image, why) in [
        (with(4, (length - 2) | SQUEEZED), "a trailer off its word"),
        (with(4, 0x18 | SQUEEZED), "no room for the trailer"),
        (with(trailer, 44), "an expanded size no multiple of eight"),
        (with(trailer, 8), "more pairs than the size holds"),
        (with(trailer + 4, length), "codes past the module's start"),
        (with(trailer + 8, length), "tables past the module's start"),
        (with(trailer + 12, 1), "a table past its bytes"),
        (
            with(trailer + 12, 0x701),
            "a table longer than nibbles name",
        ),
    ] {
        assert_eq!(
            unsqueeze(&image, 1 << 20),
            Err(Unsqueezed::Malformed),
            "{why}"
        );
    }
    assert_eq!(unsqueeze(&good, 8), Err(Unsqueezed::TooLarge));
    assert_eq!(unsqueeze(&[0; 3], 1 << 20), Err(Unsqueezed::Malformed));
}

#[test]
fn a_word_reaching_past_its_codes_or_its_table_is_refused() {
    let module = module_of(&[0x1234_5678, 0]);
    let mut codes = all(Code::Zero, &module);
    codes[8] = Code::Whole;
    let mut image = squeezed(&module, 32, &codes, (&[], &[]), &[]);
    let at = image.len() - TRAILER + 4;
    image[at..at + 4].copy_from_slice(&3u32.to_le_bytes());
    assert_eq!(
        unsqueeze(&image, 1 << 20),
        Err(Unsqueezed::Malformed),
        "a whole word with three bytes left"
    );
    codes[8] = Code::Short(1);
    let tables = whole_table(&[5], 4);
    let image = squeezed(&module, 32, &codes, (&[5], &[]), &tables);
    assert_eq!(
        unsqueeze(&image, 1 << 20),
        Err(Unsqueezed::Malformed),
        "an entry the table does not hold"
    );
}

#[test]
fn an_unsqueezed_module_is_not_squeezed() {
    let module = module_of(&[1, 2]);
    assert!(!is_squeezed(&module));
    assert!(!is_squeezed(&[0, 0, 0]));
}
