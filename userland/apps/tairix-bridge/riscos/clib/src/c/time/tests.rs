extern crate std;

use std::vec::Vec;

use tairix_fuzzseed::Prng;
use tairix_riscos_kernel::module::Code;

use super::*;
use crate::c::locale::setlocale;
use crate::c::testing::{Client, FREE, STATICS};

mod recorded;

use recorded::{
    ASCTIME, CTIME, GMTIME, LOCALTIME, MKTIME, STRFTIME_C, STRFTIME_SHORT, STRFTIME_UK,
};

const TM: u32 = FREE;
const TIME: u32 = FREE + 0x40;
const OUT: u32 = FREE + 0x100;
const FORMAT: u32 = FREE + 0x200;
const EMPTY: u32 = FREE + 0x280;

fn put_tm(client: &mut Client, at: u32, tm: [i32; 9]) {
    client
        .session
        .kernel
        .os()
        .write_words(at, &tm.map(i32::cast_unsigned))
        .expect("written");
}

fn tm_at(client: &mut Client, at: u32) -> [i32; 9] {
    let mut words = [0; 9];
    client
        .session
        .kernel
        .os()
        .read_words(at, &mut words)
        .expect("read");
    words.map(u32::cast_signed)
}

/// The `struct tm` `entry` answers for `time`, which must be in `buffer`.
fn tm_for(client: &mut Client, entry: Code, time: u32, buffer: u32) -> [i32; 9] {
    client.put(TIME, &time.to_le_bytes());
    assert_eq!(
        client.answer(entry, &[TIME]),
        STATICS + buffer,
        "the library's buffer"
    );
    tm_at(client, STATICS + buffer)
}

/// The zone configured `quarters` of an hour from UTC, daylight saving as
/// `dst` says.
fn configure(client: &mut Client, quarters: i8, dst: bool) {
    let mut os = client.session.kernel.os();
    let alarm = read_cmos(&mut os, cmos::ALARM_AND_TIME).expect("read");
    let alarm = if dst {
        alarm | cmos::DST
    } else {
        alarm & !cmos::DST
    };
    for (location, value) in [
        (cmos::TIME_ZONE, quarters.to_le_bytes()[0]),
        (cmos::ALARM_AND_TIME, alarm),
    ] {
        let mut regs = Regs::default();
        regs.r[..3].copy_from_slice(&[cmos::WRITE_CMOS, u32::from(location), u32::from(value)]);
        os.perform(X | kos::BYTE, &mut regs).expect("written");
    }
}

#[test]
fn gmtime_answers_as_genuine_does() {
    let mut client = Client::new();
    for (time, genuine) in GMTIME {
        assert_eq!(
            tm_for(&mut client, gmtime, time, at::GMTIME),
            genuine,
            "gmtime({time:#X})"
        );
    }
}

#[test]
fn localtime_answers_as_genuine_does_in_every_configuration() {
    let mut client = Client::new();
    for (quarters, dst, times) in LOCALTIME {
        configure(&mut client, quarters, dst);
        for (time, genuine) in times {
            let tm = tm_for(&mut client, localtime, time, at::LOCALTIME);
            assert_eq!(
                tm, genuine,
                "localtime({time:#X}), zone {quarters}, daylight saving {dst}"
            );
        }
    }
}

#[test]
fn ctime_and_asctime_write_genuine_s_text() {
    let mut client = Client::new();
    for (time, text) in CTIME {
        client.put(TIME, &time.to_le_bytes());
        assert_eq!(client.answer(ctime, &[TIME]), STATICS + at::TIME_TEXT);
        assert_eq!(
            client.string(STATICS + at::TIME_TEXT),
            text,
            "ctime({time:#X})"
        );
    }
    let (tm, text) = ASCTIME;
    put_tm(&mut client, TM, tm);
    assert_eq!(client.answer(asctime, &[TM]), STATICS + at::TIME_TEXT);
    assert_eq!(client.string(STATICS + at::TIME_TEXT), text);
}

#[test]
fn asctime_writes_any_struct_tm_within_its_buffer() {
    let mut client = Client::new();
    let widest = [i32::MIN; 9];
    put_tm(&mut client, TM, widest);
    client.answer(asctime, &[TM]);
    let text = client.string(STATICS + at::TIME_TEXT);
    assert_eq!(
        text.len() + 1,
        TIME_TEXT_BYTES as usize,
        "every number eleven characters"
    );
    assert!(
        text.starts_with(b"??? ???-2147483648 -2147483648:"),
        "{text:?}"
    );
    assert_eq!(
        tm_at(&mut client, STATICS + at::GMTIME),
        [0; 9],
        "the next buffer untouched"
    );
}

#[test]
fn mktime_normalises_as_genuine_does() {
    let mut client = Client::new();
    for (given, answer, after) in MKTIME {
        put_tm(&mut client, TM, given);
        assert_eq!(client.answer(mktime, &[TM]), answer, "mktime({given:?})");
        assert_eq!(
            tm_at(&mut client, TM),
            after,
            "mktime({given:?})'s struct tm"
        );
    }
}

#[test]
fn mktime_answers_no_time_outside_time_t_s_range() {
    let mut client = Client::new();
    for (tm, normal) in [
        (
            [59, 59, 23, 31, 11, 69, 0, 0, 0],
            [59, 59, 23, 31, 11, 69, 3, 364, 0],
        ),
        (
            [16, 28, 6, 6, 1, 206, 0, 0, 0],
            [16, 28, 6, 6, 1, 206, 0, 36, 0],
        ),
        ([0, 0, 0, 1, 0, 70, 0, 0, 1], [0, 0, 0, 1, 0, 70, 4, 0, 1]),
    ] {
        put_tm(&mut client, TM, tm);
        assert_eq!(client.answer(mktime, &[TM]), NO_TIME, "mktime({tm:?})");
        assert_eq!(tm_at(&mut client, TM), normal, "still normalised");
    }
    let past_int = [0, 0, 0, 1, i32::MAX, i32::MAX, 0, 0, 0];
    put_tm(&mut client, TM, past_int);
    assert_eq!(client.answer(mktime, &[TM]), NO_TIME);
    assert_eq!(tm_at(&mut client, TM), past_int, "left as it was");
}

#[test]
fn mktime_takes_back_every_time_gmtime_gives() {
    let mut prng = Prng::new(tairix_fuzzseed::start(
        "mktime_round_trip",
        "TAIRIX_CLIB_MKTIME_SEED",
    ));
    let mut client = Client::new();
    for _ in 0..2_000 {
        let time = prng.next_u32();
        if time == NO_TIME {
            continue;
        }
        let tm = tm_for(&mut client, gmtime, time, at::GMTIME);
        let mut shuffled = tm;
        shuffled[6..8].copy_from_slice(&[-9, 4000]);
        put_tm(&mut client, TM, shuffled);
        assert_eq!(
            client.answer(mktime, &[TM]),
            time,
            "mktime(gmtime({time:#X}))"
        );
        assert_eq!(
            tm_at(&mut client, TM),
            tm,
            "weekday and day of year set again"
        );
    }
}

#[test]
fn difftime_answers_the_difference_in_seconds() {
    let mut client = Client::new();
    for (later, earlier, difference) in
        [(10, 3, 7.0), (3, 10, -7.0), (0, u32::MAX, -4_294_967_295.0)]
    {
        client.call(difftime, &[later, earlier]);
        let bits = client.session.kernel.os().fpa_double(0).expect("F0");
        assert_eq!(
            bits,
            f64::to_bits(difference),
            "difftime({later}, {earlier})"
        );
    }
}

/// `strftime` of `format` and `tm` into `room` bytes at [`OUT`], cleared
/// first: its answer.
fn formatted(client: &mut Client, format: &[u8], tm: &[i32; 9], room: u32) -> u32 {
    client.put(OUT, &[0; 80]);
    client.put(FORMAT, format);
    client.put(FORMAT + u32::try_from(format.len()).expect("short"), &[0]);
    put_tm(client, TM, *tm);
    client.answer(strftime, &[OUT, room, FORMAT, TM])
}

#[test]
fn strftime_in_the_c_locale_writes_as_genuine_does() {
    let mut client = Client::new();
    for (format, tm, answer, text) in STRFTIME_C {
        assert_eq!(
            formatted(&mut client, format, tm, 64),
            answer,
            "{:?}",
            format.escape_ascii()
        );
        assert_eq!(client.string(OUT), text, "{:?}", format.escape_ascii());
    }
}

/// The current time zone's name, as the territory gives it.
fn zone_name(client: &mut Client) -> Vec<u8> {
    let mut regs = Regs::default();
    client
        .session
        .kernel
        .os()
        .perform(X | territory_swi::READ_CURRENT_TIME_ZONE, &mut regs)
        .expect("a zone");
    client.string(regs.r[0])
}

#[test]
fn strftime_in_a_territory_s_locale_writes_its_words_and_forms() {
    let mut client = Client::new();
    client.put(EMPTY, &[0]);
    assert_ne!(
        client.answer(setlocale, &[31, EMPTY]),
        0,
        "the native territory"
    );
    let zone = zone_name(&mut client);
    for (format, tm, answer, text) in STRFTIME_UK {
        // Genuine's machine kept GMT; the territory names whatever zone the host keeps.
        let (answer, text) = if format == b"%Z" {
            (u32::try_from(zone.len()).expect("short"), zone.as_slice())
        } else {
            (answer, text)
        };
        assert_eq!(
            formatted(&mut client, format, tm, 64),
            answer,
            "{:?}",
            format.escape_ascii()
        );
        assert_eq!(client.string(OUT), text, "{:?}", format.escape_ascii());
    }
}

#[test]
fn strftime_answers_nought_where_its_answer_would_not_fit() {
    let mut client = Client::new();
    for (room, answer, bytes) in STRFTIME_SHORT {
        assert_eq!(
            formatted(&mut client, b"%A", &recorded::TM_A, room),
            answer,
            "{room} bytes"
        );
        assert_eq!(client.bytes(OUT, bytes.len()), bytes, "{room} bytes");
    }
    client.put(OUT, &[0xFF; 8]);
    client.put(FORMAT, b"%A\0");
    put_tm(&mut client, TM, recorded::TM_A);
    assert_eq!(client.answer(strftime, &[OUT, 5, FORMAT, TM]), 0);
    assert_eq!(client.bytes(OUT, 6), b"Tuesd\xFF", "no terminator written");
    assert_eq!(
        client.answer(strftime, &[OUT, 0, FORMAT, TM]),
        0,
        "no room at all"
    );
}

#[test]
fn clock_counts_from_the_program_s_start() {
    let mut client = Client::new();
    let now = time_now(&mut client);
    client.put(
        STATICS + at::CLOCK_START,
        &now.wrapping_sub(25).to_le_bytes(),
    );
    assert_eq!(client.answer(clock, &[]), 25);
    client.put(
        STATICS + at::CLOCK_START,
        &now.wrapping_add(1).to_le_bytes(),
    );
    assert_eq!(
        client.answer(clock, &[]),
        u32::MAX,
        "as the monotonic clock wraps"
    );
}

fn time_now(client: &mut Client) -> u32 {
    monotonic_time(&mut client.session.kernel.os()).expect("the monotonic time")
}

#[test]
fn time_answers_the_real_time_clock_s_seconds_since_1970() {
    let mut client = Client::new();
    let millennium = 946_684_800;
    assert_eq!(
        client.answer(time, &[0]),
        millennium,
        "the session's clock starts at 2000"
    );
    client.put(TIME, &[0; 4]);
    assert_eq!(client.answer(time, &[TIME]), millennium);
    assert_eq!(client.word(TIME), millennium, "stored where asked");
}

#[test]
fn time_answers_no_time_before_1970_or_past_an_unsigned_word_of_seconds() {
    let mut client = Client::new();
    for (centiseconds, answer) in [
        (0u64, NO_TIME),
        (100, NO_TIME),
        (0x9A_1234_5678, NO_TIME),
        (0x55_FFFF_FFFF, 0x587E_7742),
    ] {
        let mut block = [0u8; 8];
        block[0] = word::set::UTC;
        block[1..6].copy_from_slice(&centiseconds.to_le_bytes()[..5]);
        client.put(TIME, &block);
        let mut set = Regs::default();
        set.r[..2].copy_from_slice(&[word::SET_CLOCK, TIME]);
        client
            .session
            .kernel
            .os()
            .perform(X | kos::WORD, &mut set)
            .expect("the clock set");
        assert_eq!(
            client.answer(time, &[0]),
            answer,
            "{centiseconds:#X} centiseconds since 1900"
        );
    }
}
