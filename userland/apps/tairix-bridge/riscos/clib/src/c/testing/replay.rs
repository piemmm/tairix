//! Genuine's recorded answers replayed through the library's own entries.
//! A record is a line: the entry as `chunk.entry`, `R=` its four argument
//! registers (`@E` the address of the probes' buffer E), `S=` the stacked
//! words where not the default pointers, `F=` F0 to F3 where set, then after
//! `->` the answers its function defines — none where it trapped — `errno`,
//! the trap, and the FPSR's cumulative flags after.

extern crate std;

use std::collections::BTreeMap;
use std::format;
use std::string::String;
use std::vec::Vec;

use tairix_riscos_kernel::FRESH_FPSR;

use super::{Client, FREE, STATICS};
use crate::c::statics::at;
use crate::chunk;

/// The probes' eight buffers, A to H, four kilobytes each, and E, which an
/// answer may be stored through.
const BUFFERS: u32 = FREE;
const BUFFER_BYTES: u32 = 0x1000;
const E: u32 = BUFFERS + 4 * BUFFER_BYTES;

/// A word given: a value, or the address of one of the buffers.
fn word(text: &str) -> u32 {
    match text.strip_prefix('@') {
        Some(letter) => BUFFERS + BUFFER_BYTES * u32::from(letter.as_bytes()[0] - b'A'),
        None => u32::from_str_radix(text, 16).expect("a hex word"),
    }
}

/// One recorded call and what genuine answered.
#[derive(Default)]
pub(crate) struct Call {
    pub(crate) chunk: u32,
    pub(crate) entry: usize,
    regs: [u32; 4],
    stack: [u32; 4],
    fregs: Option<[u64; 4]>,
    answers: BTreeMap<String, u64>,
}

impl Call {
    /// The double R`n` and R`n + 1` hold.
    pub(crate) fn double(&self, n: usize) -> f64 {
        f64::from_bits(u64::from(self.regs[n]) << 32 | u64::from(self.regs[n + 1]))
    }

    /// The double R0 and R1 hold, the first argument of most.
    pub(crate) fn x(&self) -> f64 {
        self.double(0)
    }

    /// The raw single R`n` holds.
    pub(crate) fn single(&self, n: usize) -> f64 {
        f64::from(f32::from_bits(self.regs[n]))
    }

    /// F0 to F3 as the call set them, the doubles a helper takes.
    pub(crate) fn operands(&self) -> [f64; 4] {
        self.fregs.unwrap_or_default().map(f64::from_bits)
    }

    /// What genuine answered under `key`.
    pub(crate) fn answer(&self, key: &str) -> Option<u64> {
        self.answers.get(key).copied()
    }
}

fn parse(line: &str) -> Call {
    let mut call = Call {
        stack: ["@E", "@F", "@G", "@H"].map(word),
        ..Call::default()
    };
    let mut fields = line.split_whitespace();
    let (chunk, entry) = fields
        .next()
        .and_then(|at| at.split_once('.'))
        .expect("chunk.entry");
    (call.chunk, call.entry) = (
        chunk.parse().expect("a chunk"),
        entry.parse().expect("an entry"),
    );
    for field in fields {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        let list = || value.split(',');
        match key {
            "R" => list()
                .zip(call.regs.iter_mut())
                .for_each(|(w, r)| *r = word(w)),
            "S" => list()
                .zip(call.stack.iter_mut())
                .for_each(|(w, s)| *s = word(w)),
            "F" => {
                let mut fregs = [0; 4];
                list()
                    .zip(fregs.iter_mut())
                    .for_each(|(w, f)| *f = u64::from_str_radix(w, 16).expect("hex"));
                call.fregs = Some(fregs);
            }
            "errno" => {
                call.answers
                    .insert(key.into(), value.parse().expect("errno"));
            }
            _ => {
                call.answers
                    .insert(key.into(), u64::from_str_radix(value, 16).expect("hex"));
            }
        }
    }
    call
}

/// What our entry answers to `call`, in the recorded answers' terms.
fn ours(client: &mut Client, call: &Call) -> BTreeMap<String, u64> {
    let chunk = chunk::chunk(call.chunk).expect("a built chunk");
    let code = chunk::ROUTINES[chunk::routine(chunk, call.entry).expect("an entry")];
    client.put(E, &[0; 16]);
    client.put(STATICS + at::ERRNO, &[0; 4]);
    client.running();
    {
        let mut os = client.session.kernel.os();
        let sp = os.registers().r[13];
        os.write_words(sp, &call.stack).expect("the stack");
        os.set_fpsr(FRESH_FPSR);
        for (n, bits) in (0u8..).zip(call.fregs.unwrap_or_default()) {
            os.set_fpa_double(n, bits);
        }
    }
    let outcome = client.outcome(code, &call.regs);
    let mut answers = BTreeMap::new();
    answers.insert("trap".into(), u64::from(outcome.is_err()));
    answers.insert(
        "flags".into(),
        u64::from(client.session.kernel.os().fpsr() & 0x1F),
    );
    answers.insert("errno".into(), u64::from(client.word(STATICS + at::ERRNO)));
    if let Ok(regs) = outcome {
        for (key, n) in [("F0", 0), ("F1", 1)] {
            answers.insert(
                key.into(),
                client.session.kernel.os().fpa_double(n).unwrap_or(u64::MAX),
            );
        }
        answers.insert("R0".into(), u64::from(regs.r[0]));
        answers.insert("R1".into(), u64::from(regs.r[1]));
        answers.insert(
            "E".into(),
            u64::from(client.word(E)) << 32 | u64::from(client.word(E + 4)),
        );
    }
    answers
}

/// Whether a known difference excuses our `got` for genuine's `want` under
/// `key` in `call`.
pub(crate) type Excused = fn(&Call, &str, u64, u64) -> bool;

/// Every recorded call in `records` whose answers differ from ours in a way
/// `excused` does not excuse, with what differs.
pub(crate) fn disagreements(records: &str, excused: Excused) -> Vec<String> {
    let mut client = Client::with_heap();
    let mut found = Vec::new();
    for line in records
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
    {
        let call = parse(line);
        let got = ours(&mut client, &call);
        let differing: Vec<String> = call
            .answers
            .iter()
            .filter(|&(key, &want)| {
                got.get(key)
                    .is_none_or(|&ours| ours != want && !excused(&call, key, want, ours))
            })
            .map(|(key, want)| format!("{key} {want:X} ours {:X?}", got.get(key)))
            .collect();
        if !differing.is_empty() {
            found.push(format!("{line}\n    {}", differing.join("; ")));
        }
    }
    found
}

/// Whether two doubles' bits lie within `ulps` places of each other.
pub(crate) fn within_ulps(want: u64, got: u64, ulps: u64) -> bool {
    want >> 63 == got >> 63 && want.abs_diff(got) <= ulps
}
