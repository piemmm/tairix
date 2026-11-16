//! The machines the new-machine assistant offers: each model, and the
//! choices each is made with — a Model B a new blank disc in drive 0 or none,
//! an Electron its Plus 1 or none, a Commodore 64 its standard, its SID and
//! its drives.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_machines::assistant::{Assistant, Choice, Offer};

use super::library::{describe_fitting, Fitting, SIDS, STANDARDS};
use crate::machine::c64::{sid, Standard};
use crate::machine::Model;

/// The models offered, in order.
pub const MODELS: [Model; 3] = [Model::ModelB, Model::Electron, Model::C64];

/// The model offered first.
pub const OFFERED: usize = 0;

const DISCS: [&str; 2] = ["A new blank disc", "None"];
const PLUS1: [&str; 2] = ["Fitted", "None"];
/// A C64's drives, by how many: one, two, or none.
const DRIVES: [&str; 3] = ["One 1541", "Two 1541s", "None"];
const DRIVE_COUNTS: [usize; 3] = [1, 2, 0];
/// The first of a Model B's or an Electron's choices: a disc made, a Plus 1
/// fitted.
const WITH: usize = 0;

const MODEL_B: [Choice; 1] = [Choice::Offered {
    label: "Drive 0",
    choices: &DISCS,
}];
const ELECTRON: [Choice; 1] = [Choice::Offered {
    label: "Plus 1",
    choices: &PLUS1,
}];
const C64: [Choice; 3] = [
    Choice::Offered {
        label: "Standard",
        choices: &STANDARDS,
    },
    Choice::Offered {
        label: "SID",
        choices: &SIDS,
    },
    Choice::Offered {
        label: "Drives",
        choices: &DRIVES,
    },
];

fn offers() -> Vec<Offer> {
    vec![
        Offer {
            label: Model::ModelB.title(),
            choices: &MODEL_B,
        },
        Offer {
            label: Model::Electron.title(),
            choices: &ELECTRON,
        },
        Offer {
            label: Model::C64.title(),
            choices: &C64,
        },
    ]
}

/// The assistant offering [`MODELS`], each under the name `names` gives it.
#[must_use]
pub fn assistant(names: Vec<String>) -> Assistant {
    Assistant::new(offers(), names, OFFERED, describe)
}

/// What an answer to make offer `model` with further `choices` makes: the
/// machine's fitting, and whether a blank disc is made for its drive 0.
#[must_use]
pub fn chosen(model: usize, choices: &[Option<usize>]) -> Option<(Fitting, bool)> {
    let choice = |at: usize| choices.get(at).copied().flatten().unwrap_or(0);
    let with = choice(0) == WITH;
    Some(match MODELS.get(model)? {
        Model::ModelB => (Fitting::model_b(), with),
        Model::Electron => (Fitting::electron(with), false),
        Model::C64 => (
            Fitting::c64(
                *Standard::ALL.get(choice(0))?,
                *sid::Model::ALL.get(choice(1))?,
                *DRIVE_COUNTS.get(choice(2))?,
            ),
            false,
        ),
    })
}

fn describe(model: usize, choices: &[Option<usize>]) -> String {
    chosen(model, choices).map_or_else(String::new, |(fitting, _)| describe_fitting(&fitting))
}

#[cfg(test)]
#[path = "assistant_tests.rs"]
mod tests;
