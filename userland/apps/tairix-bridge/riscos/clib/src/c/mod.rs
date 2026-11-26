//! The C library, chunk 2 (`plans/RISCOS-EMULATOR.md` §10): the ANSI library
//! over the library kernel, its entries in the PRM's order.

pub(crate) mod alloc;
pub(crate) mod args;
pub(crate) mod backtrace;
pub(crate) mod c99;
pub(crate) mod ctype;
pub(crate) mod errno;
pub(crate) mod fenv;
pub(crate) mod fpa;
pub(crate) mod init;
pub(crate) mod locale;
pub(crate) mod math;
pub(crate) mod multibyte;
pub(crate) mod real;
pub(crate) mod report;
pub(crate) mod setjmp;
pub(crate) mod signal;
pub(crate) mod statics;
pub(crate) mod stdio;
pub(crate) mod stdlib;
pub(crate) mod string;
pub(crate) mod support;
pub(crate) mod swi;
pub(crate) mod time;

use tairix_riscos_kernel::module::Code;

use crate::chunk::{own, Entry};
use crate::kernel;

/// `Finalise`'s place among chunk 2's entries.
pub(crate) const FINALISE: usize = 181;

/// Chunk 2's entries: the PRM's, then RISC OS 5's `_swi`, `_swix`, `_vswi`
/// and `_vswix`; an entry RISC OS 5.30 gives as another's routine shares it.
pub(crate) const ENTRIES: [Entry; 187] = shared(own(ROUTINES));

/// The entries that share another's routine: (entry, chunk, its entry).
const SHARED: [(usize, u32, usize); 9] = [
    (4, 1, 27),
    (5, 1, 28),
    (6, 1, 31),
    (7, 1, 32),
    (8, 1, 34),
    (10, 1, 35),
    (39, 2, 38),
    (80, 2, 78),
    (182, 2, 84),
];

const fn shared(mut entries: [Entry; 187]) -> [Entry; 187] {
    let mut n = 0;
    while n < SHARED.len() {
        let (at, chunk, entry) = SHARED[n];
        entries[at] = Entry::Shared { chunk, entry };
        n += 1;
    }
    entries
}

/// Each entry's routine, in the PRM's order.
const ROUTINES: [Code; 187] = [
    signal::trap_handler,
    signal::uncaught_trap_handler,
    signal::event_handler,
    signal::unhandled_event_handler,
    kernel::stack::split_0frame,
    kernel::stack::split_frame,
    kernel::arith::udiv,
    kernel::arith::urem,
    kernel::arith::sdiv,
    support::divtest,
    kernel::arith::srem,
    support::multiply,
    support::read_byte_check,
    support::read_half_check,
    support::read_word_check,
    support::write_byte_check,
    support::write_half_check,
    support::write_word_check,
    init::main,
    init::exit,
    init::clib_initialise,
    backtrace::backtrace,
    support::count_next,
    support::count_with_position,
    support::stfp,
    support::ldfp,
    stdio::printf::printf_without_floats,
    stdio::printf::fprintf_without_floats,
    stdio::printf::sprintf_without_floats,
    time::clock,
    time::difftime,
    time::mktime,
    time::time,
    time::asctime,
    time::ctime,
    time::gmtime,
    time::localtime,
    time::strftime,
    string::memmove,
    string::memmove,
    string::strcpy,
    string::strncpy,
    string::strcat,
    string::strncat,
    string::memcmp,
    string::strcmp,
    string::strncmp,
    string::memchr,
    string::strchr,
    string::strcspn,
    string::strpbrk,
    string::strrchr,
    string::strspn,
    string::strstr,
    string::strtok,
    string::memset,
    string::strerror,
    string::strlen,
    real::atof,
    stdlib::atoi,
    stdlib::atoi,
    real::strtod,
    stdlib::strtol,
    stdlib::strtoul,
    stdlib::rand,
    stdlib::srand,
    alloc::calloc,
    alloc::free,
    alloc::malloc,
    alloc::realloc,
    signal::abort,
    init::atexit,
    init::exit,
    stdlib::getenv,
    stdlib::system,
    stdlib::bsearch,
    stdlib::qsort,
    stdlib::abs,
    stdlib::div,
    stdlib::abs,
    stdlib::div,
    stdio::open::remove,
    stdio::open::rename,
    stdio::open::tmpfile,
    stdio::open::tmpnam,
    stdio::open::fclose,
    stdio::fflush,
    stdio::open::fopen,
    stdio::open::freopen,
    stdio::setbuf,
    stdio::setvbuf,
    stdio::printf::printf,
    stdio::printf::fprintf,
    stdio::printf::sprintf,
    stdio::scan::scanf,
    stdio::scan::fscanf,
    stdio::scan::sscanf,
    stdio::printf::vprintf,
    stdio::printf::vfprintf,
    stdio::printf::vsprintf,
    stdio::printf::vfprintf_without_floats,
    stdio::fgetc,
    stdio::fgets,
    stdio::fputc,
    stdio::fputs,
    stdio::filbuf,
    stdio::fgetc,
    stdio::getchar,
    stdio::gets,
    stdio::flsbuf,
    stdio::fputc,
    stdio::putchar,
    stdio::puts,
    stdio::ungetc,
    stdio::fread,
    stdio::fwrite,
    stdio::fgetpos,
    stdio::fseek,
    stdio::fsetpos,
    stdio::ftell,
    stdio::rewind,
    stdio::clearerr,
    stdio::feof,
    stdio::ferror,
    stdio::perror,
    signal::ignore_handler,
    signal::error_marker,
    signal::default_handler,
    signal::signal,
    signal::raise,
    setjmp::setjmp,
    setjmp::longjmp,
    math::acos,
    math::asin,
    math::atan,
    math::atan2,
    math::cos,
    math::sin,
    math::tan,
    math::cosh,
    math::sinh,
    math::tanh,
    math::exp,
    math::frexp,
    math::ldexp,
    math::log,
    math::log10,
    math::modf,
    math::pow,
    math::sqrt,
    math::ceil,
    math::fabs,
    math::floor,
    math::fmod,
    locale::setlocale,
    ctype::isalnum,
    ctype::isalpha,
    ctype::iscntrl,
    ctype::isdigit,
    ctype::isgraph,
    ctype::islower,
    ctype::isprint,
    ctype::ispunct,
    ctype::isspace,
    ctype::isupper,
    ctype::isxdigit,
    ctype::tolower,
    ctype::toupper,
    signal::assert,
    string::memmove,
    string::block_set,
    locale::localeconv,
    multibyte::mblen,
    multibyte::mbtowc,
    multibyte::wctomb,
    multibyte::mbstowcs,
    multibyte::wcstombs,
    locale::strxfrm,
    locale::strcoll,
    init::clib_finalisemodule,
    init::clib_version,
    init::finalise,
    stdio::open::tmpnam,
    swi::swi,
    swi::swix,
    swi::vswi,
    swi::vswix,
];

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entries_named_by_place_are_their_routines() {
        assert!(
            core::ptr::fn_addr_eq(ROUTINES[FINALISE], init::finalise as Code),
            "Finalise"
        );
        assert!(
            matches!(ENTRIES[FINALISE], Entry::Own(_)),
            "Finalise a routine of its own"
        );
        assert_eq!(ENTRIES.len(), 187, "the PRM's chunk 2 and RISC OS 5's four");
    }

    #[test]
    fn a_shared_entry_s_routine_is_the_one_it_shares() {
        for (at, chunk, entry) in SHARED {
            let shared = match chunk {
                1 => crate::kernel::ENTRIES[entry],
                _ => ROUTINES[entry],
            };
            assert!(
                core::ptr::fn_addr_eq(ROUTINES[at], shared),
                "{at} is {chunk}.{entry}'s routine"
            );
        }
    }
}
