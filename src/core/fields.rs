//! A reusable list of header fields: one byte buffer and a span per field.

use alloc::vec::Vec;

/// An ordered list of fields, kept in one buffer that is reused from one
/// exchange to the next ([`clear`](FieldList::clear) keeps the capacity).
/// Names are stored in lowercase.
///
/// ```
/// use proxy_net::core::FieldList;
/// let mut f = FieldList::new();
/// f.push("Accept", b"text/html");
/// f.push("x-a", b"1");
/// f.push("X-A", b"2");
/// assert_eq!(f.get_all("x-a").collect::<Vec<_>>(), [&b"1"[..], b"2"]);
/// f.remove("x-a");
/// assert_eq!(f.iter().collect::<Vec<_>>(), [("accept", &b"text/html"[..])]);
/// ```
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldList {
    bytes: Vec<u8>,
    /// Name start, name end (= value start), value end.
    spans: Vec<(u32, u32, u32)>,
}

impl FieldList {
    /// An empty list.
    pub fn new() -> FieldList {
        FieldList::default()
    }

    /// Removes every field and keeps the buffers.
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.spans.clear();
    }

    /// Adds a field at the end; the name is stored in lowercase.
    pub fn push(&mut self, name: &str, value: &[u8]) {
        let start = self.bytes.len();
        self.bytes
            .extend(name.bytes().map(|b| b.to_ascii_lowercase()));
        let mid = self.bytes.len();
        self.bytes.extend_from_slice(value);
        let end = self.bytes.len();
        self.spans.push((start as u32, mid as u32, end as u32));
    }

    /// Removes every field named `name` (any case).
    pub fn remove(&mut self, name: &str) {
        let bytes = &self.bytes;
        self.spans.retain(|&(s, m, _)| {
            !bytes[s as usize..m as usize].eq_ignore_ascii_case(name.as_bytes())
        });
    }

    /// Replaces every field named `name` with one holding `value`, at the end.
    pub fn set(&mut self, name: &str, value: &[u8]) {
        self.remove(name);
        self.push(name, value);
    }

    /// The fields in order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[u8])> + '_ {
        self.spans.iter().map(|&(s, m, e)| {
            let name = core::str::from_utf8(&self.bytes[s as usize..m as usize]).unwrap_or("");
            (name, &self.bytes[m as usize..e as usize])
        })
    }

    /// The values of every field named `name` (any case), in order.
    pub fn get_all<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a [u8]> + 'a {
        self.iter()
            .filter(move |(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    }

    /// The first value of `name`.
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    }

    /// The number of fields.
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// True when there are no fields.
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_field_list_operations() {
        let mut f = FieldList::new();
        assert!(f.is_empty());
        f.push("A", b"1");
        f.push("b", b"2");
        f.set("a", b"3");
        assert_eq!(f.len(), 2);
        assert_eq!(
            f.iter().collect::<Vec<_>>(),
            [("b", &b"2"[..]), ("a", &b"3"[..])]
        );
        assert_eq!(f.get("A"), Some(&b"3"[..]));
        assert_eq!(f.get("c"), None);
        f.clear();
        assert!(f.is_empty());
        f.push("x", b"");
        assert_eq!(f.get("x"), Some(&b""[..]));
    }
}
