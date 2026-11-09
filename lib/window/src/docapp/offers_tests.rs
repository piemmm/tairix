use alloc::string::String;

use tairix_abi::Errno;
use tairix_geometry::Point;

use super::{FileOffer, FileOffers, OfferStep};
use crate::client::OfferedMedia;

const fn offer(window_id: u64, drag: u32, serial: u32) -> FileOffer {
    FileOffer {
        window_id,
        drag,
        serial,
        at: Point { x: 3, y: 4 },
    }
}

fn read(asked: FileOffer, media: Result<&str, Errno>) -> OfferedMedia {
    OfferedMedia {
        window_id: asked.window_id,
        serial: asked.serial,
        media: media.map(String::from),
    }
}

#[test]
fn a_drag_is_read_once_and_every_offer_it_makes_waits_only_on_that_read() {
    let mut offers = FileOffers::new(true);
    let first = offer(7, 1, 10);
    assert_eq!(offers.offered(first), OfferStep::Ask(first));
    let moved = offer(7, 1, 11);
    assert_eq!(
        offers.offered(moved),
        OfferStep::Rest,
        "the drag is already being read"
    );
    assert_eq!(offers.media(1), None);
    assert_eq!(
        offers.landed(read(first, Ok("image/png"))),
        (OfferStep::Answer(moved), None),
        "the newest offer is the one answered"
    );
    assert_eq!(offers.media(1), Some("image/png"));
    let elsewhere = offer(8, 1, 12);
    assert_eq!(
        offers.offered(elsewhere),
        OfferStep::Answer(elsewhere),
        "the drag's type serves its offers over every window"
    );
}

#[test]
fn an_offer_that_ended_before_its_read_is_read_again_under_a_later_one() {
    let mut offers = FileOffers::new(true);
    let first = offer(7, 1, 10);
    assert_eq!(offers.offered(first), OfferStep::Ask(first));
    let over_another = offer(8, 1, 11);
    assert_eq!(offers.offered(over_another), OfferStep::Rest);
    assert_eq!(
        offers.landed(read(first, Err(Errno::NotFound))),
        (OfferStep::Ask(over_another), None)
    );
    assert_eq!(
        offers.media(1),
        None,
        "an ended offer says nothing of the file"
    );
    assert_eq!(
        offers.landed(read(over_another, Err(Errno::NotFound))),
        (OfferStep::Rest, None),
        "with no later offer, nothing is asked again"
    );
    assert_eq!(
        offers.landed(read(over_another, Ok("image/png"))),
        (OfferStep::Rest, None),
        "an answer nothing asked is not taken"
    );
    assert_eq!(offers.media(1), None);
}

#[test]
fn a_new_drag_is_read_afresh_and_a_superseded_read_is_ignored() {
    let mut offers = FileOffers::new(true);
    let old = offer(7, 1, 10);
    assert_eq!(offers.offered(old), OfferStep::Ask(old));
    let new = offer(7, 2, 11);
    assert_eq!(offers.offered(new), OfferStep::Ask(new));
    assert_eq!(
        offers.landed(read(old, Ok("text/plain"))),
        (OfferStep::Rest, None)
    );
    assert_eq!(offers.media(1), None);
    assert_eq!(
        offers.landed(read(new, Ok("image/svg+xml"))),
        (OfferStep::Answer(new), None)
    );
    assert_eq!(offers.media(2), Some("image/svg+xml"));
    assert_eq!(offers.media(1), None);
}

#[test]
fn a_failed_read_is_said_and_answers_the_drag_without_placing() {
    let mut offers = FileOffers::new(true);
    let first = offer(7, 1, 10);
    assert_eq!(offers.offered(first), OfferStep::Ask(first));
    assert_eq!(
        offers.landed(read(first, Err(Errno::BadMagic))),
        (OfferStep::Answer(first), Some(Errno::BadMagic))
    );
    assert_eq!(offers.media(1), None);
    let later = offer(7, 1, 11);
    assert_eq!(
        offers.offered(later),
        OfferStep::Answer(later),
        "a failed read is not asked again for every offer"
    );
}

#[test]
fn an_ended_offer_is_not_answered_once_its_read_lands() {
    let mut offers = FileOffers::new(true);
    let first = offer(7, 1, 10);
    assert_eq!(offers.offered(first), OfferStep::Ask(first));
    offers.ended(7);
    assert_eq!(
        offers.landed(read(first, Ok("image/png"))),
        (OfferStep::Rest, None)
    );
    assert_eq!(offers.media(1), Some("image/png"));
}

#[test]
fn a_closed_window_hands_the_read_to_an_offer_waiting_elsewhere() {
    let mut offers = FileOffers::new(true);
    let first = offer(7, 1, 10);
    assert_eq!(offers.offered(first), OfferStep::Ask(first));
    let over_another = offer(8, 1, 11);
    assert_eq!(offers.offered(over_another), OfferStep::Rest);
    assert_eq!(
        offers.closed(7),
        OfferStep::Ask(over_another),
        "the closed window's read is never delivered"
    );
    assert_eq!(offers.closed(9), OfferStep::Rest);
    assert_eq!(
        offers.landed(read(over_another, Ok("image/png"))),
        (OfferStep::Answer(over_another), None)
    );
}

#[test]
fn an_unsent_ask_is_asked_again_by_the_next_offer() {
    let mut offers = FileOffers::new(true);
    let first = offer(7, 1, 10);
    assert_eq!(offers.offered(first), OfferStep::Ask(first));
    offers.unasked(first);
    let next = offer(7, 1, 11);
    assert_eq!(offers.offered(next), OfferStep::Ask(next));
}

#[test]
fn a_view_that_places_nothing_reads_and_answers_nothing() {
    let mut offers = FileOffers::new(false);
    assert_eq!(offers.offered(offer(7, 1, 10)), OfferStep::Rest);
    assert_eq!(offers.offered(offer(7, 1, 11)), OfferStep::Rest);
    assert_eq!(offers.closed(7), OfferStep::Rest);
}
