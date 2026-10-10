//! What reading a file's metadata allocates: one allocation a kept tag, with
//! the list that holds them grown amortised rather than an entry at a time,
//! and nothing in proportion to a block it steps over.

use tairix_fuzzseed::meter::{metered, Metered};
use tairix_sound::flac_encode::{encode, Options, Params};
use tairix_sound::{DecodeLimits, PcmSource};

#[global_allocator]
static ALLOC: Metered = Metered;

fn chunk(id: &[u8], body: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend(u32::try_from(body.len()).expect("small").to_le_bytes());
    out.extend(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
    out
}

#[test]
fn keeping_many_tags_allocates_once_a_tag() {
    const TAGS: usize = 1000;
    let mut format = Vec::new();
    for field in [1u16, 1] {
        format.extend(field.to_le_bytes());
    }
    format.extend(8000u32.to_le_bytes());
    format.extend(16_000u32.to_le_bytes());
    format.extend([2u16, 16].iter().flat_map(|field| field.to_le_bytes()));
    let mut info = b"INFO".to_vec();
    for _ in 0..TAGS {
        info.extend(chunk(b"ICMT", b"tag"));
    }
    let body = [
        chunk(b"fmt ", &format),
        chunk(b"LIST", &info),
        chunk(b"data", &[0; 8]),
    ]
    .concat();
    let mut file = b"RIFF".to_vec();
    file.extend(u32::try_from(body.len() + 4).expect("small").to_le_bytes());
    file.extend(b"WAVE");
    file.extend(body);
    let limits = DecodeLimits::new(8, 128 * 1024, 0);
    let (opened, metering) = metered(|| {
        let mut input = file.as_slice();
        PcmSource::open(&mut input, &limits)
    });
    assert_eq!(opened.expect("opens").metadata().tags.len(), TAGS);
    assert!(
        metering.allocations < TAGS + 64,
        "{} allocations for {TAGS} tags",
        metering.allocations
    );
}

/// A picture block of mebibytes is stepped over, natively and in Ogg, in no
/// more memory than a page of the file.
#[test]
fn stepping_over_a_large_picture_holds_none_of_it() {
    const PICTURE: usize = 3 << 20;
    let params = Params {
        rate: 44_100,
        channels: 1,
        bits: 16,
    };
    let samples: Vec<i32> = (0..4096).map(|n| (n % 200) - 100).collect();
    let mut picture = 3u32.to_be_bytes().to_vec();
    for text in [&b"image/png"[..], b""] {
        picture.extend(u32::try_from(text.len()).expect("small").to_be_bytes());
        picture.extend(text);
    }
    picture.extend([0u8; 16]);
    picture.extend(u32::try_from(PICTURE).expect("small").to_be_bytes());
    picture.extend(vec![0x5A; PICTURE]);
    let limits = DecodeLimits::new(2, 4096, 0);
    for ogg in [false, true] {
        let mut writer = encode(params, &samples, Options::default()).expect("encodes");
        writer.block(6, &picture);
        let file = if ogg {
            writer.finish_ogg(9, 4096)
        } else {
            writer.finish()
        };
        let (opened, metering) = metered(|| {
            let mut input = file.as_slice();
            PcmSource::open(&mut input, &limits).map(|_| ())
        });
        assert_eq!(opened, Ok(()));
        assert!(
            metering.peak < 256 * 1024,
            "{} bytes held stepping over a picture of {PICTURE}, ogg {ogg}",
            metering.peak
        );
    }
}
