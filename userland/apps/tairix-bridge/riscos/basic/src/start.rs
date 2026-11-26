//! BASIC starting as the application and leaving: its workspace laid out,
//! the environment handlers it runs under installed — the error, escape,
//! exit and `UpCall` handlers, as BASIC V installs them — and given back when
//! it quits or another application takes its place; `*BASIC`'s options.

use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Ended, Fail, Os, Regs, Unwind};
use tairix_riscos_kernel::swi::os::environment::{ERROR, ESCAPE, EXIT, UPCALL};
use tairix_riscos_kernel::swi::os::exit::ABEX;
use tairix_riscos_kernel::swi::{os as swi, X};

use crate::machine::{flag, Machine, Mode, Trap};
use crate::module::{module_of, routine};
use crate::value::{Dialect, Text};
use crate::workspace::{self as ws, reg};

/// The environment handlers BASIC installs, with the routine each is and
/// whether the string accumulator is its buffer.
const HANDLERS: [(u32, usize, bool); 4] = [
    (ERROR, routine::ERROR, true),
    (ESCAPE, routine::ESCAPE, false),
    (EXIT, routine::EXIT, false),
    (UPCALL, routine::UPCALL, false),
];
/// `@%`'s value at the start: general layout, ten digits, ten wide.
const AT_PERCENT: u32 = 0x90A;
/// `LISTO`'s at the start, as recorded.
const LISTO: u32 = 0;

/// The application's start: the workspace laid out, the handlers taken, the
/// options followed, then the machine's first slice.
pub(crate) fn start(os: &mut Os<'_>, tail: u32, dialect: Dialect) -> Result<(), Fail> {
    let (tail, _) = os.read_until(tail, crate::value::STRING_MOST, |b| b < b' ')?;
    let mut env = Regs::default();
    os.perform(X | swi::GET_ENV, &mut env)?;
    let limit = env.r[1];
    lay_out(os, limit, dialect)?;
    take_handlers(os, module_of(dialect))?;
    let options = Options::of(parameters(&tail));
    let mut m = match Machine::load(os) {
        Ok(m) => m,
        Err(trap) => return Err(fatal(os, trap)),
    };
    let raised = match begin(&mut m, &options) {
        Ok(()) => None,
        Err(trap) => m.error(trap).map_err(Fail::Unwind)?,
    };
    if let Err(trap) = m.save() {
        return Err(fatal(m.os, trap));
    }
    if let Some(leave) = raised {
        return crate::module::depart(os, leave);
    }
    let run = os
        .routine_of(module_of(dialect), routine::RUN)
        .ok_or_else(|| os.no_memory())?;
    let mut registers = [0; 15];
    registers[8] = ws::ARGP;
    registers[13] = os.read_word(reg::SP.at())?;
    let bits = os.caller_bits();
    Err(os.continue_in_user(bits, run, registers, 0))
}

/// The workspace as a fresh BASIC has it: an empty program at `PAGE`, the
/// stack empty at `limit`, `@%` its default, nothing else set.
fn lay_out(os: &mut Os<'_>, limit: u32, dialect: Dialect) -> Result<(), Fail> {
    if limit < ws::END + MIN_ROOM {
        return Err(os.no_memory());
    }
    let page = match dialect {
        Dialect::V => ws::PAGE_DEFAULT,
        Dialect::VI => ws::PAGE_DEFAULT_VI,
    };
    os.fill(ws::BASE, page - ws::BASE, 0)?;
    os.write_bytes(page, &[b'\r', tairix_openbasic::program::END])?;
    let top = page + 2;
    let lomem = top.next_multiple_of(4);
    for (slot, value) in [
        (ws::PAGE, page),
        (ws::TOP, top),
        (ws::LOMEM, lomem),
        (ws::FSA, lomem),
        (ws::HIMEM, limit),
        (ws::MEMLIMIT, limit),
        (ws::AT_PERCENT, AT_PERCENT),
        (ws::WIDTHLOC, u32::MAX),
        (reg::SP, limit),
        (reg::LISTO, LISTO),
        (reg::MODE, Mode::Prompt as u32),
        (
            reg::FLAGS,
            if dialect == Dialect::VI {
                flag::BASIC64
            } else {
                0
            },
        ),
    ] {
        os.write_word(slot.at(), value)?;
    }
    let mut regs = Regs::default();
    regs.r[0] = READ_MONOTONIC;
    os.perform(X | swi::READ_MONOTONIC_TIME, &mut regs)?;
    let seed = u64::from(regs.r[0]) | 1 << 32;
    os.write_bytes(reg::SEED.at(), &seed.to_le_bytes()[..5])?;
    Ok(())
}

/// The least room past the workspace BASIC starts in.
const MIN_ROOM: u32 = 4096;
/// `OS_ReadMonotonicTime` takes no reason.
const READ_MONOTONIC: u32 = 0;

/// BASIC's handlers installed, the ones they replace kept to give back.
fn take_handlers(os: &mut Os<'_>, builtin: &'static Builtin) -> Result<(), Fail> {
    for (i, &(handler, which, buffered)) in (0..).zip(&HANDLERS) {
        let code = os
            .routine_of(builtin, which)
            .ok_or_else(|| os.no_memory())?;
        let buffer = if buffered { ws::STRACC.at() } else { 0 };
        let mut regs = Regs::default();
        regs.r[..4].copy_from_slice(&[handler, code, ws::ARGP, buffer]);
        os.perform(X | swi::CHANGE_ENVIRONMENT, &mut regs)?;
        let saved = ws::SAVED_HANDLERS.at() + 12 * i;
        os.write_words(saved, &regs.r[1..4])?;
    }
    Ok(())
}

/// The handlers BASIC replaced given back, once.
pub(crate) fn give_back_handlers(os: &mut Os<'_>) -> Result<(), Fail> {
    for (i, &(handler, _, _)) in (0..).zip(&HANDLERS) {
        let saved = ws::SAVED_HANDLERS.at() + 12 * i;
        let code = os.read_word(saved)?;
        if code == 0 {
            continue;
        }
        let mut regs = Regs::default();
        regs.r[0] = handler;
        os.read_words(saved, &mut regs.r[1..4])?;
        os.perform(X | swi::CHANGE_ENVIRONMENT, &mut regs)?;
        os.fill(saved, 12, 0)?;
    }
    Ok(())
}

/// BASIC left: its handlers given back, then `ERROR EXT`'s error raised to
/// its parent or `OS_Exit` with the return code.
pub(crate) fn quit(os: &mut Os<'_>) -> Result<(), Fail> {
    let flags = os.read_word(reg::FLAGS.at())?;
    let code = os.read_word(reg::EXIT_CODE.at())?;
    give_back_handlers(os)?;
    if flags & flag::OS_ERROR != 0 && code != 0 {
        let mut regs = Regs::default();
        regs.r[0] = code;
        return os.perform(swi::GENERATE_ERROR, &mut regs);
    }
    let mut regs = Regs::default();
    regs.r[..3].copy_from_slice(&[0, ABEX, code]);
    os.perform(swi::EXIT, &mut regs)
}

/// The machine's own state refused: BASIC's handlers given back and the
/// program ended as one the kernel cannot carry on.
pub(crate) fn fatal(os: &mut Os<'_>, trap: Trap) -> Fail {
    if let Trap::Unwind(unwind) = trap {
        return Fail::Unwind(unwind);
    }
    let _ = give_back_handlers(os);
    Fail::Unwind(Unwind::Ended(Ended::Fault))
}

/// `*BASIC`'s options.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
struct Options {
    help: bool,
    action: Action,
    source: Option<Source>,
}

/// What a program given is for.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
enum Action {
    /// Run, then the prompt.
    #[default]
    Chain,
    /// Loaded only.
    Load,
    /// Run, crunched, and BASIC quits at its end.
    Quit,
}

/// Where a program comes from.
#[derive(Clone, PartialEq, Eq, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "read once from the command tail, where a name is carried inline"
)]
enum Source {
    /// A file.
    File(Text),
    /// Memory: `@start,end`, in hexadecimal.
    Memory(u32, u32),
    /// An in-core description that is not one.
    BadMemory,
}

impl Options {
    /// The options a command tail gives.
    fn of(tail: &[u8]) -> Self {
        let mut options = Self::default();
        for word in tail.split(|&b| b == b' ').filter(|w| !w.is_empty()) {
            match word {
                b"-help" | b"-HELP" => options.help = true,
                b"-chain" | b"-CHAIN" => options.action = Action::Chain,
                b"-load" | b"-LOAD" => options.action = Action::Load,
                b"-quit" | b"-QUIT" => options.action = Action::Quit,
                [b'@', rest @ ..] => options.source = Some(memory(rest)),
                name if options.source.is_none() => {
                    options.source = Text::of(name).map(Source::File);
                }
                _ => {}
            }
        }
        options
    }
}

/// The parameters of the command line an application is entered with, which
/// names the application first.
fn parameters(line: &[u8]) -> &[u8] {
    let start = line.iter().take_while(|&&b| b == b' ').count();
    let name = line[start..].iter().take_while(|&&b| b != b' ').count();
    &line[start + name..]
}

/// `@xxxxxxxx,xxxxxxxx`: the start and end of a program in memory.
fn memory(text: &[u8]) -> Source {
    let hex = |digits: &[u8]| -> Option<u32> {
        if digits.len() != 8 {
            return None;
        }
        digits
            .iter()
            .try_fold(0_u32, |n, &d| Some(n << 4 | (d as char).to_digit(16)?))
    };
    match text
        .split(|&b| b == b',')
        .collect::<alloc::vec::Vec<_>>()
        .as_slice()
    {
        [start, end] => match (hex(start), hex(end)) {
            (Some(start), Some(end)) if start <= end => Source::Memory(start, end),
            _ => Source::BadMemory,
        },
        _ => Source::BadMemory,
    }
}

/// The options followed: the banner or the help, then a program loaded and
/// run, or the prompt.
fn begin(m: &mut Machine<'_, '_>, options: &Options) -> crate::machine::Step {
    if options.help {
        m.output(HELP_TEXT)?;
        m.newline()?;
    }
    let Some(source) = &options.source else {
        banner(m)?;
        return Ok(());
    };
    if options.action == Action::Quit {
        m.flags |= flag::QUIT_AT_END;
    }
    match source {
        Source::File(name) => m.load_program(name)?,
        Source::Memory(start, end) => m.load_memory(*start, *end)?,
        Source::BadMemory => return Err(crate::messages::Error::BadInCore.into()),
    }
    if options.action == Action::Quit && variable_set(m, CRUNCH_VARIABLE)? {
        m.crunch(CRUNCH_ALL)?;
    }
    match options.action {
        Action::Load => {
            banner(m)?;
            m.mode = Mode::Prompt;
        }
        Action::Chain | Action::Quit => m.run_program()?,
    }
    Ok(())
}

/// `CRUNCH %1111`, which `-quit` gives a program only while
/// [`CRUNCH_VARIABLE`] is set, as recorded on RISC OS 5.
const CRUNCH_ALL: u32 = 0b1111;
/// The system variable that has `-quit` crunch its program.
const CRUNCH_VARIABLE: &[u8] = b"BASIC$Crunch";

/// Whether system variable `name` is set: `OS_ReadVarVal` asked for its
/// length alone.
fn variable_set(m: &mut Machine<'_, '_>, name: &[u8]) -> crate::machine::Step<bool> {
    let at = ws::STRACC.at();
    m.os.write_bytes(at, name)?;
    m.os.write_byte(at + super::machine::len32(name.len()), 0)?;
    let mut regs = Regs::default();
    regs.r[..5].copy_from_slice(&[at, 0, u32::MAX, 0, 0]);
    m.os.perform_quietly(X | swi::READ_VAR_VAL, &mut regs)?;
    Ok(regs.r[2] != 0)
}

/// The banner and the room free.
fn banner(m: &mut Machine<'_, '_>) -> crate::machine::Step {
    let title: &[u8] = match m.dialect() {
        Dialect::V => BANNER_V,
        Dialect::VI => BANNER_VI,
    };
    m.output(title)?;
    m.newline()?;
    m.output(b"Starting with ")?;
    let free = m.himem.saturating_sub(m.fsa);
    let digits = crate::machine::functions::decimal(free.cast_signed());
    m.output(digits.as_bytes())?;
    m.output(b" bytes free")?;
    m.newline()?;
    m.newline()
}

/// The banners, in TAIRiX Bridge's own words: the first line `REPORT`
/// gives before any error.
const BANNER_V: &[u8] = b"TAIRiX Bridge BBC BASIC V version 1.85";
const BANNER_VI: &[u8] = b"TAIRiX Bridge BBC BASIC VI version 1.85";
/// What `-help` writes.
const HELP_TEXT: &[u8] = b"Options: -help, -chain <file>, -load <file>, -quit <file>, or @<start>,<end> for a program in memory.";

#[cfg(test)]
mod tests {
    use super::{memory, parameters, Action, Options, Source};
    use crate::value::Text;

    #[test]
    fn the_command_line_s_first_word_is_the_application_s_name() {
        let options = Options::of(parameters(b"BASIC -quit HostFS:$.Lang"));
        assert_eq!(options.action, Action::Quit);
        assert_eq!(
            options.source,
            Some(Source::File(Text::of(b"HostFS:$.Lang").expect("short")))
        );
        assert_eq!(Options::of(parameters(b"  BASIC64")).source, None);
        assert!(parameters(b"BASIC").is_empty());
    }

    #[test]
    fn options_are_read_as_the_manual_gives_them() {
        let of = |s: &str| Options::of(s.as_bytes());
        assert_eq!(of("").source, None);
        assert_eq!(of("-quit prog").action, Action::Quit);
        assert_eq!(of("-load prog").action, Action::Load);
        assert_eq!(of("prog").action, Action::Chain);
        assert_eq!(
            of("-chain prog").source,
            Some(Source::File(Text::of(b"prog").expect("short")))
        );
        assert!(of("-help").help);
    }

    #[test]
    fn an_in_core_program_is_eight_hex_digits_each_side() {
        assert_eq!(
            memory(b"000ADF0C,000AE345"),
            Source::Memory(0xA_DF0C, 0xA_E345)
        );
        assert_eq!(
            memory(b"ADF0C,AE345"),
            Source::BadMemory,
            "leading noughts are needed"
        );
        assert_eq!(
            memory(b"000AE345,000ADF0C"),
            Source::BadMemory,
            "the end after the start"
        );
    }
}
