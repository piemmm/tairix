use super::*;

const ODD: Matrix = Matrix([43690, 1234, -567, 70000, 0, 0]);

fn bbox(x0: i32, y0: i32, x1: i32, y1: i32) -> metrics::Box {
    metrics::Box { x0, y0, x1, y1 }
}

#[test]
fn each_product_is_floored_on_its_own() {
    let odd = Transform::of(ODD);
    assert_eq!(odd.vector((8400, 0)), (5599, 158));
    assert_eq!(odd.vector((7800, 0)), (5199, 146));
    assert_eq!(odd.vector((-1200, 0)), (-800, -23));
    let mirrored = Transform::of(Matrix([-43690, 0, 0, 65536, 0, 0]));
    assert_eq!(mirrored.vector((8400, 0)), (-5600, 0));
    assert_eq!(
        odd.bbox(bbox(600, 0, 7812, 8412)),
        bbox(326, 11, 5207, 9131)
    );
    assert_eq!(
        odd.bbox(bbox(360, -2400, 3612, 6012)),
        bbox(186, -2558, 2427, 6489)
    );
}

#[test]
fn a_font_s_matrix_then_the_painting_one_combine_first() {
    let shear = Transform::of(Matrix([65536, 0, 16384, 65536, 100, -50]));
    assert_eq!(
        shear.bbox(bbox(-1200, 1200, 13212, 10812)),
        bbox(-800, 1150, 16015, 10762)
    );
    let both = shear.then(Transform::of(ODD));
    assert_eq!(
        both.bbox(bbox(-1200, 1200, 13212, 10812)),
        bbox(-544, 1212, 10582, 11795)
    );
    assert_eq!(
        Transform::IDENTITY.then(Transform::IDENTITY),
        Transform::IDENTITY
    );
    assert_eq!(
        Transform::of(ODD).then(Transform::IDENTITY),
        Transform::of(ODD)
    );
}

#[test]
fn sizes_scale_thousandths_of_an_em_down() {
    assert_eq!(scaled(700, 192), 8400);
    assert_eq!(scaled(333, 100), 2081);
    assert_eq!(scaled(-1, 100), -7);
    assert_eq!(scaled(37, 100), 231);
    assert_eq!(scaled(278, 1), 17);
    assert_eq!(
        scaled(i32::MAX, i32::MAX),
        scaled(i32::MAX, i32::MAX),
        "wraps, never traps"
    );
}
