//! `*Repeat <command> <directory> [-Directories] [-Applications]
//! [-Files|-Type <type>] [-CommandTail <tail>] [-Tasks]`: the command run on
//! each object the directory holds that the switches choose, as
//! `<command> <directory>.<leaf> <tail>`.
//!
//! As RISC OS 5's, it becomes the application to do it, so a program it
//! runs on an object hands the walk back its turn when it exits, and what
//! called it is not returned to: the walk ends by exiting, to whatever
//! started the program before it. A switch is any leading part of its name,
//! in either case; with none, every object is chosen. A directory that
//! cannot be listed holds nothing to walk. RISC OS 5 was not seen to return
//! from a switch it does not know nor from `-CommandTail`; an unknown switch
//! here is the command's syntax error, and the tail is the next parameter,
//! as the PRM gives it.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_obey::command::MOST_LINE;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{self as swi, fs_control};
use tairix_riscos_kernel::swi::X;

use super::objects::{objects, Object, DIRECTORY};
use super::{parameters, State, TITLE};
use crate::script::{self, OnError, Source};

pub(super) const SYNTAX: &str = "Syntax: *Repeat <command> <directory> [-Directories] [-Applications] [-Files|-Type <type>] [-CommandTail <tail>] [-Tasks]";

/// `OS_Module`'s reason that enters a module as the application.
const ENTER: u32 = 2;

/// Each switch, by its name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Switch {
    Directories,
    Applications,
    Files,
    Type,
    CommandTail,
    Tasks,
}

const SWITCHES: [(&[u8], Switch); 6] = [
    (b"Directories", Switch::Directories),
    (b"Applications", Switch::Applications),
    (b"Files", Switch::Files),
    (b"Type", Switch::Type),
    (b"CommandTail", Switch::CommandTail),
    (b"Tasks", Switch::Tasks),
];

/// The switch `word`, after its `-`, names: the first whose name it begins.
fn switch(word: &[u8]) -> Option<Switch> {
    let name = word.strip_prefix(b"-").filter(|name| !name.is_empty())?;
    SWITCHES
        .iter()
        .find(|(full, _)| {
            full.get(..name.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(name))
        })
        .map(|&(_, switch)| switch)
}

/// What a walk chooses: what each switch names, together; everything where
/// none is given.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) struct Choice<'a> {
    pub(super) files: bool,
    pub(super) directories: bool,
    pub(super) applications: bool,
    pub(super) file_type: Option<&'a [u8]>,
}

impl Choice<'_> {
    /// Whether `object` is chosen, `file_type` the number the type named.
    fn chooses(&self, object: &Object, file_type: Option<u32>) -> bool {
        let choosing = self.files || self.directories || self.applications || file_type.is_some();
        if !choosing {
            return true;
        }
        let directory = object.kind == DIRECTORY;
        (self.files && !directory)
            || (self.directories && directory)
            || (self.applications && directory && object.leaf.starts_with(b"!"))
            || (!directory && file_type.is_some() && object.file_type() == file_type)
    }
}

/// What a walk was asked: the command, the directory, what it chooses, the
/// tail after each object and whether each runs as a task.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(super) struct Walk<'a> {
    pub(super) command: &'a [u8],
    pub(super) directory: &'a [u8],
    pub(super) choice: Choice<'a>,
    pub(super) tail: Option<&'a [u8]>,
    pub(super) tasks: bool,
}

/// `tail` read as `*Repeat` reads it; none where it is not its syntax.
pub(super) fn walk(tail: &[u8]) -> Option<Walk<'_>> {
    let mut words = parameters(tail);
    let command = words.next()?;
    let directory = words.next()?;
    let mut walk = Walk {
        command,
        directory,
        choice: Choice::default(),
        tail: None,
        tasks: false,
    };
    while let Some(word) = words.next() {
        match switch(word)? {
            Switch::Directories => walk.choice.directories = true,
            Switch::Applications => walk.choice.applications = true,
            Switch::Files => walk.choice.files = true,
            Switch::Type => walk.choice.file_type = Some(words.next()?),
            Switch::CommandTail => walk.tail = Some(words.next()?),
            Switch::Tasks => walk.tasks = true,
        }
    }
    Some(walk)
}

impl Walk<'_> {
    /// The parts of the line run on `object`.
    fn line<'a>(&'a self, object: &'a Object) -> [&'a [u8]; 8] {
        let tail = self.tail.unwrap_or_default();
        let gap: &[u8] = if self.tail.is_some() { b" " } else { b"" };
        let task: &[u8] = if self.tasks { b"WimpTask " } else { b"" };
        [
            task,
            self.command,
            b" ",
            self.directory,
            b".",
            &object.leaf,
            gap,
            tail,
        ]
    }
}

/// `*Repeat`: the walk's lines made, then the module entered as the
/// application to run them.
pub(super) fn repeat(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (tail, _) = os.read_until(regs.r[0], MOST_LINE, |b| b < b' ')?;
    let Some(walk) = walk(&tail) else {
        return Err(os.syntax_failure(SYNTAX));
    };
    let file_type = match walk.choice.file_type {
        Some(name) => Some(type_number(os, name)?),
        None => None,
    };
    let found = match objects(os, walk.directory) {
        Ok(found) => found,
        Err(Fail::Error(_)) => Vec::new(),
        Err(unwind) => return Err(unwind),
    };
    let mut lines = VecDeque::new();
    for object in found
        .iter()
        .filter(|object| walk.choice.chooses(object, file_type))
    {
        let parts = walk.line(object);
        if parts.iter().map(|part| part.len()).sum::<usize>() >= MOST_LINE {
            return Err(os.error(tairix_riscos_kernel::error::LINE_TOO_LONG));
        }
        let line = tairix_util::fallible::joined(&parts).ok_or_else(|| os.no_memory())?;
        if lines.try_reserve(1).is_err() {
            return Err(os.no_memory());
        }
        lines.push_back(line);
    }
    super::with(os, |state: &mut State| state.walk = Some(lines))?;
    let entered = os.with_text(TITLE.as_bytes(), 0, |os, title| {
        os.with_text(b"", 0, |os, empty| {
            let mut enter = Regs::default();
            enter.r[..3].copy_from_slice(&[ENTER, title, empty]);
            os.perform(X | swi::MODULE, &mut enter)
        })
    });
    if let Err(Fail::Error(_)) = entered {
        super::with(os, |state: &mut State| state.walk = None)?;
    }
    entered
}

/// The type a `-Type` parameter names, by number or by name.
fn type_number(os: &mut Os<'_>, name: &[u8]) -> Result<u32, Fail> {
    os.with_text(name, 0, |os, at| {
        let mut regs = Regs::default();
        regs.r[..2].copy_from_slice(&[fs_control::TYPE_NUMBER, at]);
        os.perform(X | swi::FS_CONTROL, &mut regs)?;
        Ok(regs.r[2])
    })
}

/// The module's start: the walk `*Repeat` left run as the application, which
/// then exits.
pub(super) fn start(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let lines = super::with(os, |state: &mut State| state.walk.take())?.unwrap_or_default();
    match script::run(os, Source::Made(lines), OnError::Stop) {
        Ok(_) => script::exit(os),
        Err(Fail::Error(block)) => script::raise(os, block),
        Err(unwind) => Err(unwind),
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{walk, Choice, Walk};
    use crate::bootcommands::objects::{Object, DIRECTORY, FILE};

    fn object(leaf: &[u8], kind: u32, file_type: u32) -> Object {
        Object {
            leaf: leaf.to_vec(),
            kind,
            load: 0xFFF0_0000 | (file_type << 8),
            exec: 0,
        }
    }

    #[test]
    fn a_walk_is_its_command_and_directory_then_switches_by_any_leading_part_in_either_case() {
        assert_eq!(
            walk(b"Cmd Dir"),
            Some(Walk {
                command: b"Cmd",
                directory: b"Dir",
                choice: Choice::default(),
                tail: None,
                tasks: false,
            })
        );
        let walked = walk(b"Cmd Dir -f -DIR -a -Ty Obey -c \"x y\" -ta").expect("its syntax");
        let choice = walked.choice;
        assert!(choice.files && choice.directories && choice.applications && walked.tasks);
        assert_eq!(choice.file_type, Some(&b"Obey"[..]));
        assert_eq!(walked.tail, Some(&b"x y"[..]));
        assert_eq!(
            walk(b"Cmd Dir -T FEB").and_then(|w| w.choice.file_type),
            Some(&b"FEB"[..]),
            "-T is -Type, the first"
        );
    }

    #[test]
    fn what_is_not_a_walks_syntax_is_refused() {
        for tail in [
            &b""[..],
            b"Cmd",
            b"Cmd Dir -Bogus",
            b"Cmd Dir -",
            b"Cmd Dir -Type",
            b"Cmd Dir -CommandTail",
            b"Cmd Dir extra",
        ] {
            assert_eq!(
                walk(tail),
                None,
                "{}",
                core::str::from_utf8(tail).unwrap_or("?")
            );
        }
    }

    #[test]
    fn switches_choose_the_union_of_what_each_names_and_none_chooses_all() {
        let file = object(b"F", FILE, 0xFEB);
        let other = object(b"G", FILE, 0xFFF);
        let directory = object(b"D", DIRECTORY, 0);
        let application = object(b"!A", DIRECTORY, 0);
        let all = [&file, &other, &directory, &application];
        let chosen = |tail: &[u8], file_type: Option<u32>| -> Vec<Vec<u8>> {
            let walked = walk(tail).expect("its syntax");
            all.iter()
                .filter(|o| walked.choice.chooses(o, file_type))
                .map(|o| o.leaf.clone())
                .collect()
        };
        let leaves = |names: &[&[u8]]| names.iter().map(|n| n.to_vec()).collect::<Vec<_>>();
        assert_eq!(chosen(b"C D", None), leaves(&[b"F", b"G", b"D", b"!A"]));
        assert_eq!(chosen(b"C D -Files", None), leaves(&[b"F", b"G"]));
        assert_eq!(chosen(b"C D -Directories", None), leaves(&[b"D", b"!A"]));
        assert_eq!(chosen(b"C D -Applications", None), leaves(&[b"!A"]));
        assert_eq!(chosen(b"C D -Type FEB", Some(0xFEB)), leaves(&[b"F"]));
        assert_eq!(
            chosen(b"C D -Type FEB -Applications", Some(0xFEB)),
            leaves(&[b"F", b"!A"])
        );
    }

    #[test]
    fn each_line_is_the_command_the_object_and_the_tail() {
        let object = object(b"Leaf", FILE, 0xFFF);
        let joined = |tail: &[u8]| walk(tail).expect("its syntax").line(&object).concat();
        assert_eq!(joined(b"Cmd Dir"), b"Cmd Dir.Leaf");
        assert_eq!(joined(b"Cmd Dir -CommandTail -x"), b"Cmd Dir.Leaf -x");
        assert_eq!(joined(b"Cmd Dir -Tasks"), b"WimpTask Cmd Dir.Leaf");
    }
}
