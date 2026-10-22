//! The shell's settable options and the `shopt` builtin that sets them.
//!
//! The options are bash's filename-generation set, under bash's names and
//! with bash's defaults: `dotglob`, `extglob`, `failglob`, `globstar`,
//! `nocaseglob` and `nullglob`, all off, and `noglob` (bash's `set -f`),
//! reached as `shopt -o`. `globasciiranges` and `globskipdots` are reported
//! on and cannot be turned off: ranges always compare code points, and a
//! listing never holds `.` or `..`.

use alloc::format;
use alloc::string::String;

use crate::builtin::BuiltinContext;
use tairix_util::quote::quote_if_needed;

/// One shell option, as `shopt` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShellOption {
    /// Wildcards match a leading `.`.
    Dotglob,
    /// The extended pattern operators `?(…)`, `*(…)`, `+(…)`, `@(…)` and
    /// `!(…)` are recognised.
    Extglob,
    /// A pattern matching nothing is an error rather than left as written.
    Failglob,
    /// Ranges compare code points; always on.
    Globasciiranges,
    /// `.` and `..` are never matched; always on, as no listing holds them.
    Globskipdots,
    /// A `**` component matches any number of directory levels.
    Globstar,
    /// Patterns match without regard to case.
    Nocaseglob,
    /// A pattern matching nothing expands to no word at all.
    Nullglob,
    /// No filename generation happens at all (`set -f`).
    Noglob,
}

/// The `shopt` options, in the order bash lists them.
const SHOPT_NAMES: &[(&str, ShellOption)] = &[
    ("dotglob", ShellOption::Dotglob),
    ("extglob", ShellOption::Extglob),
    ("failglob", ShellOption::Failglob),
    ("globasciiranges", ShellOption::Globasciiranges),
    ("globskipdots", ShellOption::Globskipdots),
    ("globstar", ShellOption::Globstar),
    ("nocaseglob", ShellOption::Nocaseglob),
    ("nullglob", ShellOption::Nullglob),
];

/// The `set -o` options `shopt -o` reaches.
const SET_NAMES: &[(&str, ShellOption)] = &[("noglob", ShellOption::Noglob)];

/// The options in force for the session: every settable one off by
/// default, as bash's are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShellOptions(u16);

impl ShellOptions {
    /// Whether `option` is on.
    pub(crate) fn is_on(self, option: ShellOption) -> bool {
        match option {
            ShellOption::Globasciiranges | ShellOption::Globskipdots => true,
            _ => self.0 & bit(option) != 0,
        }
    }

    /// Every settable option off.
    #[cfg(test)]
    pub(crate) const NONE: Self = Self(0);

    /// These options with `option` also on.
    #[cfg(test)]
    pub(crate) const fn with(self, option: ShellOption) -> Self {
        Self(self.0 | bit(option))
    }

    /// Set `option` to `on`, or say why it cannot be.
    fn set(&mut self, option: ShellOption, on: bool) -> Result<(), &'static str> {
        match option {
            ShellOption::Globasciiranges if !on => Err("ranges always compare code points"),
            ShellOption::Globskipdots if !on => Err("a listing never holds `.` or `..`"),
            ShellOption::Globasciiranges | ShellOption::Globskipdots => Ok(()),
            _ if on => {
                self.0 |= bit(option);
                Ok(())
            }
            _ => {
                self.0 &= !bit(option);
                Ok(())
            }
        }
    }
}

const fn bit(option: ShellOption) -> u16 {
    1 << option as u16
}

/// Whether a `shopt` invocation sets, unsets, or only reports options.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Action {
    #[default]
    Report,
    Set,
    Unset,
}

/// What a `shopt` invocation asked for.
#[derive(Default)]
struct Request<'a> {
    action: Action,
    print: bool,
    quiet: bool,
    set_o: bool,
    names: &'a [String],
}

const USAGE: &str = "shopt: usage: shopt [-pqsu] [-o] [optname ...]\n";

/// `shopt [-pqsu] [-o] [optname ...]`, as bash's: with `-s`/`-u`, set or
/// unset the named options (or list those on/off when none is named);
/// otherwise report them — every option, or the named ones — as a table, or
/// with `-p` as the commands that would restore them. `-q` reports through
/// the status alone: `0` when every named option is on.
pub(crate) fn shopt(ctx: &mut BuiltinContext<'_>, args: &[String]) -> i32 {
    let request = match parse(args) {
        Ok(request) => request,
        Err(refusal) => {
            ctx.console.write_stderr(&refusal.message);
            return refusal.status;
        }
    };
    let table = if request.set_o {
        SET_NAMES
    } else {
        SHOPT_NAMES
    };
    let mut status = 0;
    let mut chosen = alloc::vec::Vec::new();
    for word in request.names {
        if let Some(&option) = table.iter().find(|(known, _)| known == word) {
            chosen.push(option);
        } else {
            ctx.console.write_stderr(&format!(
                "shopt: {}: invalid shell option name\n",
                quote_if_needed(word)
            ));
            status = 1;
        }
    }
    let on = match request.action {
        Action::Report => {
            if request.names.is_empty() {
                for &option in table {
                    report(ctx, option, &request);
                }
            }
            for &option in &chosen {
                if !ctx.options.is_on(option.1) {
                    status = 1;
                }
                report(ctx, option, &request);
            }
            return status;
        }
        Action::Set => true,
        Action::Unset => false,
    };
    if request.names.is_empty() {
        // Bare `-s`/`-u` lists the options already in that state.
        for &option in table.iter().filter(|(_, n)| ctx.options.is_on(*n) == on) {
            report(ctx, option, &request);
        }
        return status;
    }
    for &(word, option) in &chosen {
        if let Err(reason) = ctx.options.set(option, on) {
            ctx.console
                .write_stderr(&format!("shopt: {word}: cannot be turned off: {reason}\n"));
            status = 1;
        }
    }
    status
}

/// Why `shopt`'s flags were refused, and the status that refusal leaves.
struct Refusal {
    message: String,
    status: i32,
}

/// Read `shopt`'s flags.
fn parse(args: &[String]) -> Result<Request<'_>, Refusal> {
    let mut request = Request::default();
    let mut at = 0;
    while let Some(arg) = args.get(at) {
        if arg == "--" {
            at += 1;
            break;
        }
        let Some(flags) = arg.strip_prefix('-').filter(|flags| !flags.is_empty()) else {
            break;
        };
        for flag in flags.chars() {
            let action = match flag {
                's' => Action::Set,
                'u' => Action::Unset,
                'p' => {
                    request.print = true;
                    continue;
                }
                'q' => {
                    request.quiet = true;
                    continue;
                }
                'o' => {
                    request.set_o = true;
                    continue;
                }
                other => {
                    return Err(Refusal {
                        message: format!("shopt: -{other}: invalid option\n{USAGE}"),
                        status: 2,
                    });
                }
            };
            if request.action != Action::Report && request.action != action {
                return Err(Refusal {
                    message: String::from(
                        "shopt: cannot set and unset shell options simultaneously\n",
                    ),
                    status: 1,
                });
            }
            request.action = action;
        }
        at += 1;
    }
    request.names = &args[at..];
    Ok(request)
}

/// Print one option's state as the request asks: nothing under `-q`, the
/// restoring command under `-p`, else bash's `name<TAB>on|off` table row.
fn report(ctx: &BuiltinContext<'_>, (word, option): (&str, ShellOption), request: &Request<'_>) {
    if request.quiet {
        return;
    }
    let on = ctx.options.is_on(option);
    let line = match (request.print, request.set_o) {
        (true, false) => format!("shopt {} {word}\n", if on { "-s" } else { "-u" }),
        (true, true) => format!("set {}o {word}\n", if on { '-' } else { '+' }),
        (false, _) => format!("{word:<15}\t{}\n", if on { "on" } else { "off" }),
    };
    ctx.console.write_stdout(&line);
}

#[cfg(test)]
mod tests {
    use super::ShellOption;
    use crate::test_support::Fixture;

    #[test]
    fn options_default_to_bash_s_and_list_as_a_table() {
        let mut fx = Fixture::new();
        assert_eq!(fx.run(&["shopt"]), Some(0));
        let table = fx.console.stdout();
        assert!(table.starts_with("dotglob        \toff\n"), "{table:?}");
        assert!(table.contains("extglob        \toff\n"));
        assert!(table.contains("globstar       \toff\n"));
        assert!(table.contains("globskipdots   \ton\n"));
        assert!(!table.contains("noglob"), "set -o options need -o");
    }

    #[test]
    fn set_and_unset_change_the_session_options() {
        let mut fx = Fixture::new();
        assert_eq!(fx.run(&["shopt", "-s", "nullglob", "globstar"]), Some(0));
        assert!(fx.options.is_on(ShellOption::Nullglob));
        assert!(fx.options.is_on(ShellOption::Globstar));
        assert_eq!(fx.run(&["shopt", "-u", "nullglob"]), Some(0));
        assert!(!fx.options.is_on(ShellOption::Nullglob));
        assert!(fx.options.is_on(ShellOption::Globstar));
        assert_eq!(fx.run(&["shopt", "-o", "-s", "noglob"]), Some(0));
        assert!(fx.options.is_on(ShellOption::Noglob));
        assert_eq!(fx.run(&["shopt", "-ou", "noglob"]), Some(0));
        assert!(!fx.options.is_on(ShellOption::Noglob));
    }

    #[test]
    fn query_status_reports_whether_every_named_option_is_on() {
        let mut fx = Fixture::new();
        fx.options = fx.options.with(ShellOption::Dotglob);
        assert_eq!(fx.run(&["shopt", "-q", "dotglob"]), Some(0));
        assert_eq!(fx.run(&["shopt", "-q", "dotglob", "failglob"]), Some(1));
        assert_eq!(fx.console.stdout(), "", "-q prints nothing");
        assert_eq!(fx.run(&["shopt", "failglob"]), Some(1));
        assert_eq!(fx.console.stdout(), "failglob       \toff\n");
    }

    #[test]
    fn print_form_restores_the_state() {
        let mut fx = Fixture::new();
        fx.options = fx.options.with(ShellOption::Extglob);
        assert_eq!(fx.run(&["shopt", "-p", "extglob", "dotglob"]), Some(1));
        assert_eq!(fx.console.stdout(), "shopt -s extglob\nshopt -u dotglob\n");
        fx.console.clear();
        assert_eq!(fx.run(&["shopt", "-op", "noglob"]), Some(1));
        assert_eq!(fx.console.stdout(), "set +o noglob\n");
    }

    #[test]
    fn bare_set_lists_the_options_on() {
        let mut fx = Fixture::new();
        fx.options = fx.options.with(ShellOption::Nocaseglob);
        assert_eq!(fx.run(&["shopt", "-s"]), Some(0));
        assert_eq!(
            fx.console.stdout(),
            "globasciiranges\ton\nglobskipdots   \ton\nnocaseglob     \ton\n"
        );
    }

    #[test]
    fn refusals_name_the_fault() {
        let mut fx = Fixture::new();
        assert_eq!(fx.run(&["shopt", "-s", "nosuch"]), Some(1));
        assert!(fx
            .console
            .stderr()
            .contains("shopt: nosuch: invalid shell option name"));
        assert_eq!(fx.run(&["shopt", "-x"]), Some(2));
        assert!(fx.console.stderr().contains("usage: shopt"));
        assert_eq!(fx.run(&["shopt", "-s", "-u", "dotglob"]), Some(1));
        assert!(
            !fx.options.is_on(ShellOption::Dotglob),
            "a contradictory request changes nothing"
        );
        // The fixed behaviours stay on, and saying so is not a failure.
        assert_eq!(fx.run(&["shopt", "-s", "globskipdots"]), Some(0));
        assert_eq!(fx.run(&["shopt", "-u", "globasciiranges"]), Some(1));
        assert!(fx.console.stderr().contains("cannot be turned off"));
        // `noglob` is a `set -o` option, reached only with `-o`.
        assert_eq!(fx.run(&["shopt", "-s", "noglob"]), Some(1));
        assert!(!fx.options.is_on(ShellOption::Noglob));
    }
}
