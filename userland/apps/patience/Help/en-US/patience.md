## NAME

patience — Klondike, FreeCell and Spider, with a solver behind every hint

## SYNOPSIS

`patience`

## DESCRIPTION

Opens a desktop window laid out for a game of patience, the card games also
called solitaire. Six games are on the Game menu, and `1` to `6` choose them:
Klondike drawing one card or three, FreeCell, and Spider with one, two or four
suits.

In Klondike, build the seven columns down in alternating colours and send each
suit home to its foundation, from the ace to the king. Click the stock to turn
cards onto the waste; once it is empty, click its place to turn the waste back.
In FreeCell every card is dealt face up and each of the four free cells holds
one card; a run moves as one through the free cells and empty columns as long as
they have room for it. In Spider, build down in any suit, but move a run only
when it is all one suit; a run from the king down to the ace goes home by
itself. Click the stock to deal a card onto every column, which needs every
column to hold one first.

Drag cards where you want them, or click them: a click sends cards to the best
place they can go, home first, then onto another column, then to an empty
column, then to a free cell. Cards that can go nowhere hop where they are.
Cards no later move could need go home by themselves, and once nothing can stop
the game being won they all do. With Send cards home turned off, Finish (`F`)
sends them.

Every hint comes from a solver that plays the game out ahead of you, so a hint
(`H`) is a move that keeps the game winnable, not merely one the rules allow.
The solver looks at every position you reach. When it proves that no winning
line is left, the status line says so, and Back to last winnable (`Ctrl+B`)
takes you back to the last position the game could still be won from.

With Winnable deals only turned on, as it is at first, every new deal is one the
solver has already won, and a check mark beside the deal's number says so. The
solver gives up on a deal it cannot settle quickly, and the deal it then gives
you is marked as unchecked; Spider with four suits is often too deep to settle
in time. Every deal is numbered on the status line, and the table's own menu,
opened with the secondary button, plays a deal by its number. FreeCell's deals
are numbered as other FreeCell games number them, so a numbered deal is the
same game everywhere.

Klondike is scored the standard way, or as Vegas, where each game costs $52 and
each card sent home wins $5, with one pass through the stock drawing one card
and three drawing three, or as cumulative Vegas, which carries the winnings from
game to game. Spider starts at 500 points, loses one for each move and wins 100
for each run sent home. For each game the statistics keep the games played and
won, the current run of wins or losses, the longest run of wins, the fastest
win and the win in fewest moves. Dealing again before a game is won counts it
as lost.

The game in play is kept, so closing the window or quitting carries it over to
next time, and the clock stops while the window is hidden. The game's icon-bar
menu and the table's own menu hold the options: winnable deals, the scoring,
sending cards home, four-colour suits, the card backs and the table.

The keyboard plays everything the pointer does. The arrow keys move a ring from
pile to pile and up a column's cards; `Space` picks the cards up and puts them
down wherever the ring is, `Enter` sends them to their best place, and `Escape`
puts them back. `D` turns the stock, `Ctrl+Z` takes a move back and `Ctrl+Y`
makes it again, `N` deals a new game, `R` deals this one again, and `S` shows
the statistics.

The game is launched from the desktop's Program Library, under Games, or by name
from a shell. It requires a running graphical session: without one the window
channel is unreachable and the game reports the refusal on the standard error
stream and exits.

## EXIT STATUS

Zero after a clean quit; non-zero when the window channel or the shared frame
region was refused (the reason is stated on the standard error stream).
