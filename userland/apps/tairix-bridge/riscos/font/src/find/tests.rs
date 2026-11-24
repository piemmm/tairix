use super::*;
use crate::handles::Source;

#[test]
fn a_full_identifier_gives_the_name_encoding_and_matrix_risc_os_5_30_gives() {
    let mut master = Master::new(
        b"Omega".to_vec(),
        Some(b"Big O".to_vec()),
        Some(b"latin1".to_vec()),
        false,
        b"HostFS:$.Lib2.Omega.".to_vec(),
        b"Resources:$.Fonts.Encodings.Latin1".to_vec(),
        Source::Face,
    );
    assert_eq!(
        full_identifier(&master, None, 1),
        b"\\FOmega\\f1 Big O\\Elatin1"
    );
    master.name = None;
    let matrix = Matrix([65536, 0, 0, 65536, 100, -200]);
    assert_eq!(
        full_identifier(&master, Some(matrix), 1),
        b"\\FOmega\\Elatin1\\M 65536 0 0 65536 100 -200"
    );
    master.encoding = None;
    assert_eq!(full_identifier(&master, None, 1), b"\\FOmega");
}
