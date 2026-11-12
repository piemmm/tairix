//! The prefetch queue, modelled for what software can observe of it.
//!
//! Between control transfers the bus interface unit keeps the queue full, so
//! the next `len` code bytes have already been read when an instruction
//! writes memory. A store into those bytes changes memory but not what
//! executes. The model records the byte the queue holds for every such
//! address and hands it to the next fetch of that address; a control transfer
//! empties the queue.

/// The stale bytes a full queue still holds.
#[derive(Clone, Debug)]
pub struct Queue {
    len: u8,
    held: [(u32, u8); 6],
    count: u8,
    window_base: u32,
    window_ip: u16,
}

impl Queue {
    /// An empty queue `len` bytes long (four on an 8-bit bus, six on 16).
    #[must_use]
    pub const fn new(len: u8) -> Self {
        Self {
            len,
            held: [(0, 0); 6],
            count: 0,
            window_base: 0,
            window_ip: 0,
        }
    }

    /// The queue's length in bytes.
    #[must_use]
    pub const fn len(&self) -> u8 {
        self.len
    }

    /// Whether no stale byte is held.
    #[must_use]
    pub const fn holds_nothing(&self) -> bool {
        self.count == 0
    }

    /// Where the queue starts: the code segment's base and IP.
    #[must_use]
    pub const fn window(&self) -> (u32, u16) {
        (self.window_base, self.window_ip)
    }

    /// Empties the queue, as any control transfer does.
    pub fn flush(&mut self) {
        self.count = 0;
    }

    /// Notes where the queue starts: the first code byte not yet executed,
    /// at `ip` in the code segment based at `base`.
    pub fn set_window(&mut self, base: u32, ip: u16) {
        self.window_base = base;
        self.window_ip = ip;
    }

    /// The physical address of the queue's `i`th byte.
    fn window_addr(&self, i: u8) -> u32 {
        (self.window_base + u32::from(self.window_ip.wrapping_add(u16::from(i)))) & 0xF_FFFF
    }

    /// Whether `addr` is one of the bytes the queue holds.
    #[must_use]
    pub fn covers(&self, addr: u32) -> bool {
        (0..self.len).any(|i| self.window_addr(i) == addr)
    }

    /// Records that the queue holds `old` for `addr`, which is about to be
    /// overwritten. A byte already recorded keeps its first value.
    pub fn hold(&mut self, addr: u32, old: u8) {
        if self.held[..usize::from(self.count)]
            .iter()
            .any(|&(a, _)| a == addr)
        {
            return;
        }
        if usize::from(self.count) < self.held.len() {
            self.held[usize::from(self.count)] = (addr, old);
            self.count += 1;
        }
    }

    /// Takes the queued byte for `addr`, if the queue holds a stale one.
    pub fn take(&mut self, addr: u32) -> Option<u8> {
        if self.count == 0 {
            return None;
        }
        let n = usize::from(self.count);
        let at = self.held[..n].iter().position(|&(a, _)| a == addr)?;
        let byte = self.held[at].1;
        self.held.copy_within(at + 1..n, at);
        self.count -= 1;
        Some(byte)
    }
}

crate::snapshot::state!(
    Queue { held, count, window_base, window_ip }
    as built { len }
    check |queue: &Queue| {
        // As many bytes held as there are places, from a segment's base.
        usize::from(queue.count) <= queue.held.len() && queue.window_base & !0xF_FFF0 == 0
    }
);
