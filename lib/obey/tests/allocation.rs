//! The command-line machinery under the heap's refusals and against its
//! growth.
//!
//! Every request a reading makes is refused in turn: each refusal is answered
//! as `SystemHeapFull`, never an abort, and nothing outlives the answer. A
//! result built a piece at a time grows by doubling, so its requests are
//! logarithmic in its length, not one a piece.

use tairix_fuzzseed::meter::{metered, refused_at_each, Metered};
use tairix_obey::args::substitute;
use tairix_obey::command::{read_only, Observer};
use tairix_obey::expr::evaluate;
use tairix_obey::readargs::read_args;
use tairix_obey::vars::{set, Request};
use tairix_obey::{Budget, Error, Store, Value, Variables};
use tairix_util::fallible;

#[global_allocator]
static ALLOCATOR: Metered = Metered;

/// A store holding a string and a macro that names it, made fallibly so a
/// refusal while it is made is answered too.
fn store() -> Result<Store, Error> {
    let mut vars = Store::with_room(8, 256).ok_or(Error::SystemHeapFull)?;
    let text = |bytes: &[u8]| fallible::copied(bytes).ok_or(Error::SystemHeapFull);
    vars.assign(b"A", Value::String(text(b"alpha")?))?;
    vars.assign(b"M", Value::Macro(text(b"<A>-<A>")?))?;
    Ok(vars)
}

struct Ignored;

impl Observer for Ignored {
    fn icon_sprites(&mut self, _: &[u8], _: &mut Store, _: &mut Budget) {}
}

#[test]
fn every_refusal_is_answered_as_a_full_heap() {
    let refused = |error: &Error| *error == Error::SystemHeapFull;
    refused_at_each(
        || substitute(b"one two", b"%0 and %1 %*0", true, 256),
        refused,
    );
    refused_at_each(
        || evaluate(b"\"ab\"+\"cd\"+\"ef\"", &mut store()?, &mut Budget::new(16)),
        refused,
    );
    refused_at_each(
        || {
            let mut vars = store()?;
            let template = b"from/a,to,quiet/s";
            read_args(
                template,
                b"source dest -quiet",
                &mut vars,
                &mut Budget::new(16),
                0x8000,
                256,
            )
        },
        refused,
    );
    refused_at_each(
        || {
            let mut vars = store()?;
            set(
                &mut vars,
                b"New",
                Request::String(b"<A> and <M>"),
                256,
                &mut Budget::new(16),
            )
        },
        refused,
    );
    refused_at_each(
        || {
            let mut vars = store()?;
            let file = b"Set X 1\nIf 1 Then Set Y <X><M>\n";
            read_only(
                file,
                b"$.Dir",
                &mut vars,
                &mut Ignored,
                &mut Budget::new(16),
            )
        },
        refused,
    );
}

#[test]
fn a_substitution_grows_by_doubling() {
    let template = b"%0".repeat(2000);
    let (out, metering) = metered(|| substitute(b"X", &template, false, 1 << 20));
    assert_eq!(
        out.map(|out| out.len()),
        Ok(2001),
        "each argument and the end"
    );
    assert!(
        metering.allocations <= 32,
        "{} requests",
        metering.allocations
    );
}
