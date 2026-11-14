use alloc::string::String;

use super::*;
use crate::front::library::{Image, Shipped, Socket};

#[test]
fn a_model_b_is_made_with_a_blank_disc_or_none() {
    let (fitting, disc) = chosen(0, Some(0)).expect("a Model B");
    assert_eq!(fitting.model(), Model::ModelB);
    assert!(disc);
    assert_eq!(chosen(0, Some(1)).map(|(_, disc)| disc), Some(false));
}

#[test]
fn an_electron_is_made_with_its_plus_1_or_none() {
    let (with, disc) = chosen(1, Some(0)).expect("an Electron");
    assert!(!disc, "an Electron has no drive to fill");
    assert!(with.sockets().any(|(socket, image)| {
        socket == Socket::Plus1 && *image == Image::Shipped(Shipped::Plus1)
    }));
    let (without, _) = chosen(1, Some(1)).expect("an Electron");
    assert!(without.sockets().all(|(socket, _)| socket != Socket::Plus1));
    assert!(chosen(2, None).is_none(), "no third model");
}

#[test]
fn the_assistant_offers_each_model_under_its_own_name() {
    let assistant = assistant(alloc::vec![
        String::from("BBC Micro Model B"),
        String::from("Acorn Electron"),
    ]);
    assert_eq!(describe(1, Some(0)), "Acorn Electron with a Plus 1");
    assert_eq!(describe(0, Some(1)), "BBC Micro Model B with the DFS");
    assert!(!assistant.column().groups().is_empty());
}
