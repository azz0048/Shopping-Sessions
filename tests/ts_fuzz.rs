// Differential fuzz: fast timestamp parser vs the reference one.
#[path = "../src/ts.rs"]
#[allow(dead_code, unused_imports)]
mod ts;
use ts::*;

fn rng(s: &mut u64) -> u64 { *s ^= *s << 13; *s ^= *s >> 7; *s ^= *s << 17; *s }

#[test]
fn fuzz() {
    init_month_table(2026);
    let mut st = 0x1234_5678_9abc_def1u64;
    let alphabet = b"0123456789-:TZ+tz .\r9";
    let bases: Vec<&[u8]> = vec![
        b"2026-10-03T23:58:10Z", b"2026-10-03T19:58:10-04:00", b"2028-02-29T00:00:00Z", b"2027-02-29T00:00:00Z",
        b"1791071890000", b"0001791071890000", b"2026-12-31T23:59:59+23:59", b"2000-02-29T12:00:00-00:00",
        b"1900-02-29T12:00:00Z", b"2026-04-31T00:00:00Z", b"9999-12-31T23:59:59Z", b"0000-01-01T00:00:00Z",
    ];
    let mut n = 0;
    for _ in 0..3_000_000 {
        let base = bases[(rng(&mut st) % bases.len() as u64) as usize];
        let mut s = base.to_vec();
        let muts = rng(&mut st) % 4;
        for _ in 0..muts {
            let i = (rng(&mut st) % s.len() as u64) as usize;
            match rng(&mut st) % 4 {
                0 | 1 => s[i] = alphabet[(rng(&mut st) % alphabet.len() as u64) as usize],
                2 => s[i] = (rng(&mut st) & 0xFF) as u8,
                _ => { if s.len() > 1 { s.remove(i); } }
            }
        }
        let a = parse_ts(&s);
        let b = parse_ts_fast(&s);
        assert_eq!(a, b, "mismatch on {:?}", String::from_utf8_lossy(&s));
        n += 1;
    }
    // exhaustive-ish dates
    for y in [1999i64, 2000, 2024, 2025, 2026, 2027, 2028, 2100] {
        for m in 0..=13 { for d in 0..=32 {
            let s = format!("{y:04}-{m:02}-{d:02}T10:20:30Z");
            assert_eq!(parse_ts(s.as_bytes()), parse_ts_fast(s.as_bytes()), "{s}");
            if parse_ts(s.as_bytes()).is_some() {
                assert_eq!(parse_ts(s.as_bytes()), parse_ingest(s.as_bytes()), "ingest {s}");
            }
        }}
    }
    eprintln!("{n} ok");
}

#[test]
fn fuzz_f3() {
    init_month_table(2026);
    let mut st = 0xdead_beef_1234_5678u64;
    for _ in 0..2_000_000 {
        let n = (rng(&mut st) % 25) as usize + 1;
        let mut v: Vec<u8> = (0..n).map(|_| b'0' + (rng(&mut st) % 10) as u8).collect();
        match rng(&mut st) % 6 {
            0 => { let i = (rng(&mut st) % n as u64) as usize; v[i] = [b'.', b'-', b'e', b' ', b'Z', 0xd9][(rng(&mut st) % 6) as usize]; }
            1 => { for x in v.iter_mut().take((rng(&mut st) % n as u64) as usize) { *x = b'0'; } }
            2 => { v = b"9223372036854775807".to_vec(); if rng(&mut st) % 2 == 0 { v = b"9223372036854775808".to_vec(); } }
            _ => {}
        }
        assert_eq!(parse_ts(&v), parse_ts_fast(&v), "{:?}", String::from_utf8_lossy(&v));
    }
    for s in ["0", "00000000000000000", "1", "9999999999999999", "99999999999999999", "0000000000000000001791071890000", "\u{0662}\u{0665}"] {
        assert_eq!(parse_ts(s.as_bytes()), parse_ts_fast(s.as_bytes()), "{s}");
    }
}
