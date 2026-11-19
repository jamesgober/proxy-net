//! SipHash, written out so every node and platform hashes keys the same way.

/// SipHash with `C` compression and `D` finalization rounds over `data`, keyed
/// by `key` (two 64-bit words, little-endian as in the reference).
pub(crate) fn siphash<const C: usize, const D: usize>(key: [u64; 2], data: &[u8]) -> u64 {
    let mut v0 = key[0] ^ 0x736f_6d65_7073_6575;
    let mut v1 = key[1] ^ 0x646f_7261_6e64_6f6d;
    let mut v2 = key[0] ^ 0x6c79_6765_6e65_7261;
    let mut v3 = key[1] ^ 0x7465_6462_7974_6573;
    let round = |v0: &mut u64, v1: &mut u64, v2: &mut u64, v3: &mut u64| {
        *v0 = v0.wrapping_add(*v1);
        *v1 = v1.rotate_left(13);
        *v1 ^= *v0;
        *v0 = v0.rotate_left(32);
        *v2 = v2.wrapping_add(*v3);
        *v3 = v3.rotate_left(16);
        *v3 ^= *v2;
        *v0 = v0.wrapping_add(*v3);
        *v3 = v3.rotate_left(21);
        *v3 ^= *v0;
        *v2 = v2.wrapping_add(*v1);
        *v1 = v1.rotate_left(17);
        *v1 ^= *v2;
        *v2 = v2.rotate_left(32);
    };
    let mut chunks = data.chunks_exact(8);
    for chunk in &mut chunks {
        let mut word = [0u8; 8];
        word.copy_from_slice(chunk);
        let m = u64::from_le_bytes(word);
        v3 ^= m;
        for _ in 0..C {
            round(&mut v0, &mut v1, &mut v2, &mut v3);
        }
        v0 ^= m;
    }
    let rest = chunks.remainder();
    let mut last = [0u8; 8];
    last[..rest.len()].copy_from_slice(rest);
    // The low byte of the length goes in the top byte (the reference's `b`).
    last[7] = (data.len() & 0xff) as u8;
    let m = u64::from_le_bytes(last);
    v3 ^= m;
    for _ in 0..C {
        round(&mut v0, &mut v1, &mut v2, &mut v3);
    }
    v0 ^= m;
    v2 ^= 0xff;
    for _ in 0..D {
        round(&mut v0, &mut v1, &mut v2, &mut v3);
    }
    v0 ^ v1 ^ v2 ^ v3
}

/// The key hash used for consistent hashing and request keys: SipHash-1-3.
pub(crate) fn hash_key(key: [u64; 2], data: &[u8]) -> u64 {
    siphash::<1, 3>(key, data)
}

/// splitmix64: a fast, well-mixed step for seeded pseudo-random picks.
pub(crate) fn mix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first vectors of the SipHash-2-4 reference (`vectors.h`): key
    /// 00..0f, messages 00, 00 01, ..., as little-endian 64-bit outputs.
    #[test]
    fn test_siphash_2_4_reference_vectors() {
        let key = [
            u64::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7]),
            u64::from_le_bytes([8, 9, 10, 11, 12, 13, 14, 15]),
        ];
        let expected: [[u8; 8]; 9] = [
            [0x31, 0x0e, 0x0e, 0xdd, 0x47, 0xdb, 0x6f, 0x72],
            [0xfd, 0x67, 0xdc, 0x93, 0xc5, 0x39, 0xf8, 0x74],
            [0x5a, 0x4f, 0xa9, 0xd9, 0x09, 0x80, 0x6c, 0x0d],
            [0x2d, 0x7e, 0xfb, 0xd7, 0x96, 0x66, 0x67, 0x85],
            [0xb7, 0x87, 0x71, 0x27, 0xe0, 0x94, 0x27, 0xcf],
            [0x8d, 0xa6, 0x99, 0xcd, 0x64, 0x55, 0x76, 0x18],
            [0xce, 0xe3, 0xfe, 0x58, 0x6e, 0x46, 0xc9, 0xcb],
            [0x37, 0xd1, 0x01, 0x8b, 0xf5, 0x00, 0x02, 0xab],
            [0x62, 0x24, 0x93, 0x9a, 0x79, 0xf5, 0xf5, 0x93],
        ];
        let msg: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
        for (len, want) in expected.iter().enumerate() {
            assert_eq!(
                siphash::<2, 4>(key, &msg[..len]),
                u64::from_le_bytes(*want),
                "len {len}"
            );
        }
    }

    #[test]
    fn test_hash_key_depends_on_key_and_data() {
        let a = hash_key([0, 0], b"10.0.0.1:80");
        assert_eq!(a, hash_key([0, 0], b"10.0.0.1:80"));
        assert_ne!(a, hash_key([0, 1], b"10.0.0.1:80"));
        assert_ne!(a, hash_key([0, 0], b"10.0.0.2:80"));
        assert_ne!(mix(1), mix(2));
    }
}
