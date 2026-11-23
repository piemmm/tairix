use alloc::vec;
use alloc::vec::Vec;

use super::*;

fn word(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("a word"))
}

const RECT: DamageRect = DamageRect {
    x: 1,
    y: 2,
    width_px: 3,
    height_px: 4,
};

#[test]
fn each_request_is_laid_out_as_the_specification_has_it() {
    let create = resource_create_2d(7, Format::B8G8R8X8, 640, 480);
    assert_eq!(create.as_bytes().len(), 40);
    assert_eq!(word(create.as_bytes(), 0), 0x0101);
    assert_eq!(
        [24, 28, 32, 36].map(|at| word(create.as_bytes(), at)),
        [7, 2, 640, 480]
    );
    let scanout = set_scanout(0, 7, &RECT);
    assert_eq!(scanout.as_bytes().len(), 48);
    assert_eq!(
        [24, 28, 32, 36, 40, 44].map(|at| word(scanout.as_bytes(), at)),
        [1, 2, 3, 4, 0, 7]
    );
    let transfer = transfer_to_host_2d(7, &RECT, 0x1_0000_0008);
    assert_eq!(transfer.as_bytes().len(), 56);
    assert_eq!(
        u64::from_le_bytes(transfer.as_bytes()[40..48].try_into().expect("an offset")),
        0x1_0000_0008
    );
    assert_eq!(word(transfer.as_bytes(), 48), 7);
    let backing = attach_backing(7, 0xDEAD_0000, 4096);
    assert_eq!(backing.as_bytes().len(), 48);
    assert_eq!([24, 28].map(|at| word(backing.as_bytes(), at)), [7, 1]);
    assert_eq!(word(backing.as_bytes(), 40), 4096);
    let flush = resource_flush(7, &RECT);
    assert_eq!(flush.as_bytes().len(), 48);
    assert_eq!(word(flush.as_bytes(), 40), 7);
    let cursor = update_cursor(0, (10, 20), 9, (1, 2));
    assert_eq!(cursor.as_bytes().len(), 56);
    assert_eq!(
        [24, 28, 32, 40, 44, 48].map(|at| word(cursor.as_bytes(), at)),
        [0, 10, 20, 9, 1, 2]
    );
    let moved = move_cursor(0, (30, 40), 9, (1, 2));
    assert_eq!(word(moved.as_bytes(), 0), 0x0301);
    assert_eq!(
        [24, 28, 32, 40, 44, 48].map(|at| word(moved.as_bytes(), at)),
        [0, 30, 40, 9, 1, 2],
        "a move names the pointer it moves"
    );
    assert_eq!(get_edid(1).as_bytes().len(), 32);
    assert_eq!(detach_backing(7).as_bytes().len(), 32);
    assert_eq!(resource_unref(7).as_bytes().len(), 32);
}

fn response(kind: u32, len: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; len];
    bytes[..4].copy_from_slice(&kind.to_le_bytes());
    bytes
}

#[test]
fn a_display_info_response_names_each_scanout_s_mode() {
    let mut bytes = response(RESP_OK_DISPLAY_INFO, DISPLAY_INFO_LEN);
    let at = HEADER_LEN;
    bytes[at + 8..at + 12].copy_from_slice(&1280u32.to_le_bytes());
    bytes[at + 12..at + 16].copy_from_slice(&800u32.to_le_bytes());
    bytes[at + 16..at + 20].copy_from_slice(&1u32.to_le_bytes());
    let scanouts = display_info(&bytes).expect("decoded");
    assert_eq!(
        scanouts[0],
        Scanout {
            width: 1280,
            height: 800,
            enabled: true
        }
    );
    assert!(!scanouts[1].enabled);
    assert_eq!(
        display_info(&bytes[..DISPLAY_INFO_LEN - 1]),
        Err(DriverError::DeviceFault),
        "a short response is the device's error"
    );
}

#[test]
fn a_refusal_is_read_for_what_it_says() {
    assert_eq!(ok_nodata(&response(RESP_OK_NODATA, HEADER_LEN)), Ok(()));
    assert_eq!(
        ok_nodata(&response(RESP_ERR_OUT_OF_MEMORY, HEADER_LEN)),
        Err(DriverError::OutOfMemory)
    );
    assert_eq!(
        ok_nodata(&response(RESP_ERR_INVALID_RESOURCE_ID, HEADER_LEN)),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        ok_nodata(&response(RESP_OK_DISPLAY_INFO, HEADER_LEN)),
        Err(DriverError::DeviceFault),
        "an answer to another request"
    );
    assert_eq!(ok_nodata(&[0; 3]), Err(DriverError::DeviceFault));
    assert_eq!(
        ok_nodata(&response(RESP_OK_NODATA, HEADER_LEN)[..HEADER_LEN - 1]),
        Err(DriverError::DeviceFault),
        "every answer is a whole header"
    );
}

#[test]
fn an_edid_response_carries_no_more_than_it_holds() {
    let mut bytes = response(RESP_OK_EDID, EDID_LEN);
    bytes[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&128u32.to_le_bytes());
    bytes[HEADER_LEN + 8] = 0x00;
    bytes[HEADER_LEN + 9] = 0xFF;
    assert_eq!(edid(&bytes).expect("decoded").len(), 128);
    bytes[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&2048u32.to_le_bytes());
    assert_eq!(edid(&bytes), Err(DriverError::DeviceFault));
}

#[test]
fn a_fenced_request_asks_for_its_answer_once_it_has_run() {
    let mut header = [0xAA; HEADER_LEN];
    fence(&mut header, 0x0102_0304_0506_0708);
    assert_eq!(
        word(&header, 4),
        1,
        "VIRTIO_GPU_FLAG_FENCE and no other flag"
    );
    assert_eq!(
        u64::from_le_bytes(header[8..16].try_into().expect("a fence")),
        0x0102_0304_0506_0708
    );
    assert_eq!(header[..4], [0xAA; 4], "the type is left as it was");
    assert_eq!(header[16..], [0xAA; 8], "the context is left as it was");
}

#[test]
fn only_an_answer_carrying_its_fence_says_the_command_has_run() {
    let mut answer = [0u8; HEADER_LEN];
    answer[..4].copy_from_slice(&0x1100u32.to_le_bytes());
    assert_eq!(
        fenced(&answer, 9),
        Err(DriverError::DeviceFault),
        "no fence"
    );
    answer[4..8].copy_from_slice(&1u32.to_le_bytes());
    answer[8..16].copy_from_slice(&8u64.to_le_bytes());
    assert_eq!(
        fenced(&answer, 9),
        Err(DriverError::DeviceFault),
        "another fence"
    );
    answer[8..16].copy_from_slice(&9u64.to_le_bytes());
    assert_eq!(fenced(&answer, 9), Ok(()));
    assert_eq!(
        fenced(&answer[..12], 9),
        Err(DriverError::DeviceFault),
        "cut short"
    );
}

fn word64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("a word"))
}

#[test]
fn each_3d_request_is_laid_out_as_the_specification_has_it() {
    let info = get_capset_info(2);
    assert_eq!(info.as_bytes().len(), 32);
    assert_eq!([0, 24].map(|at| word(info.as_bytes(), at)), [0x0108, 2]);
    let capset = get_capset(CAPSET_VENUS, 3);
    assert_eq!(capset.as_bytes().len(), 32);
    assert_eq!(
        [0, 24, 28].map(|at| word(capset.as_bytes(), at)),
        [0x0109, 4, 3]
    );
    let create = ctx_create(9, CAPSET_VENUS, b"venus");
    let bytes = create.as_bytes();
    assert_eq!(bytes.len(), 96);
    assert_eq!([0, 16, 24, 28].map(|at| word(bytes, at)), [0x0200, 9, 5, 4]);
    assert_eq!(&bytes[32..37], b"venus");
    assert!(bytes[37..].iter().all(|&byte| byte == 0));
    let long = ctx_create(9, CAPSET_VENUS, &[b'n'; 100]);
    assert_eq!(word(long.as_bytes(), 24), 64, "a name is cut at its field");
    let destroy = ctx_destroy(9);
    assert_eq!(destroy.as_bytes().len(), 24);
    assert_eq!([0, 16].map(|at| word(destroy.as_bytes(), at)), [0x0201, 9]);
    let spec = BlobSpec {
        blob_id: 0x1122_3344_5566_7788,
        size: 0x2_0000_0000,
        mappable: true,
        shareable: true,
    };
    let blob = resource_create_blob(9, 0x1_0005, &spec);
    let bytes = blob.as_bytes();
    assert_eq!(bytes.len(), 56);
    assert_eq!(
        [0, 16, 24, 28, 32, 36].map(|at| word(bytes, at)),
        [0x010C, 9, 0x1_0005, 2, 3, 0],
        "host memory, mappable and shareable, no guest entries"
    );
    assert_eq!(
        [40, 48].map(|at| word64(bytes, at)),
        [spec.blob_id, spec.size]
    );
    let plain = resource_create_blob(
        9,
        7,
        &BlobSpec {
            mappable: false,
            shareable: false,
            ..spec
        },
    );
    assert_eq!(word(plain.as_bytes(), 32), 0);
    let map = resource_map_blob(0x1_0005, 0x40_1000);
    assert_eq!(map.as_bytes().len(), 40);
    assert_eq!(
        [0, 24].map(|at| word(map.as_bytes(), at)),
        [0x0208, 0x1_0005]
    );
    assert_eq!(word64(map.as_bytes(), 32), 0x40_1000);
    let unmap = resource_unmap_blob(0x1_0005);
    assert_eq!(unmap.as_bytes().len(), 32);
    assert_eq!(
        [0, 24].map(|at| word(unmap.as_bytes(), at)),
        [0x0209, 0x1_0005]
    );
    let submit = submit_3d(9, 128);
    assert_eq!(submit.as_bytes().len(), 32);
    assert_eq!(
        [0, 16, 24, 28].map(|at| word(submit.as_bytes(), at)),
        [0x0207, 9, 128, 0]
    );
}

#[test]
fn a_ring_fence_names_its_ring_beside_the_fence() {
    let mut header = [0xAA; HEADER_LEN];
    fence_on_ring(&mut header, 77, 12);
    assert_eq!(word(&header, 4), 3, "VIRTIO_GPU_FLAG_FENCE and the ring's");
    assert_eq!(word64(&header, 8), 77);
    assert_eq!(header[20], 12);
    assert_eq!(header[16..20], [0xAA; 4], "the context is left as it was");
}

#[test]
fn each_3d_answer_is_read_fail_closed() {
    let mut info = response(RESP_OK_CAPSET_INFO, CAPSET_INFO_LEN);
    for (at, value) in [(24, 4u32), (28, 1), (32, 160)] {
        info[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }
    assert_eq!(
        capset_info(&info),
        Ok(CapsetInfo {
            id: 4,
            max_version: 1,
            max_size: 160
        })
    );
    assert_eq!(
        capset_info(&info[..CAPSET_INFO_LEN - 5]),
        Err(DriverError::DeviceFault)
    );
    assert_eq!(
        capset_info(&response(RESP_OK_NODATA, CAPSET_INFO_LEN)),
        Err(DriverError::DeviceFault)
    );
    let mut capset_bytes = response(RESP_OK_CAPSET, HEADER_LEN + 8);
    capset_bytes[HEADER_LEN..].copy_from_slice(&[9; 8]);
    assert_eq!(capset(&capset_bytes, 8), Ok(&[9u8; 8][..]));
    assert_eq!(capset(&capset_bytes, 9), Err(DriverError::DeviceFault));
    let mut map = response(RESP_OK_MAP_INFO, MAP_INFO_LEN);
    for (stated, caching) in [
        (1u32, Some(RegionCaching::WriteBack)),
        (2, Some(RegionCaching::Uncached)),
        (3, Some(RegionCaching::WriteCombine)),
        (0x13, Some(RegionCaching::WriteCombine)),
        (0, None),
        (4, None),
    ] {
        map[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&stated.to_le_bytes());
        assert_eq!(map_info(&map), Ok(caching), "{stated}");
    }
    assert_eq!(
        map_info(&map[..HEADER_LEN]),
        Ok(None),
        "a mapping whose caching was not written"
    );
    assert_eq!(
        map_info(&response(RESP_ERR_INVALID_RESOURCE_ID, MAP_INFO_LEN)),
        Err(DriverError::NotFound)
    );
    assert_eq!(
        ok_nodata(&response(RESP_ERR_INVALID_CONTEXT_ID, HEADER_LEN)),
        Err(DriverError::NotFound)
    );
}
