use alloc::sync::Arc;
use alloc::vec;

use tairix_image::{IndexDepth, SpriteMode, SpriteName, SpritePalette, Unkept};
use tairix_sandbox::imageedit::KeptReason;
use tairix_sandbox::imagerender::ViewFormat;

use super::{free_name, Document, Entry, Kept, Origin, Picture, SpriteInfo, MAX_ENTRIES};
use crate::canvas::{Canvas, Kind, Sample};
use crate::save::{SaveFormat, SaveSettings};

fn plain() -> Picture {
    Picture::plain(Canvas::new(4, 4, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits"))
}

fn named(name: &str) -> Entry {
    let mut picture = plain();
    picture.sprite = Some(SpriteInfo {
        name: SpriteName::new(name).expect("a name"),
        mode: SpriteMode::truecolour((1, 1), false),
        palette: SpritePalette::Implied,
        masked: false,
    });
    Entry::Picture(picture)
}

#[test]
fn a_document_is_a_sprite_area_by_what_it_holds_or_was_read_from() {
    assert!(!Document::new(plain()).is_sprite_area());
    let read = Document::of(
        vec![Entry::Picture(plain())],
        Origin::Read(ViewFormat::Sprite),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("entries");
    assert!(read.is_sprite_area());
    let with_name = Document::of(
        vec![named("icon")],
        Origin::New(SaveFormat::Png),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("entries");
    assert!(with_name.is_sprite_area());
}

#[test]
fn names_are_found_without_regard_to_case() {
    let doc = Document::of(
        vec![named("one"), named("two")],
        Origin::New(SaveFormat::Png),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("ok");
    let two = SpriteName::from_bytes(b"TWO").expect("a name");
    assert_eq!(doc.find(&two), Some(1));
    assert!(doc.names_taken(&two, None));
    assert!(
        !doc.names_taken(&two, Some(1)),
        "a sprite's own name is not taken from it"
    );
}

#[test]
fn a_snapshot_shares_the_pixels_it_froze() {
    let doc = Document::new(plain());
    let snapshot = doc.snapshot().expect("room");
    let (Some(live), Some(Entry::Picture(frozen))) = (doc.picture(), snapshot.entries.first())
    else {
        panic!("a picture each");
    };
    assert!(Arc::ptr_eq(live.canvas().tile(0), frozen.canvas().tile(0)));
}

#[test]
fn a_document_holds_between_one_and_the_bound_of_entries() {
    assert!(Document::of(
        vec![],
        Origin::New(SaveFormat::Png),
        Unkept::default(),
        SaveSettings::default()
    )
    .is_none());
    let kept = Kept {
        name: SpriteName::new("odd").expect("a name"),
        reason: KeptReason::UnsupportedType,
        bytes: Arc::new(vec![0; 44]),
    };
    let many = vec![kept; MAX_ENTRIES + 1]
        .into_iter()
        .map(Entry::Kept)
        .collect();
    assert!(Document::of(
        many,
        Origin::New(SaveFormat::Png),
        Unkept::default(),
        SaveSettings::default()
    )
    .is_none());
}

#[test]
fn a_sprite_s_pixels_take_its_mode_s_shape() {
    let mut picture = plain();
    assert_eq!(picture.pixel_aspect(), (1, 1));
    picture.sprite = Some(SpriteInfo {
        name: SpriteName::new("tall").expect("a name"),
        mode: SpriteMode::indexed(IndexDepth::Four, (1, 2), false),
        palette: SpritePalette::Implied,
        masked: false,
    });
    assert_eq!(picture.pixel_aspect(), (1, 2));
}

/// A worker's tiles are put in place only where each fits its slot: tiles
/// of a canvas of another shape change nothing.
#[test]
fn tiles_of_another_shape_are_not_adopted() {
    let white = |side| Canvas::new(side, side, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    let mut document = Document::new(Picture::plain(white(40)));
    let wider = white(70);
    let foreign = vec![(0, Arc::clone(wider.tile(0)))];
    assert_eq!(document.adopt_tiles(0, foreign), Ok(false));
    assert_eq!(document.generation(), 0, "nothing changed");
    let same = white(40);
    let fitting = vec![(0, Arc::clone(same.tile(0)))];
    assert_eq!(document.adopt_tiles(0, fitting), Ok(true));
    assert_eq!(document.generation(), 1);
}

/// A picture asked to replace a sprite kept as its bytes changes nothing and
/// says so.
#[test]
fn a_kept_entry_is_not_replaced() {
    let kept = Entry::Kept(Kept {
        name: SpriteName::new("odd").expect("a name"),
        reason: KeptReason::UnsupportedType,
        bytes: Arc::new(vec![0; 44]),
    });
    let mut document = Document::of(
        vec![kept],
        Origin::New(SaveFormat::Png),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("entries");
    assert_eq!(document.replace_picture(plain()), Ok(false));
    assert_eq!(document.generation(), 0);
    assert!(document.picture().is_none());
}

/// A full document states its bound from the one constant that sets it.
#[test]
fn a_full_document_states_how_many_it_holds() {
    let said = alloc::format!("{}", super::ListRefusal::Full);
    assert!(said.contains(&alloc::format!("{MAX_ENTRIES}")), "{said}");
}

/// A quarter turn of a sixteen-bit sprite swaps its pixels' shape in a mode
/// of its own layout, rather than keeping the shape the turn undid.
#[test]
fn a_quarter_turn_swaps_a_direct_sprites_pixel_shape() {
    use crate::transform::{Transform, Turn};
    let mode = SpriteMode::from_value((5 << 27) | (45 << 14) | (90 << 1) | 1).expect("a word");
    let sprite = SpriteInfo {
        name: SpriteName::new("wide").expect("a name"),
        mode,
        palette: SpritePalette::Implied,
        masked: false,
    };
    let canvas = Canvas::new(4, 2, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let turned = sprite.refit(Transform::Turn(Turn::Quarter), &canvas);
    assert_eq!(turned.mode.eig(), (mode.eig().1, mode.eig().0));
    assert_eq!(turned.mode.layout(), mode.layout(), "still sixteen-bit");
}

fn names(names: &[&str]) -> vec::Vec<SpriteName> {
    names
        .iter()
        .map(|name| SpriteName::new(name).expect("a name"))
        .collect()
}

fn free(base: &str, taken: &[&str]) -> Option<alloc::string::String> {
    let base = SpriteName::from_bytes(base.as_bytes()).expect("a name");
    free_name(&base, &names(taken)).map(|name| alloc::format!("{name}"))
}

#[test]
fn a_free_name_is_the_base_itself_or_the_least_number_free_after_it() {
    assert_eq!(free("tree", &["bush"]).as_deref(), Some("tree"));
    assert_eq!(
        free("sprite", &["Sprite", "sprite1", "SPRITE2", "sprite4"]).as_deref(),
        Some("sprite3"),
        "matched without regard to case"
    );
    assert_eq!(free("", &[]).as_deref(), Some("sprite"));
    assert_eq!(free("", &["sprite"]).as_deref(), Some("sprite1"));
}

/// A number is read from where the stem cut to make room for it ends, so a
/// base's own digits, a leading zero, and a stem cut short are each read as
/// what they are.
#[test]
fn a_free_name_reads_numbers_from_where_their_stem_ends() {
    assert_eq!(free("a1", &["a1", "a11"]).as_deref(), Some("a12"));
    assert_eq!(free("a", &["a", "a11"]).as_deref(), Some("a1"));
    assert_eq!(free("x", &["x", "x01", "x02"]).as_deref(), Some("x1"));
    assert_eq!(
        free("abcdefghijkl", &["abcdefghijkl"]).as_deref(),
        Some("abcdefghijk1")
    );
    let mut taken = vec!["abcdefghijkl"];
    let numbered: vec::Vec<alloc::string::String> = (1..=9)
        .map(|number| alloc::format!("abcdefghijk{number}"))
        .collect();
    taken.extend(numbered.iter().map(alloc::string::String::as_str));
    assert_eq!(
        free("abcdefghijkl", &taken).as_deref(),
        Some("abcdefghij10")
    );
}

/// More numbers are tried than a document holds names, so a free one is
/// found for any document; only more names than that leave none.
#[test]
fn a_free_name_runs_out_only_past_what_a_document_holds() {
    let held: vec::Vec<SpriteName> = core::iter::once(alloc::string::String::from("s"))
        .chain((1..MAX_ENTRIES).map(|number| alloc::format!("s{number}")))
        .map(|name| SpriteName::new(&name).expect("a name"))
        .collect();
    let base = SpriteName::new("s").expect("a name");
    let found = free_name(&base, &held).expect("one is free");
    assert_eq!(alloc::format!("{found}"), alloc::format!("s{MAX_ENTRIES}"));
    let mut over = held;
    over.extend(
        (MAX_ENTRIES..=MAX_ENTRIES + 1)
            .map(|number| SpriteName::new(&alloc::format!("s{number}")).expect("a name")),
    );
    assert_eq!(free_name(&base, &over), None);
}

#[test]
fn a_tiffs_pages_are_not_a_sprite_area_however_many() {
    let pages = |origin| {
        Document::of(
            vec![Entry::Picture(plain()), Entry::Picture(plain())],
            origin,
            Unkept::default(),
            SaveSettings::default(),
        )
        .expect("entries")
    };
    let tiff = pages(Origin::Read(ViewFormat::Tiff));
    assert!(tiff.is_pages() && !tiff.is_sprite_area());
    let made = pages(Origin::New(SaveFormat::Tiff));
    assert!(made.is_pages());
    let png = pages(Origin::Read(ViewFormat::Png));
    assert!(
        png.is_sprite_area() && !png.is_pages(),
        "several pictures of a PNG become sprites"
    );
    let named_pages = Document::of(
        vec![Entry::Picture(plain()), named("icon")],
        Origin::Read(ViewFormat::Tiff),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("entries");
    assert!(
        named_pages.is_sprite_area() && !named_pages.is_pages(),
        "a name makes sprites"
    );
    assert!(Document::new_as(plain(), SaveFormat::Sprites).is_sprite_area());
}

fn colour_layer(colour: [u8; 4], name: &str) -> super::Layer {
    let canvas = Canvas::new(4, 4, Kind::Rgba, Sample::Rgba(colour)).expect("fits");
    super::Layer::new(canvas, alloc::string::String::from(name))
}

#[test]
fn a_picture_of_layers_holds_one_or_more_alike() {
    use super::{Layer, MOST_LAYERS};
    assert!(Picture::layered(vec![], 0).is_none(), "none");
    let two = || vec![colour_layer([1; 4], "a"), colour_layer([2; 4], "b")];
    assert!(
        Picture::layered(two(), 2).is_none(),
        "painted on past the last"
    );
    let picture = Picture::layered(two(), 1).expect("two alike");
    assert_eq!((picture.layers().len(), picture.active()), (2, 1));
    assert_eq!(
        picture.canvas().colour_at(0, 0),
        Some([2; 4]),
        "the one painted on"
    );
    let mut wider = two();
    wider[1].canvas = Canvas::new(5, 4, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    assert!(Picture::layered(wider, 0).is_none(), "of another size");
    let palette = || Kind::Indexed {
        depth: IndexDepth::One,
        palette: vec![[0, 0, 0, 255], [255; 4]],
        masked: false,
    };
    let ink = |name: &str| {
        let canvas = Canvas::new(4, 4, palette(), Sample::Index(0, 255)).expect("fits");
        Layer::new(canvas, alloc::string::String::from(name))
    };
    assert!(
        Picture::layered(vec![ink("a")], 0).is_some(),
        "a palette picture's one layer"
    );
    assert!(
        Picture::layered(vec![ink("a"), ink("b")], 0).is_none(),
        "a palette holds one"
    );
    let crowd = (0..=MOST_LAYERS)
        .map(|_| colour_layer([0; 4], "c"))
        .collect();
    assert!(Picture::layered(crowd, 0).is_none(), "past the most");
    let long = "n".repeat(tairix_sandbox::imageedit::MAX_LAYER_NAME + 1);
    let named = vec![colour_layer([0; 4], &long)];
    assert!(Picture::layered(named, 0).is_none(), "named too long");
}

#[test]
fn layer_changes_are_refused_where_a_picture_cannot_take_them() {
    use super::{LayerRefusal, Shown, MOST_LAYERS};
    let mut document = Document::new(plain());
    assert_eq!(document.remove_layer(0), Err(LayerRefusal::LastLayer));
    assert_eq!(document.move_layer(0, 1), Err(LayerRefusal::NoSuchLayer));
    let long = "x".repeat(tairix_sandbox::imageedit::MAX_LAYER_NAME + 1);
    let shown = Shown {
        name: long,
        opacity: 255,
        visible: true,
    };
    assert_eq!(document.show_layer(0, shown), Err(LayerRefusal::Unlike));
    let other = Canvas::new(5, 4, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    let unlike = super::Layer::new(other, alloc::string::String::from("wide"));
    assert_eq!(document.insert_layer(1, unlike), Err(LayerRefusal::Unlike));
    for _ in 1..MOST_LAYERS {
        document
            .insert_layer(1, colour_layer([0; 4], "more"))
            .expect("room");
    }
    assert_eq!(
        document.insert_layer(1, colour_layer([0; 4], "one more")),
        Err(LayerRefusal::Full)
    );
    let palette = Kind::Indexed {
        depth: IndexDepth::One,
        palette: vec![[0, 0, 0, 255], [255; 4]],
        masked: false,
    };
    let mut ink = Document::new(Picture::plain(
        Canvas::new(4, 4, palette, Sample::Index(0, 255)).expect("fits"),
    ));
    assert_eq!(
        ink.insert_layer(1, colour_layer([0; 4], "over")),
        Err(LayerRefusal::Palette)
    );
    let faded = Shown {
        name: alloc::string::String::from("Background"),
        opacity: 100,
        visible: true,
    };
    assert_eq!(
        ink.show_layer(0, faded),
        Err(LayerRefusal::Palette),
        "shown wholly"
    );
    assert_eq!(ink.generation(), 0, "nothing changed");
}

#[test]
fn a_new_layer_is_named_for_a_number_no_layer_has() {
    use super::{copy_name, new_layer_name};
    let layers = [
        colour_layer([0; 4], "Background"),
        colour_layer([0; 4], "Layer 3"),
    ];
    assert_eq!(new_layer_name(&layers[..1]).expect("room"), "Layer 2");
    assert_eq!(
        new_layer_name(&layers).expect("room"),
        "Layer 4",
        "3 is taken"
    );
    assert_eq!(copy_name("Sky").expect("room"), "Sky copy");
    let max = tairix_sandbox::imageedit::MAX_LAYER_NAME;
    let long = "é".repeat(max);
    let copied = copy_name(&long).expect("room");
    assert!(
        copied.len() <= max && copied.ends_with(" copy"),
        "cut to fit"
    );
}

#[test]
fn a_flattened_picture_of_one_plain_layer_shares_its_pixels() {
    let picture = plain();
    let flat = picture.flattened().expect("room");
    assert!(Arc::ptr_eq(flat.tile(0), picture.canvas().tile(0)));
    assert!(picture.single());
    let mut faded = plain();
    faded.layers_mut()[0].opacity = 128;
    assert!(!faded.single(), "a faded layer is laid over nothing");
}
