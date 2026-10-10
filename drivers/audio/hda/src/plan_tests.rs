extern crate std;

use std::collections::BTreeMap;
use std::vec::Vec;

use tairix_abi::driver::audio::{ChannelMap, ChannelPosition};

use super::{Hop, Plan};
use crate::codec::Function;
use crate::model::{kind, ModelCodec, ModelLink, ModelWidget};

fn plan(codec: ModelCodec) -> (Function, Plan) {
    let mut link = ModelLink {
        codecs: BTreeMap::from([(0, codec)]),
    };
    let function = Function::read_all(&mut link, 0)
        .expect("readable")
        .remove(0);
    let plan = Plan::of(&function).expect("planned");
    (function, plan)
}

fn nodes(hops: &[Hop]) -> Vec<u8> {
    hops.iter().map(|hop| hop.nid).collect()
}

#[test]
fn qemus_codec_presents_one_stereo_line_out() {
    let (_, plan) = plan(ModelCodec::qemu_output());
    assert_eq!(plan.outputs.len(), 1);
    assert!(plan.inputs.is_empty());
    let output = &plan.outputs[0];
    assert_eq!(output.lanes.len(), 1);
    assert_eq!(nodes(output.lanes[0].hops()), [3, 2]);
    assert_eq!(output.lanes[0].hops()[0].select, Some(0));
    assert_eq!(output.channel_map, ChannelMap::STEREO);
    assert_eq!(output.name.as_str(), "Line Out (Green)");
    assert!(!output.digital && output.mirrors.is_empty());
}

/// The rear panel's association is one eight-channel output, each pin's
/// converter carrying the pair its sequence names, in HDA's order.
#[test]
fn an_association_becomes_one_multichannel_output_in_sequence_order() {
    use ChannelPosition::{
        FrontCentre, FrontLeft, FrontRight, LowFrequency, RearLeft, RearRight, SideLeft, SideRight,
    };
    let (_, plan) = plan(ModelCodec::desktop());
    let rear = &plan.outputs[0];
    let converters: Vec<u8> = rear.lanes.iter().map(super::Route::upstream).collect();
    let pins: Vec<u8> = rear.lanes.iter().map(super::Route::downstream).collect();
    assert_eq!(pins, [0x14, 0x16, 0x15, 0x17]);
    assert_eq!(converters, [0x02, 0x04, 0x03, 0x05]);
    assert_eq!(
        rear.channel_map,
        ChannelMap::new(&[
            FrontLeft,
            FrontRight,
            FrontCentre,
            LowFrequency,
            RearLeft,
            RearRight,
            SideLeft,
            SideRight
        ])
        .expect("eight distinct positions")
    );
    assert_eq!(rear.name.as_str(), "Line Out (Rear, Green)");
}

/// The front headphone jack reaches only converters the rear panel took, so
/// it plays the front pair rather than nothing.
#[test]
fn a_pin_left_without_a_converter_shares_the_front_pair_it_can_reach() {
    let (_, plan) = plan(ModelCodec::desktop());
    let rear = &plan.outputs[0];
    assert_eq!(rear.mirrors.len(), 1);
    assert_eq!(nodes(rear.mirrors[0].hops()), [0x1B, 0x0C, 0x02]);
    assert!(plan
        .outputs
        .iter()
        .all(|output| output.lanes.iter().all(|lane| lane.downstream() != 0x1B)));
}

#[test]
fn a_digital_pin_takes_a_digital_converter_and_stands_alone() {
    let (_, plan) = plan(ModelCodec::desktop());
    let spdif = plan
        .outputs
        .iter()
        .find(|output| output.digital)
        .expect("S/PDIF");
    assert_eq!(nodes(spdif.lanes[0].hops()), [0x1E, 0x06]);
    assert_eq!(spdif.name.as_str(), "S/PDIF Out (Rear)");
    assert_eq!(
        plan.outputs.len(),
        2,
        "the unconnected pin presents nothing"
    );
}

/// Inputs prefer a converter no other input has; the third shares one, and
/// each route selects its own pin on its selector.
#[test]
fn each_input_finds_a_converter_through_its_selector() {
    let (function, plan) = plan(ModelCodec::desktop());
    let routes: Vec<(u8, Vec<u8>, &str)> = plan
        .inputs
        .iter()
        .map(|input| {
            (
                input.route.upstream(),
                nodes(input.route.hops()),
                input.name.as_str(),
            )
        })
        .collect();
    assert_eq!(
        routes,
        [
            (0x12, std::vec![0x08, 0x23, 0x12], "Microphone (Internal)"),
            (0x18, std::vec![0x09, 0x22, 0x18], "Microphone (Rear, Pink)"),
            (0x1A, std::vec![0x08, 0x23, 0x1A], "Line In (Rear, Blue)"),
        ]
    );
    let selector = function.widget(0x23).expect("selector");
    assert_eq!(
        plan.inputs[0].route.hops()[1].select,
        selector.source_index(0x12)
    );
    assert_eq!(
        plan.inputs[2].route.hops()[1].select,
        selector.source_index(0x1A)
    );
}

/// The loopback mixer feeds the front mixer and is fed by it; the search
/// visits each widget once and still finds the shortest route.
#[test]
fn a_cyclic_graph_is_walked_once_and_shortest_first() {
    let (_, plan) = plan(ModelCodec::desktop());
    assert_eq!(nodes(plan.outputs[0].lanes[0].hops()), [0x14, 0x0C, 0x02]);
}

#[test]
fn a_route_longer_than_the_bound_is_not_taken() {
    let mut codec = ModelCodec::qemu_output();
    let chain: Vec<u8> = (10..20).collect();
    for (at, &nid) in chain.iter().enumerate() {
        let source = chain.get(at + 1).copied().unwrap_or(2);
        codec
            .widgets
            .push(ModelWidget::new(nid, kind::MIXER | kind::STEREO).sources(&[source]));
    }
    codec.widget_mut(3).sources = std::vec![10];
    let (_, plan) = plan(codec);
    assert!(plan.outputs.is_empty());
}

#[test]
fn a_display_connector_is_named_for_what_it_is() {
    let (_, plan) = plan(ModelCodec::display(None));
    let output = &plan.outputs[0];
    assert!(output.display && output.digital);
    assert_eq!(output.name.as_str(), "HDMI");
}

#[test]
fn a_lone_mono_converter_presents_one_channel() {
    let mut codec = ModelCodec::qemu_output();
    codec.widget_mut(2).caps &= !kind::STEREO;
    let (_, plan) = plan(codec);
    assert_eq!(plan.outputs[0].channel_map, ChannelMap::MONO);
}
