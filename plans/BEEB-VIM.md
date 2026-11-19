# BEEB-VIM — the Open Micro emulator's editor ROM

Binding under `AGENTS.md`. This plan owns `vim.rom`: a vi-style text editor
for the BBC Micro Model B as a 16 KiB sideways ROM, written with the crate's
6502 builder like the clean-room ROMs (`plans/BEEB.md` §3), its commands
those of the TAIRiX `vim` command (`plans/VIM.md`) where 16 KiB allows. It
has no original, so every observation of it is this tree's own.

Read first: `plans/BEEB.md` §0 and §3, `plans/VIM.md`.

## Ledger

| Item | What | Status |
|---|---|---|
| VI1 | The frame: header, `*VIM` and `*HELP VIM`, the language entry, workspace, keys and BRK, leaving for the language it was started from | done |
| VI2 | The text: the gap buffer, the register and the undo record, lines | done |
| VI3 | The screen: modes, text rows and the status line, sideways scrolling, redraws scoped to what changed | done |
| VI4 | Normal and insert modes: motions, counts, operators, edits, undo, repeat, search | done |
| VI5 | The command line: files, lines, substitution, options, modes, OS commands | done |
| VI6 | Files: text as `*BUILD` writes it, tokenised BASIC listed on load and tokenised on save | done |
| VI7 | Highlighting by the colours a mode has | done |
| VI9 | Help: `:help` and `*HELP VIM` | done |
| VI10 | Shipped: the firmware image, fitted by default, the app's help and docs | done |
| VI11 | Sideways RAM: the text, the undo record and the register running on into free banks once main memory is full, so a file larger than main memory is read, edited and written | done |

## 0. Binding decisions

1. **A language with a service entry** (ROM type &C2). `*VIM [file]` from
   any language records the current language to leave for and enters this
   one through OSBYTE &8E. Nothing is claimed at reset, so fitting the ROM
   leaves PAGE where it was and the machine's start-up unchanged.
2. **Workspace is a language's**: zero page &00–&6F, leaving &70–&8F to the
   user, and pages &04–&07. The text, the undo record and the register lie
   in that order in one memory from OSHWM (3), the text a gap buffer whose
   gap moves only when an edit needs it there.
3. **Sideways RAM.** That memory is main memory up to HIMEM, then each bank
   of sideways RAM the editor holds from the bank's second page: the first
   keeps the claim's mark, and whole pages then map to whole pages, so an
   address past HIMEM becomes a bank's by its high byte alone. Sixteen-bit
   addresses reach two banks past any HIMEM, so the editor holds at most
   two, 32,256 bytes, and every mode reaches both. A bank is taken only when
   main memory and the banks held are full — for an edit, a read, a listing
   or a mode — so a text with room below HIMEM takes none, and one taken is
   held until the editor next starts a fresh text. A bank is free where the
   MOS found no ROM in it (the type table at &02A1), it holds no ROM image's
   copyright since, and a write to it holds; the claim leaves its copyright
   offset pointing at itself, so no BREAK takes it for a ROM. What pages a
   bank in runs from page &07, copied there at each entry, since a bank
   paged in at &8000 pages the editor out; it pages through &F4 and &FE30 as
   the MOS does, and while a bank is held a move goes a part at a time, none
   crossing a page at either end. An Electron, whose ROMs page elsewhere,
   gives none. A held bank another program took while the editor ran an OS
   command, met an error or was reset loses the text with it: the text, the
   undo record and the register are emptied, the name dropped so nothing
   writes the empty text over its file, and the editor says so. A bank is
   known by its mark, as the MOS knows a ROM by its header, so a command
   that writes into a held bank past the mark goes unnoticed, as one
   writing into the text below HIMEM does. A delete
   takes room for the undo record and the register before either is made,
   so one refused for want of it changes nothing.
4. **Keys.** While it runs the cursor keys give codes (`*FX4,1`), ESCAPE
   is a key (`*FX229,1`) and CAPS LOCK and SHIFT LOCK are off, since vi
   tells its commands apart by case; all three are put back as found on
   leaving. It enables interrupts on entry, as MOS 1.20 leaves a language
   to. An OS error is reported on the status line, the keys and the mode
   are taken again, and the editor carries on with its text.
5. **Lines** end in CR in the buffer and hold at most 255 characters; a
   file with a longer line is refused on load, and an edit that would make
   one is refused.
6. **Files.** Text is lines ending in CR, as `*BUILD` writes them; LF,
   CR LF and LF CR (as `*SPOOL` writes them) are read and written back as
   found, a last line given its end. A tokenised BASIC II
   program is listed to text on load, each line its number and its text as
   stored, and tokenised on save by `lib/openbasic`'s BASIC II rules; the
   tests hold the ROM's two converters to `lib/openbasic`'s `tokenise` and
   listing byte for byte. A save refuses a line without a number, a number
   past 32767 or numbers out of order; a blank line is dropped. A file
   keeps its kind when written back, and `:set ft` changes it. A text with
   single line ends lying wholly below HIMEM is written from where it lies
   by one OSFILE call. Anything else — a program, paired line ends, a text
   past HIMEM — is built in the gap in main memory and written by one
   OSFILE call; once it outgrows the gap or main memory, each finished part
   is written with OSGBPB to the file opened for it, the line in progress
   kept back so its length can still be set, and OSFILE 1 gives the file
   its addresses, so a file that loaded can always be saved in one pass. A
   file is read by one OSFILE call where it fits below HIMEM, and otherwise
   a page at a time with OSGBPB, each page moved on into the room made for
   it. The DFS spends a revolution of the disc on each sector OSGBPB moves,
   however much one call asks for, so a file past HIMEM reads and writes in
   about five times as long as OSFILE takes; the original DFS 1.20 is as
   slow.
7. **Modes.** Each mode the Model B has, chosen with `:mode N`; a mode whose
   screen leaves the text too little room takes on a bank of sideways RAM,
   and is refused where none is free. Highlighting
   follows the colours a mode has: MODE 7 by teletext codes, each taking the
   cell before the run it colours; MODE 2 by its colours; MODE 1 and 5 by
   four, the palette set; MODE 0, 3, 4 and 6 not at all.
8. **Highlighting** marks keywords, strings, numbers and line numbers, the
   rest of a line after `REM` or `DATA`, and star commands, by the same
   keyword matching the tokeniser uses, so what is coloured as a keyword is
   what a save tokenises. A BASIC program is highlighted with its line
   numbers; a text, such as a `!BOOT` file `*EXEC` types, as statements.
9. **BREAK** re-enters the editor as the language; a text whose workspace
   survived is kept.
10. **Name.** The ROM is titled `VIM`, the TAIRiX vim's form for the BBC
    Micro; it claims no other ROM's name.
11. **It runs on the originals as on the clean-room ROMs.** It reaches the
    machine only through MOS 1.20's documented interface — the OS calls and
    vectors, the OSBYTE calls, the ROM type table where OSBYTE &AA says it
    lies, the error pointer at &FD, the paging register at &FE30 with its
    copy at &F4 — so it runs alike under MOS 1.20, BASIC II and DFS 1.20
    and their clean-room equivalents, and fitted beside either set, and on
    an Electron. It is checked against the originals as black boxes, as
    the corpus is, outside the tree.

## 1. What it does

Normal mode: `h j k l`, the cursor keys, SPACE and DELETE, `0 ^ $`,
`w b e`, `G` and `gg` with a count, `H M L`, CTRL-F, CTRL-B, CTRL-D, CTRL-U;
`x X r ~ J`, `i a I A o O`, `d c y` over a motion and `dd cc yy D C Y`,
`p P`, `u` (the last change, undone and redone in turn), `.`, `/ ? n N`,
`ZZ`, CTRL-G and CTRL-L. Insert mode types, DELETE rubs out what it typed,
RETURN breaks the line, ESCAPE leaves.

The command line: `:w [file]`, `:q`, `:q!`, `:wq`, `:x`, `:e [file]`,
`:e!`, `:r file`, `:N`, `:$`, `:[%]s/old/new/[g]`, `:set nu`, `:set nonu`,
`:set ft=basic|text`, `:syntax on|off`, `:mode N`, `:!command` and
`:help`; `:d` deletes the range's lines, and `ZQ` leaves as `:q!` does.
