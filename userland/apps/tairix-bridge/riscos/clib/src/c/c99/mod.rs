//! RISC OS 5's C99 library, chunk 5, in RISC OS 5.30's order, and the
//! chunks that give parts of it again under their own numbers: chunk 7 the
//! mathematics, chunk 8 the complex arithmetic, chunk 9 the rest — each
//! entry the very routine chunk 5's is, as RISC OS 5.30's are.

mod complex;
mod elementary;
mod exact;
mod numbers;

use tairix_riscos_arm::fpa::{Exception, Precision};
use tairix_riscos_kernel::module::Code;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_softfloat::fpa_words;

use super::fpa::{answer_as_client, as_client, quietly, Unit};
use super::stdio::{self, open, printf, scan};
use super::{alloc, ctype, fenv, init, real as reading, signal, stdlib, time};
use crate::chunk::{own, Entry};
use complex as cx;
use elementary as el;
use numbers as r;

/// The precision a float or double answer is worked in.
const fn precision(float: bool) -> Precision {
    if float {
        Precision::Single
    } else {
        Precision::Double
    }
}

/// `3π/4`, the angle of an infinity in the second quadrant.
const FRAC_3_PI_4: f64 = 3.0 * core::f64::consts::FRAC_PI_4;

/// Whether the finite `value` lies beyond a float's range: where RISC OS
/// 5.30's float functions overflow with the flags the client had on entry.
fn overflows_a_float(value: f64) -> bool {
    value.is_finite() && quietly(|own| own.single(value)).is_infinite()
}

/// `work`'s double answered in F0.
fn give_double(
    os: &mut Os<'_>,
    work: impl FnOnce(&mut Unit) -> Result<f64, Exception>,
) -> Result<(), Fail> {
    answer_as_client(os, work)
}

/// `work`'s answer rounded to a float in F0, as the client's FPA rounds it.
fn give_single(
    os: &mut Os<'_>,
    work: impl FnOnce(&mut Unit) -> Result<f64, Exception>,
) -> Result<(), Fail> {
    answer_as_client(os, |unit| {
        let value = work(unit)?;
        unit.single(value)
    })
}

/// `work`'s word answered in R0.
fn give_word(
    os: &mut Os<'_>,
    regs: &mut Regs,
    work: impl FnOnce(&mut Unit) -> Result<i32, Exception>,
) -> Result<(), Fail> {
    if let Some(value) = as_client(os, work)? {
        regs.r[0] = value.cast_unsigned();
    }
    Ok(())
}

/// `work`'s 64-bit answer in R0 and R1, its low word first.
fn give_long(
    os: &mut Os<'_>,
    regs: &mut Regs,
    work: impl FnOnce(&mut Unit) -> Result<i64, Exception>,
) -> Result<(), Fail> {
    if let Some(value) = as_client(os, work)? {
        let [high, low] = fpa_words(value.cast_unsigned());
        [regs.r[0], regs.r[1]] = [low, high];
    }
    Ok(())
}

/// Chunk 5's routines, in RISC OS 5.30's order.
const ROUTINES: [Code; 189] = [
    r::fpclassify::<true>,
    r::fpclassify::<false>,
    r::signbit,
    r::signbit,
    r::copysign_in_registers::<false>,
    r::copysign_in_registers::<true>,
    reading::nan,
    reading::nan,
    r::nextafter::<false>,
    r::nextafter::<true>,
    r::fdim::<false>,
    r::fdim::<true>,
    r::extreme::<false, true>,
    r::extreme::<true, true>,
    r::extreme::<false, false>,
    r::extreme::<true, false>,
    r::fabsf,
    r::hypot::<false>,
    r::hypot::<true>,
    fenv::feclearexcept,
    fenv::fegetexceptflag,
    fenv::feraiseexcept,
    fenv::fesetexceptflag,
    fenv::fetestexcept,
    fenv::fegetround,
    fenv::fesetround,
    fenv::fegetenv,
    fenv::feholdexcept,
    fenv::fesetenv,
    fenv::feupdateenv,
    printf::snprintf_without_floats,
    printf::snprintf,
    printf::vsnprintf,
    scan::vfscanf,
    scan::vscanf,
    scan::vsscanf,
    r::round_to::<true, { r::UP }>,
    r::round_to::<true, { r::DOWN }>,
    r::nearbyint::<false>,
    r::nearbyint::<true>,
    r::round_to::<false, { r::NEAREST }>,
    r::round_to::<true, { r::NEAREST }>,
    r::lrint,
    r::lrint,
    r::round,
    r::round,
    r::lround,
    r::lround,
    r::round_to::<false, { r::ZERO }>,
    r::round_to::<true, { r::ZERO }>,
    r::remainder::<false>,
    r::remainder::<true>,
    stdlib::llabs,
    stdlib::lldiv,
    stdlib::atoll,
    stdlib::strtoll,
    stdlib::strtoull,
    stdlib::llabs,
    stdlib::lldiv,
    stdlib::strtoll,
    stdlib::strtoull,
    signal::assert2,
    init::exit_immediately,
    el::single_instruction::<{ el::ACS }>,
    el::single_instruction::<{ el::ASN }>,
    el::single_instruction::<{ el::ATN }>,
    el::atan2f,
    el::single_instruction::<{ el::COS }>,
    el::single_instruction::<{ el::SIN }>,
    el::single_instruction::<{ el::TAN }>,
    el::near_unity::<{ el::ACOSH }, false>,
    el::near_unity::<{ el::ACOSH }, true>,
    el::near_unity::<{ el::ASINH }, false>,
    el::near_unity::<{ el::ASINH }, true>,
    el::near_unity::<{ el::ATANH }, false>,
    el::near_unity::<{ el::ATANH }, true>,
    el::single_instruction::<{ el::EXP }>,
    el::exp2,
    el::exp2f,
    el::near_unity::<{ el::EXPM1 }, false>,
    el::near_unity::<{ el::EXPM1 }, true>,
    r::frexpf,
    r::ilogb,
    r::ilogb,
    r::scalbn::<true>,
    el::single_instruction::<{ el::LGN }>,
    el::single_instruction::<{ el::LOG }>,
    el::near_unity::<{ el::LOG1P }, false>,
    el::near_unity::<{ el::LOG1P }, true>,
    el::log2,
    el::log2f,
    r::logb::<false>,
    r::logb::<true>,
    r::modff,
    r::fmodf,
    r::scalbn::<false>,
    r::scalbn::<true>,
    r::scalbn::<false>,
    r::scalbn::<true>,
    el::cbrt,
    el::cbrtf,
    el::powf,
    el::single_instruction::<{ el::SQT }>,
    el::error_function::<{ el::ERF }, false>,
    el::error_function::<{ el::ERF }, true>,
    el::error_function::<{ el::ERFC }, false>,
    el::error_function::<{ el::ERFC }, true>,
    el::gamma::<{ el::LGAMMA }, false>,
    el::gamma::<{ el::LGAMMA }, true>,
    el::gamma::<{ el::TGAMMA }, false>,
    el::gamma::<{ el::TGAMMA }, true>,
    r::nextafter::<false>,
    r::nextafter::<true>,
    r::fma::<true>,
    ctype::isblank,
    reading::strtof,
    r::copysign::<false>,
    r::copysign::<true>,
    r::fma::<false>,
    r::remquo::<false>,
    r::remquo::<true>,
    r::llrint,
    r::llrint,
    r::llround::<false>,
    r::llround::<true>,
    cx::helper::<{ cx::MULTIPLY }, false>,
    cx::helper::<{ cx::MULTIPLY }, true>,
    cx::helper::<{ cx::DIVIDE }, false>,
    cx::helper::<{ cx::DIVIDE }, true>,
    cx::helper::<{ cx::DIVIDE_REVERSED }, false>,
    cx::helper::<{ cx::DIVIDE_REVERSED }, true>,
    cx::function::<{ cx::ACOS }, false>,
    cx::function::<{ cx::ACOS }, true>,
    cx::function::<{ cx::ASIN }, false>,
    cx::function::<{ cx::ASIN }, true>,
    cx::function::<{ cx::ATAN }, false>,
    cx::function::<{ cx::ATAN }, true>,
    cx::function::<{ cx::COS }, false>,
    cx::function::<{ cx::COS }, true>,
    cx::function::<{ cx::SIN }, false>,
    cx::function::<{ cx::SIN }, true>,
    cx::function::<{ cx::TAN }, false>,
    cx::function::<{ cx::TAN }, true>,
    cx::function::<{ cx::ACOSH }, false>,
    cx::function::<{ cx::ACOSH }, true>,
    cx::function::<{ cx::ASINH }, false>,
    cx::function::<{ cx::ASINH }, true>,
    cx::function::<{ cx::ATANH }, false>,
    cx::function::<{ cx::ATANH }, true>,
    cx::function::<{ cx::COSH }, false>,
    cx::function::<{ cx::COSH }, true>,
    cx::function::<{ cx::SINH }, false>,
    cx::function::<{ cx::SINH }, true>,
    cx::function::<{ cx::TANH }, false>,
    cx::function::<{ cx::TANH }, true>,
    cx::function::<{ cx::EXP }, false>,
    cx::function::<{ cx::EXP }, true>,
    cx::function::<{ cx::LOG }, false>,
    cx::function::<{ cx::LOG }, true>,
    cx::cabs::<false>,
    cx::cabs::<true>,
    cx::cpow,
    cx::cpowf,
    cx::function::<{ cx::SQRT }, false>,
    cx::function::<{ cx::SQRT }, true>,
    cx::carg::<false>,
    cx::carg::<true>,
    cx::part::<true, false>,
    cx::part::<true, true>,
    cx::conj::<false>,
    cx::conj::<true>,
    cx::cproj::<false>,
    cx::cproj::<true>,
    cx::part::<false, false>,
    cx::part::<false, true>,
    stdio::fgetpos64,
    open::fopen64,
    open::freopen64,
    stdio::fseeko64,
    stdio::fsetpos64,
    stdio::ftello64,
    open::tmpfile64,
    alloc::aligned_alloc,
    init::at_quick_exit,
    init::quick_exit,
    time::timespec_get,
    el::hyperbolic_float::<{ el::COSH }>,
    el::hyperbolic_float::<{ el::SINH }>,
    el::hyperbolic_float::<{ el::TANH }>,
];

/// Chunk 5's entries, every one its own routine.
pub(crate) const ENTRIES: [Entry; 189] = own(ROUTINES);

/// `N` entries sharing chunk 5's routines, those from each run's first in
/// turn.
const fn sharing<const N: usize>(runs: &[(u32, usize, usize)]) -> [Entry; N] {
    let mut entries = [Entry::Shared { chunk: 5, entry: 0 }; N];
    let (mut at, mut run) = (0, 0);
    while run < runs.len() {
        let (chunk, from, count) = runs[run];
        let mut n = 0;
        while n < count {
            entries[at] = Entry::Shared {
                chunk,
                entry: from + n,
            };
            at += 1;
            n += 1;
        }
        run += 1;
    }
    assert!(at == N, "the runs fill the chunk");
    entries
}

/// Chunk 7: the mathematics.
pub(crate) const MATHS: [Entry; 63] = sharing(&[(5, 63, 51), (5, 116, 9), (5, 186, 3)]);

/// Chunk 8: the complex arithmetic.
pub(crate) const COMPLEX: [Entry; 50] = sharing(&[(5, 125, 50)]);

/// Chunk 9: the rest, `_vswi` and `_vswix` chunk 2's, and an `atexit` of its
/// own.
pub(crate) const EXTRAS: [Entry; 25] = {
    let shared: [Entry; 24] = sharing(&[(5, 54, 9), (5, 114, 2), (5, 175, 11), (2, 185, 2)]);
    let mut entries = [Entry::Own(init::atexit); 25];
    let mut n = 0;
    while n < shared.len() {
        entries[n] = shared[n];
        n += 1;
    }
    entries
};

#[cfg(test)]
mod tests;
