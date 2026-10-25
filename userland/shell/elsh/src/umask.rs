//! The `umask` builtin: report and set the file mode creation mask the shell
//! and every command it launches afterwards create files under.
//!
//! # Usage
//!
//! ```text
//! umask [-S] [mode]
//! ```
//!
//! * `umask` — print the mask as four octal digits (`0022`).
//! * `umask -S` — print the permissions the mask leaves, symbolically
//!   (`u=rwx,g=rx,o=rx`).
//! * `umask mode` — set the mask. An octal `mode` is the mask itself; a
//!   symbolic one edits the permissions it leaves, so `umask g-w` adds group
//!   write to the mask. With `-S` the new mask is printed symbolically too.
//!
//! `mode` is read by the one grammar `chmod` reads (`tairix_util::mode`),
//! keeping only what a mask can hold: an octal value's permission bits, or
//! clauses whose permissions are `r`, `w` and `x`, a clause naming no one
//! acting on every field, as bash's does.

use alloc::format;
use alloc::string::String;

use tairix_abi::fs::FS_PERMISSION_BITS;
use tairix_util::mode::{Mode, Perms, PERM_EXEC, PERM_READ, PERM_WRITE};
use tairix_util::quote::quote_subject;

use crate::builtin::BuiltinContext;

/// Status returned on success.
const OK: i32 = 0;
/// Status returned when the command is used incorrectly (bad flag, a mode
/// that is no mask, too many operands).
const USAGE_ERROR: i32 = 1;

/// Run `umask` with `args` (everything after the command name).
pub(crate) fn umask(ctx: &mut BuiltinContext<'_>, args: &[String]) -> i32 {
    let mut symbolic = false;
    let mut rest = args;
    // Leading `-S` flags, alone or run together; a bare `-` is an operand.
    while let Some((arg, after)) = rest.split_first() {
        if arg == "--" {
            rest = after;
            break;
        }
        let Some(letters) = arg.strip_prefix('-').filter(|letters| !letters.is_empty()) else {
            break;
        };
        if letters.bytes().any(|letter| letter != b'S') {
            ctx.console.write_stderr(&format!(
                "umask: {flag}: invalid option\n",
                flag = quote_subject(arg)
            ));
            return USAGE_ERROR;
        }
        symbolic = true;
        rest = after;
    }
    let mask = match rest {
        [] => ctx.host.umask(),
        [spec] => {
            let Some(mask) = parse_mask(spec, ctx.host.umask()) else {
                ctx.console.write_stderr(&format!(
                    "umask: {spec}: invalid mode\n",
                    spec = quote_subject(spec)
                ));
                return USAGE_ERROR;
            };
            ctx.host.set_umask(mask);
            if !symbolic {
                return OK;
            }
            mask
        }
        _ => {
            ctx.console.write_stderr("umask: too many arguments\n");
            return USAGE_ERROR;
        }
    };
    let shown = if symbolic {
        format!("{}\n", symbolic_permissions(mask))
    } else {
        format!("{mask:04o}\n")
    };
    ctx.console.write_stdout(&shown);
    OK
}

/// The mask `spec` sets when the mask is `current`, or `None` when it names
/// no mask: an octal value's permission bits, or symbolic clauses applied to
/// the permissions `current` leaves.
fn parse_mask(spec: &str, current: u32) -> Option<u32> {
    let all = FS_PERMISSION_BITS;
    let mode = Mode::parse(spec)?;
    let mask = match &mode {
        Mode::Octal { bits, .. } => bits & all,
        Mode::Symbolic(changes) => {
            let letters = PERM_READ | PERM_WRITE | PERM_EXEC;
            let masks_only = changes.iter().all(
                |change| matches!(change.perms, Perms::Letters(perms) if perms & !letters == 0),
            );
            if !masks_only {
                return None;
            }
            let left = !current & all;
            // Every field a clause naming no one changes: there is no mask
            // beneath this one to spare.
            !mode.resolve(left, false, 0) & all
        }
    };
    Some(mask)
}

/// The permissions `mask` leaves, as `-S` prints them: `u=rwx,g=rx,o=rx`.
fn symbolic_permissions(mask: u32) -> String {
    let left = !mask & FS_PERMISSION_BITS;
    let mut shown = String::new();
    for (who, shift) in [('u', 6), ('g', 3), ('o', 0)] {
        if !shown.is_empty() {
            shown.push(',');
        }
        shown.push(who);
        shown.push('=');
        for (letter, bit) in [('r', 0o4), ('w', 0o2), ('x', 0o1)] {
            if (left >> shift) & bit != 0 {
                shown.push(letter);
            }
        }
    }
    shown
}

#[cfg(test)]
mod tests {
    use crate::test_support::Fixture;

    /// Run `umask` with `words`, answering its status.
    fn umask(fx: &mut Fixture<'_>, words: &[&str]) -> i32 {
        let mut argv = alloc::vec!["umask"];
        argv.extend_from_slice(words);
        fx.run(&argv).expect("umask is a builtin")
    }

    /// bash 5.2: `umask` prints `0022`, `-S` the permissions it leaves.
    #[test]
    fn the_mask_is_shown_in_octal_and_symbolically() {
        let mut fx = Fixture::new();
        assert_eq!(umask(&mut fx, &[]), 0);
        assert_eq!(fx.console.stdout(), "0022\n");
        fx.console.clear();
        assert_eq!(umask(&mut fx, &["-S"]), 0);
        assert_eq!(fx.console.stdout(), "u=rwx,g=rx,o=rx\n");
        fx.host.set_umask_to(0o002);
        fx.console.clear();
        assert_eq!(umask(&mut fx, &["-S"]), 0);
        assert_eq!(fx.console.stdout(), "u=rwx,g=rwx,o=rx\n");
        fx.host.set_umask_to(0o777);
        fx.console.clear();
        assert_eq!(umask(&mut fx, &["--"]), 0);
        assert_eq!(umask(&mut fx, &["-S", "-S"]), 0);
        assert_eq!(fx.console.stdout(), "0777\nu=,g=,o=\n");
    }

    /// bash 5.2: an octal operand is the mask itself, its permission bits
    /// kept; `-S` with one sets it and prints it.
    #[test]
    fn an_octal_mode_sets_the_mask() {
        for (spec, want) in [
            ("7", 0o007),
            ("027", 0o027),
            ("0022", 0o022),
            ("0000000022", 0o022),
            ("1777", 0o777),
        ] {
            let mut fx = Fixture::new();
            assert_eq!(umask(&mut fx, &[spec]), 0, "{spec}");
            assert_eq!(fx.host.umask_now(), want, "{spec}");
            assert_eq!(fx.console.stdout(), "", "{spec} prints nothing");
        }
        let mut fx = Fixture::new();
        assert_eq!(umask(&mut fx, &["-S", "027"]), 0);
        assert_eq!(fx.host.umask_now(), 0o027);
        assert_eq!(fx.console.stdout(), "u=rwx,g=rx,o=\n");
    }

    /// bash 5.2 from `022`: a symbolic operand edits the permissions the mask
    /// leaves, a clause naming no one acting on every field.
    #[test]
    fn a_symbolic_mode_edits_what_the_mask_leaves() {
        for (spec, want) in [
            ("g-w", 0o022),
            ("u=rwx,g=rx,o=", 0o027),
            ("a=", 0o777),
            ("+x", 0o022),
            ("+w", 0o000),
            ("=rx", 0o222),
            ("=", 0o777),
            ("u+", 0o022),
            ("o-r", 0o026),
            ("go+w", 0o000),
        ] {
            let mut fx = Fixture::new();
            assert_eq!(umask(&mut fx, &[spec]), 0, "{spec}");
            assert_eq!(fx.host.umask_now(), want, "{spec}");
        }
    }

    /// bash 5.2 refuses each of these with status 1 and leaves the mask; so
    /// do the shell's own options and operand count.
    #[test]
    fn an_invalid_mode_is_reported_and_changes_nothing() {
        for spec in [
            "999", "8", "77777", "a+q", "u+s", "a+t", "a+X", "g=u", "+0", "u", ",", "u+w,", "",
        ] {
            let mut fx = Fixture::new();
            assert_eq!(umask(&mut fx, &[spec]), 1, "{spec:?}");
            assert_eq!(fx.host.umask_now(), 0o022, "{spec:?}");
            assert!(
                fx.console.stderr().ends_with(": invalid mode\n"),
                "{spec:?}: {}",
                fx.console.stderr()
            );
        }
        let mut fx = Fixture::new();
        assert_eq!(umask(&mut fx, &["a+q"]), 1);
        assert_eq!(fx.console.stderr(), "umask: a+q: invalid mode\n");
        fx.console.clear();
        assert_eq!(umask(&mut fx, &["-x"]), 1);
        assert_eq!(fx.console.stderr(), "umask: -x: invalid option\n");
        fx.console.clear();
        assert_eq!(umask(&mut fx, &["022", "033"]), 1);
        assert_eq!(fx.console.stderr(), "umask: too many arguments\n");
        assert_eq!(fx.host.umask_now(), 0o022, "nothing was set");
    }

    /// `--` ends the options, so a mode may begin with `-`.
    #[test]
    fn a_mode_after_double_dash_may_begin_with_a_dash() {
        let mut fx = Fixture::new();
        assert_eq!(umask(&mut fx, &["--", "-w"]), 0);
        assert_eq!(fx.host.umask_now(), 0o222);
    }
}
