//! Buffers kept on each thread between exchanges.
//!
//! A worker runs one exchange at a time, and every exchange needs the same
//! few buffers: the client's fields, the upstream request's fields and
//! target, the bytes sent, the response's fields, and a relay buffer. Taking
//! them from here and giving them back when the exchange ends means a warm
//! worker allocates none of them again.
//!
//! Bounded: at most [`KEEP`] of each kind per thread, and a buffer that grew
//! past [`MAX_KEPT`] bytes is freed instead of kept, so one large request
//! does not hold memory on the thread afterwards. A kept buffer is cleared;
//! its old bytes are past its length, where safe code cannot read them, as
//! with memory the allocator hands out again.

use std::cell::RefCell;
use std::vec::Vec;

use crate::core::FieldList;

/// Buffers of each kind kept per thread.
const KEEP: usize = 3;

/// The largest buffer kept, in bytes of capacity.
const MAX_KEPT: usize = 16 * 1024;

#[derive(Default)]
struct Spare {
    lists: Vec<FieldList>,
    bytes: Vec<Vec<u8>>,
}

thread_local! {
    static SPARE: RefCell<Spare> = RefCell::new(Spare::default());
}

/// An empty field list, reused when the thread has one.
pub(crate) fn list() -> FieldList {
    SPARE
        .try_with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.lists.pop()))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Keeps `list` for the thread's next exchange.
pub(crate) fn give_list(mut list: FieldList) {
    if list.capacity() > MAX_KEPT {
        return;
    }
    list.clear();
    let _ = SPARE.try_with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            if s.lists.len() < KEEP {
                s.lists.push(list);
            }
        }
    });
}

/// An empty byte buffer, reused when the thread has one.
pub(crate) fn bytes() -> Vec<u8> {
    SPARE
        .try_with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.bytes.pop()))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Keeps `buf` for the thread's next exchange.
pub(crate) fn give_bytes(mut buf: Vec<u8>) {
    if buf.capacity() == 0 || buf.capacity() > MAX_KEPT {
        return;
    }
    buf.clear();
    let _ = SPARE.try_with(|s| {
        if let Ok(mut s) = s.try_borrow_mut() {
            if s.bytes.len() < KEEP {
                s.bytes.push(buf);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buffers_come_back_empty_and_large_ones_are_not_kept() {
        let mut l = list();
        l.push("a", b"1");
        give_list(l);
        let l = list();
        assert!(l.is_empty());
        assert!(l.capacity() > 0);
        let mut b = bytes();
        b.extend_from_slice(b"abc");
        give_bytes(b);
        let b = bytes();
        assert!(b.is_empty() && b.capacity() >= 3);
        give_bytes(Vec::with_capacity(MAX_KEPT + 1));
        assert_eq!(bytes().capacity(), 0);
        for _ in 0..KEEP + 2 {
            give_bytes(Vec::with_capacity(8));
        }
        let kept: Vec<Vec<u8>> = (0..KEEP + 2).map(|_| bytes()).collect();
        assert_eq!(kept.iter().filter(|b| b.capacity() > 0).count(), KEEP);
    }
}
