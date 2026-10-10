//! Host tests of the audio device-channel hand-off policy.

extern crate alloc;
use alloc::vec;
use alloc::vec::Vec;

use super::*;
use crate::testsink::RecordingSink;
use tairix_abi::audio::{AudioGain, AudioLocation};
use tairix_abi::hwtree::{HwDeviceClass, HwMatchKey, HwResource, HW_NODE_ROOT};
use tairix_log::DiscardSink;

/// What a recording [`AudiodBind`] was asked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Asked {
    Bind { endpoint: u64, location: u64 },
    Unbind(u64),
    Baseline(AudioBaseline),
}

/// A recording [`AudiodBind`] double: captures each call and answers each
/// with a scripted result, popped from the back.
struct RecordingBind {
    asked: Vec<Asked>,
    results: Vec<Result<(), Errno>>,
}

impl RecordingBind {
    fn new(results: Vec<Result<(), Errno>>) -> Self {
        Self {
            asked: Vec::new(),
            results,
        }
    }

    fn answer(&mut self, asked: Asked) -> Result<(), Errno> {
        self.asked.push(asked);
        self.results.pop().unwrap_or(Ok(()))
    }

    fn binds(&self) -> Vec<u64> {
        self.asked
            .iter()
            .filter_map(|asked| match asked {
                Asked::Bind { endpoint, .. } => Some(*endpoint),
                _ => None,
            })
            .collect()
    }
}

impl AudiodBind for RecordingBind {
    fn bind_driver(&mut self, endpoint: u64, location: u64) -> Result<(), Errno> {
        self.answer(Asked::Bind { endpoint, location })
    }

    fn unbind_driver(&mut self, endpoint: u64) -> Result<(), Errno> {
        self.answer(Asked::Unbind(endpoint))
    }

    fn deliver_baseline(&mut self, baseline: AudioBaseline) -> Result<(), Errno> {
        self.answer(Asked::Baseline(baseline))
    }
}

/// A sound device node with id `id` under `parent`, at bus address
/// `address`, with a register window at `base` (none for zero).
fn device(id: u32, parent: u32, address: u32, base: u64) -> HwNode {
    let mut node = HwNode::new(id, parent, HwDeviceClass::Audio);
    node.set_address(address);
    if base != 0 {
        node.push_resource(HwResource::mmio(base, 0x1000))
            .expect("push window");
    }
    node
}

/// A bus node with id `id` under the root.
fn bus(id: u32, address: u32) -> HwNode {
    let mut node = HwNode::new(id, HW_NODE_ROOT, HwDeviceClass::Bus);
    node.set_address(address);
    node
}

/// An `audiochan` node with id `id` under `parent` publishing `endpoint`.
fn audiochan_node(id: u32, parent: u32, endpoint: u64) -> HwNode {
    let mut node = HwNode::new(id, parent, HwDeviceClass::Audio);
    node.push_match_key(HwMatchKey::compatible(AUDIOCHAN_NODE_COMPATIBLE).expect("key"))
        .expect("push key");
    node.push_resource(HwResource::endpoint(endpoint))
        .expect("push endpoint");
    node
}

/// A tree of one bus carrying two sound devices, each with its channel.
fn two_devices() -> Vec<HwNode> {
    vec![
        bus(1, 0),
        device(2, 1, 3, 0xfe00_0000),
        device(3, 1, 4, 0xfe01_0000),
        audiochan_node(4, 2, 100),
        audiochan_node(5, 3, 101),
    ]
}

#[test]
fn only_an_audiochan_node_carrying_an_endpoint_is_recognised() {
    assert_eq!(
        audiochan_endpoint(&audiochan_node(1, 2, 4_242)),
        Some(4_242)
    );
    // A node of the right class but the wrong compatible string.
    let mut other = HwNode::new(2, HW_NODE_ROOT, HwDeviceClass::Audio);
    other
        .push_match_key(HwMatchKey::compatible(b"tairix,netchan").expect("key"))
        .expect("push key");
    other
        .push_resource(HwResource::endpoint(9))
        .expect("push endpoint");
    assert_eq!(audiochan_endpoint(&other), None);
    // The right node with no endpoint resource is malformed, never guessed.
    let mut bare = HwNode::new(3, HW_NODE_ROOT, HwDeviceClass::Audio);
    bare.push_match_key(HwMatchKey::compatible(AUDIOCHAN_NODE_COMPATIBLE).expect("key"))
        .expect("push key");
    assert_eq!(audiochan_endpoint(&bare), None);
}

/// Node ids are handed out in discovery order; a place is not, so the same
/// devices discovered in another order keep their locations.
#[test]
fn a_location_names_the_place_and_not_the_discovery_order() {
    let first = two_devices();
    let renumbered = vec![
        bus(10, 0),
        device(30, 10, 4, 0xfe01_0000),
        device(20, 10, 3, 0xfe00_0000),
        audiochan_node(50, 30, 101),
        audiochan_node(40, 20, 100),
    ];
    let place = |nodes: &[HwNode], endpoint: u64| {
        let channel = nodes
            .iter()
            .find(|node| audiochan_endpoint(node) == Some(endpoint))
            .expect("a channel");
        device_location(nodes, channel).expect("placed")
    };
    assert_eq!(place(&first, 100), place(&renumbered, 100));
    assert_eq!(place(&first, 101), place(&renumbered, 101));
    assert_ne!(
        place(&first, 100),
        place(&first, 101),
        "two places, two names"
    );
    // The same device moved to another bus position is another place.
    let moved = vec![
        bus(1, 0),
        device(2, 1, 7, 0xfe00_0000),
        audiochan_node(4, 2, 100),
    ];
    assert_ne!(place(&moved, 100), place(&first, 100));
}

/// Two identical devices with nothing to tell them apart but their order
/// under one parent are still two places.
#[test]
fn identical_siblings_are_told_apart_by_their_rank() {
    let nodes = vec![
        bus(1, 0),
        device(2, 1, 0, 0),
        device(3, 1, 0, 0),
        audiochan_node(4, 2, 100),
        audiochan_node(5, 3, 101),
    ];
    let first = device_location(&nodes, &nodes[3]).expect("placed");
    let second = device_location(&nodes, &nodes[4]).expect("placed");
    assert_ne!(first, second);
    assert_ne!(first, 0);
}

#[test]
fn a_channel_the_tree_does_not_place_is_not_handed_over() {
    let orphan = vec![audiochan_node(4, 99, 100)];
    assert_eq!(device_location(&orphan, &orphan[0]), None);
    let mut state = AudioBindState::new();
    let mut bind = RecordingBind::new(Vec::new());
    bind_new_channels(&orphan, &mut state, &mut bind, &DiscardSink);
    assert!(bind.asked.is_empty());
    assert!(state.has_deferred_work(), "a later generation may place it");
}

#[test]
fn each_channel_is_handed_over_once_with_its_place() {
    let nodes = two_devices();
    let mut state = AudioBindState::new();
    let mut bind = RecordingBind::new(Vec::new());
    bind_new_channels(&nodes, &mut state, &mut bind, &DiscardSink);
    bind_new_channels(&nodes, &mut state, &mut bind, &DiscardSink);
    assert_eq!(bind.binds(), vec![100, 101]);
    assert!(state.is_bound(100) && state.is_bound(101));
    let placed = device_location(&nodes, &nodes[3]).expect("placed");
    assert_eq!(
        bind.asked[0],
        Asked::Bind {
            endpoint: 100,
            location: placed
        }
    );
}

/// A device whose channel leaves the tree is retired and forgotten, so the
/// same endpoint coming back — a replug — is handed over again (D245).
#[test]
fn a_vanished_channel_is_retired_and_its_return_is_handed_over() {
    let nodes = two_devices();
    let mut state = AudioBindState::new();
    let mut bind = RecordingBind::new(Vec::new());
    bind_new_channels(&nodes, &mut state, &mut bind, &DiscardSink);

    let unplugged: Vec<HwNode> = nodes[..4].to_vec();
    bind_new_channels(&unplugged, &mut state, &mut bind, &DiscardSink);
    assert_eq!(bind.asked.last(), Some(&Asked::Unbind(101)));
    assert!(!state.is_bound(101));

    bind_new_channels(&nodes, &mut state, &mut bind, &DiscardSink);
    assert_eq!(bind.binds(), vec![100, 101, 101], "handed over again");
}

#[test]
fn a_refused_retirement_is_retried() {
    let nodes = two_devices();
    let mut state = AudioBindState::new();
    let mut bind = RecordingBind::new(Vec::new());
    bind_new_channels(&nodes, &mut state, &mut bind, &DiscardSink);
    let unplugged: Vec<HwNode> = nodes[..4].to_vec();
    bind.results = vec![Ok(()), Err(Errno::NotConnected)];
    bind_new_channels(&unplugged, &mut state, &mut bind, &DiscardSink);
    assert!(state.is_bound(101), "a refused retirement keeps it");
    assert!(state.has_deferred_work());
    bind_new_channels(&unplugged, &mut state, &mut bind, &DiscardSink);
    assert!(!state.is_bound(101), "retried and retired");
    // A service that already let the device go answers NotFound, which is
    // the same as done.
    let nodes = two_devices();
    bind_new_channels(&nodes, &mut state, &mut bind, &DiscardSink);
    bind.results = vec![Err(Errno::NotFound)];
    bind_new_channels(&unplugged, &mut state, &mut bind, &DiscardSink);
    assert!(!state.is_bound(101));
}

#[test]
fn a_refused_hand_off_is_retried_rather_than_recorded() {
    let nodes = two_devices();
    let mut state = AudioBindState::new();
    // Refuse the first, accept every one after.
    let mut bind = RecordingBind::new(vec![Ok(()), Ok(()), Err(Errno::NotConnected)]);
    bind_new_channels(&nodes, &mut state, &mut bind, &DiscardSink);
    assert!(!state.is_bound(100), "a refused bind is not recorded");
    bind_new_channels(&nodes, &mut state, &mut bind, &DiscardSink);
    assert!(state.is_bound(100), "retried and bound");
    assert_eq!(bind.binds(), vec![100, 101, 100]);
}

#[test]
fn a_refusal_records_what_the_audio_service_said() {
    // A record that says only "failed" leaves an operator nothing to act
    // on; the errno is the whole diagnosis.
    let nodes = two_devices();
    let mut state = AudioBindState::new();
    let mut bind = RecordingBind::new(vec![Ok(()), Err(Errno::PermissionDenied)]);
    let sink = RecordingSink::new();
    bind_new_channels(&nodes, &mut state, &mut bind, &sink);

    let id = events::AUDIOD_BIND_FAILED.0;
    assert_eq!(sink.field_of(id, "endpoint").as_deref(), Some("100"));
    assert!(
        sink.field_of(id, "error").is_some(),
        "the refusal record carries what the service said"
    );
}

/// A scripted baseline read.
struct Baselines(Vec<Option<AudioBaseline>>);

impl AudioBaselineSource for Baselines {
    fn load(&mut self) -> Option<AudioBaseline> {
        if self.0.len() > 1 {
            self.0.remove(0)
        } else {
            self.0.first().copied().flatten()
        }
    }
}

fn configured() -> AudioBaseline {
    AudioBaseline {
        output: AudioLocation::new(0x51e7, 0).ok(),
        input: None,
        level: AudioGain::new(-900).expect("attenuation"),
    }
}

/// The administrator's baseline is on the root volume, readable only once
/// it is mounted: the shipped one is delivered first, then the
/// administrator's when it appears, and nothing twice.
#[test]
fn a_baseline_is_delivered_whenever_it_differs_from_the_last() {
    let mut source = Baselines(vec![
        None,
        Some(AudioBaseline::DEFAULT),
        Some(AudioBaseline::DEFAULT),
        Some(configured()),
    ]);
    let mut state = AudioBindState::new();
    let mut bind = RecordingBind::new(Vec::new());
    for _ in 0..5 {
        deliver_audio_baseline(&mut source, &mut state, &mut bind, &DiscardSink);
    }
    assert_eq!(
        bind.asked,
        vec![
            Asked::Baseline(AudioBaseline::DEFAULT),
            Asked::Baseline(configured()),
        ]
    );
    assert!(!state.has_deferred_work());
}

#[test]
fn a_refused_baseline_is_retried() {
    let mut source = Baselines(vec![Some(configured())]);
    let mut state = AudioBindState::new();
    let mut bind = RecordingBind::new(vec![Ok(()), Err(Errno::NotConnected)]);
    deliver_audio_baseline(&mut source, &mut state, &mut bind, &DiscardSink);
    assert!(state.has_deferred_work());
    deliver_audio_baseline(&mut source, &mut state, &mut bind, &DiscardSink);
    assert!(!state.has_deferred_work());
    assert_eq!(bind.asked.len(), 2);
}
