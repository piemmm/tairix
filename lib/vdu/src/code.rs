//! The VDU stream's control codes, 0 to 31 and 127, which the BBC Micro's MOS
//! and RISC OS define alike, each with the parameter bytes it gathers before
//! it acts; and what VDU 23 does by its first parameter.

macro_rules! controls {
    ($($(#[$doc:meta])* $name:ident = $code:literal, $parameters:literal;)*) => {
        /// A control code.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Control {
            $($(#[$doc])* $name = $code,)*
        }

        impl Control {
            /// The control code `byte` is; `None` for a character.
            #[must_use]
            pub const fn of(byte: u8) -> Option<Self> {
                match byte {
                    $($code => Some(Self::$name),)*
                    _ => None,
                }
            }

            /// The parameter bytes it gathers before it acts.
            #[must_use]
            pub const fn parameters(self) -> u8 {
                match self {
                    $(Self::$name => $parameters,)*
                }
            }
        }
    };
}

controls! {
    /// VDU 0: nothing.
    Nothing = 0, 0;
    /// VDU 1: the next byte to the printer alone.
    PrinterNext = 1, 1;
    /// VDU 2: the printer on.
    PrinterOn = 2, 0;
    /// VDU 3: the printer off.
    PrinterOff = 3, 0;
    /// VDU 4: text at the text cursor.
    TextAtText = 4, 0;
    /// VDU 5: text at the graphics cursor.
    TextAtGraphics = 5, 0;
    /// VDU 6: the VDU drivers on.
    Enable = 6, 0;
    /// VDU 7: the bell.
    Bell = 7, 0;
    /// VDU 8: the cursor back a character.
    Back = 8, 0;
    /// VDU 9: the cursor on a character.
    Forward = 9, 0;
    /// VDU 10: the cursor down a line.
    Down = 10, 0;
    /// VDU 11: the cursor up a line.
    Up = 11, 0;
    /// VDU 12: the text window cleared, `CLS`.
    ClearText = 12, 0;
    /// VDU 13: the cursor to the line's start.
    Return = 13, 0;
    /// VDU 14: paged mode on.
    PagedOn = 14, 0;
    /// VDU 15: paged mode off.
    PagedOff = 15, 0;
    /// VDU 16: the graphics window cleared, `CLG`.
    ClearGraphics = 16, 0;
    /// VDU 17: a text colour, `COLOUR`.
    TextColour = 17, 1;
    /// VDU 18: a graphics action and colour, `GCOL`.
    GraphicsColour = 18, 2;
    /// VDU 19: a logical colour's physical colour.
    Palette = 19, 5;
    /// VDU 20: the default colours and palette.
    DefaultColours = 20, 0;
    /// VDU 21: the VDU drivers off.
    Disable = 21, 0;
    /// VDU 22: a screen mode, `MODE`.
    Mode = 22, 1;
    /// VDU 23: an [`Operation`] by its first parameter, or a character
    /// defined.
    Miscellaneous = 23, 9;
    /// VDU 24: the graphics window.
    GraphicsWindow = 24, 8;
    /// VDU 25: a `PLOT`.
    Plot = 25, 5;
    /// VDU 26: the default windows.
    DefaultWindows = 26, 0;
    /// VDU 27: nothing; the escape code.
    Escape = 27, 0;
    /// VDU 28: the text window.
    TextWindow = 28, 4;
    /// VDU 29: the graphics origin.
    Origin = 29, 4;
    /// VDU 30: the text cursor home.
    Home = 30, 0;
    /// VDU 31: the text cursor moved, `TAB`.
    MoveText = 31, 2;
    /// VDU 127: the character behind the cursor deleted.
    Delete = 127, 0;
}

/// VDU 23's first parameter from which it defines a character, that
/// character, rather than performs an operation.
pub const FIRST_DEFINED: u8 = 32;

macro_rules! operations {
    ($($(#[$doc:meta])* $name:ident = $code:literal;)*) => {
        /// What VDU 23 does by its first parameter. RISC OS 5 performs these;
        /// the others below [`FIRST_DEFINED`] it passes to the unknown-VDU 23
        /// vector, where the font manager takes 25 and 26.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum Operation {
            $($(#[$doc])* $name = $code,)*
        }

        impl Operation {
            /// The operation VDU 23's first parameter `byte` names; `None`
            /// for one passed on or a character defined.
            #[must_use]
            pub const fn of(byte: u8) -> Option<Self> {
                match byte {
                    $($code => Some(Self::$name),)*
                    _ => None,
                }
            }
        }
    };
}

operations! {
    /// 23,0: a video controller register, the cursor's shape among them.
    Controller = 0;
    /// 23,1: the text cursor shown or hidden.
    Cursor = 1;
    /// 23,2: the first fill pattern, pixel by pixel.
    Pattern1 = 2;
    /// 23,3: the second.
    Pattern2 = 3;
    /// 23,4: the third.
    Pattern3 = 4;
    /// 23,5: the fourth.
    Pattern4 = 5;
    /// 23,6: the dotted line's pattern.
    LinePattern = 6;
    /// 23,7: the text window or screen scrolled.
    Scroll = 7;
    /// 23,8: a block of the text window cleared.
    ClearBlock = 8;
    /// 23,9: the first flashing colour's time.
    FirstFlash = 9;
    /// 23,10: the second's.
    SecondFlash = 10;
    /// 23,11: the default fill patterns.
    DefaultPatterns = 11;
    /// 23,12: the first fill pattern, by colour.
    SimplePattern1 = 12;
    /// 23,13: the second.
    SimplePattern2 = 13;
    /// 23,14: the third.
    SimplePattern3 = 14;
    /// 23,15: the fourth.
    SimplePattern4 = 15;
    /// 23,16: how the cursor moves.
    CursorMovement = 16;
    /// 23,17: tints, the fill patterns' reading, character sizes and the
    /// colours' details, by its second parameter.
    Colours = 17;
    /// 23,18: Teletext's display.
    Teletext = 18;
    /// 23,27: sprites.
    Sprites = 27;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_control_code_gathers_its_parameters_and_nothing_else_is_one() {
        let gathered: [u8; 32] = core::array::from_fn(|code| {
            let code = u8::try_from(code).unwrap();
            Control::of(code).unwrap().parameters()
        });
        assert_eq!(
            gathered,
            [
                0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 2, 5, 0, 0, 1, 9, 8, 5, 0, 0,
                4, 4, 0, 2
            ]
        );
        assert_eq!(Control::of(127), Some(Control::Delete));
        assert_eq!(Control::Delete.parameters(), 0);
        for byte in (32..127).chain(128..=255) {
            assert_eq!(Control::of(byte), None, "{byte}");
        }
        for code in (0..32).chain([127]) {
            assert_eq!(Control::of(code).map(|control| control as u8), Some(code));
        }
    }

    #[test]
    fn vdu_23_performs_what_risc_os_5_does_and_passes_the_rest_on() {
        let performed: [bool; 32] =
            core::array::from_fn(|byte| Operation::of(u8::try_from(byte).unwrap()).is_some());
        let passed_on = [19, 20, 21, 22, 23, 24, 25, 26, 28, 29, 30, 31];
        for (byte, &done) in performed.iter().enumerate() {
            assert_eq!(done, !passed_on.contains(&byte), "23,{byte}");
        }
        for byte in FIRST_DEFINED..=255 {
            assert_eq!(Operation::of(byte), None, "a character defined");
        }
        assert_eq!(Operation::of(27), Some(Operation::Sprites));
    }
}
