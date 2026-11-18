use super::*;

#[test]
fn each_model_s_boot_screen_unpacks_to_its_canvas_and_draws_at_a_card_s_size() {
    for model in Model::ALL {
        assert_eq!(
            tairix_compress::decompressed_len(packed(model)),
            Ok(CANVAS_WIDTH * CANVAS_HEIGHT),
            "{model:?}"
        );
        let picture = picture(model, (200, 150)).expect("a picture");
        assert_eq!(picture.len(), 200 * 150 * DOT);
        assert!(
            picture
                .as_chunks::<DOT>()
                .0
                .iter()
                .any(|dot| dot[..3] != [0, 0, 0]),
            "{model:?}'s screen shows something"
        );
    }
}

#[test]
fn a_size_too_small_for_the_monitor_draws_nothing() {
    assert_eq!(picture(Model::ModelB, (0, 0)), None);
}
