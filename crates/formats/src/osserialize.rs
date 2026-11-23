//! Apple's kernel serialization formats: the XML form without a plist
//! wrapper (with ID/IDREF references), as in kernelcache prelink info and
//! touch firmware configs, and the binary form, as in AOP firmware.
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Keys in document order.
    Dict(Vec<(String, Value)>),
    Array(Vec<Value>),
    String(String),
    Integer(u64),
    Data(Vec<u8>),
    Bool(bool),
}

impl Value {
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Integer(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_data(&self) -> Option<&[u8]> {
        match self {
            Value::Data(d) => Some(d),
            _ => None,
        }
    }
}

const MAX_DEPTH: usize = 64;

/// Reads the XML form.
pub fn xml(bytes: &[u8]) -> Result<Value> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error("serialized XML is not UTF-8"))?;
    let mut p = Xml {
        s: text.trim_end_matches('\0'),
        at: 0,
        ids: Vec::new(),
    };
    p.skip_prolog();
    let value = p.element(0)?;
    p.skip_space();
    if p.at != p.s.len() {
        return Err(Error("data after the serialized XML"));
    }
    Ok(value)
}

struct Xml<'a> {
    s: &'a str,
    at: usize,
    ids: Vec<(String, Value)>,
}

impl Xml<'_> {
    fn rest(&self) -> &str {
        &self.s[self.at..]
    }

    fn skip_space(&mut self) {
        self.at = self.s.len() - self.rest().trim_start().len();
    }

    /// Skips an `<?xml …?>` declaration, a doctype and comments.
    fn skip_prolog(&mut self) {
        loop {
            self.skip_space();
            let end = if self.rest().starts_with("<?") {
                self.rest().find("?>").map(|i| i + 2)
            } else if self.rest().starts_with("<!--") {
                self.rest().find("-->").map(|i| i + 3)
            } else if self.rest().starts_with("<!") {
                self.rest().find('>').map(|i| i + 1)
            } else {
                None
            };
            match end {
                Some(n) => self.at += n,
                None => return,
            }
        }
    }

    fn element(&mut self, depth: usize) -> Result<Value> {
        if depth > MAX_DEPTH {
            return Err(Error("serialized XML nests too deeply"));
        }
        self.skip_prolog();
        let Tag { name, attrs, empty } = self.start_tag()?;
        if let Some(id) = attr(&attrs, "IDREF") {
            if !empty {
                self.end_tag(&name)?;
            }
            return self
                .ids
                .iter()
                .find(|(k, _)| k == id)
                .map(|(_, v)| v.clone())
                .ok_or(Error("unknown IDREF"));
        }
        let value = match name.as_str() {
            "dict" | "array" => {
                let mut items = Vec::new();
                if !empty {
                    loop {
                        self.skip_prolog();
                        if self.rest().starts_with("</") {
                            break;
                        }
                        items.push(self.element(depth + 1)?);
                    }
                    self.end_tag(&name)?;
                }
                if name == "array" {
                    Value::Array(items)
                } else {
                    if items.len() % 2 != 0 {
                        return Err(Error("dictionary has a key without a value"));
                    }
                    let mut entries = Vec::new();
                    let mut it = items.into_iter();
                    while let (Some(k), Some(v)) = (it.next(), it.next()) {
                        let Value::String(k) = k else {
                            return Err(Error("dictionary key is not a string"));
                        };
                        entries.push((k, v));
                    }
                    Value::Dict(entries)
                }
            }
            "true" | "false" => {
                if !empty {
                    self.end_tag(&name)?;
                }
                Value::Bool(name == "true")
            }
            "key" | "string" | "integer" | "data" => {
                let text = if empty {
                    String::new()
                } else {
                    self.text_until(&name)?
                };
                match name.as_str() {
                    "integer" => Value::Integer(integer(text.trim())?),
                    "data" => Value::Data(base64(&text)?),
                    _ => Value::String(text),
                }
            }
            _ => return Err(Error("unexpected tag in serialized XML")),
        };
        if let Some(id) = attr(&attrs, "ID") {
            self.ids.push((id.to_owned(), value.clone()));
        }
        Ok(value)
    }

    /// Reads `<name attr="v" …>` or `<name …/>`.
    fn start_tag(&mut self) -> Result<Tag> {
        let rest = self.rest();
        let end = rest.find('>').ok_or(Error("unterminated XML tag"))?;
        if !rest.starts_with('<') || rest.starts_with("</") {
            return Err(Error("expected an XML tag"));
        }
        let inner = &rest[1..end];
        let (inner, empty) = match inner.strip_suffix('/') {
            Some(i) => (i, true),
            None => (inner, false),
        };
        let mut parts = inner.trim().splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or("").to_owned();
        let mut attrs = Vec::new();
        let mut a = parts.next().unwrap_or("").trim();
        while !a.is_empty() {
            let eq = a.find('=').ok_or(Error("bad XML attribute"))?;
            let key = a[..eq].trim().to_owned();
            let after = a[eq + 1..].trim_start();
            let quote = after
                .chars()
                .next()
                .filter(|c| *c == '"' || *c == '\'')
                .ok_or(Error("bad XML attribute"))?;
            let close = after[1..].find(quote).ok_or(Error("bad XML attribute"))?;
            attrs.push((key, unescape(&after[1..1 + close])?));
            a = after[close + 2..].trim_start();
        }
        self.at += end + 1;
        Ok(Tag { name, attrs, empty })
    }

    fn end_tag(&mut self, name: &str) -> Result<()> {
        self.skip_space();
        let tag = format!("</{name}>");
        if !self.rest().starts_with(&tag) {
            return Err(Error("missing XML end tag"));
        }
        self.at += tag.len();
        Ok(())
    }

    fn text_until(&mut self, name: &str) -> Result<String> {
        let end = self
            .rest()
            .find('<')
            .ok_or(Error("unterminated XML text"))?;
        let text = unescape(&self.rest()[..end])?;
        self.at += end;
        self.end_tag(name)?;
        Ok(text)
    }
}

struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
    /// Written as `<name/>`, without content or end tag.
    empty: bool,
}

fn attr<'a>(attrs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    attrs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn unescape(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let end = rest[i..].find(';').ok_or(Error("bad XML entity"))? + i;
        out.push(match &rest[i + 1..end] {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            num => {
                let code = match num.strip_prefix("#x") {
                    Some(hex) => u32::from_str_radix(hex, 16),
                    None => num
                        .strip_prefix('#')
                        .ok_or(Error("bad XML entity"))?
                        .parse(),
                };
                code.ok()
                    .and_then(char::from_u32)
                    .ok_or(Error("bad XML entity"))?
            }
        });
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn integer(text: &str) -> Result<u64> {
    let parsed = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16).ok(),
        None if text.starts_with('-') => text.parse::<i64>().ok().map(|n| n as u64),
        None => text.parse().ok(),
    };
    parsed.ok_or(Error("bad integer in serialized XML"))
}

fn base64(text: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => return Err(Error("bad base64 data")),
        };
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

const BINARY_MAGIC: &[u8] = b"\xd3\0\0\0";
// Object kinds in the binary form.
const DICT: u32 = 1;
const ARRAY: u32 = 2;
const NUMBER: u32 = 4;
const SYMBOL: u32 = 8;
const STRING: u32 = 9;
const DATA: u32 = 10;
const BOOLEAN: u32 = 11;
const REFERENCE: u32 = 12;
/// Objects (including back-references) a binary document may hold.
const MAX_OBJECTS: usize = 1 << 16;

/// Reads the binary form.
pub fn binary(bytes: &[u8]) -> Result<Value> {
    if !bytes.starts_with(BINARY_MAGIC) {
        return Err(Error("not a binary serialized document"));
    }
    let mut b = Binary {
        data: bytes,
        at: 4,
        objects: Vec::new(),
    };
    let value = b.read(0)?;
    if b.at != bytes.len() {
        return Err(Error("data after the binary serialized document"));
    }
    Ok(value)
}

struct Binary<'a> {
    data: &'a [u8],
    at: usize,
    objects: Vec<Value>,
}

impl Binary<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8]> {
        let bytes = self.data.get(
            self.at
                ..self
                    .at
                    .checked_add(n)
                    .ok_or(Error("binary size overflow"))?,
        );
        let bytes = bytes.ok_or(Error("truncated binary serialized document"))?;
        self.at += n;
        Ok(bytes)
    }

    fn read(&mut self, depth: usize) -> Result<Value> {
        if depth > MAX_DEPTH || self.objects.len() > MAX_OBJECTS {
            return Err(Error(
                "binary serialized document is too large or nests too deeply",
            ));
        }
        let header = u32::from_le_bytes(self.take(4)?.try_into().unwrap());
        let (kind, size) = (header >> 24 & 0x7f, (header & 0xff_ffff) as usize);
        if kind == REFERENCE {
            return self
                .objects
                .get(size)
                .cloned()
                .ok_or(Error("bad back-reference"));
        }
        let value = match kind {
            DICT | ARRAY => {
                // Reserve the slot first: back-references count containers
                // before their contents.
                let slot = self.objects.len();
                self.objects.push(Value::Bool(false));
                let mut items = Vec::new();
                for _ in 0..size * if kind == DICT { 2 } else { 1 } {
                    items.push(self.read(depth + 1)?);
                }
                let value = if kind == ARRAY {
                    Value::Array(items)
                } else {
                    let mut entries = Vec::new();
                    let mut it = items.into_iter();
                    while let (Some(k), Some(v)) = (it.next(), it.next()) {
                        let Value::String(k) = k else {
                            return Err(Error("dictionary key is not a string"));
                        };
                        if entries.iter().any(|(e, _)| *e == k) {
                            return Err(Error("repeated dictionary key"));
                        }
                        entries.push((k, v));
                    }
                    Value::Dict(entries)
                };
                self.objects[slot] = value.clone();
                return Ok(value);
            }
            SYMBOL | STRING | DATA => {
                let raw = self.take(size)?.to_vec();
                self.take((4 - size % 4) % 4)?;
                if kind == DATA {
                    Value::Data(raw)
                } else {
                    let text = raw.split(|&b| b == 0).next().unwrap_or(&[]);
                    Value::String(
                        String::from_utf8(text.to_vec())
                            .map_err(|_| Error("string is not UTF-8"))?,
                    )
                }
            }
            NUMBER if size == 64 => {
                Value::Integer(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
            }
            BOOLEAN if size <= 1 => Value::Bool(size == 1),
            _ => return Err(Error("unsupported object in binary serialized document")),
        };
        self.objects.push(value.clone());
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_with_references_and_entities() {
        let doc =
            br#"<dict><key>a</key><string ID="1">x &amp; y</string><key>b</key><string IDREF="1"/>
            <key>n</key><integer size="64">0x10</integer><key>d</key><data>aGVsbG8=</data>
            <key>t</key><true/><key>l</key><array><integer>7</integer></array></dict>"#;
        let v = xml(doc).unwrap();
        assert_eq!(v.get("a").unwrap().as_str(), Some("x & y"));
        assert_eq!(v.get("b"), v.get("a"));
        assert_eq!(v.get("n").unwrap().as_u64(), Some(16));
        assert_eq!(v.get("d").unwrap().as_data(), Some(&b"hello"[..]));
        assert_eq!(v.get("t"), Some(&Value::Bool(true)));
        assert_eq!(v.get("l").unwrap().as_array().unwrap()[0].as_u64(), Some(7));
        assert!(xml(b"<dict><key>a</key></dict>").is_err());
        assert!(xml(b"<dict></array>").is_err());
    }

    #[test]
    fn binary_with_back_reference() {
        let word = |kind: u32, size: u32, last: bool| {
            ((kind << 24) | size | if last { 0x8000_0000 } else { 0 }).to_le_bytes()
        };
        let mut doc = BINARY_MAGIC.to_vec();
        doc.extend(word(1, 2, true)); // dict with 2 entries
        doc.extend(word(8, 2, false)); // symbol "a\0"
        doc.extend(b"a\0\0\0");
        doc.extend(word(10, 3, false)); // data
        doc.extend(b"xyz\0");
        doc.extend(word(8, 2, false));
        doc.extend(b"b\0\0\0");
        doc.extend(word(12, 2, true)); // back-reference to object 2 (the data)
        let v = binary(&doc).unwrap();
        assert_eq!(v.get("a").unwrap().as_data(), Some(&b"xyz"[..]));
        assert_eq!(v.get("b"), v.get("a"));
        assert!(binary(&doc[..doc.len() - 1]).is_err());
    }
}
