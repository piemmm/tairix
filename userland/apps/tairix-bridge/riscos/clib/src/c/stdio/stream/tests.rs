use tairix_fuzzseed::Prng;

use super::*;
use crate::c::testing::{Client, FREE, STATICS};

const FILE: u32 = FREE;
const EXTRA: u32 = FREE + 0x40;
const BUFFER: u32 = FREE + 0x100;

fn statics() -> Statics {
    Statics::of_base(STATICS)
}

#[test]
fn a_stream_s_blocks_read_back_as_they_were_written() {
    let mut prng = Prng::new(tairix_fuzzseed::start(
        "stream_blocks",
        "TAIRIX_CLIB_STREAM_SEED",
    ));
    let mut client = Client::new();
    let mut os = client.session.kernel.os();
    for _ in 0..200 {
        let mut file = [0u32; 10];
        let mut extra = [0u32; EXTRA_WORDS];
        file.iter_mut()
            .chain(extra.iter_mut())
            .for_each(|word| *word = prng.next_u32());
        file[9] = EXTRA;
        (extra[2], extra[4]) = (0, 0);
        os.write_words(FILE, &file).expect("written");
        os.write_words(EXTRA, &extra).expect("written");
        let stream = Stream::load(&mut os, FILE).expect("loaded");
        os.fill(FILE, 0x40 + EXTRA_BYTES, 0xEE).expect("cleared");
        stream.store(&mut os).expect("stored");
        let (mut file_after, mut extra_after) = ([0; 10], [0; EXTRA_WORDS]);
        os.read_words(FILE, &mut file_after).expect("read");
        os.read_words(EXTRA, &mut extra_after).expect("read");
        assert_eq!((file_after, extra_after), (file, extra));
    }
}

#[test]
fn the_words_read_alone_lie_where_a_stream_loads_them() {
    let mut client = Client::new();
    let mut os = client.session.kernel.os();
    let mut file: [u32; 10] = core::array::from_fn(|n| 0x100 + u32::try_from(n).expect("small"));
    file[9] = 0;
    os.write_words(FILE, &file).expect("written");
    let stream = Stream::load(&mut os, FILE).expect("loaded");
    assert_eq!(os.read_word(FILE + word::FLAG).expect("read"), stream.flag);
    assert_eq!(
        os.read_word(FILE + word::HANDLE).expect("read"),
        stream.handle
    );
}

#[test]
fn a_stream_with_no_extra_block_writes_only_its_file_block() {
    let mut client = Client::new();
    let mut os = client.session.kernel.os();
    os.fill(FILE, 0x40 + EXTRA_BYTES, 0xEE).expect("filled");
    let stream = Stream {
        at: FILE,
        flag: flag::READ,
        bytes: [1; 4],
        bufpos: 7,
        ..Stream::default()
    };
    stream.store(&mut os).expect("stored");
    assert_eq!(os.read_word(FILE + word::FLAG).expect("read"), flag::READ);
    assert_eq!(
        os.read_word(EXTRA).expect("read"),
        0xEEEE_EEEE,
        "nothing written where no extra block is"
    );
}

#[test]
fn the_streams_start_as_the_library_lays_them() {
    let mut client = Client::with_heap();
    let mut os = client.session.kernel.os();
    start(&mut os, statics()).expect("started");
    let first_extra = Stream::load(&mut os, file_at(statics(), 0))
        .expect("stdin")
        .extra;
    assert_ne!(first_extra, 0);
    for n in 0..STREAMS {
        let stream = Stream::load(&mut os, file_at(statics(), n)).expect("loaded");
        let (flag, bufsiz) = match n {
            0 => (flag::READ, BUFSIZ),
            1 | 2 => (flag::WRITE, BUFSIZ),
            _ => (0, 0),
        };
        assert_eq!(
            (
                stream.flag,
                stream.bufsiz,
                stream.ptr,
                stream.base,
                stream.handle
            ),
            (flag, bufsiz, 0, 0, 0),
            "stream {n}"
        );
        assert_eq!(
            stream.extra,
            first_extra + EXTRA_BYTES * n,
            "stream {n}'s extra block follows the one before"
        );
        let mut words = [u32::MAX; EXTRA_WORDS];
        os.read_words(stream.extra, &mut words).expect("read");
        assert_eq!(words, [0; EXTRA_WORDS], "stream {n}'s extra block cleared");
    }
}

#[test]
fn only_the_library_s_file_blocks_are_streams() {
    let statics = statics();
    for n in 0..STREAMS {
        assert!(is_stream(statics, file_at(statics, n)), "stream {n}");
        assert!(
            !is_stream(statics, file_at(statics, n) + 4),
            "within stream {n}"
        );
    }
    assert!(!is_stream(statics, file_at(statics, 0) - STREAM_BYTES));
    assert!(!is_stream(statics, file_at(statics, STREAMS)));
    assert!(!is_stream(statics, 0));
}

#[test]
fn one_character_is_pushed_back_at_most_and_a_position_counts_it_off() {
    let mut stream = Stream {
        flag: flag::READ | flag::EOF | flag::SET_UP,
        icnt: 5,
        ptr: BUFFER + 3,
        base: BUFFER,
        bufpos: 100,
        ..Stream::default()
    };
    assert_eq!(stream.tell(), 103);
    assert_eq!(stream.unget(EOF), EOF, "EOF is never pushed back");
    assert_eq!(stream.unget(0x141), 0x41, "the character as a byte");
    assert_eq!(
        (stream.bytes[1], stream.saved_icnt, stream.icnt),
        (0x41, 5, 0)
    );
    assert_eq!(
        stream.flag & (flag::UNGOT | flag::EOF),
        flag::UNGOT,
        "the end of the file forgotten"
    );
    assert_eq!(stream.tell(), 102);
    assert_eq!(stream.unget(u32::from(b'B')), EOF, "a second refused");
    assert_eq!(stream.bytes[1], 0x41);
}

#[test]
fn a_stream_in_error_refuses_input_until_its_error_is_cleared() {
    let mut client = Client::new();
    let mut os = client.session.kernel.os();
    let mut stream = Stream {
        flag: flag::READ | flag::ERROR | flag::SET_UP,
        icnt: -3,
        ocnt: -4096,
        ptr: BUFFER,
        base: BUFFER,
        data_end: BUFFER + 3,
        bufsiz: BUFSIZ,
        bufsize: BUFSIZ,
        ..Stream::default()
    };
    assert_eq!(stream.get(&mut os, statics()).expect("refused"), EOF);
    assert_eq!(
        (stream.icnt, stream.ocnt, stream.ptr),
        (0, 0, BUFFER),
        "both counts sent to the library"
    );
    stream.flag &= !flag::ERROR;
    os.write_bytes(BUFFER, b"abc").expect("written");
    assert_eq!(
        stream.get(&mut os, statics()).expect("read"),
        u32::from(b'a')
    );
    assert_eq!(stream.icnt, 2);
}

#[test]
fn output_is_refused_on_a_stream_not_open_to_write_or_holding_input() {
    let mut client = Client::new();
    let mut os = client.session.kernel.os();
    for state in [
        flag::READ | flag::SET_UP,
        flag::READ | flag::WRITE | flag::READ_VALID | flag::SET_UP,
    ] {
        let mut stream = Stream {
            flag: state,
            icnt: 2,
            ocnt: -1,
            ptr: BUFFER + 1,
            base: BUFFER,
            ..Stream::default()
        };
        assert_eq!(stream.put(&mut os, statics(), b'x').expect("refused"), EOF);
        assert_eq!(
            (stream.icnt, stream.ocnt, stream.flag & flag::ERROR),
            (0, 0, flag::ERROR),
            "{state:#X}"
        );
    }
}
