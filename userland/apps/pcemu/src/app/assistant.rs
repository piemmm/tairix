//! The machines the new-machine assistant offers: the four templates, and for
//! each with a fixed disk controller, whether a hard disk is made for it.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_machines::assistant::{Assistant, Choice, Offer};

use super::library::describe_config;
use crate::machine::Template;

/// The machines offered, in order, and what each is called.
pub const TEMPLATES: [(Template, &str); 4] = [
    (Template::IbmPc, "IBM Personal Computer"),
    (Template::IbmXt, "IBM Personal Computer XT"),
    (Template::TurboXt, "Turbo XT"),
    (Template::Workstation186, "80186 workstation"),
];

/// The machine offered first: the quickest the default disk boots on.
pub const OFFERED: usize = 2;

const DISKS: [&str; 2] = ["A new 30 MB hard disk", "None"];
/// The disk choice that makes one.
const A_NEW_DISK: usize = 0;

/// What each of [`TEMPLATES`] is offered with: a hard disk to choose where it
/// has a controller to drive one.
fn offers() -> Vec<Offer> {
    TEMPLATES
        .iter()
        .map(|&(template, label)| Offer {
            label,
            choice: if template.config().fixed.is_some() {
                Choice::Offered {
                    label: "Hard disk",
                    choices: &DISKS,
                }
            } else {
                Choice::Fixed {
                    label: "Hard disk",
                    shown: "None",
                    why: "This machine has no hard disk controller.",
                }
            },
        })
        .collect()
}

/// The assistant offering [`TEMPLATES`], each under the name `names` gives it.
#[must_use]
pub fn assistant(names: Vec<String>) -> Assistant {
    Assistant::new(offers(), names, OFFERED, describe)
}

/// The template an answer to make offer `model` names, and whether a hard
/// disk is made for it with further `choice`.
#[must_use]
pub fn chosen(model: usize, choice: Option<usize>) -> Option<(Template, bool)> {
    let &(template, _) = TEMPLATES.get(model)?;
    Some((template, choice == Some(A_NEW_DISK)))
}

fn describe(model: usize, choice: Option<usize>) -> String {
    chosen(model, choice).map_or_else(String::new, |(template, disk)| {
        describe_config(&template.config(), usize::from(disk))
    })
}

#[cfg(test)]
#[path = "assistant_tests.rs"]
mod tests;
