#![allow(unsafe_op_in_unsafe_fn)]
// Shopping Sessions pipeline. See rules.md.
//
// Phases (all parallel over NCPU threads):
//   L  links: parse, intern IDs per hash bucket, union-find, name = byte-smallest ID
//   P1 parse events (-> buckets by event_id hash) and clicks (-> buckets by user hash)
//   P2 dedup each event_id bucket (smallest ingest_ts wins, then validity) -> buckets by user hash
//   P3 per user bucket: map user -> person with that bucket's (cache-resident) link table -> person buckets
//   P4 per person bucket: sort, sessionize, attribute, aggregate; write user_daily parts
mod arena;
mod csv;
mod files;
mod hash;
mod out;
mod pq;
mod ts;

pub use arena::*;
pub use csv::*;
pub use files::*;
pub use hash::*;
pub use out::*;
pub use ts::*;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::*};
use std::time::Instant;


pub const DAY: i64 = 86_400_000;
pub const GAP: i64 = 1_800_000;
pub const WEEK: i64 = 604_800_000;
pub const LATE: i64 = 86_400_000;

const EB_BITS: u32 = 10; // event-id buckets
const EB: usize = 1 << EB_BITS;
const UB_BITS: u32 = 10; // user / person buckets (same bits for both)
const UB: usize = 1 << UB_BITS;

// ---------- string refs (pointer into leaked mmaps + length) ----------
const PTR_MASK: u64 = (1u64 << 47) - 1;
#[inline(always)]
pub fn pack(s: &[u8]) -> u64 {
    (s.as_ptr() as u64) | ((s.len() as u64) << 47)
}
#[inline(always)]
pub fn unpack(r: u64) -> &'static [u8] {
    unsafe { std::slice::from_raw_parts((r & PTR_MASK) as *const u8, (r >> 47) as usize) }
}

// ---------- small hash table (u64 key -> u32 value) ----------
pub struct SmallMap {
    keys: Vec<u64>,
    vals: Vec<u32>,
    mask: usize,
}
impl SmallMap {
    pub fn new(n: usize) -> Self {
        let cap = (n * 2 + 16).next_power_of_two();
        SmallMap { keys: vec![0; cap], vals: vec![0; cap], mask: cap - 1 }
    }
    /// Inserts if absent; returns the stored value.
    #[inline(always)]
    pub fn insert(&mut self, h: u64, v: u32) -> u32 {
        let mut s = (h as usize) & self.mask;
        loop {
            let k = unsafe { *self.keys.get_unchecked(s) };
            if k == 0 {
                self.keys[s] = h;
                self.vals[s] = v;
                return v;
            }
            if k == h {
                return self.vals[s];
            }
            s = (s + 1) & self.mask;
        }
    }
    /// Clear and size for up to n keys.
    pub fn reset(&mut self, n: usize) {
        let cap = (n * 2 + 16).next_power_of_two();
        self.keys.clear();
        self.keys.resize(cap, 0);
        if self.vals.len() < cap {
            self.vals.resize(cap, 0);
        }
        self.mask = cap - 1;
    }
    #[inline(always)]
    pub fn get(&self, h: u64) -> Option<u32> {
        let mut s = (h as usize) & self.mask;
        loop {
            let k = unsafe { *self.keys.get_unchecked(s) };
            if k == h {
                return Some(unsafe { *self.vals.get_unchecked(s) });
            }
            if k == 0 {
                return None;
            }
            s = (s + 1) & self.mask;
        }
    }
}

/// Exact string -> index lookup. Fast path when every key is PREFIX + canonical decimal
/// (no leading zeros): then a string maps to a key iff it has that exact form.
pub struct KeyMap {
    prefix: Vec<u8>,
    arr: Vec<u32>,
    fast: bool,
    map: SmallMap,
}
impl KeyMap {
    pub fn build(keys: &[(&'static [u8], u32)]) -> KeyMap {
        let mut km = KeyMap { prefix: vec![], arr: vec![], fast: false, map: SmallMap::new(keys.len()) };
        for &(k, v) in keys {
            km.map.insert(hash1(k), v);
        }
        if let Some(&(k0, _)) = keys.first() {
            let pl = k0.iter().position(|c| c.is_ascii_digit()).unwrap_or(k0.len());
            let prefix = k0[..pl].to_vec();
            let mut ok = true;
            let mut maxv = 0u64;
            let mut vals = Vec::with_capacity(keys.len());
            for &(k, v) in keys {
                match canon_num(k, &prefix) {
                    Some(n) if n < (1 << 24) => {
                        maxv = maxv.max(n);
                        vals.push((n, v));
                    }
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                let mut arr = vec![u32::MAX; maxv as usize + 1];
                for (n, v) in vals {
                    if arr[n as usize] == u32::MAX {
                        arr[n as usize] = v;
                    }
                }
                km.prefix = prefix;
                km.arr = arr;
                km.fast = true;
            }
        }
        km
    }
    #[inline(always)]
    pub fn get(&self, s: &[u8]) -> Option<u32> {
        if self.fast {
            let n = canon_num(s, &self.prefix)?;
            let v = *self.arr.get(n as usize)?;
            if v == u32::MAX { None } else { Some(v) }
        } else {
            self.map.get(hash1(s))
        }
    }
}
#[inline(always)]
fn canon_num(s: &[u8], prefix: &[u8]) -> Option<u64> {
    let pl = prefix.len();
    if s.len() <= pl || s.len() > pl + 9 || &s[..pl] != prefix {
        return None;
    }
    let d = &s[pl..];
    if d[0] == b'0' && d.len() > 1 {
        return None;
    }
    let mut v = 0u64;
    for &c in d {
        if !c.is_ascii_digit() {
            return None;
        }
        v = v * 10 + (c - b'0') as u64;
    }
    Some(v)
}

// ---------- records ----------
/// Dedup key, scattered by event_id hash. gid = thread << GID_BITS | per-thread event number.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Key {
    h1: u64,
    h2: u32,
    ingest: i32, // seconds since window start (saturated)
    gid: u32,
}
/// A valid event copy, scattered by user hash.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Rec {
    user: u64,
    uref: u64,
    ts: u32, // ms since window start
    pc: u32, // purchase price (low 24 bits, 0 if not a purchase) | category << 24
    gid: u32,
}
/// A surviving event, scattered by person key.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Ev {
    pub person: u64,
    pub name: u64,
    pub ts: u32,
    pub pc: u32,
}
/// A valid click. ts is ms since window start - 7 days.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Ck {
    pub person: u64,
    pub cid: u64,
    pub ts: u32,
    pub camp: u32,
}
const GID_BITS: u32 = 27;

#[inline(always)]
fn eb_of(h: u64) -> usize {
    (h >> (64 - EB_BITS)) as usize
}
#[inline(always)]
pub fn ub_of(h: u64) -> usize {
    (h >> (64 - UB_BITS)) as usize
}

pub struct Chunk {
    pub data: &'static [u8],
    pub start: usize,
    pub end: usize,
    pub kind: u8, // 0 events, 1 clicks, 2 links
}

struct Ctx {
    ws: i64,
    we: i64,
    prod: KeyMap,
    unknown_cat: u16,
    camp: KeyMap,
}

struct ParseOut {
    keys: Vec<Bucket<Key>>,
    recs: Vec<Bucket<Rec>>,
    ck: Vec<Bucket<Ck>>,
    nev: u32,
    tid: u32,
}

fn parse_events_chunk(c: &Chunk, ctx: &Ctx, out: &mut ParseOut) {
    let ws = ctx.ws;
    let we = ctx.we;
    let d = c.data;
    let mut p = chunk_start(d, c.start, c.end);
    let mut f: [&'static [u8]; 7] = [&[]; 7];
    while p < c.end {
        let (np, ok) = split_at::<7>(d, p, &mut f);
        p = np;
        if !ok {
            continue;
        }
        let eid = f[0];
        if eid.is_empty() {
            continue;
        }
        let (h1, h2) = hash_wide(eid);
        let ingest = parse_ts_fast(f[3]).unwrap_or(i64::MAX);
        let gid = (out.tid << GID_BITS) | out.nev;
        out.nev += 1;
        let ing_rel = ((ingest.saturating_sub(ws)) / 1000).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        out.keys[eb_of(h1)].push(Key { h1, h2, ingest: ing_rel, gid });
        // validity of this copy
        let user = f[1];
        if user.is_empty() || f[5].is_empty() {
            continue;
        }
        let ts = match parse_ts_fast(f[2]) {
            Some(t) => t,
            None => continue,
        };
        if ts < ws || ts >= we || ingest.saturating_sub(ts) > LATE {
            continue;
        }
        let pc = match f[4] {
            b"view" | b"cart" => 0u32,
            b"purchase" => {
                let price = match parse_price(f[6]) {
                    Some(p) => p,
                    None => continue,
                };
                let cat = match ctx.prod.get(f[5]) {
                    Some(c) => c,
                    None => ctx.unknown_cat as u32,
                };
                price | (cat << 24)
            }
            _ => continue,
        };
        let uh = hash1(user);
        out.recs[ub_of(uh)].push(Rec { user: uh, uref: pack(user), ts: (ts - ws) as u32, pc, gid });
    }
}

fn parse_clicks_chunk(c: &Chunk, ctx: &Ctx, out: &mut ParseOut) {
    let lo = ctx.ws - WEEK;
    let hi = ctx.we;
    let d = c.data;
    let mut p = chunk_start(d, c.start, c.end);
    let mut f: [&'static [u8]; 4] = [&[]; 4];
    while p < c.end {
        let (np, ok) = split_at::<4>(d, p, &mut f);
        p = np;
        if !ok || f[0].is_empty() || f[1].is_empty() {
            continue;
        }
        let ts = match parse_ts_fast(f[3]) {
            Some(t) => t,
            None => continue,
        };
        if ts < lo || ts >= hi {
            continue;
        }
        let camp = match ctx.camp.get(f[2]) {
            Some(c) => c,
            None => continue,
        };
        let u = hash1(f[1]);
        out.ck[ub_of(u)].push(Ck { person: u, cid: pack(f[0]), ts: (ts - lo) as u32, camp });
    }
}

// ---------- links ----------
struct LinkParse {
    sides: Vec<Vec<(u64, u64, u32)>>, // per bucket: (hash, ref, local side index)
    npairs: usize,
}

fn parse_links_chunk(c: &Chunk, out: &mut LinkParse) {
    let d = c.data;
    let mut p = chunk_start(d, c.start, c.end);
    let mut f: [&'static [u8]; 2] = [&[]; 2];
    while p < c.end {
        let (np, ok) = split_at::<2>(d, p, &mut f);
        p = np;
        if !ok || f[0].is_empty() || f[1].is_empty() {
            continue;
        }
        let i = out.npairs as u32;
        out.npairs += 1;
        let ha = hash1(f[0]);
        let hb = hash1(f[1]);
        out.sides[ub_of(ha)].push((ha, pack(f[0]), 2 * i));
        out.sides[ub_of(hb)].push((hb, pack(f[1]), 2 * i + 1));
    }
}

pub struct Links {
    tables: Vec<SmallMap>,
    base: Vec<u32>,
    pkey: Vec<u64>,
    pref: Vec<u64>,
}
impl Links {
    #[inline(always)]
    fn map(&self, b: usize, h: u64, r: u64) -> (u64, u64) {
        match self.tables[b].get(h) {
            Some(l) => {
                let i = self.base[b] as usize + l as usize;
                unsafe { (*self.pkey.get_unchecked(i), *self.pref.get_unchecked(i)) }
            }
            None => (h, r),
        }
    }
}

fn find(parent: &mut [u32], mut x: u32) -> u32 {
    unsafe {
        while *parent.get_unchecked(x as usize) != x {
            let p = *parent.get_unchecked(x as usize);
            let gp = *parent.get_unchecked(p as usize);
            *parent.get_unchecked_mut(x as usize) = gp;
            x = gp;
        }
    }
    x
}

struct SendPtr<T>(*mut T);
unsafe impl<T> Send for SendPtr<T> {}
unsafe impl<T> Sync for SendPtr<T> {}

fn run_threads<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    std::thread::scope(|sc| {
        let f = &f;
        let hs: Vec<_> = (0..n).map(|t| sc.spawn(move || f(t))).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

type BucketIntern = (SmallMap, Vec<u64>, Vec<u64>, Vec<(u32, u32)>);

fn build_links(lp: Vec<LinkParse>, nthreads: usize) -> Links {
    let tl = Instant::now();
    let verbose = std::env::var("SS_VERBOSE").is_ok();
    let lap = |w: &str| if verbose { eprintln!("      links {:>7.1} ms {w}", tl.elapsed().as_secs_f64()*1000.0) };
    let mut off = Vec::with_capacity(lp.len());
    let mut npairs = 0usize;
    for l in &lp {
        off.push(2 * npairs as u32);
        npairs += l.npairs;
    }
    // intern per bucket
    let next = AtomicUsize::new(0);
    let parts: Vec<Vec<(usize, BucketIntern)>> = run_threads(nthreads, |_| {
        let mut res = Vec::new();
        loop {
            let b = next.fetch_add(1, Relaxed);
            if b >= UB {
                break;
            }
            let n: usize = lp.iter().map(|l| l.sides[b].len()).sum();
            let mut t = SmallMap::new(n);
            let mut keys = Vec::new();
            let mut refs = Vec::new();
            let mut lids = Vec::with_capacity(n);
            for (ti, l) in lp.iter().enumerate() {
                for &(h, r, si) in &l.sides[b] {
                    let id = t.insert(h, keys.len() as u32);
                    if id as usize == keys.len() {
                        keys.push(h);
                        refs.push(r);
                    }
                    lids.push((off[ti] + si, id));
                }
            }
            res.push((b, (t, keys, refs, lids)));
        }
        res
    });
    lap("interned");
    let mut per: Vec<Option<BucketIntern>> = (0..UB).map(|_| None).collect();
    for v in parts {
        for (b, x) in v {
            per[b] = Some(x);
        }
    }
    let mut base = vec![0u32; UB];
    let mut n = 0usize;
    for b in 0..UB {
        base[b] = n as u32;
        n += per[b].as_ref().unwrap().1.len();
    }
    let mut pair_ids = vec![0u32; 2 * npairs];
    {
        let pp = SendPtr(pair_ids.as_mut_ptr());
        let per_ref = &per;
        let base = &base;
        let next = AtomicUsize::new(0);
        run_threads(nthreads, |_| {
            let pp = &pp;
            loop {
                let b = next.fetch_add(1, Relaxed);
                if b >= UB {
                    break;
                }
                let bb = base[b];
                for &(si, id) in &per_ref[b].as_ref().unwrap().3 {
                    unsafe { *pp.0.add(si as usize) = bb + id };
                }
            }
        });
    }
    let mut keys = Vec::with_capacity(n);
    let mut refs = Vec::with_capacity(n);
    let mut tables = Vec::with_capacity(UB);
    for p in per.into_iter() {
        let (t, k, r, _) = p.unwrap();
        keys.extend_from_slice(&k);
        refs.extend_from_slice(&r);
        tables.push(t);
    }
    lap("pair ids + concat");
    let mut parent: Vec<u32> = (0..n as u32).collect();
    for i in 0..npairs {
        let ra = find(&mut parent, pair_ids[2 * i]);
        let rb = find(&mut parent, pair_ids[2 * i + 1]);
        if ra != rb {
            if ra < rb {
                parent[rb as usize] = ra
            } else {
                parent[ra as usize] = rb
            }
        }
    }
    lap("union-find");
    let mut minid = vec![u32::MAX; n];
    let mut root = vec![0u32; n];
    for i in 0..n {
        let r = find(&mut parent, i as u32);
        root[i] = r;
        let m = minid[r as usize];
        if m == u32::MAX || unpack(refs[i]) < unpack(refs[m as usize]) {
            minid[r as usize] = i as u32;
        }
    }
    lap("min names");
    let mut pkey = vec![0u64; n];
    let mut pref = vec![0u64; n];
    for i in 0..n {
        let m = minid[root[i] as usize] as usize;
        pkey[i] = keys[m];
        pref[i] = refs[m];
    }
    lap("person arrays");
    if verbose {
        eprintln!("      links: {npairs} pairs, {n} ids");
    }
    Links { tables, base, pkey, pref }
}

fn main() {
    let t0 = Instant::now();
    let args: Vec<String> = std::env::args().collect();
    let data_dir = PathBuf::from(
        std::env::var("DATA_DIR").ok().or_else(|| args.get(1).cloned()).unwrap_or("/data".into()),
    );
    let out_dir = PathBuf::from(
        std::env::var("OUTPUT_DIR").ok().or_else(|| args.get(2).cloned()).unwrap_or("/output".into()),
    );
    let nthreads: usize = std::env::var("NCPU")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(8))
        .max(1);
    let verbose = std::env::var("SS_VERBOSE").is_ok();
    let lap = |what: &str| {
        if verbose {
            eprintln!("{:>8.1} ms  {}", t0.elapsed().as_secs_f64() * 1000.0, what);
        }
    };

    let mtext = std::fs::read_to_string(data_dir.join("manifest.json")).expect("manifest");
    let ws = manifest_ts(&mtext, "\"window_start\"");
    let we = manifest_ts(&mtext, "\"window_end\"");
    assert!(we - ws + WEEK < (1i64 << 32), "window too long");

    let ev_files: Vec<&'static [u8]> = list_csv(&data_dir.join("events")).iter().map(|p| map_file(p)).collect();
    let ck_files: Vec<&'static [u8]> = list_csv(&data_dir.join("clicks")).iter().map(|p| map_file(p)).collect();
    let lk_files: Vec<&'static [u8]> = list_csv(&data_dir.join("links")).iter().map(|p| map_file(p)).collect();

    // products & campaigns
    let prows = read_small_table(&data_dir.join("products.csv"));
    let mut cats: Vec<&'static [u8]> = prows.iter().map(|r| r[1]).collect();
    cats.push(b"unknown");
    cats.sort_unstable();
    cats.dedup();
    let unknown_cat = cats.binary_search(&&b"unknown"[..]).unwrap() as u16;
    let pkeys: Vec<(&'static [u8], u32)> =
        prows.iter().map(|r| (r[0], cats.binary_search(&r[1]).unwrap() as u32)).collect();
    let prod = KeyMap::build(&pkeys);
    let crows = read_small_table(&data_dir.join("campaigns.csv"));
    let mut camp_names: Vec<&'static [u8]> = Vec::new();
    let mut camp_chan: Vec<&'static [u8]> = Vec::new();
    let mut ckeys = Vec::new();
    {
        let mut seen = SmallMap::new(crows.len());
        for r in &crows {
            let i = camp_names.len() as u32;
            if seen.insert(hash1(r[0]), i) == i {
                camp_names.push(r[0]);
                camp_chan.push(r[1]);
                ckeys.push((r[0], i));
            }
        }
    }
    let camp = KeyMap::build(&ckeys);
    assert!(cats.len() <= 256, "more than 255 categories");
    let organic = camp_names.len() as u32;
    camp_names.push(b"organic");
    camp_chan.push(b"organic");
    let ncamp = camp_names.len();
    lap("setup");

    // L: links
    let mut lchunks = Vec::new();
    let ltotal: usize = lk_files.iter().map(|d| d.len()).sum();
    make_chunks(&lk_files, 2, (ltotal / (nthreads * 4)).clamp(1 << 16, 8 << 20), &mut lchunks);
    let next = AtomicUsize::new(0);
    let lp: Vec<LinkParse> = run_threads(nthreads, |_| {
        let est = ltotal / 22 / nthreads / UB * 2 + 8;
        let mut out = LinkParse { sides: (0..UB).map(|_| Vec::with_capacity(est)).collect(), npairs: 0 };
        loop {
            let i = next.fetch_add(1, Relaxed);
            if i >= lchunks.len() {
                break;
            }
            parse_links_chunk(&lchunks[i], &mut out);
        }
        out
    });
    lap("links parsed");
    let links = build_links(lp, nthreads);
    lap("links built");

    let ctx = Ctx { ws, we, prod, unknown_cat, camp };

    // P1: parse events + clicks
    let etotal: usize = ev_files.iter().map(|d| d.len()).sum();
    let ctotal: usize = ck_files.iter().map(|d| d.len()).sum();
    let target = ((etotal + ctotal) / (nthreads * 24)).clamp(1 << 18, 16 << 20);
    let total_in = etotal + ctotal;
    let block = (total_in / nthreads / 8192).clamp(4096, 65536).next_power_of_two().min(65536);
    init_arena(1 << 30, block);
    let mut chunks = Vec::new();
    make_chunks(&ev_files, 0, target, &mut chunks);
    make_chunks(&ck_files, 1, target, &mut chunks);
    chunks.sort_by_key(|c| std::cmp::Reverse(c.end - c.start));
    let next = AtomicUsize::new(0);
    let pouts: Vec<ParseOut> = run_threads(nthreads, |t| {
        let mut out = ParseOut {
            keys: (0..EB).map(|_| Bucket::new()).collect(),
            recs: (0..UB).map(|_| Bucket::new()).collect(),
            ck: (0..UB).map(|_| Bucket::new()).collect(),
            nev: 0,
            tid: t as u32,
        };
        loop {
            let i = next.fetch_add(1, Relaxed);
            if i >= chunks.len() {
                break;
            }
            let c = &chunks[i];
            if c.kind == 0 {
                parse_events_chunk(c, &ctx, &mut out);
            } else {
                parse_clicks_chunk(c, &ctx, &mut out);
            }
        }
        assert!((out.nev as u64) < (1u64 << GID_BITS));
        out
    });
    lap("P1 events+clicks parsed");

    // P2: dedup per event-id bucket: mark every copy except the earliest-ingested one
    let losers: Vec<Vec<AtomicU64>> =
        pouts.iter().map(|po| (0..(po.nev as usize / 64 + 1)).map(|_| AtomicU64::new(0)).collect()).collect();
    let mark = |gid: u32| {
        let t = (gid >> GID_BITS) as usize;
        let i = (gid & ((1 << GID_BITS) - 1)) as usize;
        losers[t][i >> 6].fetch_or(1u64 << (i & 63), Relaxed);
    };
    let next = AtomicUsize::new(0);
    run_threads(nthreads, |_| {
        let mut keys: Vec<Key> = Vec::new();
        let mut slots: Vec<u32> = Vec::new();
        loop {
            let b = next.fetch_add(1, Relaxed);
            if b >= EB {
                break;
            }
            keys.clear();
            for po in pouts.iter() {
                po.keys[b].for_slices(|x| keys.extend_from_slice(x));
            }
            let n = keys.len();
            if n == 0 {
                continue;
            }
            let cap = (n * 2).next_power_of_two();
            let mask = cap - 1;
            slots.clear();
            slots.resize(cap, 0);
            for i in 0..n {
                let r = unsafe { keys.get_unchecked(i) };
                let mut s = (r.h1 as usize) & mask;
                loop {
                    let v = unsafe { *slots.get_unchecked(s) };
                    if v == 0 {
                        slots[s] = i as u32 + 1;
                        break;
                    }
                    let o = unsafe { keys.get_unchecked((v - 1) as usize) };
                    if o.h1 == r.h1 && o.h2 == r.h2 {
                        if r.ingest < o.ingest {
                            mark(o.gid);
                            slots[s] = i as u32 + 1;
                        } else {
                            mark(r.gid);
                        }
                        break;
                    }
                    s = (s + 1) & mask;
                }
            }
        }
    });
    lap("P2 dedup");
    let mut pouts = pouts;
    for po in pouts.iter_mut() {
        for k in po.keys.iter_mut() {
            k.release();
        }
    }
    let pouts = pouts;

    // P3: drop losers, map users -> persons per user bucket
    let next = AtomicUsize::new(0);
    let mouts: Vec<(Vec<Bucket<Ev>>, Vec<Bucket<Ck>>)> = run_threads(nthreads, |_| {
        let mut eo: Vec<Bucket<Ev>> = (0..UB).map(|_| Bucket::new()).collect();
        let mut co: Vec<Bucket<Ck>> = (0..UB).map(|_| Bucket::new()).collect();
        loop {
            let b = next.fetch_add(1, Relaxed);
            if b >= UB {
                break;
            }
            for po in pouts.iter() {
                let lt = &losers[po.tid as usize];
                po.recs[b].for_slices(|x| {
                    for r in x {
                        let i = (r.gid & ((1 << GID_BITS) - 1)) as usize;
                        if lt[i >> 6].load(Relaxed) & (1u64 << (i & 63)) != 0 {
                            continue;
                        }
                        let (p, n) = links.map(b, r.user, r.uref);
                        eo[ub_of(p)].push(Ev { person: p, name: n, ts: r.ts, pc: r.pc });
                    }
                });
                po.ck[b].for_slices(|x| {
                    for c in x {
                        let (p, _) = links.map(b, c.person, 0);
                        let mut c2 = *c;
                        c2.person = p;
                        co[ub_of(p)].push(c2);
                    }
                });
            }
        }
        (eo, co)
    });
    lap("P3 person mapping");
    let mut pouts = pouts;
    for po in pouts.iter_mut() {
        po.recs.iter_mut().for_each(|x| x.release());
        po.ck.iter_mut().for_each(|x| x.release());
    }

    // P4: person buckets
    let day0 = ws.div_euclid(DAY);
    let ndays = ((we - 1).div_euclid(DAY) - day0 + 1).max(1) as usize;
    let ncats = cats.len();
    std::fs::create_dir_all(out_dir.join("user_daily")).unwrap();
    std::fs::create_dir_all(out_dir.join("campaign_daily")).unwrap();
    let next = AtomicUsize::new(0);
    let works: Vec<PersonWork> = run_threads(nthreads, |t| {
        let mut w = PersonWork {
            cnt: vec![0; ncamp],
            pos: vec![0; ncamp],
            active: Vec::new(),
            catsum: vec![0; ncats],
            touched: Vec::new(),
            camp_acc: vec![[0; 4]; ncamp * ndays],
            rows: Vec::new(),
        };
        let mut sorted: Vec<Ev> = Vec::new();
        let mut csorted: Vec<Ck> = Vec::new();
        let mut pmap = SmallMap::new(0);
        let (mut pcount, mut pid, mut pstart, mut pfill, mut cpid, mut ccount, mut cstart): (
            Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>,
        ) = Default::default();
        loop {
            let b = next.fetch_add(1, Relaxed);
            if b >= UB {
                break;
            }
            let n: usize = mouts.iter().map(|d| d.0[b].len()).sum::<usize>();
            if n == 0 {
                continue;
            }
            // group by person: hash -> local id, counting sort, then sort each person's events by ts
            pmap.reset(n);
            pcount.clear();
            pid.clear();
            for d in mouts.iter() {
                d.0[b].for_slices(|x| {
                    for e in x {
                        let np = pcount.len() as u32;
                        let id = pmap.insert(e.person, np);
                        if id == np {
                            pcount.push(0);
                        }
                        pcount[id as usize] += 1;
                        pid.push(id);
                    }
                });
            }
            let np = pcount.len();
            pstart.clear();
            let mut acc = 0u32;
            for &c in pcount.iter() {
                pstart.push(acc);
                acc += c;
            }
            pstart.push(acc);
            sorted.clear();
            sorted.reserve(n);
            unsafe { sorted.set_len(n) };
            pfill.clear();
            pfill.extend_from_slice(&pstart[..np]);
            let mut i = 0;
            for d in mouts.iter() {
                d.0[b].for_slices(|x| {
                    for e in x {
                        let id = pid[i] as usize;
                        i += 1;
                        unsafe { *sorted.get_unchecked_mut(pfill[id] as usize) = *e };
                        pfill[id] += 1;
                    }
                });
            }
            // clicks grouped by the same ids; clicks of persons with no events are irrelevant
            cpid.clear();
            ccount.clear();
            ccount.resize(np, 0);
            for d in mouts.iter() {
                d.1[b].for_slices(|x| {
                    for c in x {
                        let id = pmap.get(c.person).unwrap_or(u32::MAX);
                        cpid.push(id);
                        if id != u32::MAX {
                            ccount[id as usize] += 1;
                        }
                    }
                });
            }
            cstart.clear();
            let mut acc = 0u32;
            for &c in ccount.iter() {
                cstart.push(acc);
                acc += c;
            }
            cstart.push(acc);
            csorted.clear();
            csorted.reserve(acc as usize);
            unsafe { csorted.set_len(acc as usize) };
            pfill.clear();
            pfill.extend_from_slice(&cstart[..np]);
            let mut i = 0;
            for d in mouts.iter() {
                d.1[b].for_slices(|x| {
                    for c in x {
                        let id = cpid[i];
                        i += 1;
                        if id != u32::MAX {
                            unsafe { *csorted.get_unchecked_mut(pfill[id as usize] as usize) = *c };
                            pfill[id as usize] += 1;
                        }
                    }
                });
            }
            for id in 0..np {
                let ev = &mut sorted[pstart[id] as usize..pstart[id + 1] as usize];
                if ev.len() > 1 {
                    ev.sort_unstable_by_key(|e| e.ts);
                }
                let cs = &mut csorted[cstart[id] as usize..cstart[id + 1] as usize];
                if cs.len() > 1 {
                    cs.sort_unstable_by(|a, b| a.ts.cmp(&b.ts).then_with(|| unpack(a.cid).cmp(unpack(b.cid))));
                }
                process_person(ev, cs, &mut w, ws, day0, ndays, organic);
            }
        }
        if verbose {
            eprintln!("{:>8.1} ms  thread {t} sessions done", t0.elapsed().as_secs_f64() * 1000.0);
        }
        write_user_daily(&out_dir.join("user_daily").join(format!("part-{t:03}.parquet")), &w.rows, &cats);
        w
    });
    lap("P4 sessions + user_daily written");

    let mut crow = Vec::new();
    for c in 0..ncamp {
        for d in 0..ndays {
            let mut v = [0u64; 4];
            for w in &works {
                let a = &w.camp_acc[c * ndays + d];
                for k in 0..4 {
                    v[k] += a[k];
                }
            }
            if v[0] > 0 || v[3] > 0 {
                crow.push(CRow { camp: camp_names[c], chan: camp_chan[c], day: (day0 + d as i64) as i32, v });
            }
        }
    }
    write_campaign_daily(&out_dir.join("campaign_daily").join("part-000.parquet"), &crow);
    lap("done");
    if verbose {
        let urows: usize = works.iter().map(|w| w.rows.len()).sum();
        eprintln!("user rows {urows}, campaign rows {}", crow.len());
    }
    std::process::exit(0);
}
