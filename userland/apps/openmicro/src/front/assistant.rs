//! The machines the new-machine assistant offers: each model, and the one
//! choice each is made with — a Model B a new blank disc in drive 0 or none,
//! an Electron its Plus 1 or none.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_machines::assistant::{Assistant, Choice, Offer};

use super::library::{describe_fitting, Fitting};
use crate::machine::Model;

/// The models offered, in order.
pub const MODELS: [Model; 2] = [Model::ModelB, Model::Electron];

/// The model offered first.
pub const OFFERED: usize = 0;

const DISCS: [&str; 2] = ["A new blank disc", "None"];
const PLUS1: [&str; 2] = ["Fitted", "None"];
/// The first of either model's choices: a disc made, a Plus 1 fitted.
const WITH: usize = 0;

fn offers() -> Vec<Offer> {
    vec![
        Offer {
            label: Model::ModelB.title(),
            choice: Choice::Offered {
                label: "Drive 0",
                choices: &DISCS,
            },
        },
        Offer {
            label: Model::Electron.title(),
            choice: Choice::Offered {
                label: "Plus 1",
                choices: &PLUS1,
            },
        },
    ]
}

/// The assistant offering [`MODELS`], each under the name `names` gives it.
#[must_use]
pub fn assistant(names: Vec<String>) -> Assistant {
    Assistant::new(offers(), names, OFFERED, describe)
}

/// What an answer to make offer `model` with further `choice` makes: the
/// machine's fitting, and whether a blank disc is made for its drive 0.
#[must_use]
pub fn chosen(model: usize, choice: Option<usize>) -> Option<(Fitting, bool)> {
    let with = choice == Some(WITH);
    Some(match MODELS.get(model)? {
        Model::ModelB => (Fitting::model_b(), with),
        Model::Electron => (Fitting::electron(with), false),
    })
}

fn describe(model: usize, choice: Option<usize>) -> String {
    chosen(model, choice).map_or_else(String::new, |(fitting, _)| describe_fitting(&fitting))
}

#[cfg(test)]
#[path = "assistant_tests.rs"]
mod tests;
