#![allow(dead_code)]

/// Split one line (without '\n') into exactly N fields per the dialect. Returns false if malformed.
#[inline(always)]
pub fn split<const N: usize>(line: &'static [u8], out: &mut [&'static [u8]; N]) -> bool {
    let n = line.len();
    let mut i = 0;
    let mut k = 0;
    loop {
        if k == N {
            return false;
        }
        if i < n && line[i] == b'"' {
            let s = i + 1;
            let mut j = s;
            while j < n {
                let c = line[j];
                if c == b'"' {
                    break;
                }
                if c == b',' {
                    return false;
                }
                j += 1;
            }
            if j >= n {
                return false;
            }
            out[k] = &line[s..j];
            k += 1;
            j += 1;
            if j == n {
                break;
            }
            if line[j] != b',' {
                return false;
            }
            i = j + 1;
        } else {
            let s = i;
            let mut j = i;
            while j < n {
                let c = line[j];
                if c == b',' {
                    break;
                }
                if c == b'"' {
                    return false;
                }
                j += 1;
            }
            out[k] = &line[s..j];
            k += 1;
            if j == n {
                break;
            }
            i = j + 1;
        }
    }
    k == N
}

/// Iterate lines of data[start..end]; skips the header if start==0.
#[inline(always)]
pub fn for_lines(data: &'static [u8], start: usize, end: usize, mut f: impl FnMut(&'static [u8])) {
    let mut p = start;
    if p == 0 {
        match memchr::memchr(b'\n', &data[..end]) {
            Some(q) => p = q + 1,
            None => return,
        }
    }
    while p < end {
        let q = match memchr::memchr(b'\n', &data[p..end]) {
            Some(q) => p + q,
            None => end,
        };
        f(&data[p..q]);
        p = q + 1;
    }
}


use std::arch::x86_64::*;
pub const PREFETCH: usize = 1024;

#[inline(always)]
unsafe fn masks64x2(p: *const u8) -> ([u64; 2], [u64; 2], [u64; 2]) {
    let vn = _mm256_set1_epi8(b'\n' as i8);
    let vc = _mm256_set1_epi8(b',' as i8);
    let vq = _mm256_set1_epi8(b'"' as i8);
    let mut nl = [0u64; 2];
    let mut cm = [0u64; 2];
    let mut qt = [0u64; 2];
    for w in 0..2 {
        let v0 = _mm256_loadu_si256(p.add(64 * w) as *const __m256i);
        let v1 = _mm256_loadu_si256(p.add(64 * w + 32) as *const __m256i);
        nl[w] = (_mm256_movemask_epi8(_mm256_cmpeq_epi8(v0, vn)) as u32 as u64)
            | ((_mm256_movemask_epi8(_mm256_cmpeq_epi8(v1, vn)) as u32 as u64) << 32);
        cm[w] = (_mm256_movemask_epi8(_mm256_cmpeq_epi8(v0, vc)) as u32 as u64)
            | ((_mm256_movemask_epi8(_mm256_cmpeq_epi8(v1, vc)) as u32 as u64) << 32);
        qt[w] = (_mm256_movemask_epi8(_mm256_cmpeq_epi8(v0, vq)) as u32 as u64)
            | ((_mm256_movemask_epi8(_mm256_cmpeq_epi8(v1, vq)) as u32 as u64) << 32);
    }
    (nl, cm, qt)
}

/// Parse the line starting at data[p]. Returns (start of next line, fields valid).
/// A line is valid iff it has exactly N-1 commas and every field is either free of quotes
/// or quoted around the whole field. With commas splitting fields, that holds exactly when each
/// field's first byte is '"' iff its last byte is (and a quoted field has length >= 2), and
/// the line's total quote count is 2 x the number of quoted fields (no interior quotes).
#[cfg_attr(not(feature = "prof"), inline(always))]
#[cfg_attr(feature = "prof", inline(never))]
pub fn split_at<const N: usize>(data: &'static [u8], p: usize, out: &mut [&'static [u8]; N]) -> (usize, bool) {
    if p + 128 <= data.len() {
        if PREFETCH > 0 {
            unsafe { _mm_prefetch(data.as_ptr().wrapping_add(p + PREFETCH) as *const i8, _MM_HINT_T0) };
        }
        let base = unsafe { data.as_ptr().add(p) };
        let (nl, cm, qt) = unsafe { masks64x2(base) };
        let len = if nl[0] != 0 {
            nl[0].trailing_zeros() as usize
        } else if nl[1] != 0 {
            64 + nl[1].trailing_zeros() as usize
        } else {
            usize::MAX
        };
        if len != usize::MAX {
            let next = p + len + 1;
            // masks restricted to the line
            let (m0, m1) = if len >= 64 { (u64::MAX, (1u64 << (len - 64)) - 1) } else { ((1u64 << len) - 1, 0) };
            let mut c0 = cm[0] & m0;
            let mut c1 = cm[1] & m1;
            if (c0.count_ones() + c1.count_ones()) as usize != N - 1 {
                return (next, false);
            }
            let nquotes = (qt[0] & m0).count_ones() + (qt[1] & m1).count_ones();
            let mut a = 0usize;
            let mut valid = true;
            let mut nq = 0u32;
            for k in 0..N {
                let b = if k + 1 < N {
                    let lo = c0 != 0;
                    let w = if lo { c0 } else { c1 };
                    let pos = w.trailing_zeros() as usize + if lo { 0 } else { 64 };
                    let wc = w & w.wrapping_sub(1);
                    if lo { c0 = wc } else { c1 = wc }
                    pos
                } else {
                    len
                };
                let flen = b - a;
                let first = unsafe { *base.add(a) };
                let last = unsafe { *base.add(b + (flen == 0) as usize - 1) };
                let qf = first == b'"';
                let ql = last == b'"';
                valid &= (qf == ql) & !(qf & (flen < 2));
                let qd = qf as usize;
                nq += qf as u32;
                out[k] = unsafe { std::slice::from_raw_parts(base.add(a + qd), flen.saturating_sub(2 * qd)) };
                a = b + 1;
            }
            return (next, valid & (nquotes == 2 * nq));
        }
    }
    let q = match memchr::memchr(b'\n', &data[p..]) {
        Some(q) => p + q,
        None => data.len(),
    };
    let ok = split::<N>(&data[p..q], out);
    (q + 1, ok)
}

/// newline / comma / quote masks of 64 bytes.
#[inline(always)]
unsafe fn masks64(p: *const u8) -> (u64, u64, u64) {
    let v0 = _mm256_loadu_si256(p as *const __m256i);
    let v1 = _mm256_loadu_si256(p.add(32) as *const __m256i);
    let m = |c: u8| {
        let s = _mm256_set1_epi8(c as i8);
        (_mm256_movemask_epi8(_mm256_cmpeq_epi8(v0, s)) as u32 as u64)
            | ((_mm256_movemask_epi8(_mm256_cmpeq_epi8(v1, s)) as u32 as u64) << 32)
    };
    (m(b'\n'), m(b','), m(b'"'))
}

/// Inclusive prefix xor of a 64-bit mask (bit i = parity of bits 0..=i).
#[inline(always)]
fn pxor64(mut x: u64) -> u64 {
    x ^= x << 1;
    x ^= x << 2;
    x ^= x << 4;
    x ^= x << 8;
    x ^= x << 16;
    x ^= x << 32;
    x
}

/// Same contract as `split_at`, with validity decided on bitmasks:
/// with C = commas, Q = quotes, S = field first bytes, E = field last bytes (all within the line),
/// the line is valid iff |C| = N-1, Q is a subset of S|E, |Q| is even, and no comma lies between
/// a quote and its partner (prefix-xor of Q has no comma under it). Quote pairs then sit exactly
/// at the first and last byte of one field (a 1-byte field cannot hold a pair).
#[cfg_attr(not(feature = "prof"), inline(always))]
#[cfg_attr(feature = "prof", inline(never))]
pub fn split_at2<const N: usize>(data: &'static [u8], p: usize, out: &mut [&'static [u8]; N]) -> (usize, bool) {
    if p + 128 <= data.len() {
        if PREFETCH > 0 {
            unsafe { _mm_prefetch(data.as_ptr().wrapping_add(p + PREFETCH) as *const i8, _MM_HINT_T0) };
        }
        let base = unsafe { data.as_ptr().add(p) };
        let (n0, c0, q0) = unsafe { masks64(base) };
        let found;
        let (len, c, q): (usize, u128, u128);
        if n0 != 0 {
            let l = n0.trailing_zeros() as usize;
            let m = (1u64 << l) - 1;
            len = l;
            c = (c0 & m) as u128;
            q = (q0 & m) as u128;
            found = true;
        } else {
            let (n1, c1, q1) = unsafe { masks64(base.add(64)) };
            let l = n1.trailing_zeros() as usize; // 64 if none
            let m = if l == 64 { 0 } else { (1u64 << l) - 1 };
            len = 64 + l;
            c = c0 as u128 | (((c1 & m) as u128) << 64);
            q = q0 as u128 | (((q1 & m) as u128) << 64);
            found = n1 != 0;
        }
        if found {
            let next = p + len + 1;
            if c.count_ones() as usize != N - 1 {
                return (next, false);
            }
            // len >= N-1 >= 1 here
            let s = (c << 1) | 1;
            let e = (c >> 1) | (1u128 << (len - 1));
            let lo = pxor64(q as u64);
            let hi = pxor64((q >> 64) as u64) ^ 0u64.wrapping_sub(lo >> 63);
            let inside = lo as u128 | ((hi as u128) << 64);
            let valid = (q & !(s | e)) == 0 && q.count_ones() & 1 == 0 && (inside & c) == 0;
            let mut cc = c;
            let mut a = 0usize;
            for k in 0..N {
                let b = if k + 1 < N {
                    let t = cc.trailing_zeros() as usize;
                    cc &= cc.wrapping_sub(1);
                    t
                } else {
                    len
                };
                let qd = (unsafe { *base.add(a) } == b'"') as usize;
                out[k] = unsafe { std::slice::from_raw_parts(base.add(a + qd), (b - a).saturating_sub(2 * qd)) };
                a = b + 1;
            }
            return (next, valid);
        }
    }
    let q = match memchr::memchr(b'\n', &data[p..]) {
        Some(q) => p + q,
        None => data.len(),
    };
    let ok = split::<N>(&data[p..q], out);
    (q + 1, ok)
}

/// First line start to parse in a chunk (skips the file header for the chunk at offset 0).
#[inline(always)]
pub fn chunk_start(data: &'static [u8], start: usize, end: usize) -> usize {
    populate(data, start, end);
    if start == 0 {
        match memchr::memchr(b'\n', &data[..end]) {
            Some(q) => q + 1,
            None => end,
        }
    } else {
        start
    }
}

/// Pre-populate page tables for data[start..end] in one syscall (Linux 5.14+; errors ignored).
pub fn populate(data: &'static [u8], start: usize, end: usize) {
    if std::env::var_os("SS_POP").is_none() {
        return;
    }
    let a = (data.as_ptr() as usize + start) & !4095;
    let b = data.as_ptr() as usize + end;
    #[cfg(unix)]
    if b > a {
        unsafe { libc::madvise(a as *mut libc::c_void, b - a, 22 /* MADV_POPULATE_READ */) };
    }
}
