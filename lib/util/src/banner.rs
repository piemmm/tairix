//! The `==> name <==` banner `head` and `tail` set before each source they
//! show when they show several.

use alloc::format;
use core::fmt::Display;

use tairix_abi::Errno;

use crate::output::Output;
use crate::quote::{output_style, Quoted, Style};

/// The banners of one run over one output: whether one is written yet, and
/// the style that output shows a name from elsewhere in ([`output_style`]).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Banners {
    written: bool,
    names: Style,
}

impl Banners {
    /// No banner written yet over `out`.
    #[must_use]
    pub fn new(out: &dyn Output) -> Self {
        Self {
            written: false,
            names: output_style(out.at_terminal()),
        }
    }

    /// The banner for the file `path`.
    ///
    /// # Errors
    ///
    /// The stream's own refusal.
    pub fn file(&mut self, out: &dyn Output, path: &str) -> Result<(), Errno> {
        let names = self.names;
        self.write(out, Quoted::new(path, names))
    }

    /// The banner for standard input, named as GNU names it.
    ///
    /// # Errors
    ///
    /// The stream's own refusal.
    pub fn stdin(&mut self, out: &dyn Output) -> Result<(), Errno> {
        self.write(out, "standard input")
    }

    /// A blank line comes before every banner but the first, as GNU separates
    /// them.
    fn write(&mut self, out: &dyn Output, name: impl Display) -> Result<(), Errno> {
        let gap = if self.written { "\n" } else { "" };
        self.written = true;
        out.write_all(format!("{gap}==> {name} <==\n").as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;
    use core::cell::RefCell;

    use tairix_abi::Errno;

    use super::Banners;
    use crate::output::Output;

    #[derive(Default)]
    struct Capture {
        bytes: RefCell<Vec<u8>>,
        terminal: bool,
    }

    impl Output for Capture {
        fn write_all(&self, bytes: &[u8]) -> Result<(), Errno> {
            self.bytes.borrow_mut().extend_from_slice(bytes);
            Ok(())
        }

        fn at_terminal(&self) -> bool {
            self.terminal
        }
    }

    #[test]
    fn a_banner_names_its_source_as_the_output_shows_names() {
        for (terminal, named) in [(false, "e\u{1b}[2Jx"), (true, "'e'$'\\033''[2Jx'")] {
            let out = Capture {
                terminal,
                ..Capture::default()
            };
            let mut banners = Banners::new(&out);
            banners.file(&out, "e\u{1b}[2Jx").expect("captured");
            banners.stdin(&out).expect("captured");
            assert_eq!(
                out.bytes.borrow().as_slice(),
                alloc::format!("==> {named} <==\n\n==> standard input <==\n").as_bytes()
            );
        }
    }
}
