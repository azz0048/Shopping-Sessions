// Differential fuzz: SIMD split_at vs the scalar reference split.
#[path = "../src/csv.rs"]
#[allow(dead_code, unused_imports)]
mod csv;

fn rng(s: &mut u64) -> u64 { *s ^= *s << 13; *s ^= *s >> 7; *s ^= *s << 17; *s }

fn check<const N: usize>(seed: u64, iters: usize) {
    let mut st = seed;
    let alphabet: &[u8] = b"abcXYZ019 _-:\r\t";
    let mut buf: Vec<u8> = Vec::new();
    let mut starts = Vec::new();
    for _ in 0..iters {
        starts.push(buf.len());
        let nf = if rng(&mut st) % 10 == 0 { N + (rng(&mut st) % 3) as usize - 1 } else { N }; // mostly N fields
        let long = rng(&mut st) % 50 == 0;
        for f in 0..nf {
            if f > 0 { buf.push(b','); }
            let flen = if long { (rng(&mut st) % 60) as usize } else if N == 7 { (rng(&mut st) % 9) as usize } else { (rng(&mut st) % 14) as usize };
            let quote = rng(&mut st) % 4 == 0;
            if quote { buf.push(b'"'); }
            for _ in 0..flen {
                let r = rng(&mut st) % 100;
                let c = if r < 1 { b'"' } else if r < 2 { b',' } else { alphabet[(rng(&mut st) % alphabet.len() as u64) as usize] };
                
                buf.push(c);
            }
            if quote && rng(&mut st) % 30 != 0 { buf.push(b'"'); }
        }
        if rng(&mut st) % 200 == 0 { buf.truncate(*starts.last().unwrap()); } // empty line
        buf.push(b'\n');
    }
    let data: &'static [u8] = Box::leak(buf.into_boxed_slice());
    let mut p = 0;
    let mut i = 0;
    let (mut okc, mut badc) = (0, 0);
    while p < data.len() {
        assert_eq!(p, starts[i]);
        let mut f1: [&'static [u8]; N] = [&[]; N];
        let mut f2: [&'static [u8]; N] = [&[]; N];
        let (np, ok1) = csv::split_at::<N>(data, p, &mut f1);
        let q = p + memchr::memchr(b'\n', &data[p..]).unwrap();
        let ok2 = csv::split::<N>(&data[p..q], &mut f2);
        assert_eq!(np, q + 1);
        assert_eq!(ok1, ok2, "validity differs on {:?}", String::from_utf8_lossy(&data[p..q]));
        if ok1 {
            assert_eq!(f1, f2, "fields differ on {:?}", String::from_utf8_lossy(&data[p..q]));
            okc += 1;
        } else {
            badc += 1;
        }
        p = np;
        i += 1;
    }
    eprintln!("N={N}: {okc} valid, {badc} malformed lines agree");
}

#[test]
fn fuzz_split() {
    check::<7>(0x9e3779b97f4a7c15, 400_000);
    check::<4>(0x1234567, 400_000);
    check::<2>(0x7654321, 400_000);
}
