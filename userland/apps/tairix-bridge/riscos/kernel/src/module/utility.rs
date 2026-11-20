//! The `UtilityModule`: the kernel as a module, first in the chain. It holds
//! the kernel's `*` commands, the Supervisor `*GOS` enters, and the two
//! instruction-barrier SWIs code built for other ARM systems issues, and it
//! refuses to die. Its command table is in RISC OS's order, which decides the
//! command an abbreviation names.

use super::SWI_OUT_OF_RANGE as BAD_SWI;
use super::{commands as rm, Builtin, Code, Command, CommandKind, Help, Resource, Swis};
use crate::cli::{commands as kc, configure, files, help, supervisor};
use crate::os::{Fail, Os, Regs};

/// The `UtilityModule`.
pub(crate) static UTILITY: Builtin = Builtin {
    title: "UtilityModule",
    help_name: None,
    bridge: true,
    swis: Some(Swis {
        chunk: 0xF0_0000,
        prefix: "ARM",
        names: &["IMB", "IMBRange"],
        handler: utility_swi,
    }),
    services: None,
    commands: &COMMANDS,
    init: None,
    finalise: Some(utility_final),
    start: Some(supervisor::start),
    routines: &[],
    vectors: &[],
    resources: &[
        Resource {
            name: "Resources.Kernel.Messages",
            file_type: 0xFFF,
            data: crate::error::MESSAGES,
        },
        Resource {
            name: "Resources.Kernel.Dictionary",
            file_type: 0xFFD,
            data: DICTIONARY,
        },
    ],
    code: None,
};

/// The dictionary help text is expanded with: no words, since the kernel's
/// help carries none to expand.
const DICTIONARY: &[u8] = &[0];

/// A command whose help is text.
const fn text(
    name: &'static str,
    (min, max): (u8, u8),
    help: &'static str,
    syntax: Option<&'static str>,
    run: Code,
) -> Command {
    Command {
        name,
        kind: CommandKind::Command,
        min,
        max,
        translate: 0,
        help: Help::Text(help),
        syntax,
        run: Some(run),
    }
}

/// A keyword with help and nothing to run.
const fn keyword(name: &'static str, help: Help) -> Command {
    Command {
        name,
        kind: CommandKind::Command,
        min: 0,
        max: 0,
        translate: 0,
        help,
        syntax: None,
        run: None,
    }
}

/// The `UtilityModule`'s commands.
static COMMANDS: [Command; 57] = [
    text(
        "Append",
        (1, 1),
        "*Append adds the lines typed after it to the end of a file, until Escape.",
        Some("Syntax: *Append <filename>"),
        files::append,
    ),
    text(
        "Build",
        (1, 1),
        "*Build makes a file of the lines typed after it, until Escape.",
        Some("Syntax: *Build <filename>"),
        files::build,
    ),
    text(
        "Close",
        (0, 0),
        "*Close closes every file open on the current filing system.",
        Some("Syntax: *Close"),
        files::close,
    ),
    text(
        "Create",
        (1, 4),
        "*Create makes a file of a length, its type data, or with execution and load addresses when they are given.",
        Some("Syntax: *Create <filename> [<length> [<exec addr> [<load addr>]]]"),
        files::create,
    ),
    text(
        "Delete",
        (1, 1),
        "*Delete deletes a file or an empty directory, and says so if there is none.",
        Some("Syntax: *Delete <object>"),
        files::delete,
    ),
    text(
        "Dump",
        (1, 3),
        "*Dump shows a file's bytes in hexadecimal and as characters, from an offset in it and as if it were loaded at an address.",
        Some("Syntax: *Dump <filename> [<file offset> [<start address>]]"),
        files::dump,
    ),
    text(
        "Exec",
        (0, 1),
        "*Exec takes what the keyboard gives from a file until it ends. With no filename, it ends the file being taken.",
        Some("Syntax: *Exec [<filename>]"),
        files::exec,
    ),
    text(
        "FX",
        (1, 5),
        "*FX calls OS_Byte with the numbers given.",
        Some("Syntax: *FX <r0> [[,] <r1> [[,] <r2>]]"),
        kc::fx,
    ),
    text(
        "GO",
        (0, 255),
        "*GO runs the code at an address, &8000 if none is given, as the application, the rest of the line its parameters.\rSyntax: *GO [<address>] [; <parameters>]",
        None,
        kc::go,
    ),
    text(
        "Help",
        (0, 255),
        "*Help gives the help on the commands, keywords and modules named. *Help . gives every piece of help there is.\rSyntax: *Help [<keywords>]",
        None,
        help::help,
    ),
    text(
        "Key",
        (1, 255),
        "*Key sets the string a function key gives, GSTrans'd as it is set.",
        Some("Syntax: *Key <keynumber> [<value>]"),
        kc::key,
    ),
    text(
        "Load",
        (1, 2),
        "*Load loads a file at its own load address, or at the one given.",
        Some("Syntax: *Load <filename> [<load addr>]"),
        files::load,
    ),
    text(
        "List",
        (1, 3),
        "*List shows a file with its lines numbered and each character as GSTrans reads it.",
        Some("Syntax: *List [-File] <filename> [-TabExpand]"),
        files::list,
    ),
    text(
        "Opt",
        (0, 2),
        "*Opt sets the current filing system's options.",
        Some("Syntax: *Opt [<x> [[,] <y>]]"),
        kc::opt,
    ),
    text(
        "Print",
        (1, 1),
        "*Print writes a file's bytes as they are.",
        Some("Syntax: *Print <filename>"),
        files::print,
    ),
    text("Quit", (0, 0), "*Quit leaves the application.", Some("Syntax: *Quit"), kc::quit),
    text(
        "Remove",
        (1, 1),
        "*Remove deletes a file or an empty directory, and says nothing if there is none.",
        Some("Syntax: *Remove <object>"),
        files::remove,
    ),
    text(
        "Save",
        (2, 6),
        "*Save writes memory to a file, from a start address to an end address or for a length, with execution and load addresses when they are given.",
        Some("Syntax: *Save <filename> <start addr> <end addr>|+<length> [<exec addr> [<load addr>]]"),
        files::save,
    ),
    text(
        "Shadow",
        (0, 1),
        "*Shadow chooses the screen bank the next mode shows.",
        Some("Syntax: *Shadow [<0|1>]"),
        kc::shadow,
    ),
    text(
        "Spool",
        (0, 1),
        "*Spool copies all the text written to a new file. With no filename, it ends the copy.",
        Some("Syntax: *Spool [<filename>]"),
        files::spool,
    ),
    text(
        "SpoolOn",
        (0, 1),
        "*SpoolOn copies all the text written to the end of a file. With no filename, it ends the copy.",
        Some("Syntax: *SpoolOn [<filename>]"),
        files::spool_on,
    ),
    text(
        "TV",
        (0, 3),
        "*TV moves the picture up or down the screen, and sets its interlace.",
        Some("Syntax: *TV [<vertical position> [[,] <interlace>]]"),
        kc::tv,
    ),
    text(
        "Type",
        (1, 3),
        "*Type shows a file, each character as GSTrans reads it.",
        Some("Syntax: *Type [-File] <filename> [-TabExpand]"),
        files::type_file,
    ),
    keyword(
        "Break",
        Help::Text("Break resets the computer, as it is reset when it starts."),
    ),
    text(
        "Cache",
        (0, 1),
        "*Cache turns the processor's caches on or off. With no parameter, it says which they are.",
        Some("Syntax: *Cache [On|Off]"),
        kc::cache,
    ),
    text(
        "ChangeDynamicArea",
        (0, 255),
        "*ChangeDynamicArea sets the sizes of the system's memory areas, in bytes, or in kilobytes, megabytes or gigabytes with K, M or G after the number.",
        Some("Syntax: *ChangeDynamicArea [-FontSize <n>] [-SpriteSize <n>] [-RamFSSize <n>] [-RMASize <n>] [-ScreenSize <n>]"),
        kc::change_dynamic_area,
    ),
    Command {
        name: "Configure",
        kind: CommandKind::Command,
        min: 0,
        max: 255,
        translate: 0,
        help: Help::Code(help::configure),
        syntax: None,
        run: Some(configure::configure),
    },
    keyword("Commands", Help::Code(help::commands)),
    text(
        "Echo",
        (0, 255),
        "*Echo writes the string it is given, GSTrans'd, and a new line.\rSyntax: *Echo <string>",
        None,
        kc::echo,
    ),
    text(
        "Error",
        (1, 255),
        "*Error raises an error with the number and text it is given.",
        Some("Syntax: *Error [<number>] <text>"),
        kc::error,
    ),
    text(
        "Eval",
        (1, 255),
        "*Eval evaluates an expression and shows its value.",
        Some("Syntax: *Eval <expression>"),
        kc::eval,
    ),
    keyword("FileCommands", Help::Code(help::file_commands)),
    text(
        "GOS",
        (0, 0),
        "*GOS enters the Supervisor, where commands are typed at a * prompt.",
        Some("Syntax: *GOS"),
        supervisor::gos,
    ),
    text(
        "IF",
        (2, 255),
        "*IF runs a command if an expression is not zero, and the command after ELSE if it is.",
        Some("Syntax: *IF <expression> THEN <command> [ELSE <command>]"),
        kc::if_command,
    ),
    text(
        "Ignore",
        (0, 1),
        "*Ignore sets the character the printer stream leaves out. With no number, it leaves none out.",
        Some("Syntax: *Ignore [<number>]"),
        kc::ignore,
    ),
    Command {
        name: "Modules",
        kind: CommandKind::Command,
        min: 0,
        max: 0,
        translate: 0,
        help: Help::Code(help::modules),
        syntax: Some("Syntax: *Modules"),
        run: Some(rm::modules_command),
    },
    keyword(
        "PowerOn",
        Help::Text("When the computer starts, it is configured as its CMOS RAM says. *Configure changes that."),
    ),
    keyword(
        "Reset",
        Help::Text("A reset clears every program from memory and starts the computer again, as it is configured."),
    ),
    text(
        "RMClear",
        (0, 0),
        "*RMClear kills every module loaded into the module area, leaving those in ROM.",
        Some("Syntax: *RMClear"),
        rm::rm_clear,
    ),
    text(
        "RMEnsure",
        (2, 255),
        "*RMEnsure runs a command when a module is missing, or older than a version.",
        Some("Syntax: *RMEnsure <module title> <version> [<*command>]"),
        rm::rm_ensure,
    ),
    text(
        "RMFaster",
        (1, 1),
        "*RMFaster copies a module from ROM into the module area and runs it from there.",
        Some("Syntax: *RMFaster <module title>"),
        rm::rm_faster,
    ),
    text(
        "RMInsert",
        (1, 2),
        "*RMInsert undoes *Unplug, so that a ROM module starts again at reset. It does not start it now.",
        Some("Syntax: *RMInsert <module title> [<podule number>]"),
        rm::rm_insert,
    ),
    text(
        "RMKill",
        (1, 1),
        "*RMKill finalises a module and removes it.",
        Some("Syntax: *RMKill <module title>"),
        rm::rm_kill,
    ),
    text(
        "RMLoad",
        (1, 255),
        "*RMLoad loads a module file into the module area and initialises it.",
        Some("Syntax: *RMLoad <filename> [<parameters>]"),
        rm::rm_load,
    ),
    text(
        "RMReInit",
        (1, 255),
        "*RMReInit finalises a module and initialises it again. A ROM module *Unplug stopped is plugged back in and started.",
        Some("Syntax: *RMReInit <module title> [<parameters>]"),
        rm::rm_reinit,
    ),
    text(
        "RMRun",
        (1, 255),
        "*RMRun loads a module file, initialises it and runs it as the application.",
        Some("Syntax: *RMRun <filename> [<parameters>]"),
        rm::rm_run,
    ),
    text(
        "RMTidy",
        (0, 0),
        "*RMTidy is kept for old scripts. It does nothing: the module area is never compacted.",
        Some("Syntax: *RMTidy"),
        rm::rm_tidy,
    ),
    text(
        "ROMModules",
        (0, 0),
        "*ROMModules lists the modules in ROM, with each one's version and whether it is running, active, dormant or unplugged.",
        Some("Syntax: *ROMModules"),
        rm::rom_modules_command,
    ),
    text(
        "Set",
        (2, 255),
        "*Set sets a variable to a string, GSTrans'd as it is set.",
        Some("Syntax: *Set <varname> <value>"),
        kc::set,
    ),
    text(
        "SetEval",
        (2, 255),
        "*SetEval sets a variable to the value of an expression, a number or a string.",
        Some("Syntax: *SetEval <varname> <expression>"),
        kc::set_eval,
    ),
    text(
        "SetMacro",
        (2, 255),
        "*SetMacro sets a variable to a string that is GSTrans'd each time it is read.",
        Some("Syntax: *SetMacro <varname> <value>"),
        kc::set_macro,
    ),
    text(
        "Show",
        (0, 1),
        "*Show lists the variables a name matches, * and # its wildcards. With no name, it lists every variable.",
        Some("Syntax: *Show [<variablespec>]"),
        kc::show,
    ),
    text(
        "Status",
        (0, 255),
        "*Status shows how an option is configured. With no option, it shows every one.\rSyntax: *Status [<option>]",
        None,
        configure::status,
    ),
    keyword(
        "Syntax",
        Help::Text("A command's help ends with its syntax: <> holds what is to be given, [] what may be left out, and | separates choices."),
    ),
    text("Time", (0, 0), "*Time shows the date and time.", Some("Syntax: *Time"), kc::time),
    text(
        "Unplug",
        (0, 2),
        "*Unplug stops a ROM module now and at every reset until *RMInsert or *RMReInit. With no title, it lists the modules unplugged.",
        Some("Syntax: *Unplug [<module title> [<podule number>]]"),
        rm::unplug,
    ),
    text(
        "Unset",
        (1, 1),
        "*Unset deletes the variables a name matches, * and # its wildcards.",
        Some("Syntax: *Unset <varname>"),
        kc::unset,
    ),
];

/// `ARM_IMB` and `ARM_IMBRange`: as `OS_SynchroniseCodeAreas`, nothing to
/// do, since no instruction is held where a store leaves it stale.
fn utility_swi(os: &mut Os<'_>, offset: u32, _regs: &mut Regs) -> Result<(), Fail> {
    match offset {
        0 | 1 => Ok(()),
        _ => Err(os.error_with(BAD_SWI, &[UTILITY.title.as_bytes()])),
    }
}

/// The kernel cannot be killed; a finalisation that need not be fatal is
/// answered as done.
fn utility_final(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[10] == 0 {
        return Ok(());
    }
    Err(os.error(super::lifecycle::CANT_KILL))
}
