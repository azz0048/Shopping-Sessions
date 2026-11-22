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

/// `masks64` with AVX-512BW/VL: 256-bit compares straight into mask registers (no vpmovmskb,
/// which competes for port 0 with the scalar bit work; 256-bit width keeps the core at its
/// normal clock).
#[target_feature(enable = "avx512bw,avx512vl")]
#[inline]
pub unsafe fn masks64_v4(p: *const u8) -> (u64, u64, u64) {
    let v0 = _mm256_loadu_si256(p as *const __m256i);
    let v1 = _mm256_loadu_si256(p.add(32) as *const __m256i);
    let n = _mm256_set1_epi8(b'\n' as i8);
    let c = _mm256_set1_epi8(b',' as i8);
    let q = _mm256_set1_epi8(b'"' as i8);
    (
        (_mm256_cmpeq_epi8_mask(v0, n) as u64) | ((_mm256_cmpeq_epi8_mask(v1, n) as u64) << 32),
        (_mm256_cmpeq_epi8_mask(v0, c) as u64) | ((_mm256_cmpeq_epi8_mask(v1, c) as u64) << 32),
        (_mm256_cmpeq_epi8_mask(v0, q) as u64) | ((_mm256_cmpeq_epi8_mask(v1, q) as u64) << 32),
    )
}

/// Same as `masks64_v4` but pinned to 256-bit vectors with inline asm (LLVM fuses the two
/// intrinsic compares into one 512-bit compare, which costs clock speed on Skylake-SP class cores).
#[target_feature(enable = "avx512bw,avx512vl")]
#[inline]
pub unsafe fn masks64_v4y(p: *const u8) -> (u64, u64, u64) {
    let (n, c, q): (u64, u64, u64);
    let sn = _mm256_set1_epi8(b'\n' as i8);
    let sc = _mm256_set1_epi8(b',' as i8);
    let sq = _mm256_set1_epi8(b'"' as i8);
    std::arch::asm!(
        "vmovdqu {v0}, ymmword ptr [{p}]",
        "vmovdqu {v1}, ymmword ptr [{p} + 32]",
        "vpcmpeqb {ka}, {v0}, {sn}",
        "vpcmpeqb {kb}, {v1}, {sn}",
        "kunpckdq {ka}, {kb}, {ka}",
        "kmovq {n}, {ka}",
        "vpcmpeqb {ka}, {v0}, {sc}",
        "vpcmpeqb {kb}, {v1}, {sc}",
        "kunpckdq {ka}, {kb}, {ka}",
        "kmovq {c}, {ka}",
        "vpcmpeqb {ka}, {v0}, {sq}",
        "vpcmpeqb {kb}, {v1}, {sq}",
        "kunpckdq {ka}, {kb}, {ka}",
        "kmovq {q}, {ka}",
        p = in(reg) p,
        sn = in(ymm_reg) sn,
        sc = in(ymm_reg) sc,
        sq = in(ymm_reg) sq,
        v0 = out(ymm_reg) _,
        v1 = out(ymm_reg) _,
        ka = out(kreg) _,
        kb = out(kreg) _,
        n = out(reg) n,
        c = out(reg) c,
        q = out(reg) q,
        options(pure, readonly, nostack, preserves_flags),
    );
    (n, c, q)
}

/// P1 parser variant, one byte (all variants produce identical results; they differ in speed):
///   bits 0-1  mask kernel: 0 = AVX2, 1 = AVX-512 ymm asm, 2 = AVX-512 intrinsics (zmm)
///   bit 2     fast path for lines without any quote
///   bit 3     field boundaries via pdep (independent) instead of a serial 128-bit chain
/// Kernel default: 0 (AVX2), SS_V4MODE overrides; AVX-512 kernels only where supported.
/// SS_P1FLAGS sets bits 2-3 (diag A/B on the scorer).
pub const P1_DEFAULT_KERNEL: u8 = 0;
pub const P1_DEFAULT_FLAGS: u8 = 0;
pub fn v4_mode() -> u8 {
    let have = std::is_x86_feature_detected!("avx512bw") && std::is_x86_feature_detected!("avx512vl");
    let want = std::env::var("SS_V4MODE").ok().and_then(|v| v.parse::<u8>().ok()).unwrap_or(P1_DEFAULT_KERNEL);
    let kernel = if have { want.min(2) } else { 0 };
    let flags = std::env::var("SS_P1FLAGS").ok().and_then(|v| v.parse::<u8>().ok()).unwrap_or(P1_DEFAULT_FLAGS) & 12;
    kernel | flags
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
    split_at2g::<N, 0>(data, p, out)
}

/// split_at2 with mask kernel V (see v4_mode); V > 0 only from code compiled with
/// avx512bw+avx512vl, after checking v4_mode().
#[inline(always)]
pub fn split_at2g<const N: usize, const V: u8>(data: &'static [u8], p: usize, out: &mut [&'static [u8]; N]) -> (usize, bool) {
    if p + 128 <= data.len() {
        if PREFETCH > 0 {
            unsafe { _mm_prefetch(data.as_ptr().wrapping_add(p + PREFETCH) as *const i8, _MM_HINT_T0) };
        }
        let base = unsafe { data.as_ptr().add(p) };
        let (n0, c0, q0) = unsafe { match V & 3 { 0 => masks64(base), 1 => masks64_v4y(base), _ => masks64_v4(base) } };
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
            let (n1, c1, q1) = unsafe { match V & 3 { 0 => masks64(base.add(64)), 1 => masks64_v4y(base.add(64)), _ => masks64_v4(base.add(64)) } };
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
            if V & 4 != 0 && q == 0 {
                // no quote anywhere: valid, and every field is just the bytes between commas
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
                    out[k] = unsafe { std::slice::from_raw_parts(base.add(a), b - a) };
                    a = b + 1;
                }
                return (next, true);
            }
            // len >= N-1 >= 1 here
            let s = (c << 1) | 1;
            let e = (c >> 1) | (1u128 << (len - 1));
            let lo = pxor64(q as u64);
            let hi = pxor64((q >> 64) as u64) ^ 0u64.wrapping_sub(lo >> 63);
            let inside = lo as u128 | ((hi as u128) << 64);
            let valid = (q & !(s | e)) == 0 && q.count_ones() & 1 == 0 && (inside & c) == 0;
            let mut cc = c;
            let (c0, c1) = (c as u64, (c >> 64) as u64);
            let nc0 = c0.count_ones();
            let mut a = 0usize;
            for k in 0..N {
                let b = if k + 1 < N {
                    if V & 8 != 0 {
                        let p0 = unsafe { _pdep_u64(1u64 << k, c0) }.trailing_zeros();
                        let k1 = (k as u32).wrapping_sub(nc0) & 63;
                        let p1 = 64 + unsafe { _pdep_u64(1u64 << k1, c1) }.trailing_zeros();
                        (if (k as u32) < nc0 { p0 } else { p1 }) as usize
                    } else {
                        let t = cc.trailing_zeros() as usize;
                        cc &= cc.wrapping_sub(1);
                        t
                    }
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

/// `split_at2` with the field boundaries found independently (pdep selects the k-th comma of
/// each 64-bit half), instead of one serial 128-bit clear-lowest-bit chain.
#[cfg_attr(not(feature = "prof"), inline(always))]
#[cfg_attr(feature = "prof", inline(never))]
pub fn split_at3<const N: usize>(data: &'static [u8], p: usize, out: &mut [&'static [u8]; N]) -> (usize, bool) {
    if p + 128 <= data.len() {
        if PREFETCH > 0 {
            unsafe { _mm_prefetch(data.as_ptr().wrapping_add(p + PREFETCH) as *const i8, _MM_HINT_T0) };
        }
        let base = unsafe { data.as_ptr().add(p) };
        let (n0, c0x, q0x) = unsafe { masks64(base) };
        let found;
        let (len, c0, c1, q0, q1): (usize, u64, u64, u64, u64);
        if n0 != 0 {
            let l = n0.trailing_zeros() as usize;
            let m = (1u64 << l) - 1;
            len = l;
            (c0, c1, q0, q1) = (c0x & m, 0, q0x & m, 0);
            found = true;
        } else {
            let (n1, c1x, q1x) = unsafe { masks64(base.add(64)) };
            let l = n1.trailing_zeros() as usize; // 64 if none
            let m = if l == 64 { 0 } else { (1u64 << l) - 1 };
            len = 64 + l;
            (c0, c1, q0, q1) = (c0x, c1x & m, q0x, q1x & m);
            found = n1 != 0;
        }
        if found {
            let next = p + len + 1;
            let nc0 = c0.count_ones();
            if (nc0 + c1.count_ones()) as usize != N - 1 {
                return (next, false);
            }
            let c = c0 as u128 | ((c1 as u128) << 64);
            let q = q0 as u128 | ((q1 as u128) << 64);
            let s = (c << 1) | 1;
            let e = (c >> 1) | (1u128 << (len - 1));
            let lo = pxor64(q0);
            let hi = pxor64(q1) ^ 0u64.wrapping_sub(lo >> 63);
            let inside = lo as u128 | ((hi as u128) << 64);
            let valid = (q & !(s | e)) == 0 && (q0.count_ones() + q1.count_ones()) & 1 == 0 && (inside & c) == 0;
            let mut a = 0usize;
            for k in 0..N {
                let b = if k + 1 < N {
                    let p0 = unsafe { _pdep_u64(1u64 << k, c0) }.trailing_zeros();
                    let k1 = (k as u32).wrapping_sub(nc0) & 63;
                    let p1 = 64 + unsafe { _pdep_u64(1u64 << k1, c1) }.trailing_zeros();
                    (if (k as u32) < nc0 { p0 } else { p1 }) as usize
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
