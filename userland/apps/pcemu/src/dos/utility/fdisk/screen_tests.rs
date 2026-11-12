//! The display engine: DOS 4.0's message codes turned into the calls that
//! draw them, painted here onto a grid as the BIOS would paint the screen.

use super::*;
use crate::dos::utility::fdisk::text;
use alloc::string::String;

/// The screen a blank one becomes once `screen`'s calls are made: each
/// cell's character and attribute, and the bells rung.
fn painted(screen: &Screen) -> ([[(u8, u8); 80]; 25], usize) {
    let mut grid = [[(b' ', attr::REGULAR); 80]; 25];
    let mut bells = 0;
    let ops = screen.encoded(usize::MAX);
    let mut at = 0;
    loop {
        match ops[at] {
            op::TEXT => {
                let [row, column, attribute, len] =
                    [ops[at + 1], ops[at + 2], ops[at + 3], ops[at + 4]];
                for (i, &byte) in ops[at + 5..at + 5 + usize::from(len)].iter().enumerate() {
                    grid[usize::from(row)][usize::from(column) + i] = (byte, attribute);
                }
                at += op::TEXT_HEADER + usize::from(len);
            }
            op::CLEAR => {
                for row in ops[at + 1]..=ops[at + 3] {
                    for column in ops[at + 2]..=ops[at + 4] {
                        grid[usize::from(row)][usize::from(column)] = (b' ', attr::REGULAR);
                    }
                }
                at += op::CLEAR_LEN;
            }
            op::BEEP => {
                bells += 1;
                at += 1;
            }
            _ => return (grid, bells),
        }
    }
}

/// Row `row` of `grid`'s text, its blanks on the right dropped.
fn row_text(grid: &[[(u8, u8); 80]; 25], row: usize) -> String {
    let line: String = grid[row]
        .iter()
        .map(|&(byte, _)| char::from(byte))
        .collect();
    String::from(line.trim_end())
}

#[test]
fn a_message_moves_down_its_rows_and_chooses_its_attributes() {
    let mut screen = Screen::new();
    screen.draw(text::OPTIONS, &[]);
    let (grid, _) = painted(&screen);
    assert_eq!(
        row_text(&grid, 4),
        "                                 FDISK Options"
    );
    assert_eq!(grid[4][33].1, attr::HIGH);
    assert_eq!(
        row_text(&grid, 10),
        "    1. Create DOS Partition or Logical DOS Drive"
    );
    assert_eq!(
        (grid[10][4].1, grid[10][7].1),
        (attr::HIGH, attr::REGULAR),
        "the number highlighted"
    );
    assert_eq!(row_text(&grid, 13), "    4. Display partition information");
    assert_eq!(row_text(&grid, 24), "    Press Esc to exit FDISK");
    assert_eq!(
        (grid[24][10].1, grid[24][14].1),
        (attr::HIGH, attr::REGULAR),
        "Esc highlighted"
    );
}

#[test]
fn inserts_are_drawn_in_turn_and_the_answer_is_typed_where_marked() {
    let mut screen = Screen::new();
    screen.draw(text::CURRENT_DISK, b"2");
    screen.draw(text::ENTER_CHOICE, &[]);
    let (grid, _) = painted(&screen);
    assert_eq!(row_text(&grid, 6), "    Current fixed disk drive: 2");
    assert_eq!(grid[6][30], (b'2', attr::HIGH));
    assert_eq!(screen.input, Some((17, 19)));
    let mut short = Screen::new();
    short.draw(text::NOT_A_CHOICE, b"9");
    let (grid, bells) = painted(&short);
    assert_eq!(
        row_text(&grid, 23),
        "    9 is not a choice. Please enter    .",
        "missing inserts are blanks"
    );
    assert_eq!(bells, 1);
}

#[test]
fn a_line_cleared_from_a_mark_keeps_what_comes_before_it() {
    let mut screen = Screen::new();
    screen.draw("^0000^<R>keep this", &[]);
    screen.draw("^0005^<C>new", &[]);
    let (grid, _) = painted(&screen);
    assert_eq!(row_text(&grid, 0), "keep new");
    let mut blink = Screen::new();
    blink.draw(text::DELETE_PRIMARY_WARNING, &[]);
    let (grid, _) = painted(&blink);
    assert_eq!(
        grid[16][4].1,
        attr::HIGH | attr::BLINK,
        "the warning blinks"
    );
    assert_eq!(grid[16][13].1, attr::REGULAR, "its text does not");
    assert_eq!(blink.input, Some((17, 53)));
}

#[test]
fn a_run_of_text_is_one_call_and_an_encoding_ends_on_a_whole_one() {
    let mut screen = Screen::new();
    screen.draw("^0102^<R>abc", &[]);
    assert_eq!(
        screen.encoded(usize::MAX),
        [op::TEXT, 1, 2, attr::REGULAR, 3, b'a', b'b', b'c', op::END]
    );
    screen.draw("^0000^<H>de", &[]);
    assert_eq!(
        screen.encoded(10),
        [op::TEXT, 1, 2, attr::REGULAR, 3, b'a', b'b', b'c', op::END],
        "the second left out whole"
    );
    let mut edge = Screen::new();
    edge.draw("^0078^abcd", &[]);
    let (grid, _) = painted(&edge);
    assert_eq!(
        (grid[0][78].0, grid[0][79].0),
        (b'a', b'b'),
        "nothing past the last column"
    );
}
