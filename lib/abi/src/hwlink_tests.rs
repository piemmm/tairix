use super::*;
use crate::hwtree::{HwResource, HwResourceKind, BUS_CHILD_ENDPOINTS};

#[test]
fn a_role_is_read_from_the_block_its_endpoint_lies_in() {
    for &role in LinkRole::ALL {
        let endpoint = role.endpoints().endpoint(7);
        assert_eq!(LinkRole::of_endpoint(endpoint), Some(role));
        for &other in LinkRole::ALL {
            assert_eq!(other.endpoints().contains(endpoint), other == role);
        }
    }
    assert_eq!(LinkRole::of_endpoint(BUS_CHILD_ENDPOINTS.endpoint(7)), None);
    assert_eq!(LinkRole::of_endpoint(0x5345_1001), None);
}

#[test]
fn only_a_dma_controller_states_channels() {
    let dma = LinkRole::Dma.endpoints().endpoint(3);
    let clock = LinkRole::Clock.endpoints().endpoint(3);
    assert_eq!(
        LinkDuty::new(dma, Some(0b101)).map(|duty| duty.channels()),
        Ok(Some(0b101))
    );
    assert_eq!(LinkDuty::new(clock, Some(1)), Err(Errno::OutOfRange));
    let duty = LinkDuty::new(clock, None).expect("a clock duty");
    assert_eq!((duty.role(), duty.endpoint()), (LinkRole::Clock, clock));
    assert_eq!(
        LinkDuty::new(BUS_CHILD_ENDPOINTS.endpoint(3), None),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn a_request_holds_its_selector_and_name_exactly_or_is_refused() {
    let endpoint = LinkRole::Clock.endpoints().endpoint(9);
    let request = LinkRequest::new(endpoint, 2, &[0x1E], b"pwm").expect("valid");
    assert_eq!(request.role(), LinkRole::Clock);
    assert_eq!(request.index(), 2);
    assert_eq!(request.selector(), [0x1E]);
    assert_eq!(request.name(), b"pwm");
    let full = LinkRequest::new(endpoint, 0, &[1, 2], b"eightchr").expect("at the bounds");
    assert_eq!(
        (full.selector(), full.name()),
        (&[1, 2][..], &b"eightchr"[..])
    );
    assert_eq!(
        LinkRequest::new(endpoint, 0, &[1, 2, 3], b""),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        LinkRequest::new(endpoint, 0, &[], b"ninechars"),
        Err(Errno::LengthOutOfRange)
    );
    assert_eq!(
        LinkRequest::new(endpoint, 0, &[], b"a\0b"),
        Err(Errno::OutOfRange)
    );
    assert_eq!(
        LinkRequest::new(BUS_CHILD_ENDPOINTS.endpoint(9), 0, &[], b""),
        Err(Errno::OutOfRange)
    );
}

#[test]
fn every_role_round_trips_through_its_resource_records() {
    for &role in LinkRole::ALL {
        let endpoint = role.endpoints().endpoint(11);
        let request = LinkRequest::new(endpoint, 1, &[4, 5], b"tx").expect("valid");
        let record = HwResource::request(&request);
        assert_eq!(record.kind(), Some(HwResourceKind::LinkRequest));
        let back = HwResource::from_bytes(&record.to_le_bytes()).expect("decodes");
        assert_eq!(back.link_request(), Ok(request));
        let duty = LinkDuty::new(endpoint, None).expect("valid");
        let record = HwResource::duty(&duty);
        assert_eq!(record.kind(), Some(HwResourceKind::LinkDuty));
        let back = HwResource::from_bytes(&record.to_le_bytes()).expect("decodes");
        assert_eq!(back.link_duty(), Ok(duty));
    }
}
