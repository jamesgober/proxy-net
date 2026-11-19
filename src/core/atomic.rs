//! Compare-and-swap updates that build on every supported toolchain.
//!
//! `fetch_update` is deprecated on newer toolchains in favour of a method the
//! MSRV does not have, so the loop is written out once here.

#[cfg(not(proxy_net_loom))]
pub(crate) use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
#[cfg(proxy_net_loom)]
pub(crate) use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Applies `f` until the swap succeeds; `Err(current)` when `f` returns
/// `None`, `Ok(previous)` otherwise.
pub(crate) fn update_u32(a: &AtomicU32, mut f: impl FnMut(u32) -> Option<u32>) -> Result<u32, u32> {
    let mut current = a.load(Ordering::Acquire);
    loop {
        let next = f(current).ok_or(current)?;
        match a.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(previous) => return Ok(previous),
            Err(seen) => current = seen,
        }
    }
}

/// As [`update_u32`], for 64-bit values.
pub(crate) fn update_u64(a: &AtomicU64, mut f: impl FnMut(u64) -> Option<u64>) -> Result<u64, u64> {
    let mut current = a.load(Ordering::Acquire);
    loop {
        let next = f(current).ok_or(current)?;
        match a.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(previous) => return Ok(previous),
            Err(seen) => current = seen,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_updates() {
        let a = AtomicU32::new(1);
        assert_eq!(update_u32(&a, |v| v.checked_sub(1)), Ok(1));
        assert_eq!(update_u32(&a, |v| v.checked_sub(1)), Err(0));
        let b = AtomicU64::new(5);
        assert_eq!(update_u64(&b, |v| Some(v * 2)), Ok(5));
        assert_eq!(b.load(Ordering::Relaxed), 10);
        assert_eq!(update_u64(&b, |_| None), Err(10));
    }
}
