//! The `cd` and `pwd` builtins: the working directory as a path, kept in
//! `PWD`, beside the directory the process holds.
//!
//! # Usage
//!
//! ```text
//! cd [-L | -P [-e]] [directory]
//! cd -
//! pwd [-L | -P]
//! ```
//!
//! POSIX's `cd` and `pwd` are the reference. `cd -L`, the default, applies a
//! relative operand to `PWD` as written — each `..` removing the component
//! before it, once that is found to name a directory, and `/..` staying `/` —
//! and enters the result, which `PWD` then holds: `cd link` then `cd ..` comes
//! back to where the link is. A `PWD` that no longer names the working
//! directory is not built on; the directory's own path is, and where that
//! cannot be learned the operand is entered as `-P` enters it. `cd -P` enters
//! the operand as the kernel resolves it, links first, and sets `PWD` to the
//! directory's own path; with `-e` it fails when that path cannot be learned.
//! `cd` alone goes to `HOME`, `cd -` to `OLDPWD`, printing it, and every
//! change sets `OLDPWD` to where it left. Beneath an alias root a `..` never
//! climbs above it. `CDPATH` is not read.
//!
//! `pwd -L`, the default, prints `PWD` while it is a path spelled as the
//! grammar renders one and still reaches the working directory; otherwise,
//! and with `-P`, it prints the directory's own path.

use alloc::format;
use alloc::string::{String, ToString};
use core::fmt;

use tairix_abi::Errno;
use tairix_path::PathError;
use tairix_util::quote::quote_subject;

use crate::builtin::BuiltinContext;
use crate::env::Environment;
use crate::host::ProcessHost;

/// Status returned on success.
const OK: i32 = 0;
/// Status returned when the directory could not be changed or named, or the
/// command was used incorrectly.
const FAILED: i32 = 1;

/// How a path's `..` and links are taken.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Dots {
    /// Lexically, against `PWD` (`-L`).
    Logical,
    /// As the kernel resolves the path, links first (`-P`).
    Physical,
}

/// The leading options of `cd` or `pwd`.
#[derive(Clone, Copy, Debug)]
struct Switches {
    dots: Dots,
    /// `cd -e`: fail when `-P` entered a directory whose path cannot be
    /// learned.
    strict: bool,
}

/// Why `cd` could not enter its operand.
enum Refusal {
    /// The path the operand makes breaks the path grammar.
    Grammar(PathError),
    /// The kernel refused it.
    Kernel(Errno),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Grammar(err) => err.fmt(f),
            Self::Kernel(errno) => errno.fmt(f),
        }
    }
}

/// Run `cd` with `args` (everything after the command name).
pub(crate) fn cd(ctx: &mut BuiltinContext<'_>, args: &[String]) -> i32 {
    let Some((switches, operands)) = switches(ctx, "cd", args, true) else {
        return FAILED;
    };
    let (target, announce) = match operands {
        [] => match ctx.env.get("HOME").filter(|home| !home.is_empty()) {
            Some(home) => (home.to_string(), false),
            None => return refuse(ctx, "cd: HOME not set\n"),
        },
        [dash] if dash == "-" => match ctx.env.get("OLDPWD").filter(|old| !old.is_empty()) {
            Some(old) => (old.to_string(), true),
            None => return refuse(ctx, "cd: OLDPWD not set\n"),
        },
        [operand] => (operand.clone(), false),
        // A pattern matching several directories must not pick one of them.
        _ => return refuse(ctx, "cd: too many arguments\n"),
    };
    if target.is_empty() {
        let refused = format!("cd: {}: {}\n", quote_subject(&target), PathError::Empty);
        return refuse(ctx, &refused);
    }
    let left = logical_pwd(ctx.env)
        .map(String::from)
        .or_else(|| ctx.host.working_directory().ok());
    let entered = match switches.dots {
        Dots::Logical => enter_logically(ctx.host, ctx.env, &target),
        Dots::Physical => enter_physically(ctx.host, &target),
    };
    let named = match entered {
        Ok(named) => named,
        Err(refusal) => {
            let refused = format!("cd: {}: {refusal}\n", quote_subject(&target));
            return refuse(ctx, &refused);
        }
    };
    if let Some(left) = left {
        ctx.env.export("OLDPWD", left);
    } else {
        ctx.env.unset("OLDPWD");
    }
    match named {
        Ok(pwd) => {
            if announce {
                ctx.console.write_stdout(&format!("{pwd}\n"));
            }
            ctx.env.export("PWD", pwd);
            OK
        }
        // The change stands; only what the directory is called is unknown.
        Err(errno) => {
            ctx.env.unset("PWD");
            if !switches.strict {
                return OK;
            }
            let refused = format!(
                "cd: {}: the working directory cannot be named: {errno}\n",
                quote_subject(&target)
            );
            refuse(ctx, &refused)
        }
    }
}

/// Run `pwd` with `args` (everything after the command name).
pub(crate) fn pwd(ctx: &mut BuiltinContext<'_>, args: &[String]) -> i32 {
    let Some((switches, operands)) = switches(ctx, "pwd", args, false) else {
        return FAILED;
    };
    if !operands.is_empty() {
        return refuse(ctx, "pwd: too many arguments\n");
    }
    let logical = match switches.dots {
        Dots::Logical => logical_pwd(ctx.env)
            .filter(|pwd| names_working_directory(ctx.host, pwd))
            .map(String::from),
        Dots::Physical => None,
    };
    let shown = match logical.map_or_else(|| ctx.host.working_directory(), Ok) {
        Ok(path) => path,
        Err(errno) => {
            let refused = format!("pwd: the working directory cannot be named: {errno}\n");
            return refuse(ctx, &refused);
        }
    };
    ctx.console.write_stdout(&format!("{shown}\n"));
    OK
}

/// Bring `PWD` into step with the working directory a shell starts in, as
/// POSIX has it: a `PWD` naming the working directory stays, and otherwise
/// `PWD` takes the directory's own path. The shell never changes directory to
/// follow `PWD`. `OLDPWD` starts equal to it.
pub(crate) fn settle(env: &mut Environment, host: &dyn ProcessHost) {
    let kept = logical_pwd(env)
        .filter(|pwd| names_working_directory(host, pwd))
        .map(String::from);
    if let Some(pwd) = kept.or_else(|| host.working_directory().ok()) {
        env.export("OLDPWD", pwd.clone());
        env.export("PWD", pwd);
    } else {
        env.unset("OLDPWD");
        env.unset("PWD");
    }
}

/// Enter `target` as `-L` takes it, answering the path `PWD` then holds, or
/// why it cannot be learned: a relative operand applied to `PWD` while that
/// names the working directory, else to the directory's own path, and the
/// result, as the path grammar renders it, entered. With neither, the operand
/// is entered as `-P` enters it.
fn enter_logically(
    host: &dyn ProcessHost,
    env: &Environment,
    target: &str,
) -> Result<Result<String, Errno>, Refusal> {
    let curpath = if tairix_path::is_relative_spelling(target) {
        let kept = logical_pwd(env)
            .filter(|pwd| names_working_directory(host, pwd))
            .map(String::from);
        let Some(base) = kept.or_else(|| host.working_directory().ok()) else {
            return enter_physically(host, target);
        };
        tairix_path::join(&base, target)
    } else {
        String::from(target)
    };
    check_dot_dots(host, &curpath)?;
    let rendered = lexical(&curpath).map_err(Refusal::Grammar)?;
    host.change_directory(&rendered).map_err(Refusal::Kernel)?;
    Ok(Ok(rendered))
}

/// `curpath` with each `..` taking the component before it away, as the path
/// grammar renders the result, a `..` at the view root staying there as
/// POSIX has `/..`.
fn lexical(curpath: &str) -> Result<String, PathError> {
    let (root, steps) = tairix_path::steps(curpath)?;
    if root != tairix_path::Root::View {
        return tairix_path::parse(curpath).map(|path| path.to_string());
    }
    let mut names = alloc::vec::Vec::new();
    for step in steps {
        match step? {
            tairix_path::Step::Name(name) => names.push(name),
            tairix_path::Step::Up => {
                names.pop();
            }
        }
    }
    tairix_path::parse(&format!("/{}", names.join("/"))).map(|path| path.to_string())
}

/// Enter `target` as `-P` takes it — as the kernel resolves it — answering
/// the directory's own path, or why it cannot be learned.
fn enter_physically(
    host: &dyn ProcessHost,
    target: &str,
) -> Result<Result<String, Errno>, Refusal> {
    host.change_directory(target).map_err(Refusal::Kernel)?;
    Ok(host.working_directory())
}

/// POSIX's check before `-L` removes a `..` and the component before it:
/// that component, the path up to it taken lexically, must name a directory.
fn check_dot_dots(host: &dyn ProcessHost, curpath: &str) -> Result<(), Refusal> {
    let mut start = 0;
    for piece in curpath.split('/') {
        if piece == ".." {
            if let Ok(before) = tairix_path::parse(&curpath[..start]) {
                if !before.components().is_empty() {
                    host.directory_identity(&before.to_string())
                        .map_err(Refusal::Kernel)?;
                }
            }
        }
        start += piece.len() + 1;
    }
    Ok(())
}

/// `PWD`, when it is an absolute path spelled as the path grammar renders one
/// — no `.` or `..` component, no doubled or trailing `/` — the only kind `cd`
/// keeps there.
fn logical_pwd(env: &Environment) -> Option<&str> {
    let pwd = env.get("PWD")?;
    let parsed = tairix_path::parse(pwd).ok()?;
    (parsed.is_absolute() && parsed.to_string() == pwd).then_some(pwd)
}

/// Whether `path` reaches the working directory itself, by the identity of
/// the directory each names.
fn names_working_directory(host: &dyn ProcessHost, path: &str) -> bool {
    match (host.directory_identity(path), host.directory_identity(".")) {
        (Ok(named), Ok(current)) => !named.is_none() && named == current,
        _ => false,
    }
}

/// Read the leading options `name` takes: `-L` and `-P`, the last winning,
/// and `-e` where `strict` allows it. An unknown option is reported and
/// answers `None`; a bare `-` is an operand.
fn switches<'a>(
    ctx: &BuiltinContext<'_>,
    name: &str,
    args: &'a [String],
    strict: bool,
) -> Option<(Switches, &'a [String])> {
    let mut switches = Switches {
        dots: Dots::Logical,
        strict: false,
    };
    let mut rest = args;
    while let Some((arg, after)) = rest.split_first() {
        if arg == "--" {
            rest = after;
            break;
        }
        let Some(letters) = arg.strip_prefix('-').filter(|letters| !letters.is_empty()) else {
            break;
        };
        for letter in letters.bytes() {
            match letter {
                b'L' => switches.dots = Dots::Logical,
                b'P' => switches.dots = Dots::Physical,
                b'e' if strict => switches.strict = true,
                _ => {
                    ctx.console.write_stderr(&format!(
                        "{name}: {flag}: invalid option\n",
                        flag = quote_subject(arg)
                    ));
                    return None;
                }
            }
        }
        rest = after;
    }
    Some((switches, rest))
}

/// Report `message` on standard error, answering the failure status.
fn refuse(ctx: &BuiltinContext<'_>, message: &str) -> i32 {
    ctx.console.write_stderr(message);
    FAILED
}

#[cfg(test)]
mod tests {
    use super::settle;
    use crate::env::Environment;
    use crate::test_support::Fixture;
    use tairix_abi::Errno;

    /// A tree holding the link `/x/link`, which names `/y/real`.
    fn linked() -> Fixture<'static> {
        let fx = Fixture::new();
        fx.host.set_directory("/x");
        fx.host.set_directory("/y/real");
        fx.host.set_link("/x/link", "/y/real");
        fx
    }

    /// A `PWD` that no longer names the working directory, as after the
    /// directory was renamed, is not what a relative operand is applied to.
    #[test]
    fn a_stale_pwd_is_not_built_on() {
        let mut fx = linked();
        assert_eq!(fx.run(&["cd", "/y/real"]), Some(0));
        fx.env.export("PWD", "/x");
        assert_eq!(fx.run(&["cd", ".."]), Some(0));
        assert_eq!(fx.env.get("PWD"), Some("/y"));
        assert_eq!(fx.host.working(), "/y");
        // Where the directory's own path cannot be learned either, the kernel
        // resolves the operand from where the shell stands.
        assert_eq!(fx.run(&["cd", "real"]), Some(0));
        fx.env.export("PWD", "/x");
        fx.host.refuse_working_directory(Errno::NotFound);
        assert_eq!(fx.run(&["cd", ".."]), Some(0));
        assert_eq!(fx.host.working(), "/y");
    }

    /// `/..` is `/`, as POSIX has it, however many there are.
    #[test]
    fn a_dot_dot_at_the_root_stays_there() {
        let mut fx = linked();
        assert_eq!(fx.run(&["cd", "/.."]), Some(0));
        assert_eq!(fx.env.get("PWD"), Some("/"));
        assert_eq!(fx.host.working(), "/");
        assert_eq!(fx.run(&["cd", "../../x"]), Some(0));
        assert_eq!(fx.env.get("PWD"), Some("/x"));
        assert_eq!(fx.host.working(), "/x");
    }

    #[test]
    fn cd_applies_dot_dot_to_pwd_by_default() {
        let mut fx = linked();
        assert_eq!(fx.run(&["cd", "/x/link"]), Some(0));
        assert_eq!(fx.env.get("PWD"), Some("/x/link"));
        assert_eq!(fx.host.working(), "/y/real", "the directory held");
        assert_eq!(fx.run(&["cd", ".."]), Some(0));
        assert_eq!(fx.env.get("PWD"), Some("/x"));
        assert_eq!(fx.host.working(), "/x", "back where the link is");
        assert_eq!(fx.run(&["cd", "-L", "link/../link"]), Some(0));
        assert_eq!(fx.env.get("PWD"), Some("/x/link"));
    }

    #[test]
    fn cd_dash_p_enters_the_directory_and_names_it_by_its_own_path() {
        let mut fx = linked();
        assert_eq!(fx.run(&["cd", "-P", "/x/link"]), Some(0));
        assert_eq!(fx.env.get("PWD"), Some("/y/real"));
        assert_eq!(fx.run(&["cd", "-P", ".."]), Some(0));
        assert_eq!(fx.env.get("PWD"), Some("/y"));
        assert_eq!(fx.host.working(), "/y");
        // The last of `-L` and `-P` wins.
        assert_eq!(fx.run(&["cd", "-PL", "/x/link"]), Some(0));
        assert_eq!(fx.env.get("PWD"), Some("/x/link"));
        assert_eq!(fx.run(&["cd", "-L", "-P", "."]), Some(0));
        assert_eq!(fx.env.get("PWD"), Some("/y/real"));
    }

    #[test]
    fn cd_dash_returns_to_oldpwd_and_prints_it() {
        let mut fx = linked();
        assert_eq!(fx.run(&["cd", "-"]), Some(1));
        assert_eq!(fx.console.stderr(), "cd: OLDPWD not set\n");
        assert_eq!(fx.run(&["cd", "/x"]), Some(0));
        assert_eq!(fx.run(&["cd", "/y/real"]), Some(0));
        assert_eq!(fx.env.get("OLDPWD"), Some("/x"));
        fx.console.clear();
        assert_eq!(fx.run(&["cd", "-"]), Some(0));
        assert_eq!(fx.console.stdout(), "/x\n");
        assert_eq!(fx.env.get("PWD"), Some("/x"));
        assert_eq!(fx.env.get("OLDPWD"), Some("/y/real"));
        assert_eq!(fx.host.working(), "/x");
    }

    #[test]
    fn a_dot_dot_after_a_name_that_is_no_directory_is_refused() {
        let mut fx = linked();
        assert_eq!(fx.run(&["cd", "/x"]), Some(0));
        assert_eq!(fx.run(&["cd", "nothing/.."]), Some(1));
        assert_eq!(
            fx.console.stderr(),
            alloc::format!("cd: nothing/..: {}\n", Errno::NotFound)
        );
        assert_eq!(fx.env.get("PWD"), Some("/x"));
        assert_eq!(fx.host.working(), "/x");
    }

    #[test]
    fn a_dot_dot_never_climbs_above_an_alias_root() {
        let mut fx = linked();
        assert_eq!(fx.run(&["cd", "/x"]), Some(0));
        assert_eq!(fx.run(&["cd", "Home:/.."]), Some(1));
        assert_eq!(
            fx.console.stderr(),
            "cd: 'Home:/..': path climbs above its root\n"
        );
        assert_eq!(fx.env.get("PWD"), Some("/x"));
    }

    #[test]
    fn cd_alone_goes_home() {
        let mut fx = linked();
        assert_eq!(fx.run(&["cd"]), Some(1));
        assert_eq!(fx.console.stderr(), "cd: HOME not set\n");
        fx.env.export("HOME", "/y/real");
        assert_eq!(fx.run(&["cd"]), Some(0));
        assert_eq!(fx.env.get("PWD"), Some("/y/real"));
        assert_eq!(fx.host.working(), "/y/real");
    }

    #[test]
    fn cd_refuses_what_names_no_one_directory() {
        let mut fx = linked();
        let cases: [(&[&str], &str); 4] = [
            (&["cd", ""], "cd: '': empty path\n"),
            (&["cd", "-x", "/x"], "cd: -x: invalid option\n"),
            (&["cd", "/x", "/y"], "cd: too many arguments\n"),
            (
                &["cd", "Home:x"],
                "cd: 'Home:x': input is a resource reference or an alias path missing its `/`\n",
            ),
        ];
        for (words, said) in cases {
            fx.console.clear();
            assert_eq!(fx.run(words), Some(1), "{words:?}");
            assert_eq!(fx.console.stderr(), said, "{words:?}");
            assert_eq!(fx.host.working(), "/", "{words:?}");
        }
    }

    #[test]
    fn cd_dash_e_fails_when_the_directory_entered_cannot_be_named() {
        let mut fx = linked();
        fx.host.refuse_working_directory(Errno::PermissionDenied);
        assert_eq!(fx.run(&["cd", "-P", "/x"]), Some(0), "without -e it stands");
        assert_eq!(fx.env.get("PWD"), None);
        assert_eq!(fx.host.working(), "/x");
        assert_eq!(fx.run(&["cd", "-Pe", "/y/real"]), Some(1));
        assert_eq!(fx.host.working(), "/y/real", "the change still stands");
        assert!(fx
            .console
            .stderr()
            .starts_with("cd: /y/real: the working directory cannot be named: "));
    }

    #[test]
    fn pwd_prints_pwd_while_it_names_the_working_directory() {
        let mut fx = linked();
        assert_eq!(fx.run(&["cd", "/x/link"]), Some(0));
        let cases: [(&[&str], &str); 4] = [
            (&["pwd"], "/x/link\n"),
            (&["pwd", "-L"], "/x/link\n"),
            (&["pwd", "-P"], "/y/real\n"),
            (&["pwd", "-LP"], "/y/real\n"),
        ];
        for (words, shown) in cases {
            fx.console.clear();
            assert_eq!(fx.run(words), Some(0), "{words:?}");
            assert_eq!(fx.console.stdout(), shown, "{words:?}");
        }
    }

    #[test]
    fn pwd_prints_the_directory_s_own_path_when_pwd_names_another() {
        let mut fx = linked();
        assert_eq!(fx.run(&["cd", "/y/real"]), Some(0));
        for stale in ["/x", "/x/../y/real", "relative", "/y/real/"] {
            fx.env.export("PWD", stale);
            fx.console.clear();
            assert_eq!(fx.run(&["pwd"]), Some(0), "{stale}");
            assert_eq!(fx.console.stdout(), "/y/real\n", "{stale}");
        }
    }

    #[test]
    fn pwd_refuses_operands_and_unknown_options_and_says_why_it_cannot_name() {
        let mut fx = linked();
        assert_eq!(fx.run(&["pwd", "x"]), Some(1));
        assert_eq!(fx.console.stderr(), "pwd: too many arguments\n");
        fx.console.clear();
        assert_eq!(fx.run(&["pwd", "-e"]), Some(1));
        assert_eq!(fx.console.stderr(), "pwd: -e: invalid option\n");
        fx.console.clear();
        fx.host.refuse_working_directory(Errno::NotFound);
        assert_eq!(fx.run(&["pwd", "-P"]), Some(1));
        assert_eq!(fx.console.stdout(), "");
        assert_eq!(
            fx.console.stderr(),
            alloc::format!(
                "pwd: the working directory cannot be named: {}\n",
                Errno::NotFound
            )
        );
    }

    #[test]
    fn a_shell_starts_in_step_with_its_pwd() {
        let fx = linked();
        // A `PWD` naming the working directory stays.
        let mut env = Environment::new();
        env.export("PWD", "/");
        settle(&mut env, &fx.host);
        assert_eq!(env.get("PWD"), Some("/"));
        assert_eq!(env.get("OLDPWD"), Some("/"));
        // One naming another directory is never followed: the shell stays
        // where it was started and names that.
        let mut env = Environment::new();
        env.export("PWD", "/x/link");
        settle(&mut env, &fx.host);
        assert_eq!(env.get("PWD"), Some("/"));
        assert_eq!(fx.host.working(), "/", "never entered");
        // Started in the link's target, a `PWD` reaching it through the link
        // stays as spelled.
        assert!(crate::host::ProcessHost::change_directory(&fx.host, "/x/link").is_ok());
        let mut env = Environment::new();
        env.export("PWD", "/x/link");
        settle(&mut env, &fx.host);
        assert_eq!(env.get("PWD"), Some("/x/link"));
        // One naming nothing gives way to the directory's own path.
        let mut env = Environment::new();
        env.export("PWD", "/gone");
        settle(&mut env, &fx.host);
        assert_eq!(env.get("PWD"), Some("/y/real"));
        assert_eq!(env.get("OLDPWD"), Some("/y/real"));
        // And with no path to learn, neither is set.
        fx.host.refuse_working_directory(Errno::NotFound);
        let mut env = Environment::new();
        settle(&mut env, &fx.host);
        assert_eq!(env.get("PWD"), None);
        assert_eq!(env.get("OLDPWD"), None);
    }
}
