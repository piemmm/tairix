# `tairix-collections`

The **heap-backed** `no_std` containers TAIRiX runs on that `core` and `alloc`
have no answer for. `alloc` already supplies `Vec`, `String`, `BTreeMap`,
`BTreeSet`, `VecDeque`, and `BinaryHeap`; the one re-implemented here is the
ordered map, because `alloc`'s cannot refuse an insertion it has no memory for,
and a container with no caller in the tree is not added.
`plans/COLLECTIONS.md` is the ledger of what has landed and what is still to
come.

The containers that allocate *nothing* live in [`tairix-inline`](./inline.md) —
a crate that links neither this one nor `alloc`, so the layer running before a
heap exists can use them. This crate depends on it for the inline half of its
`SmallVec`.

Every remaining `alloc` `BTreeSet` is either order-load-bearing or small
enough that the ordered set is the cheaper structure.

## Inventory

| Type | Guarantee | Stands in for |
|---|---|---|
| `HashMap<K, V, S>` | expected O(1) lookup, insert, and remove; one control byte and one `(K, V)` slot per bucket, no per-entry node | a `BTreeMap` used where the key order was never wanted |
| `HashSet<T, S>` | the same, over a zero-sized value | a `BTreeSet` used as an unordered set |
| `LruMap<K, V, S>` | the same table with a recency order through it: expected O(1) lookup, touch, and eviction of the coldest entry | a cache's `(tick -> key)` `BTreeMap` eviction index, O(log n) on all three |
| `OrderedMap<K, V>` | ordered lookup, insertion and removal, each a descent of a B-tree's height comparing at most five keys a level; reads and removals allocate nothing, an insertion is all or nothing, and room for a run of them is reserved ahead | `alloc`'s `BTreeMap`, whose insertion cannot refuse for want of memory |
| `OrderedSet<T>` | the same, over a zero-sized value | `alloc`'s `BTreeSet`, likewise |
| `RangeMap<K, V>` | disjoint half-open ranges, each an identity: O(log n) covering lookup, insertion that refuses an overlap, and first-fit placement over the gaps | a `base -> length` `BTreeMap` plus a hand-written overlap probe, and the occupancy bitmap a window scanned to place |
| `RangeSet<K>` | the same storage canonicalised — insertion absorbs what it touches, removal splits what it cuts | a run set built by hand per subsystem, and the free-list that fragmented beside a live-region map |
| `SmallVec<T, N>` | inline to `N`, then one spill to the heap | a hot path that holds a handful of elements and allocates anyway |
| `ByteQueue` | a byte FIFO under its holder's bound, kept as one contiguous run; amortised O(1) per byte, storage committed up front or as bytes arrive, and wiped before it is given back | the session frame arena `lib/sandbox` carried privately, and the SSH transport's three stream buffers |
| `ChunkVec<T, CHUNK>` | a growable list in chunks of `CHUNK` elements, each allocated whole: an element never moves once pushed, so a push costs one write or one chunk's allocation however many it holds, and indexing one load more than a slice | the ray tracer's list of a scene's objects, a `Vec` whose doubling copied hundreds of megabytes in one unit of preparation |
| `RadixTree<V, TAGS>` | a sparse `u64`-keyed index of 64-way nodes: lookup, insertion, removal and tagging cost the tree's height — eleven nodes at most — whatever it holds, and an ordered or tagged walk resumes from the path it last took | a sorted `Vec` whose every insertion moved the entries above it |
| `IndexedHeap<P>` | a min-heap over a holder's own dense slot numbers: the least read in O(1), a slot's priority moved either way or withdrawn in O(log n), and every entry at or below a bound walked in O(k) for the k it returns | a per-slot deadline array swept whole for what was due, and `alloc::BinaryHeap`, which can neither find an entry nor move one |
| `SlotMap<K, V>` | values filed under keys it issues: O(1) insertion, lookup and removal, and a generation in every key that refuses it once its value is gone | a counter handing out identifiers with no reuse and no generation, so a stale one aliased whatever took its place |
| `BitVec` | a bitmap of run-time length: O(1) get and set, the next or previous set or clear bit in time proportional to its summary's height, nothing allocated after a fallible resize | a `Vec<bool>`, a scanned `Vec<u64>`, and the `BTreeSet` a selection aborted its holder through |
| `SharedSlice<T>`, `SharedStr` | an immutable slice or text in one block behind a thin pointer, made only fallibly; O(1) clone, the last holder freeing it, the empty one allocating nothing | an `Arc<[T]>` or `Arc<str>`, whose making aborts on a refusing heap |

## `ByteQueue`

A stream parser wants the bytes it has not consumed as one slice — to find a
frame's end, to decrypt a packet where it lies — which `VecDeque` cannot give
without rotating a record that straddles its wrap. `ByteQueue` appends at the
tail, takes from the head, and reclaims the consumed prefix by compacting
once it reaches half the arena, so the move is paid for by at least that many
consumed bytes and a queue drained as fast as it fills never moves at all.

Its bound is the holder's containment policy, not a capacity: a sandbox
session commits it at admission so the session's cost is known before it runs
(`committed`), and an SSH connection commits storage only as bytes arrive
(`new`), so an idle one costs a few kilobytes and a hostile one at most its
bound. Growth is fallible either way.

It is the one container here that scrubs. Its element is a bare byte, which
no holder can make self-zeroing, and a byte stream is exactly what a session
key or a password transits; growing by reallocation would leave a copy in
freed memory. So growth builds a fresh arena and wipes the old one, and drop
wipes what remains. Bytes consumed inside live storage are not wiped as they
go — they are overwritten, or wiped with the arena.

## `LruMap`

A cache holds what fits and drops what has gone coldest, and both halves of
that must be constant time or the cache costs more than the misses it saves.
`LruMap` is one open-addressed index over a node arena, with the recency order
and the free list threaded through that arena as two
[`IntrusiveList`](./inline.md)s:

* the index bucket holds the *node handle*, not a copy of the key, so a key is
  stored once however many structures reach it;
* each node carries the hash it was filed under, so eviction reaches its bucket
  without hashing a key and a table rebuild moves entries without hashing any;
* an evicted node returns to the free list and the next insertion reuses it, so
  a map at its steady occupancy returns to the allocator not at all;
* `clear` releases both allocations rather than keeping the capacity, because
  the callers that drain a whole map are caches a memory-pressure band is
  reclaiming and holding their peak footprint afterwards is the memory the
  drain was for.

It evicts nothing on its own. A caller bounds it by its own budget — entries,
bytes, or a pressure band — and calls `pop_lru` until the bound is met, which
is what lets one map serve a fixed-entry index and a byte-budgeted cache alike.
`get`/`get_mut` are uses and refresh an entry; `peek`/`peek_mut` and
`iter_lru` are observations and do not, so a diagnostic read never changes what
eviction takes next.

Every link it splices names a node from its own arena, so a refused splice
means something outside the map corrupted its bookkeeping. Such a refusal is
fail-closed — nothing found, nothing inserted — and a debug assertion, since no
input reaches it.

## `RadixTree`

Six key bits a level, as tall as its largest key needs: a key below 2^24
costs four nodes, and none costs more than eleven.

* Each node keeps a bitmap of its occupied slots and, per tag, one of the
  slots whose subtree holds a tagged entry, so a successor or predecessor
  search, and a walk of only the tagged entries, skip empty and untagged
  subtrees a word at a time.
* A walk keeps the path to the entry it last found and resumes from the
  deepest node that entry and its successor share, so a whole walk reaches
  each node it passes a bounded number of times, never a descent per entry.
* Nodes live in two arenas, interior and leaf, chunked as the ordered map's
  are. A removal chains the nodes it empties into its arena's free list and
  lowers the tree once its largest key no longer needs the height, so the
  nodes resident are the live keys' paths and a tree at a steady size draws
  nothing from the allocator.
  `try_shrink_to_fit` moves the live nodes to the front of their arenas and
  gives the freed ones and any spare room back, `clear` gives back
  everything, and `allocated_bytes` says what the arenas hold: a holder
  under memory pressure can account for and return a tree's memory.
* `try_reserve_key` allocates every node one insertion of a key needs and
  changes nothing else. A holder updating several trees at once makes every
  allocation first, so it changes all of them or none: the IOVA space
  (`kernel/iommu/api`), its first consumer with one tree per buddy order,
  reserves the buddies a split will record before the split removes anything.

## `IndexedHeap`

A deadline that moves — a record renewed, a question's backoff doubling, a
timer cancelled — is a priority change at a known slot. `alloc::BinaryHeap`
cannot find an entry, so a holder pushes the new deadline and skips the stale
copy when it surfaces, and the heap grows with every move; the alternative was
a deadline per slot swept whole on every timer. `IndexedHeap` records each
slot's place in the heap beside the heap itself.

* `try_insert`, `set`, `remove` and `pop` sift one path, so each costs at most
  two comparisons a level; `peek` reads the root.
* `up_to(bound)` walks the subtree at the root whose entries are at or below
  `bound` without a stack — from an entry past the bound it climbs over every
  right child to the next sibling — so finding what is due costs what is due,
  however many slots are held.
* Slots are the holder's dense indices: an arena slot, a table row. The heap
  keeps room for every slot below the largest it has reserved and never gives
  it back, so after `try_reserve_slot` no insertion of any lower slot can fail.
  A holder taking a slot reserves it beside the slot's own storage, and every
  later schedule of it is infallible. A sparse holder pays for the gaps.

Its first consumers are the multicast DNS record cache and question table in
`lib/net`, whose deadlines are each record's expiry and refresh points and
each question's next query.

## `SlotMap`

A table of objects a client names by handle needs the handle refused once its
object is gone, or a stale handle resolves to whatever took the slot next.
`SlotMap` files each value in a slot and issues a key carrying the slot's index
and its generation.

* Removal advances the slot's generation, so every key issued for the slot
  before is refused from then on. A slot whose generation is spent is retired,
  never reused: no key is ever accepted for a value it did not name.
* Vacant slots are reused most recently freed first; iteration visits slots in
  index order. `clear` and `retain` advance the generations they vacate.
* A holder names its keys with the `Key` trait, so the keys of two maps cannot
  be mixed up at compile time. A key is an identifier, not an authority: it
  names a slot of the map that issued it, so a holder keeping several maps that
  must tell their keys apart carries its own identity beside it.
* `try_reserve` makes room for that many insertions, after which none
  allocates; an index past the last a key can carry is
  `TryReserveError::CapacityOverflow`. `try_reserve_exact` makes exactly that
  room, `reserve_bytes` says beforehand what it would add, and `held_bytes`
  what the storage holds, so a holder can charge for the room before it is
  made.

Its first consumer is `lib/gpu`, which files a device's contexts and a
context's objects, jobs and timelines in it, each context's tables charged to
the context for their room. `smbfs` files its
connections in one, keying the deadline heap beside it on the dense slot index
and carrying each entry's key, so a due deadline names its connection.

## `SharedSlice`, `SharedStr`, `try_arc` and `try_box`

A value shared between holders and never changed — a document's names and
text, its dash and guide lists — wants `Arc<[T]>` or `Arc<str>`, and `alloc`
makes those only through an allocation that aborts when the heap refuses.

* A `SharedSlice` is one block: a count of its holders, a length, then the
  elements. `try_from_slice` clones into it and `try_from_vec` moves into it,
  each answering a refusal as `TryReserveError`; a clone shares the block.
  `SharedStr` is the same over UTF-8 text.
* The count is incremented `Relaxed`, since a new holder is made from one
  keeping the block alive, and decremented `Release` with an `Acquire` fence
  before the last holder drops the elements, as `Arc`'s is. It saturates
  rather than wrapping: past half the address space a block is never freed,
  as the kernel's reference counts do, where a wrapped count would free a
  block still held. `tests/loom.rs` models the hand-off.
* `ptr_eq` tells a shared block from an equal copy, so a holder can see that
  a value was passed through untouched.
* `try_arc` and `try_box` share or box any value so, the one place the
  unstable fallible `Arc` and `Box` constructors are reached.

## `OrderedMap` and `OrderedSet`

An ordered map a kernel inserts into from a system call has to be able to say
no. `alloc`'s `BTreeMap` cannot: it has no `try_insert` and no `reserve`, and
an insertion it has no memory for ends the program. `OrderedMap` is the same
structure — a B-tree of up to eleven entries a node, every node but the root
at least five full, every leaf at one depth — with that refusal designed in.

* A leaf holds its entries, its parent and its free-list link; an interior
  node holds the same and its twelve children inline — the layout production
  B-trees use, since five nodes in six are leaves and children held out of
  line would cost every lookup a dependent load a level. Leaves and interior
  nodes live in an arena each, addressed by `u32` index — the tree's height
  says which arena an index names — a freed node chained into its arena's
  free list. An arena costs the structure holding it one word: its chunks,
  counts and free chain lie behind one pointer, allocated with its first
  chunk and given back with its last, so an empty map allocates nothing and
  a map is 40 bytes where it is embedded. A fresh node is made empty where it
  will lie, its fields written in place — the arena's one `unsafe` — so a
  node far larger than a stack frame should hold is never built on the stack
  and moved in. Each arena is held in chunks: the first
  two hold one node each and each after them twice its predecessor until a
  chunk reaches 16 KiB, after which every chunk is that size. A small map so
  holds at most twice the nodes it uses, and a large one grows a chunk at a
  time — never one block of its whole size, never a copy of what it holds —
  so a kernel table the size of a process's resident set needs no large
  contiguous allocation and its growth costs no pause under the lock that
  holds it. A removal never allocates, an insertion takes a freed node before
  its arena grows, a refused reservation leaves both arenas holding exactly
  the room they had, and `try_shrink_to_fit` moves the live nodes to the
  front of each and gives back every chunk past them — or, refused, keeps
  them chained free for reuse.
* Ten thousand `u64` insertions in any order hold at most 266 416 bytes as an
  `OrderedSet` and 445 680 as an `OrderedMap`. Ascending keys meet that
  exactly: they leave every node off the last path at the fill floor, the
  most nodes so many entries can take. One node shape for leaves and interior
  nodes alike held 345 504 and 525 728.
* An insertion counts the leaves and interior nodes its splits will take —
  one for each full node on its way up, and a new root — and takes them from
  both arenas before it changes anything, so a refused allocation leaves the
  map exactly as it was. `try_reserve(n)` takes room for the worst `n`
  insertions up front, in each arena the fewer of the most nodes of that kind
  a tree of `len + n` entries can hold and the most `n` insertions take: a
  leaf each, and an interior node a level of the tallest such tree. After it
  those insertions cannot fail, and removals between them only add room,
  which is what a holder changing several maps at once needs to make every
  allocation before committing any.
* Lookup, iteration and a range query allocate nothing, and a range whose
  start lies past its end admits nothing rather than panicking.
* A mutable walk — `iter_mut`, `values_mut`, `range_mut` — cannot borrow an
  arena's nodes in key order without `unsafe`, so it moves each node it opens
  to an end of the stretch of its arena it has not yet opened, re-pointing
  whatever named the nodes it moved — the parents of the nodes beside a range
  included, whose frames it keeps once they are used up. The tree is whole
  after every step, a walk abandoned part-way leaves it whole, and a walk
  costs what it visits and the freed nodes it passes.
* An owning walk drains entries without rebalancing, since nothing reads the
  tree after it; `retain` visits in key order and searches back past each
  entry it drops.

`OrderedSet` is the map over a zero-sized value, as `HashSet` is over
`HashMap`. `RangeMap` and `RangeSet` stand on the map.

## `RangeMap` and `RangeSet`

Address space, block numbers, and slot indices are all held in *runs*, and
every subsystem that held them had written the same interval arithmetic: find
the entry at or below a point, ask whether it reaches the point, split what a
removal cuts, join what an insertion touches. Getting that wrong silently hands
out memory twice, so it is defined once.

The two types share one storage and one disjointness invariant, and differ
only in what they do to a neighbour:

* **`RangeMap` keeps neighbours apart.** An entry is an identity — a
  reservation, a mapping, a run of slots — so two abutting entries stay two,
  and an insertion that would *overlap* one is refused rather than replacing or
  splitting the holder. That refusal is the container carrying a rule its
  callers used to leave unstated: a second record over one address would make a
  fault's backing, and a release's extent, a choice between two answers.
* **`RangeSet` canonicalises.** An insertion absorbs every range it overlaps
  *or touches* and a removal splits the ranges it cuts, so two sets holding the
  same elements hold the same ranges whatever order they were built in. The
  entry count is then one per contiguous run: releasing a hundred-terabyte
  extent costs one entry, not one per block. It keeps a running `covered()`
  total in step with its entries, so a caller's accounting reads it rather than
  summing.

Both stand on the ordered map, so recording a range can be refused for want of
memory, and changes nothing when it is: an insertion, a placement, and a
removal that cuts one range in two. A `RangeSet` insertion that touches a held
range reuses that range's record — re-filed under the union's start, or
stretched to its end — so only one that touches nothing needs memory, and
`try_reserve` makes room for a run of them ahead. `allocated_bytes` says what
that room and the records removals freed cost, and `try_shrink_to_fit` gives
back what the ranges do not need.

Only ordering decides overlap, adjacency, and splitting, so `RangeKey` carries
just the arithmetic that *measures* a range: `span` turns one `(base, count)`
pair into a range with the overflow checked in one place rather than at every
call site that counts pages, blocks, or slots, and `distance_from` reports what
a range holds. `u64` and `usize` implement it — byte addresses and block
numbers for the first, slot indices for the second.

### Placement is the gaps, not a second structure

`RangeMap::place` hands out the lowest run of free elements inside a window,
first-fit over the gaps *between* what the window has already handed out. That
is the whole free-space representation: a released range is available again the
moment its record leaves, two released neighbours serve one larger request
between them, and there is no free-list or occupancy bitmap to fall out of step
with the live records.

It replaced both halves of that mistake. The anonymous placement window kept a
released-hole map beside its live-region map, and the two holes a pair of
adjacent releases left never joined, so a request larger than either was
refused while the address space for it sat free. The MMIO window kept a
`Vec<bool>` of slot occupancy and first-fit *scanned* it — up to the window's
whole ceiling per placement, growing the bitmap to the deepest slot ever
touched. A 1 GiB register window is 262 144 slots; the mapper now records the
runs it handed out and nothing per slot, and a placement walks those runs.

## The rules every container obeys

1. **Nothing that can fail panics.** Every allocating operation has a fallible
   form returning `TryReserveError` — `try_insert`, `try_reserve`,
   `try_with_capacity_and_hasher` — the ordered tier's insertions included. No
   map has an `Index` implementation, because a subscript that panics on a
   missing key has no place in a kernel.
2. **No allocation on a read path.** Lookup, iteration, and removal allocate
   nothing; growth is amortised and off the hot path.
3. **No fixed capacity ceiling.** A container here grows on demand and fails
   closed only on genuine exhaustion; a caller-chosen compile-time bound is the
   other crate's business.
4. **Order is unspecified unless the container says otherwise.** A hash
   container's iteration order varies with the hash key, the insertion
   history, and the capacity, so anything compared, logged, paged, or
   reproduced uses an ordered container.
5. **Secret hygiene is the holder's job.** A container does not scrub the slots
   it frees — reuse inside one address space is not a security boundary — so a
   holder of a key, credential, or capability token stores a value type that
   zeroes itself on drop, exactly as the userland heap in `lib/rt` already
   requires.

## Choosing a hasher

There is deliberately no default construction: `HashMap::with_hasher` takes the
`BuildHasher` explicitly, so the choice is visible wherever a map is created.

```rust,ignore
// Keys an attacker can choose or influence.
let map = HashMap::with_hasher(BuildSipHash13::keyed()?);

// Keys the kernel assigns itself.
let map = HashMap::with_hasher(BuildFastHash::new());
```

`BuildSipHash13::keyed` refuses to build until the per-boot key has been
published (see [`tairix-hash`](./hash.md)). Hashing attacker-chosen keys under
a predictable key lets an attacker pick a set that all collide, turning every
lookup into a linear scan; the keyed pseudo-random function removes the attack,
and refusing to construct is the fail-closed answer to being asked before the
key exists.

## The table

Open addressing over sixteen-lane control-byte groups. One byte beside each
slot holds either `EMPTY`, `DELETED`, or the key's seven-bit tag, and a probe
loads a whole group and asks three questions of it at once: which lanes carry
this tag, which are empty (the probe chain ends there), and which are free to
take an insertion. Lookup and insertion share that one primitive.

Groups are **aligned**: probing steps whole groups rather than a byte offset
into one. The control array therefore ends exactly at `buckets` bytes with no
wrap-around mirror of its head, and there is no paired-write invariant to
maintain on every control update — the simplification that keeps the `unsafe`
core small.

Two invariants carry the removal path:

* A group holding an empty lane has no probe chain passing *through* it.
  Insertion maintains it by only stepping past a group with no empty lane.
* Removal therefore writes `EMPTY` only into a group that already had one, and
  `DELETED` otherwise, which keeps every chain through that group intact.

The load-factor limit is seven eighths. When it is reached — by live entries,
by tombstones, or by both — the table rebuilds into a fresh allocation sized
for the live set, which grows it and clears its tombstones in one pass. A
steady live set therefore settles on a steady footprint no matter how much
churn passes through it.

## The group scan is a dispatched ops table

The scan is a [`lib/cpuops`](./cpuops.md) family: an SSE2 candidate, a NEON
candidate, and a portable word-at-a-time baseline. Which one runs is decided
once per process — the first time a table scans, against the feature set the
process was delivered, and in the kernel by `kernel/core`'s
`resolve_accelerated_ops` — through the capability gate and the mandatory
self-verify, so a vector instruction is never
reached on a core that lacks it, and a candidate that disagrees with the
baseline on any vector cannot be selected. It is never benchmarked: the control
bytes it reads are tags derived from the per-boot hash key, and a benchmark is
a timing measurement over exactly that.

A target whose vector unit is off compiles no candidate at all and calls the
baseline directly, paying neither the resolved-cell load nor an indirect call.
That is riscv64 and wasm32, which have no candidate; `x86_64-tairix-none` and
`aarch64-unknown-none` carry SSE2 and NEON in their own feature sets.
The build script makes that decision from the target's own feature set, so no
target-conditional predicate appears in the crate source.

The portable baseline's byte-equality test is *exact*. The shorter
`(x - 1) & !x` zero-byte trick lets one lane's borrow forge a match in the
next, which would make the baseline disagree with the exactly-comparing vector
candidates and so make the self-verify's bit-identity conditional on the input.

## Measurement

Probe depth, not elapsed time, is what a hash table's performance is, and
unlike a stopwatch it is reproducible on any machine — so it, and not a timing,
is what the crate's tests gate. `HashMap::probe_groups` reports the control
groups a lookup examines and `allocated_bytes` the resident footprint. At the
maximum load factor:

| Counter | Gate | Measured |
|---|---|---|
| Control-group scans per hit | ≤ 1.5 | 1.12 with 7 168 live entries; 1.00 below the limit |
| Bytes per live entry | ≤ 1.15 × (entry + control byte) | 19.43 for a 16-byte entry, against a 19.55 budget — exactly 8/7 |
| `LruMap` scans per hit | ≤ 1.5 | 1.00 at 3 584 live entries, the table's load limit |
| `LruMap` bytes per live entry | ≤ 8/7 × (handle + control byte) + one node | 58.29 for a 16-byte entry, met exactly |
| `LruMap` allocations per touch, insert-over-a-freed-node, and eviction | 0 | 0, held across 4 096 rounds of churn and at 16, 1 024, and 16 384 entries |
| Ranges a placement walks | the live run count | 3, in a window of a billion slots — the count `overlapping` reports, and the bound on the work |
| Entries a window holds | one per live run | 1 for a 1 024-page scan-out mapping in a 262 144-slot window, where the bitmap it replaces grew to 1 026 bytes |

Against the `BTreeMap` these replace, over page-aligned `u64` keys of the shape
the DMA-window index uses:

| Live entries | `BTreeMap` key comparisons per lookup | Control-group scans per lookup |
|---|---|---|
| 64 | 8.1 | 1.000 |
| 1 000 | 12.2 | 1.001 |
| 100 000 | 21.7 | 1.032 |

The ordered map's cost grows with the log of the population, and each level is
a pointer chase to a node whose fill varies between a half and full. The
table's does not move, and its footprint is what the load factor alone
accounts for.

## Tests

Unit tests live next to the code. Beyond the ordinary map and set behaviour
they cover the portable scan against a naive lane-by-lane reference over every
adjacent-byte pair, every compiled vector candidate against the same reference
at every lane, churn through tens of thousands of tombstones with every
survivor still reachable and the footprint still bounded, and one drop per
value ever inserted across growth, overwrite, removal, retention, and an
abandoned owning iterator.

The intrusive list is gated on the same kind of number: a mid-list unlink
reaches the departing node and its two neighbours and writes exactly those
three links, identically over three nodes and over ten thousand. That is what
"constant time, no search" is, stated as something a regression shows up in.

`IndexedHeap` is gated on comparisons the same way: over 4 096 slots, raising
a slot to the top, sinking it to the bottom, removing one from the middle,
popping and inserting each stay within twice the height, and a walk of
everything at or below a bound compares only what it returns and the children
that stopped it. A shuffled history of twenty thousand operations agrees with a
slot-to-priority model at every step, and `tests/fuzz_collections.rs` drives
the same model over priorities crowded into ties and slots packed and sparse.

The sequence tier's tests hold each container to the same bar: one drop per
element ever taken, across a bulk push, a wrapped drain, an eviction and a
spill; a footprint that is the elements plus their indices and no heap block;
and — for `ArrayVec::retain`, the one operation that moves elements out and back
— that a predicate which unwinds leaves the vector describing exactly what it
has written back, so the unswept tail leaks where a double drop would be
unsound.

`OrderedMap` is held after every step of a seeded history of insertions,
removals, pops, retains and walks both to `alloc`'s ordered map and to its own
invariants — keys strictly ascending, every node but the root at least five
full and none past eleven, one leaf depth, each child naming its parent, every
live node reachable — and is gated on comparisons as `IndexedHeap` is: no
lookup compares more than five keys a level. Its mutable walks are driven from
both ends in turn and abandoned part-way, the tree checked whole after each.
`tests/heap_allocation_failure.rs` refuses its allocations part-way — each in
turn of an insertion splitting a leaf, an interior node or the root — and
holds every refusal to changing nothing, the range containers' with it; runs
reserved insertions of every key shape under an allocator that refuses
everything; meters its reads, walks and removals at no allocation; and meters
the footprint of ten thousand insertions against the figures above.

`tests/fuzz_collections.rs` drives the map against a plain association list
over deliberately colliding key streams, the recency map against a naive
`(membership, order)` list — the same membership, the same victim on every
eviction, and an arena that stops growing once the churn's bound is reached —
and the range containers against a per-element set and a naive entry list over
`(base, count)` streams whose lengths are the ones a caller does not control: a
`mem_map` page count, a run off a foreign volume, and the counts that run past
the top of the key space. Those sweeps run at the bottom of the key space, deep
inside it, and hard against `u64::MAX`, so the arithmetic that must saturate
rather than wrap is exercised rather than assumed. The ordered map is driven
against `alloc`'s over the same adversarial key streams: every range walked
from either end, a mutable walk of one, and a height no greater than its entry
count allows. `tests/fuzz_sequences.rs` drives the
sequence tier against naive models over arbitrary lengths and text — the
lengths are attacker-influenced by design, since a boot audit line carries
caller-controlled text into an `ArrayString` and a console ring takes whatever
a keyboard produces — and `tests/fuzz_intrusive.rs` drives two lists over one
shared store against naive order models, since a free list's link/unlink order
is whatever a process's allocation pattern makes it. All under
`cargo xtask fuzz`, and
`cargo xtask miri` interprets the whole suite under the undefined-behaviour
oracle — the table's `unsafe` core is why that stage exists. A test suite says
what the code computes; only an interpreter says whether a raw pointer stayed
in bounds, whether a slot was initialised before it was read, and whether two
`&mut` ever aliased.

## Stability

**experimental.** The public API may change until the first tagged release.

## `BitVec`

`BitVec` keeps its bits in words of 64 under two summary towers: one marks
the words holding a set bit, the other the words holding a clear bit within
the length, each level one bit per word of the level beneath. A search looks
in its own word, climbs while each level shows nothing past where it began,
and descends to the first mark it meets, so finding the next or previous set or
clear bit reads at most twice the tower's height — eleven levels for any
length a `usize` can name — however sparse the bits are. Writing a bit carries
a change up only as far as a level's word changes between zero and not.

Reads, writes, ranges, fills, searches and the walk of the set bits allocate
nothing. Growing is fallible and all or nothing: `try_resize` reserves the
words and both towers before it changes anything, and `try_reserve` makes room
ahead so a later growth allocates nothing. There is no `Clone`; `try_clone`
copies fallibly.

