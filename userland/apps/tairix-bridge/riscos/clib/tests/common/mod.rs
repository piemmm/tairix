//! A session the probe kit's programs run in — the filing system, BASIC and
//! the library, HostFS over an in-memory disc — and the programs built: the
//! test's body assembled into a C client by BASIC V, then run.
#![allow(
    dead_code,
    reason = "each test crate including this uses the helpers its own tests need"
)]

mod kit_text;

use std::collections::BTreeSet;
use std::fmt::Write;
use std::sync::LazyLock;

use tairix_fscopy::fixture::Tree;
use tairix_riscos_filing::session::{command, disc_file, session, Setup, DISC};
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Fail, Kernel, Outcome, Unwind};

pub use kit_text::{
    C_INIT_PROCEDURE, C_STUBS, INIT_PROCEDURE, PRELUDE_ERROR, RUN, START, STUBS, TAIL, TRAPS,
};

/// The modules: the filing system's, BASIC's and the library's.
static MODULES: LazyLock<Vec<&'static Builtin>> = LazyLock::new(|| {
    tairix_riscos_filing::session::modules_with(&[
        &tairix_riscos_basic::MODULES,
        &tairix_riscos_clib::MODULES,
    ])
});

/// The instructions a program's run may spend.
const BUDGET: u64 = 1 << 32;
/// The application slot's bytes: room for the client allocators' pools the
/// programs keep at 1 MiB.
const SLOT: u32 = 8 << 20;

/// A client to build: its body, run with R11 the out area, the procedures
/// its language gives after the initialisation procedure, code before
/// `LibInit`, code after the kit's, its results' bytes, and whether it is a
/// C program.
pub struct Build<'a> {
    pub body: &'a str,
    pub procedures: &'a [&'a str],
    pub prelude: &'a str,
    pub extra: &'a str,
    pub results: u32,
    pub c: bool,
}

impl Build<'_> {
    /// `body` in the kit's plain client.
    pub const fn plain(body: &str) -> Build<'_> {
        Build {
            body,
            procedures: &["0"],
            prelude: "",
            extra: "",
            results: 8192,
            c: false,
        }
    }

    /// `body` in a C program: chunk 2 in its stubs, its language C's, whose
    /// initialisation procedure calls `_clib_initialise` and whose
    /// finalisation, trap and event procedures are the C library's.
    pub const fn c(body: &str) -> Build<'_> {
        Build {
            body,
            procedures: &C_PROCEDURES,
            prelude: PRELUDE_ERROR,
            extra: "",
            results: 8192,
            c: true,
        }
    }

    /// `body` with the kit's finalisation, trap and uncaught trap
    /// procedures, and its error handler set before `LibInit`.
    pub const fn trapping(body: &str) -> Build<'_> {
        Build {
            body,
            procedures: &["finproc", "trapproc", "uncaught"],
            prelude: PRELUDE_ERROR,
            extra: TRAPS,
            results: 8192,
            c: false,
        }
    }
}

/// The C language's procedures after its initialisation procedure: the
/// library's `Finalise`, its trap and event handlers, and no others.
const C_PROCEDURES: [&str; 8] = [
    "cvec+4*181",
    "cvec+4*0",
    "cvec+4*1",
    "cvec+4*2",
    "cvec+4*3",
    "0",
    "0",
    "0",
];

/// The BASIC program that assembles `build`'s client at &8000 and saves it
/// as `HostFS:$.CTest`.
pub fn program(build: &Build<'_>) -> String {
    let size = 4 * (5 + build.procedures.len());
    let procedures = build
        .procedures
        .iter()
        .fold(String::new(), |mut words, procedure| {
            let _ = writeln!(words, "DCD {procedure}");
            words
        });
    let (name, initialisation, stubs) = if build.c {
        ("C", C_INIT_PROCEDURE, C_STUBS)
    } else {
        ("Probe", INIT_PROCEDURE, STUBS)
    };
    let language = format!(
        ".lang DCD {size}\nDCD start\nDCD codeend\nDCD langname\nDCD initproc\n{procedures}.langend\n.langname EQUS \"{name}\"\nEQUB 0\nALIGN\n"
    );
    let start = START.replace(
        ".start SWI \"OS_GetEnv\"",
        &format!(".start{}\nSWI \"OS_GetEnv\"", build.prelude),
    );
    let tail = TAIL.replace("RESB", &build.results.to_string());
    let assembly = format!(
        "{start}{language}{initialisation}{RUN}{}{tail}{}{stubs}",
        build.body, build.extra
    );
    let statics = if build.c {
        "cvec=kvec+4*96:kstat=cvec+4*366:cstat=kstat+&31C:statend=cstat+&B48"
    } else {
        "kstat=kvec+4*96:statend=kstat+&31C"
    };
    let layout = format!(
        "kvec=(P%+3) AND NOT 3:{statics}:res=statend:ws=res+{}",
        build.results
    );
    numbered(&Builder {
        name: "Mk",
        origin: 0x8000,
        assembly: &assembly,
        layout: &layout,
        save: "SYS \"OS_File\",10,\"HostFS:$.CTest\",&FF8,,code%,O%",
    })
}

/// A BASIC program assembling `assembly` in two passes at `origin`, then
/// `layout` setting the labels past the code and `save` run; an error goes
/// to `HostFS:$.<name>Err`.
pub struct Builder<'a> {
    pub name: &'a str,
    pub origin: u32,
    pub assembly: &'a str,
    pub layout: &'a str,
    pub save: &'a str,
}

/// The program `builder` describes, numbered.
pub fn numbered(builder: &Builder<'_>) -> String {
    let mut lines = vec![
        format!(
            "ON ERROR E%=OPENOUT\"HostFS:$.{}Err\":BPUT#E%,REPORT$+\" at \"+STR$ERL:CLOSE#E%:QUIT",
            builder.name
        ),
        "DIM code% &10000".to_string(),
        "FOR pass%=4 TO 6 STEP 2".to_string(),
        format!("P%=&{:X}:O%=code%", builder.origin),
        "[OPT pass%".to_string(),
    ];
    lines.extend(
        builder
            .assembly
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string),
    );
    lines.extend([
        "]".to_string(),
        builder.layout.to_string(),
        "NEXT".to_string(),
    ]);
    lines.extend([builder.save.to_string(), "QUIT".to_string()]);
    lines
        .iter()
        .enumerate()
        .fold(String::new(), |mut program, (n, line)| {
            let _ = writeln!(program, "{} {line}", 10 * (n + 1));
            program
        })
}

/// What a client's run left: its results and statics, how it ended, and the
/// disc as it left it.
pub struct Probe {
    pub results: Vec<u8>,
    pub statics: Vec<u8>,
    /// What a child program saved, where one ran.
    pub child: Vec<u8>,
    pub outcome: Outcome,
    pub disc: Tree,
}

impl Probe {
    /// The results' word at `at`.
    pub fn word(&self, at: u32) -> u32 {
        word_at(&self.results, at)
    }

    /// The out area's word at `at`.
    pub fn out(&self, at: u32) -> u32 {
        self.word(400 + at)
    }

    /// The `n` words from `at`.
    pub fn words(&self, at: u32, n: u32) -> Vec<u32> {
        (0..n).map(|k| self.word(at + 4 * k)).collect()
    }

    /// The first path the kit marked: 0x10 a trap procedure, 0x20 an
    /// uncaught trap procedure, 0x30 the finalisation procedure, 0x40 the
    /// body run through, 0x50 the prelude's error handler.
    pub fn path(&self) -> u32 {
        self.word(1792)
    }

    /// The first error the prelude's handler took: its number and text.
    pub fn first_error(&self) -> (u32, String) {
        let text = &self.results[1752..1792];
        let end = text.iter().position(|&b| b == 0).unwrap_or(text.len());
        (
            self.word(1748),
            String::from_utf8_lossy(&text[..end]).into_owned(),
        )
    }

    /// The kernel statics' word at `at`.
    pub fn statics_word(&self, at: u32) -> u32 {
        word_at(&self.statics, at)
    }

    /// The child's saved word at `at`.
    pub fn child_word(&self, at: u32) -> u32 {
        word_at(&self.child, at)
    }
}

fn word_at(bytes: &[u8], at: u32) -> u32 {
    let at = at as usize;
    bytes.get(at..at + 4).map_or(0, |word| {
        u32::from_le_bytes(word.try_into().expect("four bytes"))
    })
}

/// `build`'s client assembled by BASIC V and run, its files read back.
pub fn run(build: &Build<'_>) -> Probe {
    run_program(&program(build), "Run HostFS:$.CTest probe args", &[])
}

/// `program` run by `*BASIC -quit`, then `line`, with `files` on the disc
/// too: the files the run left.
pub fn run_program(program: &str, line: &str, files: &[(&str, &[u8])]) -> Probe {
    run_built(&[("Mk", program)], line, files)
}

/// Each of `builders` run by `*BASIC -quit` in turn, then `line`, with
/// `files` on the disc too: the files the run left.
pub fn run_built(builders: &[(&str, &str)], line: &str, files: &[(&str, &[u8])]) -> Probe {
    let mut tree = Tree::new().dir(DISC);
    for (name, text) in builders {
        tree = tree.file(&format!("{DISC}/{name},fff"), text.as_bytes());
    }
    for (name, bytes) in files {
        tree = tree.file(&format!("{DISC}/{name}"), bytes);
    }
    let mut kernel = hosted(&tree);
    for (name, _) in builders {
        started(&mut kernel, &format!("BASIC -quit HostFS:$.{name}"));
        kernel.run(BUDGET);
        if let Some(error) = disc_file(&tree, &format!("{name}Err")) {
            panic!("{name} failed: {}", String::from_utf8_lossy(&error));
        }
    }
    started(&mut kernel, line);
    let outcome = kernel.run(BUDGET);
    let mut probe = Probe {
        results: disc_file(&tree, "CRes").unwrap_or_default(),
        statics: disc_file(&tree, "CStat").unwrap_or_default(),
        child: disc_file(&tree, "ChildRes").unwrap_or_default(),
        outcome,
        disc: tree,
    };
    probe.results.resize(probe.results.len().max(8192), 0);
    probe
}

fn hosted(tree: &Tree) -> Kernel {
    session(&Setup {
        vdu: None,
        modules: MODULES.as_slice(),
        bytes: SLOT,
        keys: &[],
        disc: Some(tree),
    })
    .kernel
}

fn started(kernel: &mut Kernel, line: &str) {
    let entered = command(&mut kernel.os(), line);
    assert!(
        matches!(entered, Err(Fail::Unwind(Unwind::Transfer))),
        "{line}: an application entered, not {entered:?}"
    );
}

/// Assembler calling the library's entries, each answer stored in the out
/// area; a label named `Lp_x` in a call's setup reaches `x` through the
/// literal pool.
#[derive(Default)]
pub struct Calls {
    lines: Vec<String>,
    literals: Vec<String>,
    pooled: BTreeSet<String>,
    slot: u32,
}

impl Calls {
    /// The library kernel's entry `entry` called after `setup`, its R0 —
    /// and R1 where `two` — stored: the out area's offset they are at.
    pub fn call(&mut self, entry: u32, setup: &[&str], two: bool) -> u32 {
        self.call_through("kvec", entry, setup, two)
    }

    /// The C library's entry `entry` called as [`Calls::call`] calls one.
    pub fn c(&mut self, entry: u32, setup: &[&str], two: bool) -> u32 {
        self.call_through("cvec", entry, setup, two)
    }

    fn call_through(&mut self, vector: &str, entry: u32, setup: &[&str], two: bool) -> u32 {
        let slot = self.slot;
        for line in setup {
            self.raw(line);
        }
        self.lines.push(format!("BL {vector}+4*{entry}"));
        self.lines.push(format!("STR r0, [r11, #{slot}]"));
        self.slot += 4;
        if two {
            self.lines.push(format!("STR r1, [r11, #{}]", slot + 4));
            self.slot += 4;
        }
        slot
    }

    /// `register` loaded with `value` from the literal pool.
    pub fn load(&mut self, register: &str, value: i64) -> String {
        let name = format!("Lv{}", self.literals.len());
        self.literals.push(format!(".{name} DCD {value}"));
        format!("LDR {register}, {name}")
    }

    /// `line` as it stands.
    pub fn raw(&mut self, line: &str) {
        for word in line.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            if let Some(label) = word.strip_prefix("Lp_") {
                if self.pooled.insert(label.to_string()) {
                    self.literals.push(format!(".Lp_{label} DCD {label}"));
                }
            }
        }
        self.lines.push(line.to_string());
    }

    /// The body: the calls, the branch to the kit's tail, the pool, then
    /// `data`.
    pub fn body(&self, data: &str) -> String {
        format!(
            "{}\nB tail\n{}\n{data}\n",
            self.lines.join("\n"),
            self.literals.join("\n")
        )
    }
}
