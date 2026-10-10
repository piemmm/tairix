use super::*;

/// An endpoint descriptor's bytes: `address`, `attributes`, packet field
/// `packet`, `interval`, and the audio-class tail when `audio` names one.
fn endpoint(
    address: u8,
    attributes: u8,
    packet: u16,
    interval: u8,
    audio: Option<(u8, u8)>,
) -> alloc::vec::Vec<u8> {
    let [low, high] = packet.to_le_bytes();
    let mut bytes = alloc::vec![
        7,
        DESC_TYPE_ENDPOINT,
        address,
        attributes,
        low,
        high,
        interval
    ];
    if let Some((refresh, synch)) = audio {
        bytes[0] = 9;
        bytes.extend([refresh, synch]);
    }
    bytes
}

#[test]
fn an_isochronous_descriptor_reads_its_sync_and_usage() {
    // Asynchronous data OUT, an audio-class descriptor naming its feedback.
    let data = EndpointDescriptor::decode(&endpoint(0x01, 0x05, 196, 1, Some((0, 0x81))))
        .expect("decodes");
    assert_eq!(
        data.kind,
        TransferKind::Isochronous {
            sync: IsoSync::Asynchronous,
            usage: IsoUsage::Data
        }
    );
    assert_eq!(data.synch_address, 0x81);
    assert_eq!((data.number(), data.is_in(), data.dci()), (1, false, 2));

    let feedback =
        EndpointDescriptor::decode(&endpoint(0x81, 0x11, 3, 1, Some((5, 0)))).expect("decodes");
    assert_eq!(
        feedback.kind,
        TransferKind::Isochronous {
            sync: IsoSync::None,
            usage: IsoUsage::Feedback
        }
    );
    assert_eq!((feedback.refresh, feedback.dci()), (5, 3));

    let implicit =
        EndpointDescriptor::decode(&endpoint(0x82, 0x25, 200, 1, None)).expect("decodes");
    assert_eq!(
        implicit.kind,
        TransferKind::Isochronous {
            sync: IsoSync::Asynchronous,
            usage: IsoUsage::ImplicitFeedbackData
        }
    );
    // High-speed high-bandwidth: two additional transactions.
    let wide = EndpointDescriptor::decode(&endpoint(0x83, 0x0D, 0x1400, 1, None)).expect("decodes");
    assert_eq!((wide.max_packet, wide.transactions), (1024, 2));
}

#[test]
fn a_malformed_endpoint_descriptor_fails_closed() {
    for bytes in [
        endpoint(0x00, 0x01, 64, 1, None),
        endpoint(0x80, 0x01, 64, 1, None),
        endpoint(0x11, 0x01, 64, 1, None),
        // Usage 3 is reserved.
        endpoint(0x01, 0x31, 64, 1, None),
    ] {
        assert_eq!(
            EndpointDescriptor::decode(&bytes),
            Err(DriverError::BadMagic)
        );
    }
    let mut short = endpoint(0x01, 0x01, 64, 1, None);
    short.truncate(6);
    assert_eq!(
        EndpointDescriptor::decode(&short),
        Err(DriverError::BadMagic)
    );
    let mut wrong_type = endpoint(0x01, 0x01, 64, 1, None);
    wrong_type[1] = 0x04;
    assert_eq!(
        EndpointDescriptor::decode(&wrong_type),
        Err(DriverError::BadMagic)
    );
}

#[test]
fn an_isochronous_interval_is_a_power_of_two_in_its_speeds_units() {
    // Full speed counts frames: bInterval 1 is every frame, eight microframes.
    let fs = ServiceInterval::isochronous(UsbSpeed::Full, 1).expect("valid");
    assert_eq!((fs.exponent(), fs.microframes()), (3, 8));
    let fs4 = ServiceInterval::isochronous(UsbSpeed::Full, 4).expect("valid");
    assert_eq!(fs4.microframes(), 64);
    // High speed counts microframes.
    let hs = ServiceInterval::isochronous(UsbSpeed::High, 1).expect("valid");
    assert_eq!((hs.exponent(), hs.microframes()), (0, 1));
    let ss = ServiceInterval::isochronous(UsbSpeed::Super, 16).expect("valid");
    assert_eq!(ss.exponent(), 15);

    for b_interval in [0, 17, 255] {
        assert_eq!(
            ServiceInterval::isochronous(UsbSpeed::High, b_interval),
            Err(DriverError::OutOfRange)
        );
    }
    // 2^15 frames is past what the controller can schedule.
    assert_eq!(
        ServiceInterval::isochronous(UsbSpeed::Full, 14),
        Err(DriverError::OutOfRange)
    );
    assert_eq!(
        ServiceInterval::isochronous(UsbSpeed::Low, 1),
        Err(DriverError::Unsupported)
    );
}

#[test]
fn an_interrupt_interval_reduces_linear_frames_and_clamps() {
    assert_eq!(ServiceInterval::interrupt(UsbSpeed::Full, 10).exponent(), 6);
    assert_eq!(ServiceInterval::interrupt(UsbSpeed::Low, 0).exponent(), 3);
    assert_eq!(
        ServiceInterval::interrupt(UsbSpeed::Full, 255).exponent(),
        10
    );
    assert_eq!(ServiceInterval::interrupt(UsbSpeed::High, 4).exponent(), 3);
    assert_eq!(
        ServiceInterval::interrupt(UsbSpeed::Super, 200).exponent(),
        15
    );
}

#[test]
fn an_isochronous_budget_is_what_its_speed_lets_it_move() {
    let fs = EndpointDescriptor::decode(&endpoint(0x01, 0x09, 1023, 1, None)).expect("decodes");
    assert_eq!(
        PeriodicBudget::isochronous(&fs, UsbSpeed::Full),
        Ok(PeriodicBudget {
            max_packet: 1023,
            max_burst: 0,
            mult: 0,
            max_esit_payload: 1023
        })
    );
    let wide = EndpointDescriptor::decode(&endpoint(0x81, 0x0D, 0x1400, 1, None)).expect("decodes");
    assert_eq!(
        PeriodicBudget::isochronous(&wide, UsbSpeed::High),
        Ok(PeriodicBudget {
            max_packet: 1024,
            max_burst: 2,
            mult: 0,
            max_esit_payload: 3072
        })
    );
    let mut ss = EndpointDescriptor::decode(&endpoint(0x81, 0x05, 1024, 1, None)).expect("decodes");
    ss.companion = Some(SsCompanion {
        max_burst: 3,
        attributes: 1,
        bytes_per_interval: 6000,
    });
    assert_eq!(
        PeriodicBudget::isochronous(&ss, UsbSpeed::Super),
        Ok(PeriodicBudget {
            max_packet: 1024,
            max_burst: 3,
            mult: 1,
            max_esit_payload: 6000
        })
    );
}

#[test]
fn an_impossible_isochronous_budget_is_refused() {
    let zero = EndpointDescriptor::decode(&endpoint(0x01, 0x01, 0, 1, None)).expect("decodes");
    assert_eq!(
        PeriodicBudget::isochronous(&zero, UsbSpeed::Full),
        Err(DriverError::BadMagic)
    );
    // Additional transactions belong to high speed alone.
    let wide = EndpointDescriptor::decode(&endpoint(0x01, 0x01, 0x0900, 1, None)).expect("decodes");
    assert_eq!(
        PeriodicBudget::isochronous(&wide, UsbSpeed::Full),
        Err(DriverError::BadMagic)
    );
    let fs_big = EndpointDescriptor::decode(&endpoint(0x01, 0x01, 1024, 1, None)).expect("decodes");
    assert_eq!(
        PeriodicBudget::isochronous(&fs_big, UsbSpeed::Full),
        Err(DriverError::BadMagic)
    );
    let three =
        EndpointDescriptor::decode(&endpoint(0x01, 0x01, 0x1C00, 1, None)).expect("decodes");
    assert_eq!(
        PeriodicBudget::isochronous(&three, UsbSpeed::High),
        Err(DriverError::BadMagic)
    );
    let mut ss = EndpointDescriptor::decode(&endpoint(0x81, 0x05, 1024, 1, None)).expect("decodes");
    assert_eq!(
        PeriodicBudget::isochronous(&ss, UsbSpeed::Super),
        Err(DriverError::BadMagic),
        "a SuperSpeed endpoint states its interval payload in its companion"
    );
    for (burst, attributes, bytes) in [(0, 3, 1024), (0, 0, 0), (0, 0, 1025), (16, 0, 1024)] {
        ss.companion = Some(SsCompanion {
            max_burst: burst,
            attributes,
            bytes_per_interval: bytes,
        });
        assert_eq!(
            PeriodicBudget::isochronous(&ss, UsbSpeed::Super),
            Err(DriverError::BadMagic)
        );
    }
    assert_eq!(
        PeriodicBudget::isochronous(&zero, UsbSpeed::Low),
        Err(DriverError::Unsupported)
    );
}

#[test]
fn burst_counts_follow_the_specifications_formula() {
    let fs = PeriodicBudget {
        max_packet: 192,
        max_burst: 0,
        mult: 0,
        max_esit_payload: 192,
    };
    assert_eq!(fs.burst_counts(192), (0, 0));
    assert_eq!(fs.burst_counts(0), (0, 0));
    let hs = PeriodicBudget {
        max_packet: 1024,
        max_burst: 2,
        mult: 0,
        max_esit_payload: 3072,
    };
    // Three packets in one burst of three.
    assert_eq!(hs.burst_counts(3072), (0, 2));
    // Two packets: one burst, its last carrying two.
    assert_eq!(hs.burst_counts(1500), (0, 1));
    let ss = PeriodicBudget {
        max_packet: 1024,
        max_burst: 3,
        mult: 1,
        max_esit_payload: 8192,
    };
    // Eight packets in bursts of four: two bursts, the last full.
    assert_eq!(ss.burst_counts(8192), (1, 3));
    // Five packets: two bursts, the last carrying one.
    assert_eq!(ss.burst_counts(4097), (1, 0));
}

#[test]
fn feedback_reads_each_speeds_own_format() {
    // 48 kHz at full speed: 48.0 frames per frame in 10.14 is 0x0C0000.
    let mut fs = FeedbackDecoder::new(UsbSpeed::Full, 48_000);
    assert_eq!(
        fs.decode(&[0x00, 0x00, 0x0C]),
        Some(FeedbackRate::from_hz(48_000))
    );
    // 44.1 kHz at high speed: 5.5125 frames per microframe in 16.16.
    let per_microframe: u32 = 44_100 * 65_536 / 8_000;
    let mut hs = FeedbackDecoder::new(UsbSpeed::High, 44_100);
    let rate = hs
        .decode(&per_microframe.to_le_bytes())
        .expect("within the window");
    assert_eq!(rate.millihertz() / 1000, 44_099);
}

#[test]
fn feedback_in_the_other_speeds_format_is_read_through_a_fixed_shift() {
    // A full-speed device sending 16.16 frames per frame, as many do.
    let mut fs = FeedbackDecoder::new(UsbSpeed::Full, 48_000);
    let rate = fs.decode(&0x0030_0000u32.to_le_bytes()).expect("read");
    assert_eq!(rate, FeedbackRate::from_hz(48_000));
    // The interpretation is fixed now: a slightly fast clock still reads.
    let faster = fs.decode(&0x0030_1000u32.to_le_bytes()).expect("read");
    assert!(faster > rate);
    // A high-speed device sending 10.14 frames per frame.
    let mut hs = FeedbackDecoder::new(UsbSpeed::High, 48_000);
    assert_eq!(
        hs.decode(&[0x00, 0x00, 0x0C, 0x00]),
        Some(FeedbackRate::from_hz(48_000))
    );
}

#[test]
fn feedback_outside_the_nominal_window_is_refused() {
    let mut fs = FeedbackDecoder::new(UsbSpeed::Full, 48_000);
    // 30 kHz lands within an eighth of 48 kHz under no shift tried.
    let thirty = (30u32 << 14).to_le_bytes();
    assert_eq!(fs.decode(&thirty[..3]), None);
    assert_eq!(fs.decode(&[0x00, 0x00]), None, "too short to hold a value");
    // Once fixed, a wild value is refused rather than followed.
    assert!(fs.decode(&[0x00, 0x00, 0x0C]).is_some());
    let wild = (60u32 << 14).to_le_bytes();
    assert_eq!(fs.decode(&wild[..3]), None);
}

#[test]
fn the_pacer_spreads_a_rate_exactly_over_an_hour() {
    let mut pacer = PacketPacer::nominal(44_100, 8);
    let first_ten: alloc::vec::Vec<u32> = (0..10).map(|_| pacer.next_frames()).collect();
    assert_eq!(first_ten.iter().sum::<u32>(), 441);
    assert!(first_ten.iter().all(|&frames| frames == 44 || frames == 45));
    let mut total = 441u64;
    for _ in 10..3_600_000 {
        total += u64::from(pacer.next_frames());
    }
    assert_eq!(total, 44_100 * 3600, "an hour of 1 ms intervals");
    assert_eq!(pacer.max_frames(), 45);
    // High speed, 125 µs: 48 kHz is six frames every interval.
    let mut hs = PacketPacer::nominal(48_000, 1);
    assert!((0..1000).all(|_| hs.next_frames() == 6));
}

#[test]
fn following_feedback_carries_the_fraction_across() {
    let mut pacer = PacketPacer::nominal(44_100, 8);
    for _ in 0..5 {
        pacer.next_frames();
    }
    // Half a frame is owed after five intervals of 44.1. The device then asks
    // for 44.0625 frames a frame, exactly representable in Q16.16.
    pacer.follow(FeedbackRate(44 * Q16 + Q16 / 16), 8);
    let next_eight: u32 = (0..8).map(|_| pacer.next_frames()).sum();
    // The owed half plus 352.5 more: 353 frames. Dropping the half would
    // have delivered 352.
    assert_eq!(next_eight, 353);
}

#[test]
fn implicit_feedback_reads_the_rate_a_data_endpoint_carried() {
    let decoder = FeedbackDecoder::new(UsbSpeed::Full, 44_100);
    // Ten 1 ms frames carrying 441 sample frames are exactly 44.1 kHz.
    assert_eq!(
        decoder.implicit(441, 80),
        Some(FeedbackRate::from_hz(44_100))
    );
    // A device running a little fast is followed.
    let fast = decoder.implicit(442, 80).expect("inside the window");
    assert!(fast > FeedbackRate::from_hz(44_100));
    // An interval count of zero, or a rate a clock could not drift to, is
    // refused rather than followed.
    assert_eq!(decoder.implicit(441, 0), None);
    assert_eq!(decoder.implicit(882, 80), None);
    assert_eq!(decoder.implicit(0, 80), None);
}

#[test]
fn advancing_many_intervals_at_once_sums_what_stepping_them_would() {
    let mut stepped = PacketPacer::nominal(44_100, 8);
    let mut advanced = stepped;
    let sum: u64 = (0..37).map(|_| u64::from(stepped.next_frames())).sum();
    assert_eq!(advanced.advance(37), sum);
    // The two stay in step afterwards: the remainder carried is the same.
    assert_eq!(advanced.next_frames(), stepped.next_frames());
    // A gap of a whole bus epoch neither overflows nor loses a frame.
    let mut long = PacketPacer::nominal(768_000, 8);
    assert_eq!(long.advance(u32::MAX), 768 * u64::from(u32::MAX));
}
