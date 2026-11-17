# `tairix-vdu`

The Acorn VDU vocabulary the BBC Micro's MOS and RISC OS share. OpenMicro's
MOS builds its character set and control-code table from it, `lib/image` reads
a sprite's numbered mode through it, and the RISC OS emulator's VDU drivers
(RO21) are to use all three parts (`plans/RISCOS-EMULATOR.md` RO8).

## Control codes

`code::Control` is every control code, 0 to 31 and 127, and
`Control::parameters` is how many bytes each gathers before it acts. The BBC
Micro and RISC OS define them alike: one each for VDU 1, 17 and 22, two for 18
and 31, four for 28 and 29, five for 19 and 25, eight for 24 and nine for 23.

`code::Operation` is what VDU 23 does by its first parameter. RISC OS 5
performs 0 to 18 and 27 itself. It passes the rest below 32 to the
unknown-VDU 23 vector, where the font manager takes 25 and 26. From
`code::FIRST_DEFINED`, 32, the first parameter is the character VDU 23
defines.

## Numbered modes

`mode::MODES` holds RISC OS 5's modes 0 to 53 by number. Each `Mode` holds the
variables `OS_ReadModeVariable` reads for it: its pixels and text size, eigen
factors, `Log2BPP` and `Log2BPC`, `NColour`, `LineLength`, `ScreenSize`,
`YShftFactor` and `ModeFlags`. `mode::numbered` answers a mode by number, and
`None` past 53, whose depth only the module that defined it knows.

Modes 3, 6 and 7 are text modes (`Flags::NON_GRAPHIC`). RISC OS refuses a
sprite in one, so `Mode::graphic` says whether one may be in it. Mode 7 is
Teletext as RISC OS 5 keeps it on every machine it now runs on: 256 colours at
640 by 500. Mode 47's pixels are square, as RISC OS 5 defines them.

## The system character set

`font::RISC_OS` is a `Glyph` for each character from 32 to 255 in RISC OS's
arrangement:

- ISO 8859-1;
- a solid block at 127;
- RISC OS's own characters at 128 to 159: the euro, the Welsh Ŵ ŵ Ŷ ŷ, the
  ellipsis, trade mark, per mille and bullet, the single and double quotation
  marks, the dashes and minus, Œ œ, the daggers and the ligatures ﬁ ﬂ;
- the seven codes there RISC OS leaves unassigned, each showing its number in
  small hex digits.

`font::BBC` is the BBC Micro's and the Electron's arrangement of 32 to 127.
It is RISC OS's but for the pound sign at 96, where RISC OS has the grave
accent, and a hatched block at 127.

A glyph is eight rows from the top, each byte's top bit its leftmost pixel.
Strokes are two pixels wide. A capital is seven rows tall, and a small letter
five, with a row below for descenders. An accented small letter is its letter
under a two-row accent. An accented capital is a five-row capital of its own
under the same accents: O is flat at top and bottom and U at its foot, so each
stays distinct from its small letter.

`font::glyph_of` finds a character's glyph wherever it is: in RISC OS's
arrangement, or among the other characters RISC OS's alphabets hold —
Latin's other accented letters, Greek, Cyrillic and Hebrew — which the RISC
OS emulator's International draws each alphabet from. Each accent and each
letter beneath one is drawn once and composed: an accented letter is its
accent over a letter's lower six rows, a letter with a mark below has it in
the descender row, and a Greek capital's tonos stands at its left.

The designs are TAIRiX's own: OpenMicro's for 32 to 126, and new ones in the
same style for the rest. No RISC OS font is used. Past RISC OS's arrangement,
a letter several scripts share is one picture — Latin A, Greek Alpha and
Cyrillic A — as is a mark with two code points, and a test holds that nothing
else there is drawn alike. Within it, a test holds that no two characters
share a picture except the pairs drawn alike on purpose:

- the space and the no-break space;
- `"` and `”`, and `'` and `’`;
- the hyphen, the soft hyphen and the minus sign.
