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

    /// Adds a field at the end whose value `write` appends to the buffer it
    /// is given, so a value built from parts needs no buffer of its own.
    /// `write` only appends; a `write` that removes bytes before its own
    /// adds no field.
    ///
    /// ```
    /// let mut f = proxy_net::core::FieldList::new();
    /// f.push_with("Via", |v| v.extend_from_slice(b"1.1 edge"));
    /// assert_eq!(f.get("via"), Some(&b"1.1 edge"[..]));
    /// ```
    pub fn push_with(&mut self, name: &str, write: impl FnOnce(&mut Vec<u8>)) {
        let start = self.bytes.len();
        self.bytes
            .extend(name.bytes().map(|b| b.to_ascii_lowercase()));
        let mid = self.bytes.len();
        write(&mut self.bytes);
        if self.bytes.len() < mid {
            // `write` cut into the name: the field is not added.
            self.bytes.truncate(start);
            return;
        }
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

    /// Bytes of capacity held by the list's buffers.
    #[cfg(feature = "client")]
    pub(crate) fn capacity(&self) -> usize {
        self.bytes.capacity() + self.spans.capacity() * core::mem::size_of::<(u32, u32, u32)>()
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

/// Header fields the rewriting functions can read in place: a slice of
/// pairs or a [`FieldList`].
pub(crate) trait FieldSource {
    /// The fields in order, names as received.
    fn fields(&self) -> impl Iterator<Item = (&str, &[u8])> + '_;
}

impl FieldSource for [(&str, &[u8])] {
    fn fields(&self) -> impl Iterator<Item = (&str, &[u8])> + '_ {
        self.iter().map(|&(n, v)| (n, v))
    }
}

impl FieldSource for Vec<(&str, &[u8])> {
    fn fields(&self) -> impl Iterator<Item = (&str, &[u8])> + '_ {
        self.iter().map(|&(n, v)| (n, v))
    }
}

impl FieldSource for FieldList {
    fn fields(&self) -> impl Iterator<Item = (&str, &[u8])> + '_ {
        self.iter()
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
        f.push_with("Y", |v| v.extend_from_slice(b"12"));
        assert_eq!(f.get("y"), Some(&b"12"[..]));
        assert_eq!(f.fields().count(), 2);
    }
}
