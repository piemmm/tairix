use super::*;

fn heap(span: usize) -> Memory {
    Memory::new(Vec::new(), span).unwrap()
}

#[test]
fn words_read_back_as_written_up_to_the_heap_end() {
    let mut memory = heap(64);
    memory.put32(60, 0x1122_3344);
    memory.put16(1, 0xBEEF);
    assert_eq!(memory.get32(60), 0x1122_3344);
    assert_eq!(memory.get16(1), 0xBEEF);
    assert_eq!(memory.get8(63), 0x11);
    assert!(!memory.fault.get());
}

#[test]
fn an_access_past_the_heap_faults_and_writes_nothing() {
    let mut memory = heap(32);
    memory.put32(30, u32::MAX);
    assert!(memory.fault.get(), "two of its bytes lie past the heap");
    assert_eq!(
        memory.bytes, [0; 32],
        "a write reaching past the heap writes no byte of it"
    );
    let memory = heap(32);
    assert_eq!(memory.get16(31), 0);
    assert!(memory.fault.get());
    let memory = heap(32);
    assert_eq!(memory.get8(u32::MAX), 0);
    assert!(memory.fault.get());
}

#[test]
fn states_are_read_whole_and_no_more_than_a_context_holds() {
    let mut memory = heap(2048);
    for k in 0..4u8 {
        memory.set_state(10 + 6 * u32::from(k), [k, 10 + k, 1, 2, 3, 4]);
    }
    let view = memory.states(10, 4).unwrap();
    for (k, state) in (0u8..).zip(view.as_chunks::<6>().0) {
        assert_eq!(*state, [k, 10 + k, 1, 2, 3, 4]);
    }
    assert!(memory.states(0, MAX_STATES).is_some());
    assert!(
        memory.states(0, MAX_STATES + 1).is_none(),
        "no context holds more than 256"
    );
    assert!(memory.states(2040, 2).is_none(), "past the heap");
}

#[test]
fn a_copy_moves_every_byte_and_one_past_the_heap_faults() {
    let mut memory = heap(4096);
    let pattern: Vec<u8> = (0..1536usize)
        .map(|k| (k * 7 + 3).to_le_bytes()[0])
        .collect();
    for (k, &byte) in (0u32..).zip(&pattern) {
        memory.put8(100 + k, u32::from(byte));
    }
    memory.copy(2000, 100, 1536);
    let copied: Vec<u8> = (0..1536).map(|k| memory.get8(2000 + k)).collect();
    assert_eq!(copied, pattern);
    assert!(!memory.fault.get());
    memory.copy(3000, 100, 1536);
    assert!(memory.fault.get());
}

#[test]
fn a_heap_long_enough_is_kept_whole_and_a_short_one_replaced_zeroed() {
    let memory = Memory::new(alloc::vec![7; 100], 50).unwrap();
    assert_eq!(
        memory.bytes, [7; 100],
        "the model before's heap is laid out on again, unwritten"
    );
    let memory = Memory::new(alloc::vec![7; 10], 50).unwrap();
    assert_eq!(memory.bytes, [0; 50]);
}
