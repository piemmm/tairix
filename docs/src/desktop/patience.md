# Patience (`patience.app`)

Patience is the desktop's card games: Klondike drawing one card or three,
FreeCell, and Spider with one, two or four suits. Every hint, every winnable
deal and every *no winning line remains* comes from a solver that plays the
position out, so the game can say what no rules engine alone can: whether the
game in front of the player can still be won.

It is a windowed application like any other — its own bundle, its own `Run`
binary, and only the authority its signed manifest asks for
(`CAP_CONSOLE_WRITE` and `CAP_SHM`). It reads no filesystem and spawns no
process.

## The games

| Game | Columns | Built down | Moves as a run | Stock |
|---|---|---|---|---|
| Klondike | 7 | alternating colours | any face-up alternating run | turned onto the waste, one or three at a time |
| FreeCell | 8, with 4 cells | alternating colours | as many as the free cells and empty columns carry | none |
| Spider | 10, two packs | any suit | a run of one suit | dealt one card to every column |

A completed Spider run from king to ace goes home by itself. Klondike's waste is
turned back without limit under standard scoring; Vegas scoring allows one pass
through the stock drawing one and three drawing three.

## The solver

`solver::Solver` is a depth-first search over *canonical* positions: a
position's fingerprint ignores which column or cell holds what, so equivalent
positions are searched once. Klondike's stock is searched as macro moves —
"turn the stock `n` times, then play the card that shows" — so the waste's
order is never a branch of its own. Moves home that no later move could want
undone are made for the search as the game makes them for the player, except a
waste card when drawing three, whose move changes how the threes fall.

A search is bounded by the positions it may visit and answers one of three
verdicts: won, with the line that wins; lost, when every position reachable has
been searched; or unknown, when the budget ran out first. Half the budget goes
on one search ordered by judgement alone, the one that can prove a loss; the
rest on shorter restarts whose near-ties are broken differently, because a
search lost in one corner of a game is usually quick from another.

What the game asks of it (`analysis`):

| Question | Budget (positions) | Asked |
|---|---|---|
| Can the position in play be won? | 60 000 | after every move |
| The same, for a hint asked for | 160 000 | on `H` with no line known |
| How far back along a lost line is the game still winnable? | 40 000 per probe | on `Ctrl+B` |
| Is this deal winnable? | 120 000 per deal | for every new deal, and the next one ahead of time |

A position the game can be won from has every earlier position of the same line
winnable too, so the rewind halves its way to the boundary. A probe the budget
cannot settle counts as not proven, which can only keep fewer steps than the
true boundary, never more: the position a rewind returns to is always one the
solver has won from. A deal dealt as winnable is not proved again.

A winnable deal carries a check mark on the status line. A deal search skips
every deal it proves lost; a search the player is waiting on gives up after two
deals it cannot settle either way, one made ahead of time after six, and the
first of those is then dealt, marked unchecked. A deal proven lost is never
dealt. FreeCell's deals are settled within the budget, Klondike's and one-suit
Spider's mostly are, and Spider with two or four suits seldom is, so its deals
are usually dealt unchecked. Since the next deal is always looked for while
this one is played, only the first deal of a game waits on a search.

Each worker's solver keeps room for 4 096 positions between searches and gives
back whatever a longer search grew beyond that, so a game left alone holds
little.

## Input

| Action | Pointer | Keyboard |
|---|---|---|
| Move cards to their best place | click | `Enter` |
| Move cards somewhere chosen | drag and drop | `Space` to pick up, arrows to carry, `Space` or `Enter` to put down |
| Put a hand back | release where nothing takes it | `Escape` |
| Turn the stock | click it | `D` |
| Undo / redo | menus | `Ctrl+Z`, `U`, `Backspace` / `Ctrl+Y`, `Ctrl+Shift+Z` |
| Hint | menus | `H` |
| Back to last winnable | menus | `Ctrl+B` |
| Finish | menus | `F` |
| New deal / this deal again | menus | `N`, `F2` / `R` |
| Play a deal by number | the table's own menu (secondary button) | — |
| Choose a game | menus | `1` to `6` |
| Statistics | menus | `S` |

A press takes hold of cards and only a drag past the desktop's drag slop lifts
them; a release lands them on the place they overlap most that takes them. A
click's best place is home for a lone card, then a column (one of the same suit
first in Spider), then an empty column unless that would leave one empty behind,
then a free cell; a card already home stays there, since taking one back is a
drag. Cards that can go nowhere hop where they are. The keyboard's
hand hovers over each pile the cursor reaches, and the pile is lit when the hand
may land there.

While *Send cards home* is on, moves home that no later move could want undone
are made after every move, and once the game is settled — nothing face down,
nothing in the stock or waste, every column descending — every card goes home;
with it off, *Finish* does that. The game makes these moves as part of the
player's, so one undo takes the whole group back.

## The window

The card's size is derived from the window: the table plans for the top row and
a column of seven face-up cards, and the card is the largest that fits, between
46 logical pixels wide, below which an index stops being legible, and 150. Every
length is logical and converted through the one shared scale. A column that
would run off the table closes up, face-down cards first, to no less than shows
each face-up card's rank.

The window opens at 900 × 680 logical pixels, capped to the display, and may be
dragged down to 600 × 460, wide enough for Spider's ten columns of the smallest
card. A resize is adopted as given, and a theme or density change restates only
the range.

Cards are drawn, not loaded: every face, court and back is vector geometry
rasterised at the active size into a tile, set in Noto Serif, and kept until the
size or the colours change, along with the shaded baize. Under memory pressure
the tiles and the baize are given back and drawn again when next shown.

## Motion, and what it costs when nothing moves

The table says where each card rests and the motion model says where it is
drawn. Every change to the table — a move, a move the game made, an undo, a
deal — starts each moved card's flight from wherever it is drawn at that moment,
so every change arrives on screen as movement with no code path of its own.
Cards the game moves follow one another; a card turning over narrows and widens
again with the other side showing. A won game throws every card off its
foundation to bounce along the foot of the table, once every card has landed.

The window's park carries the game's own one-shot deadline — the next frame of
anything moving, the clock's next second, a passing note's end, the kept game's
write — and **no deadline at all** otherwise, so a game left alone takes no wake
and no CPU. Each wake drains every event already delivered before one frame is
painted for them all, and each frame repaints the box around what moved.

Reduced motion starts no flight and no victory: a card simply rests where the
table puts it, and a host test asserts the frame is the table at rest.

## Scoring and statistics

Klondike is scored as the player chooses: standard points for its moves, less
two every ten seconds, with a bonus for a fast win; Vegas, at $52 a game and $5
a card home; or cumulative Vegas, its winnings carried from game to game.
Spider always starts at 500, loses one a move and wins 100 a run home. FreeCell
keeps no score.

For each game the statistics keep the games played and won, the current run,
the longest run of wins, the fastest win, the fewest moves and the best score.
Dealing again before a game is won counts it lost.

## The store

Preferences, statistics and the kept game share one document in the
application's own app-data store, private to this bundle and gated on its
kernel-attested identity:

| Keys | Holds |
|---|---|
| `game`, `scoring`, `deals.winnable`, `play.auto-home`, `cards.four-colour`, `cards.back`, `table.baize` | the preferences |
| `stats.<game>.played`, `.won`, `.streak`, `.best-streak`, `.fastest`, `.fewest`, `.best-score`; `vegas.bank` | the statistics, `<game>` one of `klondike-1`, `klondike-3`, `freecell`, `spider-1`, `spider-2`, `spider-4` |
| `saved.game`, `saved.scoring`, `saved.deal`, `saved.clock`, `saved.line.0` to `saved.line.15` | the kept game: its deal and its line of play |

A kept game is its deal and its line, replayed through the rules when it is
resumed, so it is exactly the game that was being played or refused whole.
Preferences are written only where they changed, and statistics are added to
what the store holds at the moment of writing, so a second instance never
writes over a game the first recorded.

Every write is an IPC round trip, so it goes to a worker, with one in flight;
whatever is owed meanwhile waits behind it. The game in play is kept a few
seconds after the moves stop, when the window is hidden or closed, and as the
program ends, which sees every owed write out.

## Where the code is

`userland/apps/patience`. The modules are listed in its `README.md`; the `Run`
binary composes the host-tested model over the window channel and the three
workers — the store's writer, the analyst and the dealer — and owns no game
logic of its own.
