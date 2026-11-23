use super::*;
use crate::hwtree::NodeEndpointBlock;

#[test]
fn every_op_and_error_is_numbered_densely_from_one() {
    for (index, op) in GpuOp::ALL.iter().enumerate() {
        assert_eq!(usize::from(op.raw()), index + 1);
        assert_eq!(GpuOp::from_raw(op.raw()), Some(*op));
    }
    assert_eq!(GpuOp::from_raw(0), None);
    let past = GpuOp::ALL.last().map(|op| op.raw() + 1).expect("an op");
    assert_eq!(GpuOp::from_raw(past), None);
    for (index, error) in GpuError::ALL.iter().enumerate() {
        assert_eq!(usize::from(error.raw()), index + 1);
        assert_eq!(GpuError::from_raw(error.raw()), Some(*error));
    }
    assert_eq!(GpuError::from_raw(0), None);
    let past = GpuError::ALL
        .last()
        .map(|error| error.raw() + 1)
        .expect("an error");
    assert_eq!(GpuError::from_raw(past), None);
}

#[test]
fn a_request_round_trips_and_every_malformed_frame_is_refused() {
    let body = [7u8; 12];
    let mut frame = [0u8; GPU_MAX_REQUEST];
    for op in GpuOp::ALL {
        let len = GpuRequest { op, body: &body }
            .encode(&mut frame)
            .expect("encoded");
        assert_eq!(
            GpuRequest::decode(&frame[..len]),
            Ok(GpuRequest { op, body: &body })
        );
    }
    let len = GpuRequest {
        op: GpuOp::Info,
        body: &[],
    }
    .encode(&mut frame)
    .expect("encoded");
    assert_eq!(
        GpuRequest::decode(&frame[..len - 1]),
        Err(Errno::BufferTooSmall)
    );
    let mut wrong = frame;
    wrong[0] ^= 1;
    assert_eq!(GpuRequest::decode(&wrong[..len]), Err(Errno::BadMagic));
    let mut wrong = frame;
    wrong[4] = 2;
    assert_eq!(
        GpuRequest::decode(&wrong[..len]),
        Err(Errno::AbiVersionUnsupported)
    );
    let mut wrong = frame;
    wrong[6..8].copy_from_slice(&999u16.to_le_bytes());
    assert_eq!(GpuRequest::decode(&wrong[..len]), Err(Errno::OutOfRange));
    let long = [0u8; GPU_MAX_BODY + 1];
    assert_eq!(
        GpuRequest {
            op: GpuOp::Info,
            body: &long
        }
        .encode(&mut [0u8; GPU_MAX_REQUEST + 8]),
        Err(Errno::LengthOutOfRange)
    );
    let mut oversized = [0u8; GPU_MAX_REQUEST + 1];
    oversized[..GPU_HEADER_LEN].copy_from_slice(&frame[..GPU_HEADER_LEN]);
    assert_eq!(GpuRequest::decode(&oversized), Err(Errno::LengthOutOfRange));
}

#[test]
fn a_reply_carries_its_status_and_names_its_op() {
    let mut frame = [0u8; GPU_MAX_REPLY];
    for status in [
        GpuStatus::Ok,
        GpuStatus::Refused(Errno::PermissionDenied),
        GpuStatus::Device(GpuError::Lost),
        GpuStatus::Device(GpuError::Interface),
    ] {
        let len = encode_reply(GpuOp::Allocate, status, &[1, 2, 3], &mut frame).expect("encoded");
        assert_eq!(
            decode_reply(&frame[..len], GpuOp::Allocate),
            Ok((status, &[1u8, 2, 3][..]))
        );
        assert_eq!(
            decode_reply(&frame[..len], GpuOp::Info),
            Err(Errno::BadMagic),
            "a reply read as another op's"
        );
    }
    let len = encode_reply(GpuOp::Status, GpuStatus::Ok, &[], &mut frame).expect("encoded");
    let mut reserved = frame;
    reserved[6] = 1;
    assert_eq!(
        decode_reply(&reserved[..len], GpuOp::Status),
        Err(Errno::BadMagic)
    );
    let mut unknown = frame;
    unknown[..4].copy_from_slice(&999i32.to_le_bytes());
    assert_eq!(
        decode_reply(&unknown[..len], GpuOp::Status),
        Err(Errno::BadMagic)
    );
    assert_eq!(
        decode_reply(&frame[..GPU_REPLY_HEADER_LEN - 1], GpuOp::Status),
        Err(Errno::BufferTooSmall)
    );
}

#[test]
fn a_region_span_is_held_within_its_region() {
    let span = RegionSpan {
        offset: 16,
        len: 32,
    };
    assert_eq!(span.within(48), Ok(16..48));
    assert_eq!(span.within(47), Err(Errno::OutOfRange));
    assert_eq!(RegionSpan { offset: 48, len: 0 }.within(48), Ok(48..48));
    assert_eq!(
        RegionSpan { offset: 49, len: 0 }.within(48),
        Err(Errno::OutOfRange)
    );
    let mut wire = [0u8; RegionSpan::WIRE_LEN];
    span.encode(&mut wire).expect("encoded");
    assert_eq!(RegionSpan::decode(&wire), Ok(span));
    assert_eq!(RegionSpan::decode(&wire[..7]), Err(Errno::BufferTooSmall));
}

#[test]
fn a_device_descriptor_round_trips_and_refuses_reserved_bits() {
    let descriptor = GpuDeviceDescriptor {
        endpoint: GPU_CHANNEL_ENDPOINTS.endpoint(7),
        node: 7,
    };
    let wire = descriptor.to_le_bytes();
    assert_eq!(GpuDeviceDescriptor::from_le_bytes(&wire), Ok(descriptor));
    let mut reserved = wire;
    reserved[15] = 1;
    assert_eq!(
        GpuDeviceDescriptor::from_le_bytes(&reserved),
        Err(Errno::BadMagic)
    );
    assert_eq!(
        GpuDeviceDescriptor::from_le_bytes(&wire[..15]),
        Err(Errno::BufferTooSmall)
    );
}

#[test]
fn the_channel_block_is_a_reserved_device_channel() {
    assert!(NodeEndpointBlock::ALL.contains(&GPU_CHANNEL_ENDPOINTS));
    assert!(NodeEndpointBlock::DEVICE_CHANNELS.contains(&GPU_CHANNEL_ENDPOINTS));
    assert_eq!(
        NodeEndpointBlock::device_channel_node(GPU_CHANNEL_ENDPOINTS.endpoint(3)),
        Some(3)
    );
}

#[test]
fn a_desk_port_is_its_process_own_and_reserved_by_nothing() {
    let ours = desk_endpoint_for(7);
    assert_ne!(ours, desk_endpoint_for(8));
    assert_eq!(ours >> 48, u64::from(u16::from_be_bytes(*b"GD")));
    assert_eq!(
        desk_endpoint_for(crate::PID_MAX + 1),
        desk_endpoint_for(0),
        "no pid reaches the tag"
    );
    assert!(!crate::ipc::is_reserved_endpoint(ours));
    assert!(NodeEndpointBlock::ALL
        .iter()
        .all(|block| !block.contains(ours)));
}
