#![allow(dead_code)]
// ---------- timestamps ----------
#[inline(always)]
pub fn dg(c: u8) -> Option<i64> {
    if c.is_ascii_digit() { Some((c - b'0') as i64) } else { None }
}
#[inline(always)]
pub fn d2(s: &[u8], i: usize) -> Option<i64> {
    Some(dg(s[i])? * 10 + dg(s[i + 1])?)
}
#[inline(always)]
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
#[inline(always)]
pub fn parse_dt(s: &[u8]) -> Option<i64> {
    // s.len() >= 19, checks YYYY-MM-DDTHH:MM:SS
    let y = d2(s, 0)? * 100 + d2(s, 2)?;
    if s[4] != b'-' || s[7] != b'-' || s[10] != b'T' || s[13] != b':' || s[16] != b':' {
        return None;
    }
    let mo = d2(s, 5)?;
    let d = d2(s, 8)?;
    let h = d2(s, 11)?;
    let mi = d2(s, 14)?;
    let se = d2(s, 17)?;
    if !(1..=12).contains(&mo) || d < 1 || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let dim = match mo {
        2 => if leap { 29 } else { 28 },
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if d > dim {
        return None;
    }
    Some((((days_from_civil(y, mo, d) * 24 + h) * 60 + mi) * 60 + se) * 1000)
}
pub fn parse_ts(s: &[u8]) -> Option<i64> {
    let n = s.len();
    if n == 0 {
        return None;
    }
    if n >= 5 && s[4] == b'-' {
        if n == 20 {
            if s[19] != b'Z' {
                return None;
            }
            return parse_dt(s);
        } else if n == 25 {
            let sign = match s[19] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let oh = d2(s, 20)?;
            let om = d2(s, 23)?;
            if s[22] != b':' || oh > 23 || om > 59 {
                return None;
            }
            return Some(parse_dt(s)? - sign * (oh * 60 + om) * 60_000);
        }
        return None;
    }
    // F3: digits only
    let mut i = 0;
    while i < n && s[i] == b'0' {
        i += 1;
    }
    let mut v: u64 = 0;
    let sig = n - i;
    for &c in &s[i..] {
        if !c.is_ascii_digit() {
            return None;
        }
        if sig <= 19 {
            v = v * 10 + (c - b'0') as u64;
        }
    }
    if sig > 19 || v > i64::MAX as u64 {
        return Some(i64::MAX); // valid form, outside any window
    }
    Some(v as i64)
}
#[cfg_attr(not(feature = "prof"), inline(always))]
#[cfg_attr(feature = "prof", inline(never))]
pub fn parse_price(s: &[u8]) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut i = 0;
    while i < s.len() && s[i] == b'0' {
        i += 1;
    }
    let mut v: u64 = 0;
    for &c in &s[i..] {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (c - b'0') as u64;
        if v > 10_000_000 {
            // keep scanning for non-digits is unnecessary: any non-digit or too big -> invalid
            return None;
        }
    }
    if v >= 1 { Some(v as u32) } else { None }
}


// ---------- SWAR fast path for "YYYY-MM-DDTHH:MM:SS" (first 19 bytes; caller guarantees len >= 20) ----------
const DIG_A: u64 = 0x00FF_FF00_FFFF_FFFF; // bytes 0-3,5,6 digits (little endian: byte i at bits 8i)
const DIG_B: u64 = 0xFFFF_00FF_FF00_FFFF; // bytes 8,9,11,12,14,15
const SEP_A_MASK: u64 = 0xFF00_00FF_0000_0000; // bytes 4 and 7
const SEP_A: u64 = ((b'-' as u64) << 32) | ((b'-' as u64) << 56);
const SEP_B_MASK: u64 = 0x0000_FF00_00FF_0000; // bytes 10, 13 (positions 2,5 in b)
const SEP_B: u64 = ((b'T' as u64) << 16) | ((b':' as u64) << 40);

#[inline(always)]
fn bad_digits(t: u64, mask: u64) -> bool {
    ((t.wrapping_add(0x7676_7676_7676_7676) | t) & 0x8080_8080_8080_8080 & (mask & 0x8080_8080_8080_8080)) != 0
}
#[inline(always)]
fn byte(t: u64, i: u32) -> i64 {
    ((t >> (8 * i)) & 0xFF) as i64
}

/// Days since epoch of the first of each month, for years [MT_BASE, MT_BASE + MT_YEARS).
const MT_YEARS: usize = 64;
static mut MT_BASE: i64 = 2000;
static mut MONTH_START: [i64; MT_YEARS * 12] = [0; MT_YEARS * 12];
static mut MONTH_START_MS: [i64; MT_YEARS * 12] = [0; MT_YEARS * 12];

/// Center the month table on the dataset's window (call once, before parsing).
#[allow(static_mut_refs)]
pub fn init_month_table(year: i64) {
    unsafe {
        MT_BASE = year - (MT_YEARS as i64) / 2;
        for i in 0..MT_YEARS * 12 {
            let y = MT_BASE + (i / 12) as i64;
            MONTH_START[i] = days_from_civil(y, (i % 12) as i64 + 1, 1);
            MONTH_START_MS[i] = MONTH_START[i] * 86_400_000;
        }
    }
}

#[inline(always)]
#[allow(static_mut_refs)]
fn month_start_ms(y: i64, mo: i64) -> i64 {
    unsafe {
        let i = y - MT_BASE;
        if (i as u64) < MT_YEARS as u64 {
            *MONTH_START_MS.get_unchecked(i as usize * 12 + mo as usize - 1)
        } else {
            days_from_civil(y, mo, 1) * 86_400_000
        }
    }
}

#[inline(always)]
#[allow(static_mut_refs)]
#[allow(dead_code)]
fn month_start(y: i64, mo: i64) -> i64 {
    unsafe {
        let i = y - MT_BASE;
        if (i as u64) < MT_YEARS as u64 {
            *MONTH_START.get_unchecked(i as usize * 12 + mo as usize - 1)
        } else {
            days_from_civil(y, mo, 1)
        }
    }
}

/// Convert digit bytes (already XORed with 0x30, separators masked to 0) into pair values:
/// byte i of the result = 10 * byte_i + byte_{i+1}.
#[inline(always)]
fn pairs(t: u64) -> u64 {
    t.wrapping_mul(10).wrapping_add(t >> 8)
}

/// Milliseconds for "YYYY-MM-DDTHH:MM:SS" at the start of s (s.len() >= 20). None if invalid.
#[inline(always)]
pub fn parse_dt_fast(s: &[u8]) -> Option<i64> {
    let a = u64::from_le_bytes(s[0..8].try_into().unwrap());
    let b = u64::from_le_bytes(s[8..16].try_into().unwrap());
    let c = u32::from_le_bytes(s[16..20].try_into().unwrap()) as u64;
    if (a & SEP_A_MASK) != SEP_A || (b & SEP_B_MASK) != SEP_B || (c & 0xFF) != b':' as u64 {
        return None;
    }
    let ta = a ^ 0x3030_3030_3030_3030;
    let tb = b ^ 0x3030_3030_3030_3030;
    let tc = c ^ 0x3030_3030;
    if bad_digits(ta, DIG_A) || bad_digits(tb, DIG_B) || bad_digits(tc, 0x00FF_FF00) {
        return None;
    }
    let pa = pairs(ta & DIG_A);
    let pb = pairs(tb & DIG_B);
    let pc = pairs(tc & 0x00FF_FF00);
    let y = byte(pa, 0) * 100 + byte(pa, 2);
    let mo = byte(pa, 5);
    let d = byte(pb, 0);
    let h = byte(pb, 3);
    let mi = byte(pb, 6);
    let se = byte(pc, 1);
    if mo.wrapping_sub(1) as u64 > 11 || d < 1 || h > 23 || mi > 59 || se > 59 {
        return None;
    }
    if d > 28 {
        let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
        let dim = match mo {
            2 => if leap { 29 } else { 28 },
            4 | 6 | 9 | 11 => 30,
            _ => 31,
        };
        if d > dim {
            return None;
        }
    }
    Some(month_start_ms(y, mo) + (d - 1) * 86_400_000 + h * 3_600_000 + mi * 60_000 + se * 1000)
}

/// Eight ASCII digits (already minus '0', most significant in the lowest byte) to their value.
#[inline(always)]
fn parse8(chunk: u64) -> u64 {
    let lower = (chunk & 0x0f00_0f00_0f00_0f00) >> 8;
    let upper = (chunk & 0x000f_000f_000f_000f) * 10;
    let chunk = lower + upper;
    let lower = (chunk & 0x00ff_0000_00ff_0000) >> 16;
    let upper = (chunk & 0x0000_00ff_0000_00ff) * 100;
    let chunk = lower + upper;
    let lower = (chunk & 0x0000_ffff_0000_0000) >> 32;
    let upper = (chunk & 0x0000_0000_0000_ffff) * 10000;
    lower + upper
}

#[cfg_attr(not(feature = "prof"), inline(always))]
#[cfg_attr(feature = "prof", inline(never))]
pub fn parse_ts_fast(s: &[u8]) -> Option<i64> {
    let n = s.len();
    if n == 20 {
        if s[19] == b'Z' {
            return parse_dt_fast(s);
        }
    } else if n == 25 {
        let sign = match s[19] {
            b'+' => 1,
            b'-' => -1,
            _ => return parse_ts_slow_f3(s),
        };
        let oh = d2(s, 20)?;
        let om = d2(s, 23)?;
        if s[22] != b':' || oh > 23 || om > 59 {
            return None;
        }
        return Some(parse_dt_fast(s)? - sign * (oh * 60 + om) * 60_000);
    }
    parse_ts_slow_f3(s)
}

const POW10: [u64; 9] = [1, 10, 100, 1_000, 10_000, 100_000, 1_000_000, 10_000_000, 100_000_000];

/// F3 (digits only) or invalid.
#[inline(always)]
fn parse_ts_slow_f3(s: &[u8]) -> Option<i64> {
    let n = s.len();
    if n == 0 {
        return None;
    }
    if n >= 8 && n <= 16 {
        // two overlapping 8-byte loads straight from the input (no stack buffer, so no
        // store-forwarding stall); bytes of the second word already covered by the first become '0'
        let a = u64::from_le_bytes(s[0..8].try_into().unwrap()) ^ 0x3030_3030_3030_3030;
        let w = u64::from_le_bytes(s[n - 8..n].try_into().unwrap());
        let keep = if n == 8 { 0 } else { u64::MAX << (8 * (16 - n)) };
        let b = (w & keep) ^ 0x3030_3030_3030_3030 ^ (0x3030_3030_3030_3030 & !keep);
        if ((a.wrapping_add(0x7676_7676_7676_7676) | a) | (b.wrapping_add(0x7676_7676_7676_7676) | b)) & 0x8080_8080_8080_8080 != 0 {
            return None;
        }
        return Some((parse8(a) * POW10[n - 8] + parse8(b)) as i64);
    }
    if n <= 16 {
        // right-align into 16 bytes padded with '0', validate all digits, convert 8 at a time
        let mut buf = [b'0'; 16];
        buf[16 - n..].copy_from_slice(s);
        let a = u64::from_le_bytes(buf[0..8].try_into().unwrap()) ^ 0x3030_3030_3030_3030;
        let b = u64::from_le_bytes(buf[8..16].try_into().unwrap()) ^ 0x3030_3030_3030_3030;
        if ((a.wrapping_add(0x7676_7676_7676_7676) | a) | (b.wrapping_add(0x7676_7676_7676_7676) | b)) & 0x8080_8080_8080_8080 != 0 {
            return None;
        }
        return Some((parse8(a) * 100_000_000 + parse8(b)) as i64);
    }
    let mut i = 0;
    while i < n && s[i] == b'0' {
        i += 1;
    }
    let sig = n - i;
    let mut v: u64 = 0;
    for &c in &s[i..] {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v.wrapping_mul(10).wrapping_add((c - b'0') as u64);
    }
    if sig > 19 || v > i64::MAX as u64 {
        return Some(i64::MAX);
    }
    Some(v as i64)
}

/// ingest_ts is always valid F1 (rules.md): fast path without digit/range validation.
/// Separators are still checked; anything else falls back to the full parser.
#[cfg_attr(not(feature = "prof"), inline(always))]
#[cfg_attr(feature = "prof", inline(never))]
pub fn parse_ingest(s: &[u8]) -> Option<i64> {
    if s.len() == 20 {
        let a = u64::from_le_bytes(s[0..8].try_into().unwrap());
        let b = u64::from_le_bytes(s[8..16].try_into().unwrap());
        let c = u32::from_le_bytes(s[16..20].try_into().unwrap()) as u64;
        if (a & SEP_A_MASK) == SEP_A && (b & SEP_B_MASK) == SEP_B && (c & 0xFF_0000_FF) == ((b'Z' as u64) << 24 | b':' as u64) {
            let pa = pairs((a ^ 0x3030_3030_3030_3030) & DIG_A);
            let pb = pairs((b ^ 0x3030_3030_3030_3030) & DIG_B);
            let pc = pairs((c ^ 0x3030_3030) & 0x00FF_FF00);
            let y = byte(pa, 0) * 100 + byte(pa, 2);
            let mo = byte(pa, 5);
            if mo.wrapping_sub(1) as u64 <= 11 {
                return Some(month_start_ms(y, mo) + (byte(pb, 0) - 1) * 86_400_000 + byte(pb, 3) * 3_600_000 + byte(pb, 6) * 60_000 + byte(pc, 1) * 1000);
            }
        }
    }
    parse_ts_fast(s)
}
