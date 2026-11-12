use alloc::vec::Vec;

use super::*;
use crate::board::BoardConfig;
use crate::cpu::jit::page::{PAGES, PAGE_BITS, PAGE_BYTES};
use crate::cpu::{Bus, CpuModel};
use crate::machine::Template;

fn board(config: &BoardConfig) -> Board {
    Board::new(config).unwrap()
}

/// The page map made whole, from nothing.
fn whole(board: &Board) -> PageMap {
    let mut pages = PageMap::default();
    for page in 0..PAGES {
        let addr = u32::try_from(page).unwrap() << PAGE_BITS;
        entry(&mut pages, &board.planar.memory, &board.waits, addr);
    }
    pages
}

/// The entry of the page holding `addr`: its base, waits, and whether it
/// takes a store.
fn page_entry(pages: &PageMap, addr: u32) -> (u64, u8, bool) {
    let at = (addr >> PAGE_BITS) as usize * crate::cpu::jit::page::ENTRY_BYTES;
    let load = |offset: usize, width: usize| pages.load(at + offset, width).unwrap();
    (
        load(0, 8),
        u8::try_from(load(8, 1)).unwrap(),
        load(9, 1) & u64::from(crate::cpu::jit::page::WRITABLE) != 0,
    )
}

fn kept(board: &mut Board) -> PageMap {
    board.update_page_map();
    board.pages.clone()
}

#[test]
fn the_page_map_kept_as_the_memory_changes_is_the_one_made_whole() {
    let mut rng = tairix_fuzzseed::Prng::new(0xB10C_0001);
    for template in [Template::TurboXt, Template::Workstation186] {
        let mut b = board(&template.config());
        assert_eq!(kept(&mut b), whole(&b), "{template:?} as built");
        let mut relocated = false;
        for _ in 0..300 {
            match rng.below(7) {
                0 => {
                    // An expanded-memory window shows a page, or nothing.
                    let window = u16::try_from(rng.below(4)).unwrap();
                    b.output(0x258 + 2 * window, u8::try_from(rng.below(128)).unwrap(), 0);
                    b.output(
                        0x259 + 2 * window,
                        if rng.below(4) == 0 { 0 } else { 0x80 },
                        0,
                    );
                }
                1 | 2 => {
                    let addr = u32::try_from(rng.below(1 << 20)).unwrap();
                    if let Some(granule) = b.planar.memory.granule(addr) {
                        if rng.below(2) == 0 {
                            b.planar.memory.watch(granule);
                        } else {
                            b.planar.memory.unwatch(granule);
                        }
                    }
                }
                3 => {
                    let addr = u32::try_from(rng.below(1 << 20)).unwrap();
                    b.write(addr, 0x5A, 0);
                }
                4 if template == Template::Workstation186 && !relocated => {
                    // The lower memory chip select's wait states.
                    b.output_word(0xFFA2, 0x0038 | u16::try_from(rng.below(4)).unwrap(), 0);
                }
                5 if template == Template::Workstation186 && !relocated => {
                    // The control block moved into memory, over a page.
                    b.output_word(0xFFFE, 0x1000 | u16::try_from(rng.below(0x100)).unwrap(), 0);
                    relocated = true;
                }
                _ => {
                    b.planar.memory.take_stale();
                }
            }
            assert_eq!(kept(&mut b), whole(&b), "{template:?}");
        }
    }
}

#[test]
fn a_page_reaches_the_bytes_its_addresses_show() {
    let mut b = board(&Template::TurboXt.config());
    b.planar
        .memory
        .map_rom(0xF_E000, &[0xEA; 16], false)
        .unwrap();
    b.output(0x258, 7, 0);
    b.output(0x259, 0x80, 0);
    b.update_page_map();
    let store = b.planar.memory.host_address(0);
    for addr in [0x0000, 0x1234, 0x9_FFFF, 0xD_0000, 0xD_3FFF] {
        let (base, _, writable) = page_entry(&b.pages, addr);
        let offset = b.planar.memory.backing(addr).unwrap();
        assert_eq!(
            base + u64::from(addr % PAGE_BYTES) - store,
            u64::from(offset),
            "{addr:05X}"
        );
        assert!(writable, "{addr:05X}");
    }
    let (base, _, _) = page_entry(&b.pages, 0xD_4000);
    assert_eq!(base, 0, "a window showing nothing");
    let (base, _, writable) = page_entry(&b.pages, 0xF_E000);
    assert!(base != 0 && !writable, "ROM");
    let video = b.planar.cards[0].window().0;
    assert_eq!(page_entry(&b.pages, video).0, 0, "a card's memory");
}

#[test]
fn a_watched_page_takes_no_store_from_translated_code() {
    let mut b = board(&Template::IbmPc.config());
    let addr = 0x1_2345;
    let granule = b.planar.memory.granule(addr).unwrap();
    b.planar.memory.watch(granule);
    b.update_page_map();
    assert!(!page_entry(&b.pages, addr).2);
    assert!(page_entry(&b.pages, addr + PAGE_BYTES).2, "the next page");
    b.write(addr, 1, 0);
    assert_eq!(b.planar.memory.take_stale(), [granule]);
    b.update_page_map();
    assert!(
        page_entry(&b.pages, addr).2,
        "written, so no longer watched"
    );
}

#[test]
fn chip_selects_with_waits_that_vary_within_a_kibibyte_hide_it() {
    let mut b = board(&BoardConfig {
        cpu: CpuModel::I80186,
        ..Template::Workstation186.config()
    });
    b.planar.memory.map_rom(0xF_F000, &[0; 16], false).unwrap();
    b.update_page_map();
    // The upper chip select covers the top kibibyte with three waits from
    // reset; the rest of the space has none.
    assert_eq!(page_entry(&b.pages, 0xF_FC00).1, 3);
    assert_eq!(page_entry(&b.pages, 0x0_0000).1, 0);
    let mixed: Vec<usize> = (0..KIBS).filter(|&kib| b.waits[kib] == MIXED).collect();
    for kib in mixed {
        let addr = u32::try_from(kib).unwrap() << 10;
        assert_eq!(page_entry(&b.pages, addr).0, 0, "{addr:05X}");
    }
}
