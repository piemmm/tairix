//! The `ulimit` builtin: report and impose the calling process's resource
//! limits.
//!
//! TAIRiX sizes resource *capacities* from discovered hardware and grows
//! them on demand; on top of those defaults a principal may impose a
//! lower ceiling on itself and its children — the TAIRiX `ulimit`/`rlimit`
//! facility. This builtin is the command-line face of that facility,
//! marshalling through the [`LimitStore`](crate::host::LimitStore) seam to
//! the `rlimit_get` / `rlimit_set` syscalls.
//!
//! # Usage
//!
//! ```text
//! ulimit [-a] [-H | -S] [<resource> [<value>]]
//! ulimit [-H | -S] -n [<value>]
//! ```
//!
//! * `ulimit` or `ulimit -a` — report every resource's soft bound (with
//!   `-H`, its hard bound).
//! * `ulimit <resource>` — report that resource's soft bound (`-H` for the
//!   hard bound).
//! * `ulimit <resource> <value>` — set the resource. With neither `-H` nor
//!   `-S` both bounds are set (POSIX); `-S` sets only the soft bound and
//!   `-H` only the hard bound. `<value>` is a decimal byte/count or the word
//!   `unlimited`.
//! * `-n` names the `open-files` resource, as bash spells it.
//!
//! `<resource>` is one of the canonical [`LimitKind`] names
//! ([`LimitKind::name`]); `ulimit -a` lists every one. Flags may be run
//! together, as in `ulimit -Sn 256`.
//!
//! Lowering a bound is always permitted; *raising* a hard bound above the
//! inherited ceiling is gated kernel-side on
//! [`CapabilityId::RLIMIT_RAISE`](tairix_abi::CapabilityId::RLIMIT_RAISE) and surfaces here as an error the builtin reports — it is never
//! silently swallowed.

use alloc::format;
use alloc::string::{String, ToString};

use tairix_abi::{Errno, LimitKind, ResourceLimit, RLIMIT_INFINITY};

use crate::builtin::BuiltinContext;
use tairix_util::quote::quote_subject;

/// Status returned on success.
const OK: i32 = 0;
/// Status returned when the command is used incorrectly (bad flag, unknown
/// resource, malformed value).
const USAGE_ERROR: i32 = 1;
/// Status returned when the kernel refuses an otherwise well-formed request
/// (e.g. raising a hard bound without `CAP_RLIMIT_RAISE`).
const DENIED_STATUS: i32 = 1;

/// Which bound(s) a `ulimit` invocation reads or writes.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Bound {
    /// The soft bound only (the default when no flag is given on a *report*).
    Soft,
    /// The hard bound only (`-H`).
    Hard,
    /// Both bounds (the default when no flag is given on a *set*, POSIX).
    Both,
}

impl Bound {
    /// The bounds named once `other` is named beside these.
    fn and(self, other: Self) -> Self {
        if self == other {
            self
        } else {
            Self::Both
        }
    }
}

/// Parsed leading flags of a `ulimit` invocation.
struct Flags {
    /// `-a`: operate over every resource.
    all: bool,
    /// The bounds `-S` and `-H` named; `None` when neither was given.
    named: Option<Bound>,
    /// `-n`: the resource is `open-files`.
    open_files: bool,
}

impl Flags {
    /// The bound to *report*: soft unless `-H` was given.
    fn report_bound(&self) -> Bound {
        match self.named {
            Some(Bound::Hard | Bound::Both) => Bound::Hard,
            Some(Bound::Soft) | None => Bound::Soft,
        }
    }

    /// The bound(s) to *set*: whichever flags were given, else both (POSIX).
    fn set_bound(&self) -> Bound {
        self.named.unwrap_or(Bound::Both)
    }

    fn name(&mut self, bound: Bound) {
        self.named = Some(self.named.map_or(bound, |named| named.and(bound)));
    }
}

/// Run `ulimit` with `args` (everything after the command name).
pub(crate) fn ulimit(ctx: &mut BuiltinContext<'_>, args: &[String]) -> i32 {
    let (flags, rest) = match parse_flags(args) {
        Ok(parsed) => parsed,
        Err(flag) => {
            ctx.console.write_stderr(&format!(
                "ulimit: {flag}: invalid option\n",
                flag = quote_subject(&flag)
            ));
            return USAGE_ERROR;
        }
    };

    // A resource name — the one `-n` names, else the first operand — then an
    // optional value. A third operand is a usage error rather than a
    // silently-ignored tail.
    let named = flags.open_files.then(|| LimitKind::OpenFiles.name());
    let mut operands = named.into_iter().chain(rest.iter().map(String::as_str));
    let (name, value) = (operands.next(), operands.next());
    if operands.next().is_some() {
        ctx.console.write_stderr("ulimit: too many arguments\n");
        return USAGE_ERROR;
    }
    match (name, value) {
        (None, _) => report_all(ctx, flags.report_bound()),
        (Some(name), None) if flags.all => {
            ctx.console.write_stderr(&format!(
                "ulimit: {name}: -a takes no resource\n",
                name = quote_subject(name)
            ));
            USAGE_ERROR
        }
        (Some(name), None) => report_one(ctx, name, flags.report_bound()),
        (Some(name), Some(_)) if flags.all => {
            ctx.console.write_stderr(&format!(
                "ulimit: {name}: -a cannot set a limit\n",
                name = quote_subject(name)
            ));
            USAGE_ERROR
        }
        (Some(name), Some(value)) => set_one(ctx, name, value, flags.set_bound()),
    }
}

/// Split leading `-a` / `-H` / `-S` / `-n` flags, alone or run together as in
/// `-Sn`, from the operands.
///
/// Returns the parsed [`Flags`] and the remaining operands, or the offending
/// token on an unknown option. A bare `-` is treated as an operand, not a
/// flag.
fn parse_flags(args: &[String]) -> Result<(Flags, &[String]), String> {
    let mut flags = Flags {
        all: false,
        named: None,
        open_files: false,
    };
    let mut index = 0;
    for arg in args {
        let Some(letters) = arg.strip_prefix('-').filter(|letters| !letters.is_empty()) else {
            break;
        };
        for letter in letters.chars() {
            match letter {
                'a' => flags.all = true,
                'H' => flags.name(Bound::Hard),
                'S' => flags.name(Bound::Soft),
                'n' => flags.open_files = true,
                _ => return Err(arg.clone()),
            }
        }
        index += 1;
    }
    Ok((flags, &args[index..]))
}

/// Report `bound` for every resource, one aligned line each.
fn report_all(ctx: &mut BuiltinContext<'_>, bound: Bound) -> i32 {
    // Width of the longest resource name, so the values line up.
    let width = LimitKind::ALL
        .iter()
        .map(|kind| kind.name().len())
        .max()
        .unwrap_or(0);
    for kind in LimitKind::ALL {
        match ctx.limits.get(kind) {
            Ok(limit) => {
                let value = bound_value(limit, bound);
                ctx.console.write_stdout(&format!(
                    "{name:<width$}  {rendered}\n",
                    name = kind.name(),
                    rendered = render_value(value),
                ));
            }
            Err(err) => {
                ctx.console
                    .write_stderr(&format!("ulimit: {}: {err}\n", kind.name()));
                return USAGE_ERROR;
            }
        }
    }
    OK
}

/// Report `bound` for the single resource named `name`.
fn report_one(ctx: &mut BuiltinContext<'_>, name: &str, bound: Bound) -> i32 {
    let Some(kind) = LimitKind::from_name(name) else {
        ctx.console.write_stderr(&format!(
            "ulimit: {name}: unknown resource\n",
            name = quote_subject(name)
        ));
        return USAGE_ERROR;
    };
    match ctx.limits.get(kind) {
        Ok(limit) => {
            let value = bound_value(limit, bound);
            ctx.console
                .write_stdout(&format!("{}\n", render_value(value)));
            OK
        }
        Err(err) => {
            ctx.console
                .write_stderr(&format!("ulimit: {}: {err}\n", kind.name()));
            USAGE_ERROR
        }
    }
}

/// Set `bound` of resource `name` to `value`.
fn set_one(ctx: &mut BuiltinContext<'_>, name: &str, value: &str, bound: Bound) -> i32 {
    let Some(kind) = LimitKind::from_name(name) else {
        ctx.console.write_stderr(&format!(
            "ulimit: {name}: unknown resource\n",
            name = quote_subject(name)
        ));
        return USAGE_ERROR;
    };
    let Some(parsed) = parse_value(value) else {
        ctx.console.write_stderr(&format!(
            "ulimit: {value}: invalid limit value\n",
            value = quote_subject(value)
        ));
        return USAGE_ERROR;
    };
    // The current limit supplies the bound the command does not change, so a
    // `-S`/`-H` set never disturbs the other bound.
    let current = match ctx.limits.get(kind) {
        Ok(limit) => limit,
        Err(err) => {
            ctx.console
                .write_stderr(&format!("ulimit: {}: {err}\n", kind.name()));
            return USAGE_ERROR;
        }
    };
    let (soft, hard) = match bound {
        Bound::Soft => (parsed, current.hard),
        Bound::Hard => (current.soft, parsed),
        Bound::Both => (parsed, parsed),
    };
    let Ok(limit) = ResourceLimit::new(soft, hard) else {
        // `soft > hard`: a soft bound above its hard ceiling. POSIX rejects
        // it rather than guessing which the user meant.
        ctx.console.write_stderr(&format!(
            "ulimit: {}: soft limit exceeds hard limit\n",
            kind.name()
        ));
        return USAGE_ERROR;
    };
    match ctx.limits.set(kind, limit) {
        Ok(()) => OK,
        Err(Errno::PermissionDenied) => {
            ctx.console.write_stderr(&format!(
                "ulimit: {}: cannot raise hard limit (requires CAP_RLIMIT_RAISE)\n",
                kind.name()
            ));
            DENIED_STATUS
        }
        Err(err) => {
            ctx.console
                .write_stderr(&format!("ulimit: {}: {err}\n", kind.name()));
            USAGE_ERROR
        }
    }
}

/// The chosen `bound` of `limit`. [`Bound::Both`] reports the soft bound,
/// matching the value `set` then stored under both.
fn bound_value(limit: ResourceLimit, bound: Bound) -> u64 {
    match bound {
        Bound::Hard => limit.hard,
        Bound::Soft | Bound::Both => limit.soft,
    }
}

/// Render a bound for display: `unlimited` for [`RLIMIT_INFINITY`], else the
/// decimal value.
fn render_value(value: u64) -> String {
    if value == RLIMIT_INFINITY {
        "unlimited".to_string()
    } else {
        format!("{value}")
    }
}

/// Parse a `ulimit` value: the word `unlimited` or a decimal `u64`.
///
/// Returns `None` on anything else (fail closed).
fn parse_value(text: &str) -> Option<u64> {
    if text == "unlimited" {
        Some(RLIMIT_INFINITY)
    } else {
        text.parse::<u64>().ok()
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::Fixture as SharedFixture;
    use alloc::vec::Vec;
    use tairix_abi::{Errno, LimitKind, ResourceLimit};

    /// The shared fixture with `ulimit`'s operand-only calling convention:
    /// `run` prepends the builtin name and unwraps the dispatch (the name is
    /// always recognised), so the assertions below read as invocations.
    struct Fixture(SharedFixture<'static>);

    impl core::ops::Deref for Fixture {
        type Target = SharedFixture<'static>;

        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl Fixture {
        fn new() -> Self {
            Self(SharedFixture::new())
        }

        fn run(&mut self, words: &[&str]) -> i32 {
            let mut argv = Vec::with_capacity(words.len() + 1);
            argv.push("ulimit");
            argv.extend_from_slice(words);
            self.0.run(&argv).expect("ulimit is a builtin")
        }
    }

    #[test]
    fn reports_all_resources_by_default() {
        let mut fx = Fixture::new();
        assert_eq!(fx.run(&[]), 0);
        let out = fx.console.stdout();
        // Every resource appears, and the unlimited default renders as a word.
        for kind in LimitKind::ALL {
            assert!(
                out.contains(kind.name()),
                "missing {}: {out:?}",
                kind.name()
            );
        }
        assert!(out.contains("unlimited"), "default is unlimited: {out:?}");
    }

    #[test]
    fn dash_a_is_the_same_as_no_args() {
        let mut fx = Fixture::new();
        assert_eq!(fx.run(&["-a"]), 0);
        assert!(fx.console.stdout().contains("processes"));
    }

    #[test]
    fn reports_one_resource_soft_bound() {
        let mut fx = Fixture::new();
        fx.limits.put(
            LimitKind::Processes,
            ResourceLimit::new(16, 64).expect("well-formed"),
        );
        assert_eq!(fx.run(&["processes"]), 0);
        assert_eq!(fx.console.stdout(), "16\n");
    }

    #[test]
    fn dash_h_reports_the_hard_bound() {
        let mut fx = Fixture::new();
        fx.limits.put(
            LimitKind::Processes,
            ResourceLimit::new(16, 64).expect("well-formed"),
        );
        assert_eq!(fx.run(&["-H", "processes"]), 0);
        assert_eq!(fx.console.stdout(), "64\n");
    }

    #[test]
    fn setting_soft_keeps_the_hard_bound() {
        let mut fx = Fixture::new();
        fx.limits.put(
            LimitKind::OpenFiles,
            ResourceLimit::new(32, 128).expect("well-formed"),
        );
        assert_eq!(fx.run(&["-S", "open-files", "8"]), 0);
        let stored = fx.limits.snapshot(LimitKind::OpenFiles);
        assert_eq!(stored.soft, 8);
        assert_eq!(stored.hard, 128);
    }

    #[test]
    fn setting_hard_keeps_the_soft_bound() {
        let mut fx = Fixture::new();
        fx.limits.put(
            LimitKind::OpenFiles,
            ResourceLimit::new(32, 128).expect("well-formed"),
        );
        assert_eq!(fx.run(&["-H", "open-files", "64"]), 0);
        let stored = fx.limits.snapshot(LimitKind::OpenFiles);
        assert_eq!(stored.soft, 32);
        assert_eq!(stored.hard, 64);
    }

    /// `-S` beside `-H` names both bounds: a set changes both, and a report
    /// shows the hard one.
    #[test]
    fn soft_and_hard_together_name_both_bounds() {
        let mut fx = Fixture::new();
        fx.limits.put(
            LimitKind::OpenFiles,
            ResourceLimit::new(32, 128).expect("well-formed"),
        );
        assert_eq!(fx.run(&["-SH", "open-files", "64"]), 0);
        assert_eq!(
            fx.limits.snapshot(LimitKind::OpenFiles),
            ResourceLimit::new(64, 64).expect("well-formed")
        );
        fx.limits.put(
            LimitKind::OpenFiles,
            ResourceLimit::new(16, 96).expect("well-formed"),
        );
        assert_eq!(fx.run(&["-H", "-S", "open-files"]), 0);
        assert_eq!(fx.console.stdout(), "96\n");
    }

    /// `-n` is bash's spelling of the descriptor limit, alone or run together
    /// with the bound it reports.
    #[test]
    fn dash_n_reports_the_open_file_limit() {
        let mut fx = Fixture::new();
        fx.limits.put(
            LimitKind::OpenFiles,
            ResourceLimit::new(1024, 4096).expect("well-formed"),
        );
        assert_eq!(fx.run(&["-n"]), 0);
        assert_eq!(fx.console.stdout(), "1024\n");
        fx.console.clear();
        assert_eq!(fx.run(&["-H", "-n"]), 0);
        assert_eq!(fx.console.stdout(), "4096\n");
        fx.console.clear();
        assert_eq!(fx.run(&["-Hn"]), 0);
        assert_eq!(fx.console.stdout(), "4096\n");
    }

    #[test]
    fn dash_n_sets_the_open_file_limit() {
        let mut fx = Fixture::new();
        assert_eq!(fx.run(&["-n", "256"]), 0);
        assert_eq!(
            fx.limits.snapshot(LimitKind::OpenFiles),
            ResourceLimit::new(256, 256).expect("well-formed")
        );
        assert_eq!(fx.run(&["-Sn", "128"]), 0);
        assert_eq!(
            fx.limits.snapshot(LimitKind::OpenFiles),
            ResourceLimit::new(128, 256).expect("well-formed")
        );
    }

    /// `-n` names the resource itself, so a further resource operand is one
    /// too many, and `-a` refuses it as it refuses any one resource.
    #[test]
    fn dash_n_takes_no_resource_operand() {
        let mut fx = Fixture::new();
        assert_eq!(fx.run(&["-n", "open-files", "8"]), 1);
        assert_eq!(fx.console.stderr(), "ulimit: too many arguments\n");
        fx.console.clear();
        assert_eq!(fx.run(&["-an"]), 1);
        assert_eq!(
            fx.console.stderr(),
            "ulimit: open-files: -a takes no resource\n"
        );
        assert_eq!(
            fx.limits.snapshot(LimitKind::OpenFiles),
            ResourceLimit::UNLIMITED,
            "nothing was set"
        );
    }

    #[test]
    fn setting_without_a_flag_sets_both_bounds() {
        let mut fx = Fixture::new();
        assert_eq!(fx.run(&["stack-bytes", "65536"]), 0);
        let stored = fx.limits.snapshot(LimitKind::StackBytes);
        assert_eq!(stored.soft, 65536);
        assert_eq!(stored.hard, 65536);
    }

    #[test]
    fn unlimited_round_trips() {
        let mut fx = Fixture::new();
        fx.limits.put(
            LimitKind::Processes,
            ResourceLimit::new(4, 4).expect("well-formed"),
        );
        assert_eq!(fx.run(&["-H", "processes", "unlimited"]), 0);
        assert_eq!(fx.limits.snapshot(LimitKind::Processes).hard, u64::MAX);
        fx.console.clear();
        assert_eq!(fx.run(&["-H", "processes"]), 0);
        assert_eq!(fx.console.stdout(), "unlimited\n");
    }

    #[test]
    fn unknown_resource_fails_closed() {
        let mut fx = Fixture::new();
        assert_eq!(fx.run(&["nonsense"]), 1);
        assert!(fx.console.stderr().contains("unknown resource"));
        assert_eq!(fx.run(&["nonsense", "10"]), 1);
        assert!(fx.console.stderr().contains("unknown resource"));
    }

    #[test]
    fn invalid_value_fails_closed() {
        let mut fx = Fixture::new();
        assert_eq!(fx.run(&["processes", "lots"]), 1);
        assert!(fx.console.stderr().contains("invalid limit value"));
        // The store was never written.
        assert_eq!(
            fx.limits.snapshot(LimitKind::Processes),
            ResourceLimit::UNLIMITED
        );
    }

    #[test]
    fn unknown_flag_fails_closed() {
        let mut fx = Fixture::new();
        assert_eq!(fx.run(&["-Z"]), 1);
        assert!(fx.console.stderr().contains("invalid option"));
    }

    #[test]
    fn soft_above_hard_is_rejected() {
        let mut fx = Fixture::new();
        fx.limits.put(
            LimitKind::Processes,
            ResourceLimit::new(4, 8).expect("well-formed"),
        );
        // Set just the soft bound above the hard ceiling.
        assert_eq!(fx.run(&["-S", "processes", "16"]), 1);
        assert!(fx.console.stderr().contains("exceeds hard limit"));
        // The store keeps its previous, well-formed value.
        assert_eq!(fx.limits.snapshot(LimitKind::Processes).soft, 4);
    }

    #[test]
    fn raise_denied_is_reported() {
        let mut fx = Fixture::new();
        fx.limits.deny_set(Errno::PermissionDenied);
        assert_eq!(fx.run(&["-H", "processes", "unlimited"]), 1);
        assert!(fx.console.stderr().contains("CAP_RLIMIT_RAISE"));
    }

    #[test]
    fn dash_a_with_a_resource_is_a_usage_error() {
        let mut fx = Fixture::new();
        assert_eq!(fx.run(&["-a", "processes"]), 1);
        assert!(fx.console.stderr().contains("-a takes no resource"));
    }

    /// An operand that needs quoting is quoted wherever a diagnostic names it.
    #[test]
    fn diagnostics_quote_an_operand_that_needs_it() {
        let cases: [(&[&str], &str); 6] = [
            (&["-a", "a b"], "ulimit: 'a b': -a takes no resource\n"),
            (
                &["-a", "a b", "1"],
                "ulimit: 'a b': -a cannot set a limit\n",
            ),
            (&["a b"], "ulimit: 'a b': unknown resource\n"),
            (&["a b", "1"], "ulimit: 'a b': unknown resource\n"),
            (&["threads", "x y"], "ulimit: 'x y': invalid limit value\n"),
            (&["-x y"], "ulimit: '-x y': invalid option\n"),
        ];
        for (words, expected) in cases {
            let mut fx = Fixture::new();
            assert_eq!(fx.run(words), 1, "{words:?}");
            assert_eq!(fx.console.stderr(), expected, "{words:?}");
        }
    }
}
