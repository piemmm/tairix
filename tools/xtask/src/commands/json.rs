//! A strict JSON reader for the machine-readable specifications the generators
//! vendor (RFC 8259): objects, arrays, strings with their escapes, numbers,
//! `true`, `false` and `null`, and nothing beyond — no comments, no trailing
//! commas.

/// A parsed JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    /// The number as written, read by [`Json::as_u64`].
    Number(String),
    String(String),
    Array(Vec<Json>),
    /// Members in document order.
    Object(Vec<(String, Json)>),
}

/// The nesting a document may reach, far past any vendored specification's.
const MAX_DEPTH: usize = 64;

impl Json {
    /// The member `key` of an object.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Self::Object(members) => members.iter().find(|(name, _)| name == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// A non-negative integer, written in decimal.
    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(text) => text.parse().ok(),
            _ => None,
        }
    }
}

/// Parse `text` as one JSON document.
pub fn parse(text: &str) -> Result<Json, String> {
    let mut reader = Reader {
        bytes: text.as_bytes(),
        at: 0,
    };
    let value = reader.value(0)?;
    reader.skip_space();
    if reader.at == reader.bytes.len() {
        Ok(value)
    } else {
        Err(reader.fault("trailing text"))
    }
}

struct Reader<'t> {
    bytes: &'t [u8],
    at: usize,
}

impl Reader<'_> {
    fn fault(&self, what: &str) -> String {
        format!("json: {what} at byte {}", self.at)
    }

    fn skip_space(&mut self) {
        while self
            .bytes
            .get(self.at)
            .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
        {
            self.at += 1;
        }
    }

    fn take(&mut self, literal: &[u8]) -> bool {
        let taken = self.bytes.get(self.at..self.at + literal.len()) == Some(literal);
        if taken {
            self.at += literal.len();
        }
        taken
    }

    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > MAX_DEPTH {
            return Err(self.fault("nesting too deep"));
        }
        self.skip_space();
        match self.bytes.get(self.at) {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => self.string().map(Json::String),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ if self.take(b"true") => Ok(Json::Bool(true)),
            _ if self.take(b"false") => Ok(Json::Bool(false)),
            _ if self.take(b"null") => Ok(Json::Null),
            _ => Err(self.fault("expected a value")),
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        self.at += 1;
        let mut members = Vec::new();
        self.skip_space();
        if self.take(b"}") {
            return Ok(Json::Object(members));
        }
        loop {
            self.skip_space();
            if self.bytes.get(self.at) != Some(&b'"') {
                return Err(self.fault("expected a member name"));
            }
            let name = self.string()?;
            self.skip_space();
            if !self.take(b":") {
                return Err(self.fault("expected `:`"));
            }
            members.push((name, self.value(depth + 1)?));
            self.skip_space();
            if self.take(b"}") {
                return Ok(Json::Object(members));
            }
            if !self.take(b",") {
                return Err(self.fault("expected `,` or `}`"));
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, String> {
        self.at += 1;
        let mut items = Vec::new();
        self.skip_space();
        if self.take(b"]") {
            return Ok(Json::Array(items));
        }
        loop {
            items.push(self.value(depth + 1)?);
            self.skip_space();
            if self.take(b"]") {
                return Ok(Json::Array(items));
            }
            if !self.take(b",") {
                return Err(self.fault("expected `,` or `]`"));
            }
        }
    }

    fn digits(&mut self) -> usize {
        let from = self.at;
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_digit) {
            self.at += 1;
        }
        self.at - from
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.at;
        self.take(b"-");
        let integer = self.at;
        let whole = self.digits();
        if whole == 0 || (whole > 1 && self.bytes[integer] == b'0') {
            return Err(self.fault("malformed number"));
        }
        if self.take(b".") && self.digits() == 0 {
            return Err(self.fault("malformed fraction"));
        }
        if matches!(self.bytes.get(self.at), Some(b'e' | b'E')) {
            self.at += 1;
            let _ = self.take(b"+") || self.take(b"-");
            if self.digits() == 0 {
                return Err(self.fault("malformed exponent"));
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.at])
            .map_err(|_| self.fault("malformed number"))?;
        Ok(Json::Number(text.to_string()))
    }

    fn string(&mut self) -> Result<String, String> {
        self.at += 1;
        let mut text = String::new();
        loop {
            let rest = &self.bytes[self.at..];
            let run = rest
                .iter()
                .position(|&b| b == b'"' || b == b'\\' || b < 0x20)
                .ok_or_else(|| self.fault("unterminated string"))?;
            text.push_str(std::str::from_utf8(&rest[..run]).map_err(|_| self.fault("not UTF-8"))?);
            self.at += run;
            match self.bytes[self.at] {
                b'"' => {
                    self.at += 1;
                    return Ok(text);
                }
                b'\\' => {
                    self.at += 1;
                    text.push(self.escape()?);
                }
                _ => return Err(self.fault("control character in a string")),
            }
        }
    }

    fn escape(&mut self) -> Result<char, String> {
        let code = *self
            .bytes
            .get(self.at)
            .ok_or_else(|| self.fault("unterminated escape"))?;
        self.at += 1;
        Ok(match code {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let high = self.hex4()?;
                if (0xD800..0xDC00).contains(&high) {
                    if !self.take(b"\\u") {
                        return Err(self.fault("unpaired surrogate"));
                    }
                    let low = self.hex4()?;
                    if !(0xDC00..0xE000).contains(&low) {
                        return Err(self.fault("unpaired surrogate"));
                    }
                    let scalar = 0x10000 + ((high - 0xD800) << 10) + (low - 0xDC00);
                    char::from_u32(scalar).ok_or_else(|| self.fault("bad scalar"))?
                } else {
                    char::from_u32(high).ok_or_else(|| self.fault("unpaired surrogate"))?
                }
            }
            _ => return Err(self.fault("unknown escape")),
        })
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits = self
            .bytes
            .get(self.at..self.at + 4)
            .and_then(|d| std::str::from_utf8(d).ok())
            .and_then(|d| u32::from_str_radix(d, 16).ok())
            .ok_or_else(|| self.fault("malformed \\u escape"))?;
        self.at += 4;
        Ok(digits)
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, Json};

    #[test]
    fn a_document_reads_as_its_values() {
        let doc =
            parse(r#" { "a" : [1, -2.5e3, true, false, null], "b": "x\"\u00e9\ud83d\ude00" } "#)
                .expect("well formed");
        let a = doc.get("a").and_then(Json::as_array).expect("an array");
        assert_eq!(a[0].as_u64(), Some(1));
        assert_eq!(a[1], Json::Number("-2.5e3".into()));
        assert_eq!(a[2].as_bool(), Some(true));
        assert_eq!(a[4], Json::Null);
        assert_eq!(doc.get("b").and_then(Json::as_str), Some("x\"é😀"));
    }

    #[test]
    fn anything_beyond_the_grammar_is_refused() {
        for bad in [
            "{\"a\":1,}",
            "[1,]",
            "01",
            "\"unterminated",
            "{\"a\" 1}",
            "[1] trailing",
            "\"\\ud800\"",
            "\"tab\there\"",
            "// comment\n1",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} was accepted");
        }
        assert!(parse(&"[".repeat(65)).is_err(), "nesting past the bound");
    }
}
