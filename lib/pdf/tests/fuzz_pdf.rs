//! Deterministic fuzz harness for the PDF reader.
//!
//! Every byte of a PDF is untrusted, so the harness's one invariant is that
//! opening a document, reading every page and interpreting each page's content
//! never panics: a typed [`PdfError`] or a display list, whatever the bytes.
//!
//! No external fuzz runner: a per-run-seeded `Prng` mutates real documents —
//! a cross-reference table and one of streams, a page of every kind of
//! content, an encrypted file — splices hostile tokens into their content, and
//! generates content from the operator alphabet itself, so the interpreter's
//! arms are reached by construction rather than by a lucky flip. A plain
//! `cargo test` runs the [`SMOKE_ITERATIONS`] sweep once from a fresh, logged
//! seed; `cargo xtask fuzz` extends it to a wall-clock budget.

use std::fmt::Write as _;

use tairix_fuzzseed::Prng;
use tairix_pdf::{Document, Options, PdfError};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 20_000;

/// What every document may decode to.
const BUDGET: u64 = 1 << 22;

/// A PDF assembled from object bodies, each placed in a cross-reference
/// table after its header; the last object is the catalog.
fn tabled(objects: &[&[u8]], trailer: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let table = out.len();
    let mut text = format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        let _ = writeln!(text, "{offset:010} 00000 n ");
    }
    let _ = write!(
        text,
        "trailer\n<</Size {} /Root 1 0 R {trailer}>>\nstartxref\n{table}\n%%EOF\n",
        objects.len() + 1
    );
    out.extend_from_slice(text.as_bytes());
    out
}

/// A stream object of `entries` over `data`.
fn stream(entries: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<<{entries} /Length {}>>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// A page painting `content` with every kind of resource.
fn rich(content: &[u8]) -> Vec<u8> {
    let content = stream("", content);
    let form = stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 50 50] /Group <</S /Transparency>>",
        b"0 1 0 rg 0 0 20 20 re f",
    );
    let image = stream("/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceRGB /BitsPerComponent 8", &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]);
    let mask = stream(
        "/Type /XObject /Subtype /Image /Width 2 /Height 2 /ImageMask true",
        &[0b0100_0000, 0b1000_0000],
    );
    let calculator = stream(
        "/FunctionType 4 /Domain [0 1] /Range [0 1 0 1 0 1]",
        b"{ dup dup }",
    );
    tabled(
        &[
            b"<</Type /Catalog /Pages 2 0 R /OCProperties <</OCGs [9 0 R] /D <</OFF [9 0 R]>>>>>>",
            b"<</Type /Pages /Kids [3 0 R]>>",
            b"<</Type /Page /MediaBox [0 0 200 200] /Rotate 90 /Contents 4 0 R /Resources 8 0 R>>",
            &content,
            &form,
            &image,
            &mask,
            b"<</XObject <</F0 5 0 R /I0 6 0 R /M0 7 0 R>> /Properties <</Off 9 0 R>> /ExtGState <</G0 <</ca 0.5 /CA 0.5 /LW 2 /D [[3 1] 0] /BM /Multiply>>>> /ColorSpace <</S0 [/Separation /Spot /DeviceCMYK 10 0 R] /L0 [/Lab <</WhitePoint [0.95 1 1.09]>>] /I0 [/Indexed /DeviceRGB 1 <ff000000ff00>]>> /Shading <</Sh0 <</ShadingType 3 /ColorSpace /DeviceRGB /Coords [10 10 0 10 10 30] /Function <</FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 2>> /Extend [true true]>>>> /Pattern <</P0 <</PatternType 2 /Shading <</ShadingType 2 /ColorSpace /DeviceGray /Coords [0 0 1 1] /Function 11 0 R>>>>>>>>",
            b"<</Type /OCG /Name (hidden)>>",
            &calculator,
            b"<</FunctionType 3 /Domain [0 1] /Functions [<</FunctionType 2 /Domain [0 1] /N 1>> <</FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1>>] /Bounds [0.5] /Encode [0 1 0 1]>>",
        ],
        "",
    )
}

/// The content every rich page starts from.
const CONTENT: &[u8] = b"q 1 0 0 1 10 10 cm 0.2 0.4 0.6 rg 0 0 50 50 re f Q \
    q /G0 gs 0 0 m 30 0 l 30 30 c 15 40 0 30 v h B* Q \
    q 10 10 80 80 re W n /Sh0 sh Q /Pattern cs /P0 scn 0 0 10 10 re f \
    /S0 cs 0.5 sc /L0 cs 50 20 -30 sc /I0 cs 1 sc 0 0 5 5 re f \
    q 40 0 0 40 100 100 cm /I0 Do /M0 Do Q /F0 Do /Off BMC EMC /OC /Off BDC 0 0 1 1 re f EMC \
    BI /W 2 /H 1 /CS /G /BPC 8 ID \x10\x20 EI";

/// A document whose cross-reference is a stream and whose objects are in an
/// object stream.
fn streamed() -> Vec<u8> {
    let objects = b"2 0 3 31 <</Type /Pages /Kids [3 0 R]>> <</Type /Page /MediaBox [0 0 50 50]>>";
    let mut out = b"%PDF-1.5\n".to_vec();
    let catalog = out.len();
    out.extend_from_slice(b"1 0 obj\n<</Type /Catalog /Pages 2 0 R>>\nendobj\n");
    let object_stream = out.len();
    out.extend_from_slice(b"4 0 obj\n");
    out.extend_from_slice(&stream("/Type /ObjStm /N 2 /First 8", objects));
    out.extend_from_slice(b"\nendobj\n");
    let xref = out.len();
    let mut rows = Vec::new();
    for (kind, field, index) in [
        (0u8, 0u32, 0u16),
        (1, u32::try_from(catalog).unwrap_or(0), 0),
        (2, 4, 0),
        (2, 4, 1),
        (1, u32::try_from(object_stream).unwrap_or(0), 0),
        (1, u32::try_from(xref).unwrap_or(0), 0),
    ] {
        rows.push(kind);
        rows.extend_from_slice(&field.to_be_bytes());
        rows.extend_from_slice(&index.to_be_bytes());
    }
    out.extend_from_slice(b"5 0 obj\n");
    out.extend_from_slice(&stream("/Type /XRef /Size 6 /W [1 4 2] /Root 1 0 R", &rows));
    out.extend_from_slice(format!("\nendobj\nstartxref\n{xref}\n%%EOF\n").as_bytes());
    out
}

/// A file encrypted with RC4 under revision 3, its user password `user`.
fn encrypted() -> Vec<u8> {
    let content = [
        0x30, 0x60, 0x2e, 0x43, 0xbe, 0x94, 0x6b, 0xb3, 0x59, 0x2a, 0x42, 0x04, 0x87, 0xc3, 0x7b,
        0x03, 0x15, 0x88, 0x48, 0xa0, 0x05, 0xfc, 0x99,
    ];
    tabled(
        &[
            b"<</Type /Catalog /Pages 2 0 R>>",
            b"<</Type /Pages /Kids [3 0 R]>>",
            b"<</Type /Page /MediaBox [0 0 100 100] /Contents 4 0 R>>",
            &stream("", &content),
            b"<</Filter /Standard /V 2 /R 3 /Length 128 /P -3904 /O <0ba3835f88f90388e74e54584125ce142be0de24c6b0d37746e075b891756671> /U <ad944bba9714b0f0d9da179955e1945000000000000000000000000000000000>>>",
        ],
        "/Encrypt 5 0 R /ID [<0123456789abcdef0123456789abcdef> <0123456789abcdef0123456789abcdef>]",
    )
}

/// `text`'s hexadecimal digits as bytes.
fn hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let digit = |byte: u8| char::from(byte).to_digit(16).unwrap_or(0);
            u8::try_from(digit(pair[0]) * 16 + digit(pair[1])).unwrap_or(0)
        })
        .collect()
}

/// A file encrypted with AES through crypt filters — AES-128 under revision
/// 4, or AES-256 under revision 6 — its user password `user`.
fn aes(revision_six: bool) -> Vec<u8> {
    let (encrypt, content) = if revision_six {
        (
            "<</Filter /Standard /V 5 /R 6 /Length 256 /P -1028 /O <8a315bf69dd058f051451db82b059609a733a4f9f4ebc58b17c6de8eccea32844f5653414c5430314f4b53414c543031> /U <2767bfdcbf33c97e7801a89cb7525250849f8643c4783ca21967491d65b0a9de555653414c543031554b53414c543031> /OE <b8d1b8f36e636b4b697c76d2c9c72cd1587e9be03c38638f15223d0d70fe5d48> /UE <338a5284dd153ef6fb90831a254ac183ae89a699708fa0c45ba82035bbbf5f4b> /CF <</StdCF <</CFM /AESV3 /Length 32>>>> /StmF /StdCF /StrF /StdCF>>",
            "202122232425262728292a2b2c2d2e2f00749bf08c14fc6d0d95b9d03db266e39afc7edfe9d7b1c7a81a92b5509e1b98",
        )
    } else {
        (
            "<</Filter /Standard /V 4 /R 4 /Length 128 /P -1028 /O <0ba3835f88f90388e74e54584125ce142be0de24c6b0d37746e075b891756671> /U <c19e29f3359b13734f81225b927a683800000000000000000000000000000000> /CF <</StdCF <</CFM /AESV2 /Length 16>>>> /StmF /StdCF /StrF /StdCF>>",
            "202122232425262728292a2b2c2d2e2f3bd9bbd23c5eee3d8d5f0a590b08be49eec07bfc0230a213d9bcf0fabab4b6bf",
        )
    };
    tabled(
        &[
            b"<</Type /Catalog /Pages 2 0 R>>",
            b"<</Type /Pages /Kids [3 0 R]>>",
            b"<</Type /Page /MediaBox [0 0 100 100] /Contents 4 0 R>>",
            &stream("", &hex(content)),
            encrypt.as_bytes(),
        ],
        "/Encrypt 5 0 R /ID [<0123456789abcdef0123456789abcdef> <0123456789abcdef0123456789abcdef>]",
    )
}

/// Open `file` and interpret every page, answering how many items they made.
fn read_whole(file: &[u8], password: Option<&[u8]>) -> Result<usize, PdfError> {
    let document = Document::open(
        file,
        &Options {
            password,
            budget: BUDGET,
        },
    )?;
    let mut items = 0;
    for index in 0..document.page_count() {
        let page = document.page(index)?;
        items += page.display_list()?.items.len();
    }
    Ok(items)
}

/// The operators content is generated from, with how many operands each takes.
const OPERATORS: &[(&str, usize)] = &[
    ("q", 0),
    ("Q", 0),
    ("cm", 6),
    ("w", 1),
    ("J", 1),
    ("j", 1),
    ("M", 1),
    ("d", 2),
    ("gs", 1),
    ("m", 2),
    ("l", 2),
    ("c", 6),
    ("v", 4),
    ("y", 4),
    ("h", 0),
    ("re", 4),
    ("S", 0),
    ("s", 0),
    ("f", 0),
    ("f*", 0),
    ("B", 0),
    ("b*", 0),
    ("n", 0),
    ("W", 0),
    ("W*", 0),
    ("g", 1),
    ("rg", 3),
    ("k", 4),
    ("G", 1),
    ("RG", 3),
    ("K", 4),
    ("cs", 1),
    ("scn", 1),
    ("sh", 1),
    ("Do", 1),
    ("BMC", 1),
    ("BDC", 2),
    ("EMC", 0),
    ("Tj", 1),
    ("BX", 0),
    ("EX", 0),
];

/// The operands content is generated from.
const OPERANDS: &[&str] = &[
    "0",
    "1",
    "-1",
    "0.5",
    "1e3",
    "99999999999",
    "-.5",
    "/G0",
    "/Sh0",
    "/F0",
    "/I0",
    "/M0",
    "/P0",
    "/S0",
    "/L0",
    "/Pattern",
    "/DeviceRGB",
    "/Off",
    "/OC",
    "(text)",
    "<00ff>",
    "[1 2]",
    "[]",
    "<</A 1>>",
    "true",
    "null",
    "/Nope",
];

#[test]
fn reading_never_panics_for_any_input() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "reading_never_panics_for_any_input",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let templates = [
        rich(CONTENT),
        streamed(),
        encrypted(),
        aes(false),
        aes(true),
    ];
    // The unmutated documents read, so the sweep below reaches the reader's
    // depths by construction.
    assert!(
        read_whole(&templates[0], None).is_ok_and(|items| items > 5),
        "the rich page reads"
    );
    assert_eq!(
        read_whole(&templates[1], None),
        Ok(0),
        "the streamed document reads"
    );
    for (template, method) in templates[2..].iter().zip(["RC4", "AES-128", "AES-256"]) {
        assert_eq!(
            read_whole(template, Some(b"user")),
            Ok(1),
            "the {method} document opens"
        );
    }
    let mut iteration: u64 = 0;
    loop {
        // 1. A real document with a handful of bytes flipped or cut short.
        let template = rng.pick(&templates);
        let mut mutated = template.clone();
        for _ in 0..rng.at_most(8) {
            let at = rng.below(mutated.len());
            mutated[at] ^= rng.next_u8();
        }
        if rng.at_most(4) == 0 {
            mutated.truncate(rng.below(mutated.len() + 1));
        }
        // Text SASLprep changes, refuses, or leaves as it is, beside the
        // templates' own password.
        let password = rng
            .pick(&[
                None,
                Some("user"),
                Some("\u{2168}\u{ad}us\u{e9}r"),
                Some("\u{627}1"),
                Some("\u{1f100}\u{fdfa}"),
            ])
            .map(str::as_bytes);
        let _ = read_whole(&mutated, password);

        // 2. Content generated from the operator alphabet, its operands drawn
        //    at random and hostile bytes spliced in.
        let mut content = Vec::new();
        for _ in 0..rng.at_most(40) {
            let &(operator, arity) = rng.pick(OPERATORS);
            for _ in 0..arity + rng.at_most(1) {
                content.extend_from_slice(rng.pick(OPERANDS).as_bytes());
                content.push(b' ');
            }
            content.extend_from_slice(operator.as_bytes());
            content.push(b'\n');
            if rng.at_most(16) == 0 {
                let mut noise = vec![0u8; rng.at_most(16)];
                rng.fill(&mut noise);
                content.extend_from_slice(&noise);
            }
        }
        let _ = read_whole(&rich(&content), None);

        // 3. Noise alone, behind a header and before a trailer.
        let mut noise = b"%PDF-1.4\n".to_vec();
        let mut body = vec![0u8; rng.at_most(512)];
        rng.fill(&mut body);
        noise.extend_from_slice(&body);
        noise.extend_from_slice(b"\ntrailer <</Root 1 0 R>>\n");
        let _ = read_whole(&noise, None);

        iteration += 1;
        if !tairix_fuzzseed::within_budget(deadline) && iteration >= SMOKE_ITERATIONS {
            break;
        }
    }
}
