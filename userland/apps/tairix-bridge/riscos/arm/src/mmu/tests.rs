use super::*;
use crate::cpu::{Cpu, Exception, Stop};
use crate::profile::BIT32;
use crate::psr::{self, Mode};
use crate::reg::Reg;

const RW: Access = Access {
    user: Rights::ReadWrite,
    privileged: Rights::ReadWrite,
    execute: true,
};
const READ_ONLY: Access = Access {
    user: Rights::Read,
    privileged: Rights::Read,
    execute: false,
};
const PRIVILEGED: Access = Access {
    user: Rights::None,
    privileged: Rights::ReadWrite,
    execute: false,
};
const DISPLAY: Watcher = match Watcher::new(0) {
    Some(watcher) => watcher,
    None => panic!(),
};
const CODE: Watcher = match Watcher::new(1) {
    Some(watcher) => watcher,
    None => panic!(),
};

/// An MMU with one zeroed frame mapped globally at `address`.
fn mapped(address: u32, access: Access) -> (Mmu, Frame) {
    let mut mmu = Mmu::new(64).unwrap();
    let frame = mmu.allocate(FrameKind::Memory).unwrap();
    mmu.map(Table::Global, address, frame, access).unwrap();
    mmu.release(frame).unwrap();
    (mmu, frame)
}

#[test]
fn frames_are_taken_lowest_first_zeroed_and_under_the_limit() {
    let mut mmu = Mmu::new(3).unwrap();
    let a = mmu.allocate(FrameKind::Memory).unwrap();
    let b = mmu.allocate(FrameKind::Memory).unwrap();
    assert_eq!((a.number(), b.number()), (0, 1));
    mmu.words_mut(a).unwrap()[5] = 0xDEAD_BEEF;
    mmu.release(a).unwrap();
    assert!(mmu.is_free(a), "nothing mapped it");
    assert_eq!(
        mmu.allocate(FrameKind::Memory),
        Ok(a),
        "the lowest free again"
    );
    assert_eq!(mmu.words(a).unwrap()[5], 0, "and zeroed");
    assert_eq!(mmu.allocate_at(Frame::new(2), FrameKind::Memory), Ok(()));
    assert_eq!(mmu.used(), 3);
    assert_eq!(mmu.allocate(FrameKind::Memory), Err(MmuError::Exhausted));
    assert_eq!(
        mmu.allocate_at(b, FrameKind::Memory),
        Err(MmuError::Frame),
        "in use"
    );
    mmu.set_limit(2);
    mmu.release(b).unwrap();
    assert_eq!(
        mmu.allocate(FrameKind::Memory),
        Err(MmuError::Exhausted),
        "still at the lowered limit"
    );
    mmu.set_limit(8);
    assert_eq!(
        mmu.allocate_at(Frame::new(6), FrameKind::Memory),
        Ok(()),
        "a number past every slot"
    );
    assert_eq!(
        mmu.allocate(FrameKind::Memory),
        Ok(b),
        "the gap below it is free"
    );
    assert_eq!(
        mmu.allocate_at(Frame::new(8), FrameKind::Memory),
        Err(MmuError::Exhausted),
        "past the limit"
    );
}

#[test]
fn a_frame_lives_while_held_or_mapped() {
    let mut mmu = Mmu::new(8).unwrap();
    let frame = mmu.allocate(FrameKind::Memory).unwrap();
    mmu.map(Table::Global, 0x1000, frame, RW).unwrap();
    mmu.map(Table::Global, 0x2000, frame, RW).unwrap();
    assert_eq!(mmu.mappings(frame), Some(2));
    mmu.release(frame).unwrap();
    assert_eq!(mmu.release(frame), Err(MmuError::Frame), "not held now");
    mmu.write(0x1004, Width::Word, 7, true).unwrap();
    assert_eq!(
        mmu.read(0x2004, Width::Word, true),
        Ok(7),
        "one frame, two pages"
    );
    assert_eq!(mmu.unmap(Table::Global, 0x1000), Ok(frame));
    assert!(!mmu.is_free(frame), "still mapped once");
    mmu.hold(frame).unwrap();
    assert_eq!(mmu.unmap(Table::Global, 0x2000), Ok(frame));
    assert!(!mmu.is_free(frame), "held");
    mmu.release(frame).unwrap();
    assert!(mmu.is_free(frame));
    assert_eq!(mmu.used(), 0);
    assert_eq!(
        mmu.map(Table::Global, 0x1000, frame, RW),
        Err(MmuError::Frame)
    );
}

#[test]
fn mappings_are_refused_where_they_cannot_go() {
    let mut mmu = Mmu::new(8).unwrap();
    let frame = mmu.allocate(FrameKind::Memory).unwrap();
    let space = mmu.create_space(0x8000).unwrap();
    assert_eq!(
        mmu.map(Table::Global, 0x1001, frame, RW),
        Err(MmuError::Placement)
    );
    assert_eq!(
        mmu.map(Table::Global, 0x4000, frame, RW),
        Err(MmuError::Placement),
        "below a space's extent"
    );
    assert_eq!(
        mmu.map(Table::Space(space), 0x8000, frame, RW),
        Err(MmuError::Placement),
        "at its extent"
    );
    mmu.map(Table::Space(space), 0x7000, frame, RW).unwrap();
    assert_eq!(
        mmu.map(Table::Space(space), 0x7000, frame, RW),
        Err(MmuError::Occupied)
    );
    assert_eq!(
        mmu.unmap(Table::Space(space), 0x6000),
        Err(MmuError::Unmapped)
    );
    assert_eq!(
        mmu.protect(Table::Global, 0x9000, RW),
        Err(MmuError::Unmapped)
    );
    let gone = mmu.create_space(0).unwrap();
    mmu.destroy_space(gone).unwrap();
    assert_eq!(
        mmu.map(Table::Space(gone), 0, frame, RW),
        Err(MmuError::Space)
    );
}

#[test]
fn a_host_frame_is_never_writable_and_fetches_as_host_code() {
    let mut mmu = Mmu::new(8).unwrap();
    let host = mmu.allocate(FrameKind::Host).unwrap();
    assert_eq!(
        mmu.map(Table::Global, 0x1_0000, host, RW),
        Err(MmuError::HostWritable)
    );
    let executable = Access {
        execute: true,
        ..READ_ONLY
    };
    mmu.map(Table::Global, 0x1_0000, host, executable).unwrap();
    assert_eq!(
        mmu.protect(Table::Global, 0x1_0000, PRIVILEGED),
        Err(MmuError::HostWritable)
    );
    mmu.words_mut(host).unwrap()[1] = 0xE7F0_00F0;
    assert_eq!(
        mmu.fetch(0x1_0004, false),
        Ok(Fetch {
            word: 0xE7F0_00F0,
            host: true,
            page: host.number(),
        })
    );
    assert_eq!(
        mmu.write(0x1_0004, Width::Word, 0, true),
        Err(Abort::Permission)
    );
    let (mut plain, _) = mapped(0x2_0000, RW);
    assert!(!plain.fetch(0x2_0000, true).unwrap().host);
}

#[test]
fn each_privilege_gets_exactly_what_its_page_grants() {
    let cases = [
        (RW, [true; 6]),
        (READ_ONLY, [true, false, false, true, false, false]),
        (PRIVILEGED, [false, false, false, true, true, false]),
        (
            Access {
                user: Rights::Read,
                privileged: Rights::ReadWrite,
                execute: true,
            },
            [true, false, true, true, true, true],
        ),
        (
            Access {
                user: Rights::None,
                privileged: Rights::Read,
                execute: true,
            },
            [false, false, false, true, false, true],
        ),
    ];
    for (access, [user_read, user_write, user_fetch, read, write, fetch]) in cases {
        let (mut mmu, _) = mapped(0x3000, access);
        let ok = |result: Result<(), Abort>| match result {
            Ok(()) => true,
            Err(abort) => {
                assert_eq!(abort, Abort::Permission, "{access:?}");
                false
            }
        };
        assert_eq!(
            ok(mmu.read(0x3000, Width::Word, false).map(drop)),
            user_read,
            "{access:?}"
        );
        assert_eq!(
            ok(mmu.write(0x3000, Width::Word, 1, false)),
            user_write,
            "{access:?}"
        );
        assert_eq!(
            ok(mmu.fetch(0x3000, false).map(drop)),
            user_fetch,
            "{access:?}"
        );
        assert_eq!(
            ok(mmu.read(0x3000, Width::Word, true).map(drop)),
            read,
            "{access:?}"
        );
        assert_eq!(
            ok(mmu.write(0x3000, Width::Word, 1, true)),
            write,
            "{access:?}"
        );
        assert_eq!(ok(mmu.fetch(0x3000, true).map(drop)), fetch, "{access:?}");
        assert_eq!(
            mmu.lookup(Table::Global, 0x3FFC).map(|(_, a)| a),
            Some(access)
        );
    }
}

#[test]
fn an_unmapped_page_is_a_translation_abort_and_a_protected_one_follows_its_change() {
    let (mut mmu, frame) = mapped(0x5000, RW);
    assert_eq!(mmu.read(0x6000, Width::Byte, true), Err(Abort::Translation));
    assert_eq!(mmu.fetch(0x4FFC, true), Err(Abort::Translation));
    mmu.write(0x5000, Width::Word, 1, false).unwrap();
    mmu.protect(Table::Global, 0x5000, READ_ONLY).unwrap();
    assert_eq!(
        mmu.write(0x5000, Width::Word, 2, false),
        Err(Abort::Permission),
        "the TLB forgot the old access"
    );
    assert_eq!(mmu.read(0x5000, Width::Word, false), Ok(1));
    assert_eq!(mmu.unmap(Table::Global, 0x5000), Ok(frame));
    assert_eq!(
        mmu.read(0x5000, Width::Word, false),
        Err(Abort::Translation),
        "nor serves a page gone"
    );
}

#[test]
fn narrow_accesses_reach_their_lanes_little_endian() {
    let (mut mmu, _) = mapped(0x7000, RW);
    mmu.write(0x7000, Width::Word, 0x4433_2211, true).unwrap();
    mmu.write(0x7005, Width::Byte, 0x1AB, true).unwrap();
    mmu.write(0x7006, Width::Half, 0x1_BEEF, true).unwrap();
    assert_eq!(mmu.read(0x7001, Width::Byte, true), Ok(0x22));
    assert_eq!(mmu.read(0x7002, Width::Half, true), Ok(0x4433));
    assert_eq!(mmu.read(0x7004, Width::Word, true), Ok(0xBEEF_AB00));
}

#[test]
fn a_space_answers_below_its_extent_and_a_switch_brings_another() {
    let mut mmu = Mmu::new(16).unwrap();
    let first = mmu.current();
    let a = mmu.create_space(0x10_0000).unwrap();
    let b = mmu.create_space(0x2_0000).unwrap();
    for (space, value) in [(a, 0xAAAA), (b, 0xBBBB)] {
        let frame = mmu.allocate(FrameKind::Memory).unwrap();
        mmu.words_mut(frame).unwrap()[0] = value;
        mmu.map(Table::Space(space), 0x8000, frame, RW).unwrap();
        mmu.release(frame).unwrap();
    }
    let global = mmu.allocate(FrameKind::Memory).unwrap();
    mmu.words_mut(global).unwrap()[0] = 0x6666;
    mmu.map(Table::Global, 0x10_0000, global, RW).unwrap();
    mmu.release(global).unwrap();

    assert_eq!(
        mmu.read(0x8000, Width::Word, false),
        Err(Abort::Translation),
        "the first space maps nothing"
    );
    mmu.switch(a).unwrap();
    assert_eq!(mmu.read(0x8000, Width::Word, false), Ok(0xAAAA));
    assert_eq!(
        mmu.read(0x10_0000, Width::Word, false),
        Ok(0x6666),
        "global above"
    );
    mmu.switch(b).unwrap();
    assert_eq!(
        mmu.read(0x8000, Width::Word, false),
        Ok(0xBBBB),
        "the TLB told them apart"
    );
    mmu.switch(a).unwrap();
    assert_eq!(mmu.read(0x8000, Width::Word, false), Ok(0xAAAA));
    assert_eq!(mmu.table_of(0xF_FFFF), Table::Space(a));
    assert_eq!(mmu.table_of(0x10_0000), Table::Global);
    assert_eq!(mmu.destroy_space(a), Err(MmuError::Space), "current");
    mmu.switch(first).unwrap();
    let used = mmu.used();
    mmu.destroy_space(a).unwrap();
    assert_eq!(mmu.used(), used - 1, "its page freed");
    assert_eq!(mmu.switch(a), Err(MmuError::Space));
    let reused = mmu.create_space(0x8000).unwrap();
    mmu.switch(reused).unwrap();
    assert_eq!(
        mmu.read(0x8000, Width::Word, false),
        Err(Abort::Translation),
        "a space made in a destroyed one's place sees none of its pages"
    );
}

#[test]
fn an_extent_moves_only_over_nothing_it_would_hide() {
    let mut mmu = Mmu::new(16).unwrap();
    let space = mmu.create_space(0x8000).unwrap();
    let frame = mmu.allocate(FrameKind::Memory).unwrap();
    mmu.map(Table::Global, 0x40_0000, frame, RW).unwrap();
    assert_eq!(
        mmu.set_extent(space, 0x40_1000),
        Err(MmuError::Placement),
        "over a global page"
    );
    assert_eq!(mmu.create_space(0x80_0000), Err(MmuError::Placement));
    mmu.set_extent(space, 0x40_0000).unwrap();
    let own = mmu.allocate(FrameKind::Memory).unwrap();
    mmu.map(Table::Space(space), 0x3F_F000, own, RW).unwrap();
    assert_eq!(
        mmu.set_extent(space, 0x3F_F000),
        Err(MmuError::Occupied),
        "over its own page"
    );
    mmu.switch(space).unwrap();
    mmu.write(0x3F_F000, Width::Word, 3, false).unwrap();
    mmu.unmap(Table::Space(space), 0x3F_F000).unwrap();
    mmu.set_extent(space, 0x1000).unwrap();
    assert_eq!(
        mmu.read(0x3F_F000, Width::Word, false),
        Err(Abort::Translation),
        "above the extent now, where nothing global is"
    );
    assert_eq!(mmu.set_extent(space, 0x1001), Err(MmuError::Placement));
}

#[test]
fn a_global_page_seen_from_one_space_is_not_served_to_another_that_covers_it() {
    let mut mmu = Mmu::new(16).unwrap();
    let low = mmu.create_space(0x1000).unwrap();
    let high = mmu.create_space(0x1000).unwrap();
    let global = mmu.allocate(FrameKind::Memory).unwrap();
    mmu.words_mut(global).unwrap()[0] = 1;
    mmu.map(Table::Global, 0x2000, global, RW).unwrap();
    mmu.switch(low).unwrap();
    assert_eq!(mmu.read(0x2000, Width::Word, false), Ok(1));
    mmu.unmap(Table::Global, 0x2000).unwrap();
    mmu.set_extent(high, 0x3000).unwrap();
    let own = mmu.allocate(FrameKind::Memory).unwrap();
    mmu.words_mut(own).unwrap()[0] = 2;
    mmu.map(Table::Space(high), 0x2000, own, RW).unwrap();
    mmu.switch(high).unwrap();
    assert_eq!(mmu.read(0x2000, Width::Word, false), Ok(2));
    mmu.switch(low).unwrap();
    assert_eq!(
        mmu.read(0x2000, Width::Word, false),
        Err(Abort::Translation),
        "the other space's page is not the low one's"
    );
}

#[test]
fn a_store_into_a_watched_granule_reports_once_until_taken() {
    let (mut mmu, frame) = mapped(0x9000, RW);
    mmu.watch(frame, 0b0110, DISPLAY).unwrap();
    mmu.write(0x9000, Width::Word, 1, true).unwrap();
    mmu.write(0x9104, Width::Byte, 2, true).unwrap();
    mmu.write(0x91FC, Width::Word, 3, true).unwrap();
    mmu.write(0x9200, Width::Half, 4, false).unwrap();
    let mut taken = Vec::new();
    mmu.take_written(DISPLAY, |frame, granules| taken.push((frame, granules)));
    assert_eq!(taken, [(frame, 0b0110)], "granule zero is not watched");
    taken.clear();
    mmu.take_written(DISPLAY, |frame, granules| taken.push((frame, granules)));
    assert!(taken.is_empty(), "nothing since");
    mmu.write(0x9200, Width::Byte, 5, true).unwrap();
    mmu.take_written(DISPLAY, |frame, granules| taken.push((frame, granules)));
    assert_eq!(taken, [(frame, 0b0100)], "watched again after the take");
    mmu.unwatch(frame, 0b0110, DISPLAY);
    mmu.write(0x9200, Width::Byte, 6, true).unwrap();
    taken.clear();
    mmu.take_written(DISPLAY, |frame, granules| taken.push((frame, granules)));
    assert!(taken.is_empty(), "unwatched");
}

#[test]
fn watchers_are_independent() {
    let (mut mmu, frame) = mapped(0xA000, RW);
    mmu.watch(frame, 0b1, DISPLAY).unwrap();
    mmu.watch(frame, 0b11, CODE).unwrap();
    mmu.write(0xA000, Width::Word, 1, true).unwrap();
    let mut display = Vec::new();
    mmu.take_written(DISPLAY, |frame, granules| display.push((frame, granules)));
    assert_eq!(display, [(frame, 0b1)]);
    mmu.write(0xA100, Width::Word, 1, true).unwrap();
    mmu.unwatch(frame, 0b11, CODE);
    let mut code = Vec::new();
    mmu.take_written(CODE, |frame, granules| code.push((frame, granules)));
    assert_eq!(
        code,
        [(frame, 0b11)],
        "a pending report outlives the unwatch"
    );
    mmu.write(0xA000, Width::Word, 2, true).unwrap();
    display.clear();
    mmu.take_written(DISPLAY, |frame, granules| display.push((frame, granules)));
    assert_eq!(
        display,
        [(frame, 0b1)],
        "the code watcher let go of only its own"
    );
}

#[test]
fn a_host_write_or_a_freed_frame_reports_every_watched_granule() {
    let (mut mmu, frame) = mapped(0xB000, RW);
    mmu.watch(frame, 0x8001, CODE).unwrap();
    let _ = mmu.words_mut(frame);
    let mut taken = Vec::new();
    mmu.take_written(CODE, |frame, granules| taken.push((frame, granules)));
    assert_eq!(taken, [(frame, 0x8001)]);
    mmu.unmap(Table::Global, 0xB000).unwrap();
    assert!(mmu.is_free(frame));
    taken.clear();
    mmu.take_written(CODE, |frame, granules| taken.push((frame, granules)));
    assert_eq!(taken, [(frame, 0x8001)], "freed under the watch");
    let again = mmu.allocate(FrameKind::Memory).unwrap();
    assert_eq!(again, frame);
    mmu.map(Table::Global, 0xB000, again, RW).unwrap();
    mmu.write(0xB000, Width::Word, 1, true).unwrap();
    taken.clear();
    mmu.take_written(CODE, |frame, granules| taken.push((frame, granules)));
    assert!(taken.is_empty(), "the new frame is nobody's watch");
    assert_eq!(mmu.watch(Frame::new(9), 1, CODE), Err(MmuError::Frame));
    assert_eq!(Watcher::new(WATCHERS), None);
}

#[test]
fn a_frame_past_the_limit_is_refused_before_anything_grows() {
    let mut mmu = Mmu::new(8).unwrap();
    let slots = mmu.store.frames.slots.len();
    let pending = mmu.store.pending.each_ref().map(BitVec::len);
    for number in [8, MOST_FRAMES, u32::MAX] {
        assert_eq!(
            mmu.allocate_at(Frame::new(number), FrameKind::Memory),
            Err(MmuError::Exhausted),
            "{number:#x}"
        );
        assert_eq!(mmu.store.frames.slots.len(), slots);
        assert_eq!(mmu.store.pending.each_ref().map(BitVec::len), pending);
    }
}

#[test]
fn no_limit_reaches_a_frame_a_table_entry_cannot_hold() {
    let mut mmu = Mmu::new(u32::MAX).unwrap();
    assert_eq!(mmu.limit(), MOST_FRAMES);
    mmu.set_limit(4);
    assert_eq!(mmu.limit(), 4);
    mmu.set_limit(u32::MAX);
    assert_eq!(mmu.limit(), MOST_FRAMES);
}

#[test]
fn a_frames_words_are_copied_and_its_copys_watchers_told() {
    let mut mmu = Mmu::new(8).unwrap();
    let from = mmu.allocate(FrameKind::Memory).unwrap();
    let to = mmu.allocate(FrameKind::Memory).unwrap();
    mmu.words_mut(from).unwrap()[3] = 0x1234;
    mmu.watch(to, 0b10, DISPLAY).unwrap();
    assert_eq!(mmu.copy_frame(from, to), Ok(()));
    assert_eq!(mmu.words(to).unwrap()[3], 0x1234);
    let mut taken = Vec::new();
    mmu.take_written(DISPLAY, |frame, granules| taken.push((frame, granules)));
    assert_eq!(taken, [(to, 0b10)]);
    mmu.words_mut(to).unwrap()[3] = 0x5678;
    assert_eq!(mmu.copy_frame(to, from), Ok(()), "either way round");
    assert_eq!(mmu.words(from).unwrap()[3], 0x5678);
    assert_eq!(mmu.copy_frame(from, from), Ok(()));
    assert_eq!(
        mmu.copy_frame(from, Frame::new(5)),
        Err(MmuError::Frame),
        "a free frame"
    );
}

#[test]
fn watches_follow_a_frames_contents_and_a_pending_report_stays() {
    let (mut mmu, from) = mapped(0x9000, RW);
    let to = mmu.allocate(FrameKind::Memory).unwrap();
    mmu.map(Table::Global, 0xA000, to, RW).unwrap();
    mmu.watch(from, 0b11, DISPLAY).unwrap();
    mmu.watch(from, 0b100, CODE).unwrap();
    mmu.write(0x9000, Width::Word, 1, true).unwrap();
    assert_eq!(mmu.move_watches(from, to), Ok(()));
    let mut taken = Vec::new();
    mmu.take_written(DISPLAY, |frame, granules| taken.push((frame, granules)));
    assert_eq!(
        taken,
        [(from, 0b1), (to, 0b11)],
        "the old report kept, all of the new one's reported"
    );
    taken.clear();
    mmu.take_written(CODE, |frame, granules| taken.push((frame, granules)));
    assert_eq!(taken, [(to, 0b100)]);
    mmu.write(0x9000, Width::Word, 2, true).unwrap();
    mmu.write(0xA000, Width::Word, 2, true).unwrap();
    taken.clear();
    mmu.take_written(DISPLAY, |frame, granules| taken.push((frame, granules)));
    assert_eq!(taken, [(to, 0b1)], "stores seen where the contents are now");
    assert_eq!(mmu.move_watches(to, Frame::new(7)), Err(MmuError::Frame));
    assert_eq!(
        mmu.move_watches(Frame::new(6), to),
        Ok(()),
        "nothing to move"
    );
    mmu.write(0xA100, Width::Word, 3, true).unwrap();
    taken.clear();
    mmu.take_written(DISPLAY, |frame, granules| taken.push((frame, granules)));
    assert_eq!(taken, [(to, 0b10)], "a refused move moved nothing");
}

#[test]
fn the_processor_runs_through_it_and_takes_its_aborts() {
    let mut mmu = Mmu::new(16).unwrap();
    let space = mmu.create_space(0x1_0000).unwrap();
    mmu.switch(space).unwrap();
    let code = mmu.allocate(FrameKind::Memory).unwrap();
    let words = mmu.words_mut(code).unwrap();
    words[0] = 0xE3A0_0042; // MOV R0, #&42
    words[1] = 0xE581_0000; // STR R0, [R1]
    words[2] = 0xE592_0000; // LDR R0, [R2]
    let user_code = Access {
        user: Rights::Read,
        privileged: Rights::Read,
        execute: true,
    };
    mmu.map(Table::Space(space), 0x8000, code, user_code)
        .unwrap();
    let data = mmu.allocate(FrameKind::Memory).unwrap();
    mmu.map(Table::Space(space), 0x9000, data, READ_ONLY)
        .unwrap();

    let mut cpu = Cpu::new(&BIT32, 4);
    cpu.set_cpsr(Mode::User.bits()).unwrap();
    cpu.set_reg(Reg::PC, 0x8000);
    cpu.set_reg(Reg::field(1), 0x9000);
    let run = cpu.run(&mut mmu, 2);
    assert_eq!(run.stop, Stop::Budget);
    assert_eq!(
        cpu.pc(),
        BIT32.vectors + Exception::DataAbort.offset(),
        "a User store to a read-only page"
    );
    assert_eq!(cpu.mode(), Mode::Abort);
    assert_eq!(cpu.reg(Reg::LR), 0x800C);

    cpu.set_cpsr(Mode::User.bits()).unwrap();
    cpu.set_reg(Reg::PC, 0x8008);
    cpu.set_reg(Reg::field(2), 0xF000);
    cpu.run(&mut mmu, 1);
    assert_eq!(
        cpu.pc(),
        BIT32.vectors + Exception::DataAbort.offset(),
        "a load from nothing"
    );

    cpu.set_cpsr(Mode::User.bits() | psr::I | psr::F).unwrap();
    cpu.set_reg(Reg::PC, 0x9000);
    cpu.run(&mut mmu, 1);
    assert_eq!(
        cpu.pc(),
        BIT32.vectors + Exception::PrefetchAbort.offset(),
        "a fetch from a page that may not be executed"
    );
}

#[test]
fn a_reservation_keeps_its_tables_until_it_is_dropped() {
    let mut mmu = Mmu::new(8).unwrap();
    mmu.reserve(Table::Global, 0x40_0000..0xC0_0000).unwrap();
    assert_eq!(mmu.tables_held(Table::Global), 2);
    mmu.reserve(Table::Global, 0x80_0000..0x80_1000).unwrap();
    let frame = mmu.allocate(FrameKind::Memory).unwrap();
    mmu.map(Table::Global, 0x40_0000, frame, RW).unwrap();
    mmu.unmap(Table::Global, 0x40_0000).unwrap();
    assert_eq!(mmu.tables_held(Table::Global), 2, "reserved though empty");
    mmu.unreserve(Table::Global, 0x40_0000..0xC0_0000);
    assert_eq!(
        mmu.tables_held(Table::Global),
        1,
        "the nested reservation stays"
    );
    mmu.unreserve(Table::Global, 0x80_0000..0x80_1000);
    assert_eq!(mmu.tables_held(Table::Global), 0);
    let space = mmu.create_space(0x1000).unwrap();
    assert_eq!(
        mmu.reserve(Table::Space(space), 0x40_0000..0x40_1000),
        Err(MmuError::Placement),
        "past the space's tables"
    );
}

#[test]
fn the_next_free_frame_is_the_lowest_at_or_past_the_start() {
    let mut mmu = Mmu::new(4).unwrap();
    let frames: Vec<Frame> = (0..3)
        .map(|_| mmu.allocate(FrameKind::Memory).unwrap())
        .collect();
    mmu.release(frames[1]).unwrap();
    assert_eq!(mmu.next_free(Frame::new(0)), Some(frames[1]));
    assert_eq!(
        mmu.next_free(Frame::new(2)),
        Some(Frame::new(3)),
        "past every slot"
    );
    assert_eq!(mmu.next_free(Frame::new(4)), None, "past the limit");
}

#[test]
fn a_reservation_the_host_cannot_make_tables_for_changes_nothing() {
    let mut mmu = Mmu::new(8).unwrap();
    mmu.reserve(Table::Global, 0x40_0000..0x80_0000).unwrap();
    let mut made = 0;
    let refused = mmu.reserve_with(Table::Global, 0x0..0x100_0000, || {
        made += 1;
        (made < 2).then(|| Box::new([0; TABLE_PAGES]))
    });
    assert_eq!(
        refused,
        Err(MmuError::Exhausted),
        "the host refused the second table"
    );
    assert_eq!(
        mmu.tables_held(Table::Global),
        1,
        "only the one reserved before"
    );
    mmu.unreserve(Table::Global, 0x40_0000..0x80_0000);
    assert_eq!(
        mmu.tables_held(Table::Global),
        0,
        "whose count the refusal left alone"
    );
}

/// A watch is held in an ordered map whose nodes are at least half full, so
/// one costs at most four times its key and value.
#[test]
fn a_frame_s_bookkeeping_is_within_its_share() {
    let watch = 4 * (core::mem::size_of::<u32>() + core::mem::size_of::<Watch>());
    let bits = (1 + WATCHERS).div_ceil(8);
    let kept = core::mem::size_of::<Slot>() + core::mem::size_of::<u32>() + watch + bits;
    assert!(kept <= (PAGE_SIZE / BOOKKEEPING_SHARE) as usize, "{kept}");
}
