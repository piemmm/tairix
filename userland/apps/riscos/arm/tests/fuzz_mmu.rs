//! Deterministic fuzz harness for the guest MMU: random runs of frame,
//! mapping, space, watch and access operations, each held to a reference
//! model that keeps every page in a plain map and has no TLB.
//!
//! The model and the MMU must agree on every answer: each refusal and its
//! reason, every byte an access returns and every abort it takes, which
//! frames are free, and each watcher's reports. The addresses drawn alias in
//! the TLB, so a stale entry anywhere shows as a disagreement.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use std::collections::BTreeMap;

use tairix_fuzzseed::Prng;
use tairix_riscos_arm::bus::{Abort, Bus, Width};
use tairix_riscos_arm::mmu::{
    Access, Frame, FrameKind, Mmu, MmuError, Rights, Space, Table, Watcher, FRAME_WORDS, PAGE_SIZE,
    WATCHERS,
};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_RUNS: u64 = 400;
/// Operations in one run.
const OPERATIONS: usize = 400;
/// Pages drawn: a low run, and the same run 4 MiB up, where every page
/// shares a TLB entry with its twin below.
const PAGES: [u32; 2] = [0, 1024];
const RUN: u32 = 12;

#[derive(Clone)]
struct ModelFrame {
    words: Vec<u32>,
    held: bool,
    maps: u32,
    host: bool,
}

#[derive(Clone, Copy, Default)]
struct ModelWatch {
    watched: [u16; WATCHERS],
    pending: [u16; WATCHERS],
}

struct ModelSpace {
    pages: BTreeMap<u32, (u32, Access)>,
    extent: u32,
}

struct Model {
    frames: BTreeMap<u32, ModelFrame>,
    limit: u32,
    global: BTreeMap<u32, (u32, Access)>,
    /// The live spaces, by the handle the MMU gave each.
    spaces: BTreeMap<Space, ModelSpace>,
    current: Space,
    watches: BTreeMap<u32, ModelWatch>,
}

impl Model {
    fn new(limit: u32, first: Space) -> Self {
        Self {
            frames: BTreeMap::new(),
            limit,
            global: BTreeMap::new(),
            spaces: BTreeMap::from([(
                first,
                ModelSpace {
                    pages: BTreeMap::new(),
                    extent: 0,
                },
            )]),
            current: first,
            watches: BTreeMap::new(),
        }
    }

    fn used(&self) -> u32 {
        u32::try_from(self.frames.len()).unwrap_or(u32::MAX)
    }

    fn lowest_free(&self) -> u32 {
        (0..=u32::MAX)
            .find(|n| !self.frames.contains_key(n))
            .unwrap_or(0)
    }

    fn allocate_at(&mut self, number: u32, host: bool) -> Result<(), MmuError> {
        if self.frames.contains_key(&number) {
            return Err(MmuError::Frame);
        }
        if number >= self.limit || self.used() >= self.limit {
            return Err(MmuError::Exhausted);
        }
        self.frames.insert(
            number,
            ModelFrame {
                words: vec![0; FRAME_WORDS],
                held: true,
                maps: 0,
                host,
            },
        );
        Ok(())
    }

    fn report(&mut self, frame: u32, granules: u16) {
        if let Some(watch) = self.watches.get_mut(&frame) {
            for w in 0..WATCHERS {
                watch.pending[w] |= watch.watched[w] & granules;
            }
        }
    }

    fn free(&mut self, frame: u32) {
        self.report(frame, u16::MAX);
        if let Some(watch) = self.watches.get_mut(&frame) {
            watch.watched = [0; WATCHERS];
        }
        self.frames.remove(&frame);
    }

    fn unmapped(&mut self, frame: u32) {
        let Some(model) = self.frames.get_mut(&frame) else {
            return;
        };
        model.maps -= 1;
        if model.maps == 0 && !model.held {
            self.free(frame);
        }
    }

    fn highest_extent(&self) -> u32 {
        self.spaces
            .values()
            .map(|space| space.extent)
            .max()
            .unwrap_or(0)
    }

    fn table(&self, table: Table) -> Option<&BTreeMap<u32, (u32, Access)>> {
        match table {
            Table::Global => Some(&self.global),
            Table::Space(space) => self.spaces.get(&space).map(|space| &space.pages),
        }
    }

    fn table_mut(&mut self, table: Table) -> Option<&mut BTreeMap<u32, (u32, Access)>> {
        match table {
            Table::Global => Some(&mut self.global),
            Table::Space(space) => self.spaces.get_mut(&space).map(|space| &mut space.pages),
        }
    }

    fn writes_host(&self, frame: u32, access: Access) -> bool {
        let host = self.frames.get(&frame).is_some_and(|model| model.host);
        host && (access.user == Rights::ReadWrite || access.privileged == Rights::ReadWrite)
    }

    fn map(
        &mut self,
        table: Table,
        address: u32,
        frame: u32,
        access: Access,
    ) -> Result<(), MmuError> {
        if !address.is_multiple_of(PAGE_SIZE) {
            return Err(MmuError::Placement);
        }
        let allowed = match table {
            Table::Global => address >= self.highest_extent(),
            Table::Space(space) => address < self.spaces.get(&space).ok_or(MmuError::Space)?.extent,
        };
        if !allowed {
            return Err(MmuError::Placement);
        }
        if !self.frames.contains_key(&frame) {
            return Err(MmuError::Frame);
        }
        if self.writes_host(frame, access) {
            return Err(MmuError::HostWritable);
        }
        let pages = self.table_mut(table).ok_or(MmuError::Space)?;
        if pages.contains_key(&(address / PAGE_SIZE)) {
            return Err(MmuError::Occupied);
        }
        pages.insert(address / PAGE_SIZE, (frame, access));
        if let Some(model) = self.frames.get_mut(&frame) {
            model.maps += 1;
        }
        Ok(())
    }

    fn unmap(&mut self, table: Table, address: u32) -> Result<Frame, MmuError> {
        if !address.is_multiple_of(PAGE_SIZE) {
            return Err(MmuError::Placement);
        }
        let pages = self.table_mut(table).ok_or(MmuError::Space)?;
        let (frame, _) = pages
            .remove(&(address / PAGE_SIZE))
            .ok_or(MmuError::Unmapped)?;
        self.unmapped(frame);
        Ok(Frame::new(frame))
    }

    fn protect(&mut self, table: Table, address: u32, access: Access) -> Result<(), MmuError> {
        if !address.is_multiple_of(PAGE_SIZE) {
            return Err(MmuError::Placement);
        }
        let pages = self.table(table).ok_or(MmuError::Space)?;
        let (frame, _) = *pages
            .get(&(address / PAGE_SIZE))
            .ok_or(MmuError::Unmapped)?;
        if self.writes_host(frame, access) {
            return Err(MmuError::HostWritable);
        }
        if let Some(pages) = self.table_mut(table) {
            pages.insert(address / PAGE_SIZE, (frame, access));
        }
        Ok(())
    }

    /// Whether a space of `extent` may be made.
    fn may_create(&self, extent: u32) -> Result<(), MmuError> {
        if !extent.is_multiple_of(PAGE_SIZE)
            || self.global.range(..extent / PAGE_SIZE).next().is_some()
        {
            return Err(MmuError::Placement);
        }
        Ok(())
    }

    fn destroy_space(&mut self, space: Space) -> Result<(), MmuError> {
        if space == self.current {
            return Err(MmuError::Space);
        }
        let model = self.spaces.remove(&space).ok_or(MmuError::Space)?;
        for (frame, _) in model.pages.into_values() {
            self.unmapped(frame);
        }
        Ok(())
    }

    fn set_extent(&mut self, space: Space, extent: u32) -> Result<(), MmuError> {
        let old = self.spaces.get(&space).ok_or(MmuError::Space)?.extent;
        if !extent.is_multiple_of(PAGE_SIZE) {
            return Err(MmuError::Placement);
        }
        if extent > old
            && self
                .global
                .range(old / PAGE_SIZE..extent / PAGE_SIZE)
                .next()
                .is_some()
        {
            return Err(MmuError::Placement);
        }
        let model = self.spaces.get_mut(&space).ok_or(MmuError::Space)?;
        if extent < old
            && model
                .pages
                .range(extent / PAGE_SIZE..old / PAGE_SIZE)
                .next()
                .is_some()
        {
            return Err(MmuError::Occupied);
        }
        model.extent = extent;
        Ok(())
    }

    /// What the processor reaches at `address`: the frame and its access.
    fn reach(&self, address: u32) -> Option<(u32, Access)> {
        let current = self.spaces.get(&self.current)?;
        let table = if address < current.extent {
            &current.pages
        } else {
            &self.global
        };
        table.get(&(address / PAGE_SIZE)).copied()
    }

    fn access(
        &self,
        address: u32,
        write: bool,
        execute: bool,
        privileged: bool,
    ) -> Result<u32, Abort> {
        let (frame, access) = self.reach(address).ok_or(Abort::Translation)?;
        let rights = if privileged {
            access.privileged
        } else {
            access.user
        };
        let granted = match (write, execute) {
            (true, _) => rights == Rights::ReadWrite,
            (false, true) => rights != Rights::None && access.execute,
            (false, false) => rights != Rights::None,
        };
        if granted {
            Ok(frame)
        } else {
            Err(Abort::Permission)
        }
    }

    fn read(&self, address: u32, width: Width, privileged: bool) -> Result<u32, Abort> {
        let frame = self.access(address, false, false, privileged)?;
        let word = self.frames[&frame].words[(address % PAGE_SIZE / 4) as usize];
        let lane = word >> (8 * (address & 3));
        Ok(match width {
            Width::Byte => lane & 0xFF,
            Width::Half => lane & 0xFFFF,
            Width::Word => word,
        })
    }

    fn write(
        &mut self,
        address: u32,
        width: Width,
        value: u32,
        privileged: bool,
    ) -> Result<(), Abort> {
        let frame = self.access(address, true, false, privileged)?;
        let shift = 8 * (address & 3);
        let mask = match width {
            Width::Byte => 0xFF << shift,
            Width::Half => 0xFFFF << shift,
            Width::Word => u32::MAX,
        };
        if let Some(model) = self.frames.get_mut(&frame) {
            let word = &mut model.words[(address % PAGE_SIZE / 4) as usize];
            *word = (*word & !mask) | ((value << shift) & mask);
        }
        self.report(frame, 1 << (address % PAGE_SIZE / 256));
        Ok(())
    }

    fn take_written(&mut self, watcher: usize) -> Vec<(Frame, u16)> {
        let mut taken = Vec::new();
        for (&frame, watch) in &mut self.watches {
            let granules = std::mem::take(&mut watch.pending[watcher]);
            if granules != 0 {
                taken.push((Frame::new(frame), granules));
            }
        }
        taken
    }
}

/// A number below `bound`.
fn below(rng: &mut Prng, bound: u32) -> u32 {
    u32::try_from(rng.below(bound as usize)).unwrap_or(0)
}

fn drawn_address(rng: &mut Prng) -> u32 {
    let page = *rng.pick(&PAGES) + below(rng, RUN);
    let offset = match rng.below(8) {
        0 => 1,
        _ => 0,
    };
    page * PAGE_SIZE + offset
}

fn drawn_extent(rng: &mut Prng) -> u32 {
    let pages = match rng.below(4) {
        0 => 0,
        1 => below(rng, RUN),
        2 => 1024 + below(rng, RUN),
        _ => RUN / 2,
    };
    pages * PAGE_SIZE + if rng.below(16) == 0 { 4 } else { 0 }
}

fn drawn_access(rng: &mut Prng) -> Access {
    let rights = |rng: &mut Prng| *rng.pick(&[Rights::None, Rights::Read, Rights::ReadWrite]);
    Access {
        user: rights(rng),
        privileged: rights(rng),
        execute: rng.below(2) == 0,
    }
}

fn drawn_width(rng: &mut Prng) -> (Width, u32) {
    match rng.below(3) {
        0 => (Width::Byte, 1),
        1 => (Width::Half, 2),
        _ => (Width::Word, 4),
    }
}

/// An address an access may be made at: naturally aligned for `width`.
fn access_address(rng: &mut Prng, size: u32) -> u32 {
    let page = *rng.pick(&PAGES) + below(rng, RUN);
    let offset = below(rng, PAGE_SIZE) & !(size - 1);
    page * PAGE_SIZE + offset
}

/// The global table, or the table of a space the MMU made, live or not.
fn table_drawn(rng: &mut Prng, handles: &[Space]) -> Table {
    if rng.below(3) == 0 {
        Table::Global
    } else {
        Table::Space(*rng.pick(handles))
    }
}

fn frame_drawn(rng: &mut Prng) -> u32 {
    below(rng, 20)
}

/// What one run reached, so a sweep can show it exercised more than refusals.
#[derive(Default)]
struct Reached {
    mapped: u64,
    read_private: u64,
    read_global: u64,
    permission: u64,
    reported: u64,
    switched: u64,
    destroyed: u64,
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per operation the harness draws"
)]
fn check_run(rng: &mut Prng, reached: &mut Reached) {
    let limit = 4 + below(rng, 16);
    let mut mmu = Mmu::new(limit).expect("an MMU");
    let mut model = Model::new(limit, mmu.current());
    // Every space handle the MMU has given, its spaces destroyed or not.
    let mut handles = vec![mmu.current()];
    let watchers: Vec<Watcher> = (0..WATCHERS).filter_map(Watcher::new).collect();
    // A space covering the low run with a page of its own, a global page in
    // its TLB twin's place, and that space current: what random draws would
    // seldom assemble.
    let extent = RUN / 2 * PAGE_SIZE;
    let space = mmu.create_space(extent).expect("a space");
    handles.push(space);
    model.spaces.insert(
        space,
        ModelSpace {
            pages: BTreeMap::new(),
            extent,
        },
    );
    for (table, address) in [
        (Table::Space(space), 2 * PAGE_SIZE),
        (Table::Global, (1024 + 2) * PAGE_SIZE),
    ] {
        let frame = mmu.allocate(FrameKind::Memory).expect("a frame");
        model
            .allocate_at(frame.number(), false)
            .expect("the model's frame");
        let access = drawn_access(rng);
        assert_eq!(
            mmu.map(table, address, frame, access),
            model.map(table, address, frame.number(), access)
        );
    }
    mmu.switch(space).expect("the space");
    model.current = space;
    for step in 0..OPERATIONS {
        match rng.below(18) {
            0 => {
                let host = rng.below(4) == 0;
                let kind = if host {
                    FrameKind::Host
                } else {
                    FrameKind::Memory
                };
                let expected = {
                    let lowest = model.lowest_free();
                    model.allocate_at(lowest, host).map(|()| Frame::new(lowest))
                };
                assert_eq!(mmu.allocate(kind), expected, "step {step}: allocate");
            }
            1 => {
                let number = frame_drawn(rng);
                let expected = model.allocate_at(number, false);
                assert_eq!(
                    mmu.allocate_at(Frame::new(number), FrameKind::Memory),
                    expected,
                    "step {step}: allocate_at {number}"
                );
            }
            2 => {
                let number = frame_drawn(rng);
                let expected = match model.frames.get_mut(&number) {
                    Some(frame) if frame.held => {
                        frame.held = false;
                        if frame.maps == 0 {
                            model.free(number);
                        }
                        Ok(())
                    }
                    _ => Err(MmuError::Frame),
                };
                assert_eq!(
                    mmu.release(Frame::new(number)),
                    expected,
                    "step {step}: release"
                );
            }
            3 => {
                let number = frame_drawn(rng);
                let expected = match model.frames.get_mut(&number) {
                    Some(frame) => {
                        frame.held = true;
                        Ok(())
                    }
                    None => Err(MmuError::Frame),
                };
                assert_eq!(mmu.hold(Frame::new(number)), expected, "step {step}: hold");
            }
            4 | 5 => {
                let table = table_drawn(rng, &handles);
                let address = drawn_address(rng);
                let frame = frame_drawn(rng);
                let access = drawn_access(rng);
                let expected = model.map(table, address, frame, access);
                assert_eq!(
                    mmu.map(table, address, Frame::new(frame), access),
                    expected,
                    "step {step}: map {table:?} {address:#X} {frame} {access:?}"
                );
                reached.mapped += u64::from(expected.is_ok());
            }
            6 => {
                let table = table_drawn(rng, &handles);
                let address = drawn_address(rng);
                let expected = model.unmap(table, address);
                assert_eq!(mmu.unmap(table, address), expected, "step {step}: unmap");
            }
            7 => {
                let table = table_drawn(rng, &handles);
                let address = drawn_address(rng);
                let access = drawn_access(rng);
                let expected = model.protect(table, address, access);
                assert_eq!(
                    mmu.protect(table, address, access),
                    expected,
                    "step {step}: protect"
                );
            }
            8 => {
                let extent = drawn_extent(rng);
                let expected = model.may_create(extent);
                let made = mmu.create_space(extent);
                assert_eq!(made.map(drop), expected, "step {step}: create {extent:#X}");
                if let Ok(space) = made {
                    assert!(
                        !handles.contains(&space),
                        "step {step}: a handle given twice"
                    );
                    handles.push(space);
                    model.spaces.insert(
                        space,
                        ModelSpace {
                            pages: BTreeMap::new(),
                            extent,
                        },
                    );
                }
            }
            9 if rng.below(4) == 0 => {
                let space = *rng.pick(&handles);
                let expected = model.destroy_space(space);
                assert_eq!(mmu.destroy_space(space), expected, "step {step}: destroy");
                reached.destroyed += u64::from(expected.is_ok());
            }
            10 => {
                let space = *rng.pick(&handles);
                let expected = if model.spaces.contains_key(&space) {
                    model.current = space;
                    Ok(())
                } else {
                    Err(MmuError::Space)
                };
                assert_eq!(mmu.switch(space), expected, "step {step}: switch");
                reached.switched += u64::from(expected.is_ok());
            }
            11 => {
                let space = *rng.pick(&handles);
                let extent = drawn_extent(rng);
                let expected = model.set_extent(space, extent);
                assert_eq!(
                    mmu.set_extent(space, extent),
                    expected,
                    "step {step}: extent"
                );
            }
            12 | 13 => {
                let (width, size) = drawn_width(rng);
                let address = access_address(rng, size);
                let privileged = rng.below(2) == 0;
                let value = rng.next_u32();
                let expected = model.write(address, width, value, privileged);
                assert_eq!(
                    mmu.write(address, width, value, privileged),
                    expected,
                    "step {step}: write {address:#X}"
                );
            }
            9 | 14 => {
                let (width, size) = drawn_width(rng);
                let address = access_address(rng, size);
                let privileged = rng.below(2) == 0;
                let expected = model.read(address, width, privileged);
                assert_eq!(
                    mmu.read(address, width, privileged),
                    expected,
                    "step {step}: read {address:#X}"
                );
                match expected {
                    Ok(_) if matches!(mmu.table_of(address), Table::Space(_)) => {
                        reached.read_private += 1;
                    }
                    Ok(_) => reached.read_global += 1,
                    Err(Abort::Permission) => reached.permission += 1,
                    Err(_) => {}
                }
            }
            15 => {
                let address = access_address(rng, 4);
                let privileged = rng.below(2) == 0;
                let expected = model.access(address, false, true, privileged).map(|frame| {
                    (
                        model.frames[&frame].words[(address % PAGE_SIZE / 4) as usize],
                        model.frames[&frame].host,
                    )
                });
                let got = mmu
                    .fetch(address, privileged)
                    .map(|fetch| (fetch.word, fetch.host));
                assert_eq!(got, expected, "step {step}: fetch {address:#X}");
            }
            16 => {
                let number = frame_drawn(rng);
                let watcher = *rng.pick(&watchers);
                let w = watchers.iter().position(|&x| x == watcher).unwrap_or(0);
                let granules = u16::try_from(rng.next_u32() & 0xFFFF).unwrap_or(0);
                if rng.below(3) == 0 {
                    mmu.unwatch(Frame::new(number), granules, watcher);
                    if let Some(watch) = model.watches.get_mut(&number) {
                        watch.watched[w] &= !granules;
                    }
                } else {
                    let expected = if model.frames.contains_key(&number) {
                        model.watches.entry(number).or_default().watched[w] |= granules;
                        Ok(())
                    } else {
                        Err(MmuError::Frame)
                    };
                    assert_eq!(
                        mmu.watch(Frame::new(number), granules, watcher),
                        expected,
                        "step {step}: watch"
                    );
                }
            }
            _ => {
                if rng.below(2) == 0 {
                    let watcher = *rng.pick(&watchers);
                    let w = watchers.iter().position(|&x| x == watcher).unwrap_or(0);
                    let expected = model.take_written(w);
                    let mut got = Vec::new();
                    mmu.take_written(watcher, |frame, granules| got.push((frame, granules)));
                    assert_eq!(got, expected, "step {step}: take");
                    reached.reported += u64::try_from(got.len()).unwrap_or(0);
                } else {
                    let number = frame_drawn(rng);
                    let index = rng.below(FRAME_WORDS);
                    let value = rng.next_u32();
                    let live = model.frames.contains_key(&number);
                    match mmu.words_mut(Frame::new(number)) {
                        Some(words) => {
                            assert!(live, "step {step}: words of a free frame");
                            words[index] = value;
                            model.report(number, u16::MAX);
                            if let Some(frame) = model.frames.get_mut(&number) {
                                frame.words[index] = value;
                            }
                        }
                        None => assert!(!live, "step {step}: no words for a live frame"),
                    }
                }
            }
        }
        assert_eq!(mmu.used(), model.used(), "step {step}: frames in use");
        for number in 0..20 {
            assert_eq!(
                mmu.is_free(Frame::new(number)),
                !model.frames.contains_key(&number),
                "step {step}: frame {number}"
            );
        }
    }
}

#[test]
fn any_run_of_operations_matches_the_reference_model() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_run_of_operations_matches_the_reference_model",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    let mut reached = Reached::default();
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        check_run(&mut rng, &mut reached);
        runs += 1;
    }
    // Floors a third of the least each counter reached over 300 seeds, so
    // the sweep cannot pass on refusals alone.
    if deadline.is_none() {
        let floors = [
            (reached.mapped, 500, "pages mapped"),
            (reached.read_private, 25, "reads through a space"),
            (reached.read_global, 250, "reads through the global table"),
            (reached.permission, 140, "permission aborts"),
            (reached.reported, 200, "reports taken"),
            (reached.switched, 2000, "switches"),
            (reached.destroyed, 400, "spaces destroyed"),
        ];
        for (count, floor, what) in floors {
            assert!(count >= floor, "{what}: {count} below {floor}");
        }
    }
}
