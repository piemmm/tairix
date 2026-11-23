//! A guest of words, a sink that keeps what it is given, and elements as
//! the probes printed RISC OS's, for the stages' tests.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use tairix_drawing::riscos::format::{END, TAG};
use tairix_riscos_kernel::os::{Fail, Unwind};

use super::{Guest, Sink, Span, Stop};
use crate::geometry::Point;
use crate::path::Element;

/// Guest memory holding only the words written to it: a read anywhere else
/// fails with its address. Its host stops a call after `steps` steps of
/// work, where given.
#[derive(Default)]
pub(crate) struct Memory {
    pub(crate) words: BTreeMap<u32, u32>,
    pub(crate) plotted: Vec<Span>,
    pub(crate) steps: Option<u64>,
    pub(crate) stepped: u64,
}

impl Memory {
    pub(crate) fn with_words(at: u32, words: &[u32]) -> Self {
        let mut memory = Self::default();
        memory.put(at, words);
        memory
    }

    pub(crate) fn put(&mut self, at: u32, words: &[u32]) {
        for (i, &word) in (0u32..).zip(words) {
            self.words.insert(at + 4 * i, word);
        }
    }
}

impl Guest for Memory {
    fn read_word(&mut self, at: u32) -> Result<u32, Fail> {
        self.words.get(&at).copied().ok_or(Fail::Error(at))
    }

    fn write_word(&mut self, at: u32, value: u32) -> Result<(), Fail> {
        self.words.insert(at, value);
        Ok(())
    }

    fn plot(&mut self, spans: &[Span]) -> Result<(), Fail> {
        self.plotted.extend_from_slice(spans);
        Ok(())
    }

    fn yield_point(&mut self) -> Result<(), Fail> {
        if self.steps == Some(self.stepped) {
            return Err(Fail::Unwind(Unwind::Stopped));
        }
        self.stepped += 1;
        Ok(())
    }
}

/// Keeps every element and whether the path finished.
#[derive(Default)]
pub(crate) struct Collect {
    pub(crate) elements: Vec<Element>,
    pub(crate) finished: bool,
}

impl Sink for Collect {
    fn element(&mut self, _: &mut dyn Guest, element: Element) -> Result<(), Stop> {
        self.elements.push(element);
        Ok(())
    }

    fn finish(&mut self, _: &mut dyn Guest) -> Result<(), Stop> {
        self.finished = true;
        Ok(())
    }
}

/// Elements written as the probes print a path: `|code x y …` for each,
/// codes in hex, `|0 …` ending it.
pub(crate) fn parse(text: &str) -> Vec<Element> {
    let mut elements = Vec::new();
    for item in text.split('|').map(str::trim).filter(|s| !s.is_empty()) {
        let mut words = item.split_whitespace();
        let code = words
            .next()
            .and_then(|c| u32::from_str_radix(c, 16).ok())
            .expect("a code");
        let numbers: Vec<i32> = words.map(|w| w.parse().expect("a number")).collect();
        let mut points = numbers.chunks(2).map(|p| Point::new(p[0], p[1]));
        if code & TAG == END {
            break;
        }
        let element = Element::decode::<()>(code & TAG, || Ok(points.next().expect("a point")))
            .expect("decoded")
            .expect("a drawing element");
        elements.push(element);
    }
    elements
}

/// Runs `elements` into `sink`, then finishes it.
pub(crate) fn feed(
    sink: &mut dyn Sink,
    guest: &mut dyn Guest,
    elements: &[Element],
) -> Result<(), Stop> {
    for &element in elements {
        sink.element(guest, element)?;
    }
    sink.finish(guest)
}
