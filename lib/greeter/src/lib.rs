//! The shared **authentication surface** every place TAIRiX asks a user to
//! prove who they are at the screen (`lib/greeter` —
//! `plans/NEW-DESKTOP-LOGIN.md`).
//!
//! One crate owns what such a surface *is* — the account tiles or the chosen
//! account's disc, name, and masked field centred on the screen, under one row
//! of chrome naming the machine and the time — together with the wording, the
//! geometry, and the state machine that turns a keystroke into a question for
//! an authority and its answer into a verdict. It knows nothing of a
//! compositor, a window manager, a seat, or IPC: an embedder gives it events,
//! a [`Verifier`] and the answers that verifier collects, takes back a painted
//! [`Surface`] and an [`Outcome`], and owns everything about *where* those
//! pixels, events and answers come from.
//!
//! It lives in `lib/*` because it has two consumers that may not depend on
//! one another: the login greeter service, and the desktop session's screen
//! lock (`userland/gui/session`), which is this surface with the account
//! fixed to the session's own. Two implementations of "prove who you are, at
//! the screen" is the duplication the charter forbids.
//!
//! # What it guarantees
//!
//! * **Only a verified secret concludes it.** [`AuthSurface::answer`]
//!   reports [`Outcome::verified`] only for a [`Verdict::Verified`] answer to
//!   the offer the surface is waiting on, named by its [`Ticket`]. There is no
//!   cancel, no timeout, no error state that falls through: a refusal, an
//!   unreachable authority, an unparseable reply, a stale answer, an empty
//!   account list, and a cooldown running out are all the same answer, and it
//!   is "still asking".
//! * **It never waits for the authority.** An authority spends a deliberate
//!   key-derivation cost on every secret, and the embedder's loop owes the
//!   user a frame, so a secret is *asked* about and its answer adopted on a
//!   later round. Meanwhile the surface says it is checking and its field
//!   takes no input; `Escape` still steps back to the chooser, leaving the
//!   answer to an offer nobody waits on to be ignored. [`Asking`] is the one
//!   [`Verifier`] that does this, over whatever [`Courier`] carries an offer
//!   off the embedder's loop.
//! * **The secret lives in one place and is erased on every path out.** It
//!   is held only in the masked field's bounded, pre-reserved buffer — which
//!   reserves once so typing can never reallocate and strand a copy in a
//!   freed block, shows the console's `[input active...]` marker rather than
//!   anything typed, and redacts itself in `Debug` — and it is wiped the
//!   moment it is handed to the [`Verifier`], which copies what it needs into
//!   storage it erases itself, as well as on every step between accounts.
//! * **The paint and the hit test cannot disagree.** The prompt block, the
//!   field, and the tile rectangles have one definition ([`panel_rect`],
//!   [`AuthSurface::field_rect`], the chooser's own grid), read by both.
//! * **Every length is authored once, in logical pixels.** The whole column
//!   is converted to physical pixels through the one shared
//!   `tairix_geometry::Scale`, so it is the same composition at any DPI.
//! * **It is operable with no pointer at all.** `Tab`, `Shift-Tab`, and the
//!   arrow keys move between tiles, `Return` picks one, and `Escape` steps
//!   back to the chooser — because a machine with no mouse must still log in.
//!
//! What it deliberately does *not* do is rate-limit or count attempts. The
//! authority behind the [`Verifier`] owns that policy and audits every
//! attempt against the account; a second policy here would be a second place
//! to get it wrong. [`AuthSurface::set_cooldown`] shows what the authority
//! reports and refuses to submit while it stands; it reads no clock.
//!
//! Nor does it draw anything behind the column but the theme's own colour.
//! [`Backdrop::Scene`] leaves the surface transparent there for an embedder
//! that composites it over a scene of its own, and every line of text then
//! carries a shadow in the scene's ground; [`AuthSurface::column_rect`] is the
//! stable rectangle such a scene keeps its brightest light out of.
//!
//! # Using it
//!
//! ```
//! use tairix_greeter::{
//!     AccountTile, AuthSurface, Backdrop, EventContext, Ticket, Tickets, Verdict, Verifier,
//! };
//! use tairix_geometry::{Rect, Scale};
//! use tairix_input::{InputEvent, Key, Modifiers, NamedKey};
//! use tairix_theme::Theme;
//!
//! /// An authority that has been asked and not yet answered.
//! #[derive(Default)]
//! struct Pending {
//!     tickets: Tickets,
//!     asked: Option<Ticket>,
//! }
//! impl Verifier for Pending {
//!     fn ask(&mut self, _account: &str, _secret: &str) -> Ticket {
//!         let ticket = self.tickets.issue();
//!         self.asked = Some(ticket);
//!         ticket
//!     }
//! }
//!
//! let theme = Theme::dark();
//! let screen = Rect::new(0, 0, 800, 600);
//! let mut surface = AuthSurface::with_accounts(vec![AccountTile::new("Ann Example", "ann")]);
//! let enter = InputEvent::KeyPressed {
//!     key: Key::Named(NamedKey::Enter),
//!     modifiers: Modifiers::default(),
//! };
//!
//! let mut authority = Pending::default();
//!
//! // The first Return picks the focused tile; the second offers a secret
//! // for the account it named, which the authority is then asked about.
//! surface.on_event(
//!     &enter,
//!     &mut EventContext { screen, scale: Scale::ONE, theme: &theme, verifier: &mut authority, now_ns: 0 },
//! );
//! assert_eq!(surface.selected_account(), Some("ann"));
//! surface.on_event(
//!     &enter,
//!     &mut EventContext { screen, scale: Scale::ONE, theme: &theme, verifier: &mut authority, now_ns: 0 },
//! );
//!
//! // Its answer arrives on a later round.
//! let asked = authority.asked.take().expect("the secret was offered");
//! let outcome = surface.answer(asked, Verdict::Refused, 0, &theme);
//! assert!(!outcome.verified());
//! if outcome.redraw() {
//!     let _frame = surface.render(screen, Scale::ONE, &theme, Backdrop::Desktop);
//! }
//! ```
//!
//! [`Surface`]: tairix_raster::Surface

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

mod asking;
mod authorise;
mod chooser;
mod cooldown;
mod layout;
mod motion;
mod surface;

pub use asking::{Asking, Courier};
pub use authorise::{
    AuthoriseAction, AuthoriseSheet, AUTHORISE_TITLE, AUTHORISE_WIDTH, REQUESTER_SIDE, TRUSTED_PATH,
};
pub use chooser::AccountTile;
pub use cooldown::Cooldown;
pub use surface::{
    panel_rect, AuthSurface, Backdrop, Chrome, EventContext, Outcome, Ticket, Tickets, Verdict,
    Verifier, MAX_CHROME, UNNAMED_ACCOUNT,
};

#[cfg(test)]
mod backdrop_tests;
#[cfg(test)]
mod checking_tests;
#[cfg(test)]
mod chooser_tests;
#[cfg(test)]
mod cooldown_tests;
#[cfg(test)]
mod damage_tests;
#[cfg(test)]
mod motion_tests;
#[cfg(test)]
mod surface_tests;
#[cfg(test)]
mod testkit;
