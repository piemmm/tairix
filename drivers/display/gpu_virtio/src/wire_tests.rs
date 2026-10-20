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
