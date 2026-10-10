extern crate std;

use std::collections::BTreeMap;
use std::vec;

use tairix_abi::DriverError;

use super::{Function, Verbs, MAX_CONNECTIONS};
use crate::model::{kind, ModelCodec, ModelLink, ModelWidget, QEMU_AMP, QEMU_PCM};
use crate::verb::{param, Verb, WidgetKind, GET_CONNECTION_LIST};

fn link(codec: ModelCodec) -> ModelLink {
    ModelLink {
        codecs: BTreeMap::from([(0, codec)]),
    }
}

#[test]
fn qemus_output_codec_reads_as_one_converter_feeding_one_jack() {
    let mut link = link(ModelCodec::qemu_output());
    let functions = Function::read_all(&mut link, 0).expect("readable");
    assert_eq!(functions.len(), 1);
    let function = &functions[0];
    assert_eq!((function.nid, function.vendor), (1, 0x1AF4_0012));
    let dac = function.widget(2).expect("converter");
    assert_eq!(dac.kind(), WidgetKind::Output);
    assert_eq!(dac.caps.channels(), 2);
    assert_eq!(dac.output_amp.0, QEMU_AMP);
    assert!(dac.pcm.runs_at(48_000));
    let pin = function.widget(3).expect("pin");
    assert_eq!(pin.kind(), WidgetKind::Pin);
    assert_eq!(pin.sources, [2]);
    assert!(pin.pin.output());
    assert_eq!(pin.config.association(), 1);
}

#[test]
fn a_widget_without_overrides_takes_the_functions_defaults() {
    let mut codec = ModelCodec::qemu_output();
    codec.afg_output_amp = QEMU_AMP;
    codec.widgets[0] = ModelWidget::new(2, kind::OUTPUT | kind::STEREO | (1 << 2));
    let mut link = link(codec);
    let function = Function::read_all(&mut link, 0)
        .expect("readable")
        .remove(0);
    let dac = function.widget(2).expect("converter");
    assert_eq!(dac.output_amp.0, QEMU_AMP);
    assert_eq!(
        dac.pcm.formats(),
        crate::format::PcmSupport::new(QEMU_PCM, 1).formats()
    );
}

/// A codec that states its first connection list with ranges.
struct Ranged(ModelCodec);

impl Verbs for Ranged {
    fn exchange(&mut self, address: u8, verb: Verb) -> Result<u32, DriverError> {
        let body = verb.body();
        if verb.nid() == 3 && body >> 8 == u32::from(GET_CONNECTION_LIST) {
            // Entries 2, then a range to 5, then 8.
            return Ok(match body & 0xFF {
                0 => 0x08_85_02,
                _ => 0,
            });
        }
        if verb.nid() == 3 && body == ((0xF00 << 8) | u32::from(param::CONNECTION_LENGTH)) {
            return Ok(3);
        }
        let _ = address;
        Ok(self.0.answer(verb))
    }
}

#[test]
fn a_connection_range_is_expanded_in_list_order() {
    let mut ranged = Ranged(ModelCodec::qemu_output());
    let function = Function::read_all(&mut ranged, 0)
        .expect("readable")
        .remove(0);
    assert_eq!(function.widget(3).expect("pin").sources, [2, 3, 4, 5, 8]);
    assert_eq!(function.widget(3).expect("pin").source_index(5), Some(3));
}

/// A codec that states one range across every node.
struct Sprawling(ModelCodec);

impl Verbs for Sprawling {
    fn exchange(&mut self, _address: u8, verb: Verb) -> Result<u32, DriverError> {
        let body = verb.body();
        if verb.nid() == 3 && body >> 8 == u32::from(GET_CONNECTION_LIST) {
            return Ok(0xFF_01);
        }
        if verb.nid() == 3 && body == ((0xF00 << 8) | u32::from(param::CONNECTION_LENGTH)) {
            return Ok(2);
        }
        Ok(self.0.answer(verb))
    }
}

#[test]
fn a_range_across_every_node_is_cut_at_the_bound() {
    let mut sprawling = Sprawling(ModelCodec::qemu_output());
    let function = Function::read_all(&mut sprawling, 0)
        .expect("readable")
        .remove(0);
    let sources = &function.widget(3).expect("pin").sources;
    assert_eq!(sources.len(), MAX_CONNECTIONS);
    assert_eq!(sources[..3], [1, 2, 3]);
}

#[test]
fn a_function_built_out_of_order_is_found_by_node() {
    let function = Function::of(
        0,
        1,
        0,
        vec![
            crate::codec::Widget {
                nid: 9,
                ..Default::default()
            },
            crate::codec::Widget {
                nid: 4,
                ..Default::default()
            },
        ],
    );
    assert_eq!(function.widgets()[0].nid, 4);
    assert!(function.widget(9).is_some() && function.widget(5).is_none());
}

#[test]
fn a_codec_that_does_not_answer_is_refused() {
    let mut link = ModelLink {
        codecs: BTreeMap::new(),
    };
    assert_eq!(
        Function::read_all(&mut link, 0),
        Err(DriverError::DeviceFault)
    );
}
