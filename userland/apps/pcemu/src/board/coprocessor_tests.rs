use super::*;
use crate::machine::Template;
use crate::snapshot::{In, Out, State};

fn fitted(template: Template) -> Board {
    let mut config = template.config();
    config.coprocessor = true;
    Board::new(&config).unwrap()
}

/// The ESC `D8+group modrm` at cycle `at`, its operand at `operand`.
fn escape(board: &mut Board, (group, modrm): (u8, u8), operand: Option<u32>, at: u64) {
    let operand = operand.map(|addr| (addr, word(board.peek(addr), board.peek(addr + 1))));
    let escape = Escape {
        opcode: (u16::from(group) << 8) | u16::from(modrm),
        address: 0x7C00,
        operand,
    };
    board.escape(escape, at);
}

fn put(board: &mut Board, addr: u32, bytes: &[u8]) {
    for (n, &byte) in (0..).zip(bytes) {
        board.write(addr + n, byte, 0);
    }
}

fn get(board: &mut Board, addr: u32, len: u32) -> Vec<u8> {
    (0..len).map(|n| board.peek(addr + n)).collect()
}

/// Lets the board's time reach CPU cycle `cycle`.
fn reach(board: &mut Board, cycle: u64) {
    let t = board.time_of(cycle);
    board.advance_to(t);
}

const FLD_M64: (u8, u8) = (5, 0x06);
const FSTP_M64: (u8, u8) = (5, 0x1E);

#[test]
fn an_empty_socket_holds_test_high_and_takes_no_escape() {
    let mut board = Board::new(&Template::IbmPc.config()).unwrap();
    assert_eq!(board.test_low_at(100), u64::MAX);
    put(&mut board, 0x500, &2.0f64.to_le_bytes());
    escape(&mut board, FLD_M64, Some(0x500), 0);
    assert!(!board.schedule_changed(), "nothing took it");
}

#[test]
fn an_instruction_ends_at_a_board_event_its_store_landing_then() {
    let mut board = fitted(Template::IbmXt);
    assert_eq!(board.test_low_at(10), 10, "TEST low with nothing to do");
    put(&mut board, 0x500, &2.5f64.to_le_bytes());
    escape(&mut board, FLD_M64, Some(0x500), 1_000);
    assert!(board.schedule_changed());
    assert_eq!(board.next_event(), Some(board.time_of(1_046)));
    assert_eq!(board.test_low_at(1_000), 1_046);
    reach(&mut board, 1_046);
    assert_eq!(board.test_low_at(1_046), 1_046);
    escape(&mut board, FSTP_M64, Some(0x600), 2_000);
    reach(&mut board, 2_101);
    assert_eq!(
        get(&mut board, 0x600, 8),
        [0; 8],
        "not before FSTP's 102 clocks end"
    );
    reach(&mut board, 2_102);
    assert_eq!(get(&mut board, 0x600, 8), 2.5f64.to_le_bytes());
}

#[test]
fn its_transfers_hold_the_cpu_off_the_bus() {
    // On an 8088 every byte past the dummy read's two is a bus cycle.
    let mut board = fitted(Template::IbmXt);
    put(&mut board, 0x500, &1.0f64.to_le_bytes());
    board.stolen = 0;
    escape(&mut board, FLD_M64, Some(0x500), 0);
    assert_eq!(board.stolen, EXCHANGE_CYCLES + 6 * u64::from(BUS_CYCLE));
    // On an 8086, a word.
    let mut config = Template::IbmXt.config();
    config.cpu = CpuModel::I8086;
    config.coprocessor = true;
    let mut board = Board::new(&config).unwrap();
    put(&mut board, 0x500, &1.0f64.to_le_bytes());
    board.stolen = 0;
    escape(&mut board, FLD_M64, Some(0x500), 0);
    assert_eq!(board.stolen, EXCHANGE_CYCLES + 3 * u64::from(BUS_CYCLE));
}

/// Raises an unmasked zero divide, interrupts enabled, its end reached.
fn zero_divide(board: &mut Board) {
    // FLDCW: zero divide unmasked, IEM clear.
    put(board, 0x500, &0x037Bu16.to_le_bytes());
    escape(board, (1, 0x2E), Some(0x500), 0);
    escape(board, (1, 0xE8), None, 100);
    escape(board, (1, 0xEE), None, 200);
    escape(board, (4, 0xF9), None, 300);
    reach(board, 1_000);
}

#[test]
fn an_unmasked_exception_reaches_nmi_through_the_mask_once_an_edge() {
    let mut board = fitted(Template::IbmXt);
    zero_divide(&mut board);
    assert!(!board.nmi(), "the mask is clear from reset");
    assert_eq!(
        board.test_low_at(1_000),
        u64::MAX,
        "BUSY held by the interrupt"
    );
    board.output(0xA0, 0x80, 1_000);
    assert!(board.nmi());
    board.acknowledge_nmi();
    assert!(!board.nmi());
    // INT still raised: no new edge until the line falls and rises.
    board.output(0xA0, 0x80, 1_010);
    assert!(!board.nmi());
    board.output(0xA0, 0x00, 1_020);
    board.output(0xA0, 0x80, 1_030);
    assert!(board.nmi());
    // FNCLEX drops INT and BUSY.
    board.acknowledge_nmi();
    escape(&mut board, (3, 0xE2), None, 1_100);
    assert_eq!(board.test_low_at(1_100), 1_100);
}

#[test]
fn a_switch_off_coprocessor_cannot_interrupt() {
    // The switch that reports the 8087 also lets its INT reach NMI: with
    // none fitted, nothing on the board raises NMI.
    let mut board = Board::new(&Template::IbmXt.config()).unwrap();
    zero_divide(&mut board);
    board.output(0xA0, 0x80, 1_000);
    assert!(!board.nmi());
}

#[test]
fn the_80186_halts_its_dma_as_it_takes_nmi_until_the_iret() {
    let mut board = fitted(Template::Workstation186);
    zero_divide(&mut board);
    board.output(0xA0, 0x80, 1_000);
    assert!(board.nmi());
    assert_eq!(board.input_word(0xFF30, 1_000) & 0x8000, 0);
    board.acknowledge_nmi();
    assert_eq!(board.input_word(0xFF30, 1_000) & 0x8000, 0x8000, "DHLT set");
    board.interrupt_return();
    assert_eq!(
        board.input_word(0xFF30, 1_000) & 0x8000,
        0,
        "and cleared by IRET"
    );
}

#[test]
fn a_snapshot_keeps_the_mask_the_latch_and_the_instruction_in_flight() {
    let config = Template::TurboXt.config();
    let mut board = Board::new(&config).unwrap();
    zero_divide(&mut board);
    board.output(0xA0, 0x80, 1_000);
    put(&mut board, 0x500, &4.0f64.to_le_bytes());
    escape(&mut board, (3, 0xE2), None, 1_100);
    escape(&mut board, FLD_M64, Some(0x500), 1_200);
    let mut out = Out::refusable();
    board.save(&mut out);
    let bytes = out.finish().unwrap();
    let mut twin = Board::new(&config).unwrap();
    twin.restore(&mut In::new(&bytes)).unwrap();
    assert_eq!(twin.nmi(), board.nmi());
    assert_eq!(twin.test_low_at(1_200), 1_246);
    reach(&mut twin, 1_246);
    escape(&mut twin, FSTP_M64, Some(0x600), 1_300);
    reach(&mut twin, 1_402);
    assert_eq!(get(&mut twin, 0x600, 8), 4.0f64.to_le_bytes());
}

#[test]
fn the_coprocessor_stays_in_its_socket_across_catch_ups() {
    let mut board = fitted(Template::IbmXt);
    // Time passing with nothing in flight leaves the 8087 fitted.
    reach(&mut board, 1_000_000);
    assert_eq!(board.test_low_at(1_000_000), 1_000_000);
    put(&mut board, 0x500, &1.0f64.to_le_bytes());
    escape(&mut board, FLD_M64, Some(0x500), 1_000_000);
    assert_eq!(board.test_low_at(1_000_000), 1_000_046);
}
