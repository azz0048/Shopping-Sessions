#![allow(dead_code)]
// ---------- hashing ----------
#[inline(always)]
pub fn mix(a: u64, b: u64) -> u64 {
    let r = (a as u128).wrapping_mul(b as u128);
    (r as u64) ^ ((r >> 64) as u64)
}
pub const K0: u64 = 0xa0761d6478bd642f;
pub const K1: u64 = 0xe7037ed1a0b428db;
pub const K2: u64 = 0x8ebc6af09c88c6e3;
pub const K3: u64 = 0x589965cc75374cc3;

#[inline(always)]
pub fn rd8(s: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(s[i..i + 8].try_into().unwrap())
}
#[inline(always)]
pub fn rd4(s: &[u8], i: usize) -> u64 {
    u32::from_le_bytes(s[i..i + 4].try_into().unwrap()) as u64
}

/// wyhash-style 64-bit hash with seed.
#[inline(always)]
pub fn hash_seed(s: &[u8], seed: u64) -> u64 {
    let n = s.len();
    let mut seed = seed ^ mix(seed ^ K0, K1);
    let (a, b);
    if n <= 16 {
        if n >= 4 {
            let q = (n >> 3) << 2;
            a = (rd4(s, 0) << 32) | rd4(s, q);
            b = (rd4(s, n - 4) << 32) | rd4(s, n - 4 - q);
        } else if n > 0 {
            a = ((s[0] as u64) << 16) | ((s[n >> 1] as u64) << 8) | (s[n - 1] as u64);
            b = 0;
        } else {
            a = 0;
            b = 0;
        }
    } else {
        let mut i = 0;
        let mut rem = n;
        while rem > 16 {
            seed = mix(rd8(s, i) ^ K1, rd8(s, i + 8) ^ seed);
            i += 16;
            rem -= 16;
        }
        a = rd8(s, n - 16);
        b = rd8(s, n - 8);
    }
    let a = a ^ K1;
    let b = b ^ seed;
    let r = (a as u128).wrapping_mul(b as u128);
    mix((r as u64) ^ K0 ^ (n as u64), ((r >> 64) as u64) ^ K1)
}
#[cfg_attr(not(feature = "prof"), inline(always))]
#[cfg_attr(feature = "prof", inline(never))]
pub fn hash1(s: &[u8]) -> u64 {
    let h = hash_seed(s, K2);
    if h == 0 { 1 } else { h }
}
#[inline(always)]
pub fn hash2(s: &[u8]) -> u32 {
    (hash_seed(s, K3) >> 32) as u32
}


/// One-pass ~96-bit fingerprint: (64-bit hash, extra 32 bits).
#[cfg_attr(not(feature = "prof"), inline(always))]
#[cfg_attr(feature = "prof", inline(never))]
pub fn hash_wide(s: &[u8]) -> (u64, u32) {
    let n = s.len();
    let mut seed = K2 ^ mix(K2 ^ K0, K1);
    let (a, b);
    if n <= 16 {
        if n >= 4 {
            let q = (n >> 3) << 2;
            a = (rd4(s, 0) << 32) | rd4(s, q);
            b = (rd4(s, n - 4) << 32) | rd4(s, n - 4 - q);
        } else if n > 0 {
            a = ((s[0] as u64) << 16) | ((s[n >> 1] as u64) << 8) | (s[n - 1] as u64);
            b = 0;
        } else {
            a = 0;
            b = 0;
        }
    } else {
        let mut i = 0;
        let mut rem = n;
        while rem > 16 {
            seed = mix(rd8(s, i) ^ K1, rd8(s, i + 8) ^ seed);
            i += 16;
            rem -= 16;
        }
        a = rd8(s, n - 16);
        b = rd8(s, n - 8);
    }
    let r = ((a ^ K1) as u128).wrapping_mul((b ^ seed) as u128);
    let lo = r as u64;
    let hi = (r >> 64) as u64;
    let h = mix(lo ^ K0 ^ (n as u64), hi ^ K1);
    let h2 = (mix(lo ^ K3, hi ^ K2 ^ (n as u64)) >> 32) as u32;
    (if h == 0 { 1 } else { h }, h2)
}
