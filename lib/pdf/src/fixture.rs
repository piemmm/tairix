//! PDFs assembled for tests: objects written in order, then a
//! cross-reference table or stream that places them.

use alloc::boxed::Box;
use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use tairix_compress::deflate::Flush;
use tairix_compress::zlib::Encoder;

/// `data` compressed as Flate's zlib stream.
pub(crate) fn flate(data: &[u8]) -> Vec<u8> {
    let mut encoder = Box::new(Encoder::new());
    let mut out = vec![0; encoder.bound(data.len())];
    let written = encoder
        .compress(data, &mut out, Flush::Finish)
        .expect("a bounded output");
    out.truncate(written);
    out
}

/// A PDF being written.
pub(crate) struct Builder {
    pub(crate) out: Vec<u8>,
    offsets: Vec<(u32, usize)>,
}

impl Builder {
    /// A PDF holding only its header.
    pub(crate) fn new() -> Self {
        Self {
            out: b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec(),
            offsets: Vec::new(),
        }
    }

    /// Write object `num` as `body`.
    pub(crate) fn object(&mut self, num: u32, body: &str) -> &mut Self {
        self.offsets.push((num, self.out.len()));
        self.out
            .extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
        self
    }

    /// Write object `num` as `text`, spelled out whole.
    pub(crate) fn raw(&mut self, num: u32, text: &[u8]) -> &mut Self {
        self.offsets.push((num, self.out.len()));
        self.out.extend_from_slice(text);
        self
    }

    /// Write object `num` as a stream of dictionary entries `entries` over
    /// `data`, its `/Length` stated.
    pub(crate) fn stream(&mut self, num: u32, entries: &str, data: &[u8]) -> &mut Self {
        self.offsets.push((num, self.out.len()));
        let head = format!(
            "{num} 0 obj\n<<{entries} /Length {}>>\nstream\n",
            data.len()
        );
        self.out.extend_from_slice(head.as_bytes());
        self.out.extend_from_slice(data);
        self.out.extend_from_slice(b"\nendstream\nendobj\n");
        self
    }

    /// Where object `num` was written.
    pub(crate) fn offset(&self, num: u32) -> usize {
        self.offsets
            .iter()
            .rev()
            .find(|&&(held, _)| held == num)
            .map(|&(_, at)| at)
            .expect("a written object")
    }

    /// The cross-reference table of every object written, one subsection per
    /// object, and its trailer of `entries` with `/Size` and `/Root` stated.
    pub(crate) fn table(&mut self, root: u32, entries: &str) -> usize {
        let at = self.out.len();
        let size = self
            .offsets
            .iter()
            .map(|&(num, _)| num + 1)
            .max()
            .unwrap_or(1);
        self.out
            .extend_from_slice(b"xref\n0 1\n0000000000 65535 f \n");
        for &(num, offset) in &self.offsets {
            self.out
                .extend_from_slice(format!("{num} 1\n{offset:010} 00000 n \n").as_bytes());
        }
        self.out.extend_from_slice(
            format!("trailer\n<</Size {size} /Root {root} 0 R {entries}>>\n").as_bytes(),
        );
        at
    }

    /// End the file with `startxref` naming `at`.
    pub(crate) fn finish(&mut self, at: usize) -> Vec<u8> {
        self.out
            .extend_from_slice(format!("startxref\n{at}\n%%EOF\n").as_bytes());
        core::mem::take(&mut self.out)
    }

    /// The whole file with a table and trailer of `entries`.
    pub(crate) fn tabled(&mut self, root: u32, entries: &str) -> Vec<u8> {
        let at = self.table(root, entries);
        self.finish(at)
    }

    /// Object `num` as a cross-reference stream placing every object written
    /// and every `(num, stream, index)` in `compressed`, its dictionary
    /// stating `entries`. Answers where it is.
    pub(crate) fn xref_stream(
        &mut self,
        num: u32,
        entries: &str,
        compressed: &[(u32, u32, u32)],
    ) -> usize {
        let at = self.out.len();
        let mut rows: Vec<(u32, [u8; 7])> = self
            .offsets
            .iter()
            .map(|&(held, offset)| {
                let offset = u32::try_from(offset)
                    .expect("a small fixture")
                    .to_be_bytes();
                (held, [1, offset[0], offset[1], offset[2], offset[3], 0, 0])
            })
            .collect();
        let own = u32::try_from(at).expect("a small fixture").to_be_bytes();
        rows.push((num, [1, own[0], own[1], own[2], own[3], 0, 0]));
        for &(held, stream, index) in compressed {
            let stream = stream.to_be_bytes();
            let index = u16::try_from(index).expect("a small index").to_be_bytes();
            rows.push((
                held,
                [
                    2, stream[0], stream[1], stream[2], stream[3], index[0], index[1],
                ],
            ));
        }
        rows.sort_by_key(|&(held, _)| held);
        let mut index = Vec::new();
        let mut data = Vec::new();
        for (held, row) in &rows {
            index.push(format!("{held} 1"));
            data.extend_from_slice(row);
        }
        let size = rows.last().map_or(1, |&(held, _)| held + 1);
        let dict = format!(
            "/Type /XRef /Size {size} /W [1 4 2] /Index [{}] {entries}",
            index.join(" ")
        );
        self.stream(num, &dict, &data);
        at
    }

    /// An object stream `num` holding `objects`, each `(number, body)`.
    pub(crate) fn object_stream(&mut self, num: u32, objects: &[(u32, &str)]) -> &mut Self {
        let mut head = Vec::new();
        let mut body = Vec::new();
        for &(held, text) in objects {
            head.push(format!("{held} {}", body.len()));
            body.extend_from_slice(text.as_bytes());
            body.push(b' ');
        }
        let head = head.join(" ") + " ";
        let mut data = head.as_bytes().to_vec();
        data.extend_from_slice(&body);
        let entries = format!(
            "/Type /ObjStm /N {} /First {} /Filter /FlateDecode",
            objects.len(),
            head.len()
        );
        self.stream(num, &entries, &flate(&data))
    }
}

/// A one-page PDF whose page draws `content`, with resources `resources`.
pub(crate) fn one_page(content: &[u8], resources: &str) -> Vec<u8> {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R] /Count 1>>")
        .object(
            3,
            &format!(
                "<</Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources {resources}>>"
            ),
        )
        .stream(4, "", content);
    pdf.tabled(1, "")
}

/// A one-page PDF also holding `objects`, each `(number, body)`, numbered
/// from 10, and `streams`, each `(number, dictionary entries, data)`.
pub(crate) fn with_objects(objects: &[(u32, &str)], streams: &[(u32, &str, &[u8])]) -> Vec<u8> {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 200 100]>>");
    for &(num, body) in objects {
        pdf.object(num, body);
    }
    for &(num, entries, data) in streams {
        pdf.stream(num, entries, data);
    }
    pdf.tabled(1, "")
}

/// The options every test opens a document with.
pub(crate) const OPTIONS: crate::Options<'static> = crate::Options {
    password: None,
    budget: 1 << 22,
};

/// The reference to object `num`.
pub(crate) const fn reference(num: u32) -> crate::object::Object {
    crate::object::Object::Ref(crate::object::ObjRef { num, gen: 0 })
}
