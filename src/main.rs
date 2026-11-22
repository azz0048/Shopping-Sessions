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
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering::*};
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
    /// Insert with automatic growth (keeps load <= 1/2). `count` = keys currently stored.
    #[inline(always)]
    pub fn insert_grow(&mut self, h: u64, v: u32, count: usize) -> u32 {
        if (count + 1) * 2 > self.mask + 1 {
            self.grow();
        }
        self.insert(h, v)
    }
    #[cold]
    fn grow(&mut self) {
        let old_k = std::mem::take(&mut self.keys);
        let old_v = std::mem::take(&mut self.vals);
        let cap = (self.mask + 1) * 2;
        self.keys = vec![0; cap];
        self.vals = vec![0; cap];
        self.mask = cap - 1;
        for (k, v) in old_k.into_iter().zip(old_v) {
            if k != 0 {
                self.insert(k, v);
            }
        }
    }
    /// Clear and size for about n keys.
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
        let mut km = KeyMap { prefix: vec![], arr: vec![], fast: false, map: SmallMap::new(0) };
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
        if !km.fast {
            km.map = SmallMap::new(keys.len());
            for &(k, v) in keys {
                km.map.insert(hash1(k), v);
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
    if s.len() <= pl || s.len() > pl + 9 {
        return None;
    }
    for i in 0..pl {
        if s[i] != prefix[i] {
            return None;
        }
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
/// Dedup key, scattered by event_id hash.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Key {
    hk: u64,     // event_id fingerprint: low 54 bits of h1 (top bits = bucket) | 10 bits of h2 << 54
    ingest: i32, // seconds since window start (saturated)
    handle: u32, // arena handle of this copy's Rec, or u32::MAX if the copy is invalid
}
/// A valid event copy, scattered by user hash. ts == u32::MAX marks a dedup loser.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Rec {
    user: u64,
    uref: u64,
    ts: u32, // ms since window start
    pc: u32, // purchase price (low 24 bits, 0 if not a purchase) | category << 24
}
/// A surviving event, scattered by person bucket (pid & (UB - 1)). In P4, pid is replaced by
/// the person's slot in the bucket (pid >> UB_BITS).
///
/// Person ids are dense u32: a linked person is the global side index of its byte-smallest ID
/// (< s_round); an unlinked user is s_round + (local << UB_BITS | user bucket), local being its
/// first-seen index among the unlinked users of that user bucket.
#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct Ev {
    pub ts: u32,
    pub pc: u32,
    pub pid: u32,
}
pub type EvP = Ev;

/// Stable LSD radix sort by ts (31 bits: 11 + 11 + 9), ping-ponging between a and b.
fn radix_ts(a: &mut Vec<EvP>, b: &mut Vec<EvP>) {
    let n = a.len();
    if n < 64 {
        a.sort_by_key(|e| e.ts); // stable
        return;
    }
    b.clear();
    b.resize(n, EvP::default());
    let mut cnt = [[0u32; 2048]; 3];
    for e in a.iter() {
        let t = e.ts;
        cnt[0][(t & 2047) as usize] += 1;
        cnt[1][((t >> 11) & 2047) as usize] += 1;
        cnt[2][((t >> 22) & 2047) as usize] += 1;
    }
    for c in cnt.iter_mut() {
        let mut acc = 0u32;
        for x in c.iter_mut() {
            let v = *x;
            *x = acc;
            acc += v;
        }
    }
    for e in a.iter() {
        let d = &mut cnt[0][(e.ts & 2047) as usize];
        unsafe { *b.get_unchecked_mut(*d as usize) = *e };
        *d += 1;
    }
    for e in b.iter() {
        let d = &mut cnt[1][((e.ts >> 11) & 2047) as usize];
        unsafe { *a.get_unchecked_mut(*d as usize) = *e };
        *d += 1;
    }
    for e in a.iter() {
        let d = &mut cnt[2][((e.ts >> 22) & 2047) as usize];
        unsafe { *b.get_unchecked_mut(*d as usize) = *e };
        *d += 1;
    }
    std::mem::swap(a, b);
}

/// A valid click keyed by user hash (P1 output, scattered by user bucket).
/// ts is ms since window start - 7 days.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct CkU {
    pub user: u64,
    pub cid: u64,
    pub ts: u32,
    pub camp: u32,
}
/// A valid click of a person (P3 output, scattered by person bucket).
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Ck {
    pub cid: u64,
    pub pid: u32,
    pub ts: u32,
    pub camp: u32,
    pub _pad: u32,
}

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
    keys: WcSet<Key, 4>,
    recs: WcSet<Rec, 8>,
    ck: WcSet<CkU, 8>,
}

fn parse_events_chunk(c: &Chunk, ctx: &Ctx, out: &mut ParseOut) {
    let ws = ctx.ws;
    let we = ctx.we;
    let d = c.data;
    let mut p = chunk_start(d, c.start, c.end);
    let mut f: [&'static [u8]; 7] = [&[]; 7];
    while p < c.end {
        let (np, ok) = split_at2::<7>(d, p, &mut f);
        p = np;
        if !ok {
            continue;
        }
        let eid = f[0];
        if eid.is_empty() {
            continue;
        }
        let (h1, h2) = hash_wide(eid);
        let hk = (h1 & ((1u64 << 54) - 1)) | (((h2 & 0x3FF) as u64) << 54);
        let ingest = parse_ingest(f[3]).unwrap_or(i64::MAX);
        let ing_rel = ((ingest.saturating_sub(ws)) / 1000).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
        // validity of this copy
        let handle = 'v: {
            let user = f[1];
            if user.is_empty() || f[5].is_empty() {
                break 'v u32::MAX;
            }
            let ts = match parse_ts_fast(f[2]) {
                Some(t) => t,
                None => break 'v u32::MAX,
            };
            if ts < ws || ts >= we || ingest.saturating_sub(ts) > LATE {
                break 'v u32::MAX;
            }
            let ty = f[4];
            let kind = if ty.len() == 4 {
                let w = u32::from_le_bytes([ty[0], ty[1], ty[2], ty[3]]);
                if w == u32::from_le_bytes(*b"view") || w == u32::from_le_bytes(*b"cart") { 1 } else { 0 }
            } else if ty.len() == 8 {
                let w = u64::from_le_bytes(ty.try_into().unwrap());
                if w == u64::from_le_bytes(*b"purchase") { 3 } else { 0 }
            } else {
                0
            };
            let pc = match kind {
                1 => 0u32,
                3 => {
                    let price = match parse_price(f[6]) {
                        Some(p) => p,
                        None => break 'v u32::MAX,
                    };
                    let cat = match ctx.prod.get(f[5]) {
                        Some(c) => c,
                        None => ctx.unknown_cat as u32,
                    };
                    price | (cat << 24)
                }
                _ => break 'v u32::MAX,
            };
            let uh = hash1(user);
            out.recs.push_h(ub_of(uh), Rec { user: uh, uref: pack(user), ts: (ts - ws) as u32, pc })
        };
        out.keys.push(eb_of(h1), Key { hk, ingest: ing_rel, handle });
    }
}

fn parse_clicks_chunk(c: &Chunk, ctx: &Ctx, out: &mut ParseOut) {
    let lo = ctx.ws - WEEK;
    let hi = ctx.we;
    let d = c.data;
    let mut p = chunk_start(d, c.start, c.end);
    let mut f: [&'static [u8]; 4] = [&[]; 4];
    while p < c.end {
        let (np, ok) = split_at2::<4>(d, p, &mut f);
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
        out.ck.push(ub_of(u), CkU { user: u, cid: pack(f[0]), ts: (ts - lo) as u32, camp });
    }
}

// ---------- links ----------
/// One side of a link line: (hash, ref, 8-byte BE prefix, local side index).
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Side {
    h: u64,
    r: u64,
    pre: u64,
    si: u32,
    _pad: u32,
}
struct LinkParse {
    sides: WcSet<Side, 2>, // scattered by user bucket
    npairs: usize,
}

fn parse_links_chunk(c: &Chunk, out: &mut LinkParse) {
    let d = c.data;
    let mut p = chunk_start(d, c.start, c.end);
    let mut f: [&'static [u8]; 2] = [&[]; 2];
    while p < c.end {
        let (np, ok) = split_at2::<2>(d, p, &mut f);
        p = np;
        if !ok || f[0].is_empty() || f[1].is_empty() {
            continue;
        }
        let i = out.npairs as u32;
        out.npairs += 1;
        let ha = hash1(f[0]);
        let hb = hash1(f[1]);
        out.sides.push(ub_of(ha), Side { h: ha, r: pack(f[0]), pre: be_prefix(f[0]), si: 2 * i, _pad: 0 });
        out.sides.push(ub_of(hb), Side { h: hb, r: pack(f[1]), pre: be_prefix(f[1]), si: 2 * i + 1, _pad: 0 });
    }
}

pub struct Links {
    // per user bucket: open-addressing table slice [tab_off, tab_off + mask + 1)
    tab_off: Vec<usize>,
    tab_mask: Vec<usize>,
    tkeys: &'static [u64],
    /// person id (global side index of the component's byte-smallest ID) per table slot
    tvals: &'static [u32],
    /// side index -> ID ref (the name, for person ids)
    dref: &'static [u64],
    /// s_total rounded up to a multiple of UB: the first unlinked person id
    s_round: u32,
}
impl Links {
    /// Person id of a linked ID (in user bucket b), or None if the ID is in no link.
    #[cfg_attr(not(feature = "prof"), inline(always))]
    #[cfg_attr(feature = "prof", inline(never))]
    fn lookup(&self, b: usize, h: u64) -> Option<u32> {
        let off = self.tab_off[b];
        let mask = self.tab_mask[b];
        let mut s = (h as usize) & mask;
        loop {
            let k = unsafe { *self.tkeys.get_unchecked(off + s) };
            if k == h {
                return Some(unsafe { *self.tvals.get_unchecked(off + s) });
            }
            if k == 0 {
                return None;
            }
            s = (s + 1) & mask;
        }
    }
}

#[inline]
fn uf_find(parent: &[AtomicU32], mut x: u32) -> u32 {
    loop {
        let p = parent[x as usize].load(Relaxed);
        if p == x {
            return x;
        }
        let gp = parent[p as usize].load(Relaxed);
        if gp != p {
            // path halving; parents only ever move to smaller indices, so this is safe to race
            let _ = parent[x as usize].compare_exchange_weak(p, gp, Relaxed, Relaxed);
        }
        x = gp;
    }
}

#[inline]
fn uf_union(parent: &[AtomicU32], a: u32, b: u32) {
    loop {
        let ra = uf_find(parent, a);
        let rb = uf_find(parent, b);
        if ra == rb {
            return;
        }
        let (hi, lo) = if ra > rb { (ra, rb) } else { (rb, ra) };
        if parent[hi as usize].compare_exchange(hi, lo, AcqRel, Relaxed).is_ok() {
            return;
        }
    }
}

#[allow(dead_code)]
/// First 8 bytes, big-endian, zero padded: a strict order on prefixes implies the same
/// byte-wise order on the full strings.
#[inline(always)]
fn be_prefix(s: &[u8]) -> u64 {
    if s.len() >= 8 {
        return u64::from_be_bytes(s[..8].try_into().unwrap());
    }
    let mut v = 0u64;
    for (i, &c) in s.iter().enumerate() {
        v |= (c as u64) << (56 - 8 * i);
    }
    v
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

/// Bucket indices ordered largest first (longest-processing-time scheduling).
fn lpt_order(sizes: impl Fn(usize) -> usize, n: usize) -> Vec<usize> {
    let mut v: Vec<(usize, usize)> = (0..n).map(|b| (sizes(b), b)).collect();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v.into_iter().map(|x| x.1).collect()
}

fn run_threads<T: Send>(n: usize, f: impl Fn(usize) -> T + Sync) -> Vec<T> {
    std::thread::scope(|sc| {
        let f = &f;
        let hs: Vec<_> = (0..n)
            .map(|t| {
                sc.spawn(move || {
                    let r = f(t);
                    arena_flush_tl();
                    r
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

fn build_links(lp: Vec<LinkParse>, nthreads: usize) -> Links {
    let tl = Instant::now();
    let verbose = std::env::var("SS_VERBOSE").is_ok();
    let lap = |w: &str| {
        if verbose {
            eprintln!("      links {:>7.1} ms {w}", tl.elapsed().as_secs_f64() * 1000.0)
        }
    };
    let mut off = Vec::with_capacity(lp.len());
    let mut npairs = 0usize;
    for l in &lp {
        off.push(2 * npairs as u32);
        npairs += l.npairs;
    }
    // per-bucket side counts -> table and id-space offsets
    let mut tab_off = vec![0usize; UB];
    let mut tab_mask = vec![0usize; UB];
    let mut side_off = vec![0usize; UB];
    let (mut t_total, mut s_total) = (0usize, 0usize);
    for b in 0..UB {
        let n: usize = lp.iter().map(|l| l.sides.len(b)).sum();
        let cap = (2 * n + 16).next_power_of_two();
        tab_off[b] = t_total;
        tab_mask[b] = cap - 1;
        side_off[b] = s_total;
        t_total += cap;
        s_total += n;
    }
    let tkeys: &'static mut [u64] = huge_zeroed(t_total);
    let tvals: &'static mut [u32] = huge_zeroed(t_total);
    let dref: &'static mut [u64] = huge_zeroed(s_total);
    let dpre: &'static mut [u64] = huge_zeroed(s_total);
    let pair_ids: &'static mut [u32] = huge_zeroed(2 * npairs);
    let used: &'static mut [u8] = huge_zeroed(s_total);
    // intern per bucket (each bucket owns disjoint slices)
    {
        let tk = SendPtr(tkeys.as_mut_ptr());
        let tv = SendPtr(tvals.as_mut_ptr());
        let dp = SendPtr(dpre.as_mut_ptr());
        let dr = SendPtr(dref.as_mut_ptr());
        let pi = SendPtr(pair_ids.as_mut_ptr());
        let us = SendPtr(used.as_mut_ptr());
        let next = AtomicUsize::new(0);
        let (lp, off, tab_off, tab_mask, side_off) = (&lp, &off, &tab_off, &tab_mask, &side_off);
        run_threads(nthreads, |_| {
            let (tk, tv, dr, dp, pi, us) = (&tk, &tv, &dr, &dp, &pi, &us);
            loop {
                let b = next.fetch_add(1, Relaxed);
                if b >= UB {
                    break;
                }
                let (to, mask, so) = (tab_off[b], tab_mask[b], side_off[b]);
                let mut nd = 0u32;
                for (ti, l) in lp.iter().enumerate() {
                    l.sides.for_slices(b, |x| for &Side { h, r, pre, si, .. } in x {
                        let mut s = (h as usize) & mask;
                        let id = loop {
                            let k = unsafe { *tk.0.add(to + s) };
                            if k == 0 {
                                unsafe {
                                    *tk.0.add(to + s) = h;
                                    *tv.0.add(to + s) = nd;
                                    *dr.0.add(so + nd as usize) = r;
                                    *dp.0.add(so + nd as usize) = pre;
                                    *us.0.add(so + nd as usize) = 1;
                                }
                                nd += 1;
                                break nd - 1;
                            }
                            if k == h {
                                break unsafe { *tv.0.add(to + s) };
                            }
                            s = (s + 1) & mask;
                        };
                        unsafe { *pi.0.add((off[ti] + si) as usize) = (so + id as usize) as u32 };
                    });
                }
            }
        });
    }
    let mut lp = lp;
    for l in lp.iter_mut() {
        l.sides.release_all();
    }
    drop(lp);
    lap("interned");
    // concurrent union-find: always link the larger root under the smaller (CAS), path halving
    let parent: &'static [AtomicU32] = {
        let m: &'static mut [u32] = huge_zeroed(s_total);
        unsafe { std::mem::transmute::<&'static mut [u32], &'static [AtomicU32]>(m) }
    };
    {
        let chunk = s_total.div_ceil(nthreads).max(1);
        run_threads(nthreads, |t| {
            for i in (t * chunk)..((t + 1) * chunk).min(s_total) {
                parent[i].store(i as u32, Relaxed);
            }
        });
        let pair_ids: &[u32] = pair_ids;
        let pchunk = npairs.div_ceil(nthreads * 8).max(1024);
        let next = AtomicUsize::new(0);
        run_threads(nthreads, |_| loop {
            let c = next.fetch_add(1, Relaxed);
            let lo = c * pchunk;
            if lo >= npairs {
                break;
            }
            for i in lo..(lo + pchunk).min(npairs) {
                uf_union(parent, pair_ids[2 * i], pair_ids[2 * i + 1]);
            }
        });
    }
    lap("union-find");
    // roots + byte-smallest ID per component (parallel; parent is read-only now)
    let root: &'static mut [u32] = huge_zeroed(s_total);
    let minid: &'static [AtomicU32] = {
        let m: &'static mut [u32] = huge_zeroed(s_total);
        m.fill(u32::MAX);
        unsafe { std::mem::transmute::<&'static mut [u32], &'static [AtomicU32]>(m) }
    };
    let chunk = s_total.div_ceil(nthreads * 8).max(1024);
    {
        let rp = SendPtr(root.as_mut_ptr());
        let (dref, dpre, used): (&[u64], &[u64], &[u8]) = (dref, dpre, used);
        let next = AtomicUsize::new(0);
        run_threads(nthreads, |_| {
            let rp = &rp;
            loop {
                let c = next.fetch_add(1, Relaxed);
                let lo = c * chunk;
                if lo >= s_total {
                    break;
                }
                for i in lo..(lo + chunk).min(s_total) {
                    if used[i] == 0 {
                        continue;
                    }
                    let r = uf_find(parent, i as u32);
                    unsafe { *rp.0.add(i) = r };
                    let slot = &minid[r as usize];
                    let mut cur = slot.load(Relaxed);
                    loop {
                        // prefix first; strict prefix order implies string order
                        if cur != u32::MAX && {
                            let (pc, pi) = (dpre[cur as usize], dpre[i]);
                            pc < pi || (pc == pi && unpack(dref[cur as usize]) <= unpack(dref[i]))
                        } {
                            break;
                        }
                        match slot.compare_exchange_weak(cur, i as u32, Relaxed, Relaxed) {
                            Ok(_) => break,
                            Err(v) => cur = v,
                        }
                    }
                }
            }
        });
    }
    lap("min names");
    // table values: local side index -> person id (global side index of the smallest ID)
    {
        let tv = SendPtr(tvals.as_mut_ptr());
        let (root, tkeys, tab_off, tab_mask, side_off): (&[u32], &[u64], &[usize], &[usize], &[usize]) =
            (root, tkeys, &tab_off, &tab_mask, &side_off);
        let next = AtomicUsize::new(0);
        run_threads(nthreads, |_| {
            let tv = &tv;
            loop {
                let b = next.fetch_add(32, Relaxed);
                if b >= UB {
                    break;
                }
                for b in b..(b + 32).min(UB) {
                    let (to, so) = (tab_off[b], side_off[b]);
                    for s in 0..=tab_mask[b] {
                        if tkeys[to + s] != 0 {
                            unsafe {
                                let v = tv.0.add(to + s);
                                *v = minid[root[so + *v as usize] as usize].load(Relaxed);
                            }
                        }
                    }
                }
            }
        });
    }
    lap("person ids");
    if verbose {
        eprintln!("      links: {npairs} pairs, {s_total} sides");
    }
    let s_round = s_total.div_ceil(UB) * UB;
    assert!(s_round < (1 << 31), "too many linked IDs");
    Links { tab_off, tab_mask, tkeys, tvals, dref, s_round: s_round as u32 }
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
    init_month_table(1970 + ws.div_euclid(DAY) / 365);
    let we = manifest_ts(&mtext, "\"window_end\"");
    assert!(we - ws + WEEK < (1i64 << 32), "window too long");

    let ev_files: Vec<&'static [u8]> = list_csv(&data_dir.join("events")).iter().map(|p| map_file(p)).collect();
    let ck_files: Vec<&'static [u8]> = list_csv(&data_dir.join("clicks")).iter().map(|p| map_file(p)).collect();
    let lk_files: Vec<&'static [u8]> = list_csv(&data_dir.join("links")).iter().map(|p| map_file(p)).collect();

    // products & campaigns
    let prows = read_small_table(&data_dir.join("products.csv"));
    // distinct categories via a hash map (strings verified), then sort only the distinct ones
    let (cats, unknown_cat, pkeys) = {
        let mut cmap = SmallMap::new(64);
        let mut distinct: Vec<&'static [u8]> = Vec::new();
        let mut row_cat: Vec<u32> = Vec::with_capacity(prows.len());
        let mut exact = true;
        for s in prows.iter().map(|r| r[1]).chain(std::iter::once(&b"unknown"[..])) {
            let i = distinct.len() as u32;
            let j = cmap.insert_grow(hash1(s), i, i as usize);
            if j == i {
                distinct.push(s);
            } else if distinct[j as usize] != s {
                exact = false;
            }
            row_cat.push(j);
        }
        if exact {
            let mut order: Vec<u32> = (0..distinct.len() as u32).collect();
            order.sort_unstable_by(|&a, &b| distinct[a as usize].cmp(distinct[b as usize]));
            let mut rank = vec![0u32; distinct.len()];
            for (r, &o) in order.iter().enumerate() {
                rank[o as usize] = r as u32;
            }
            let cats: Vec<&'static [u8]> = order.iter().map(|&o| distinct[o as usize]).collect();
            let unknown_cat = rank[*row_cat.last().unwrap() as usize] as u16;
            let pkeys: Vec<(&'static [u8], u32)> = prows.iter().zip(&row_cat).map(|(r, &c)| (r[0], rank[c as usize])).collect();
            (cats, unknown_cat, pkeys)
        } else {
            let mut cats: Vec<&'static [u8]> = prows.iter().map(|r| r[1]).collect();
            cats.push(b"unknown");
            cats.sort_unstable();
            cats.dedup();
            let unknown_cat = cats.binary_search(&&b"unknown"[..]).unwrap() as u16;
            let pkeys: Vec<(&'static [u8], u32)> =
                prows.iter().map(|r| (r[0], cats.binary_search(&r[1]).unwrap() as u32)).collect();
            (cats, unknown_cat, pkeys)
        }
    };
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

    let block: usize = std::env::var("SS_BLOCK").ok().and_then(|v| v.parse().ok()).unwrap_or(8192);
    init_arena(1 << 30, block);
    #[cfg(windows)]
    if let Some(gb) = std::env::var("SS_PRETOUCH").ok().and_then(|v| v.parse::<usize>().ok()) {
        // dev only: map + touch arena regions up front (Windows page faults are far costlier than Linux THP)
        let n = (gb << 30) / block;
        let v: Vec<(usize, u32)> = (0..n).map(|_| { let (p, id) = arena().alloc(); (p as usize, id) }).collect();
        arena().release(&v);
        arena_flush_tl();
        lap("pretouch");
    }

    // L: links
    let mut lchunks = Vec::new();
    let ltotal: usize = lk_files.iter().map(|d| d.len()).sum();
    make_chunks(&lk_files, 2, (ltotal / (nthreads * 4)).clamp(1 << 16, 8 << 20), &mut lchunks);
    let next = AtomicUsize::new(0);
    let lp: Vec<LinkParse> = run_threads(nthreads, |_| {
        let mut out = LinkParse { sides: WcSet::new(UB), npairs: 0 };
        loop {
            let i = next.fetch_add(1, Relaxed);
            if i >= lchunks.len() {
                break;
            }
            parse_links_chunk(&lchunks[i], &mut out);
        }
        out.sides.finish();
        out
    });
    lap("links parsed");
    let links = build_links(lp, nthreads);
    lap("links built");

    let ctx = Ctx { ws, we, prod, unknown_cat, camp };
    if let Ok(bf) = std::env::var("SS_BENCHFILE") {
        // single-threaded parse benchmark on one warm file (no output)
        let d = map_file(std::path::Path::new(&bf));
        let d: &'static [u8] = Box::leak(d.to_vec().into_boxed_slice());
        let c = Chunk { data: d, start: 0, end: d.len(), kind: 0 };
        if let Ok(mode) = std::env::var("SS_BENCHMODE") {
            let mode: u32 = mode.parse().unwrap();
            let mut best = f64::MAX;
            for _ in 0..9 {
                let t = Instant::now();
                let mut p = chunk_start(d, 0, d.len());
                let mut f: [&'static [u8]; 7] = [&[]; 7];
                let (mut n, mut acc) = (0usize, 0u64);
                while p < d.len() {
                    let (np, ok) = if mode >= 10 { split_at2::<7>(d, p, &mut f) } else { split_at::<7>(d, p, &mut f) };
                    p = np;
                    n += 1;
                    if !ok { continue; }
                    acc = acc.wrapping_add(f[0].len() as u64 + f[3].len() as u64 + f[6].as_ptr() as u64);
                    let mode = mode % 10;
                    if mode >= 2 { let (h1, h2) = hash_wide(f[0]); acc ^= h1 ^ h2 as u64 ^ hash1(f[1]); }
                    if mode >= 3 { acc = acc.wrapping_add(parse_ingest(f[3]).unwrap_or(0) as u64).wrapping_add(parse_ts_fast(f[2]).unwrap_or(0) as u64); }
                    if mode >= 4 { acc = acc.wrapping_add(parse_price(f[6]).unwrap_or(0) as u64).wrapping_add(f[4].len() as u64); }
                }
                std::hint::black_box(acc);
                best = best.min(t.elapsed().as_nanos() as f64 / n as f64);
            }
            eprintln!("mode {mode}: {best:.1} ns/line");
            std::process::exit(0);
        }
        let mut best = f64::MAX;
        for it in 0..15 {
            let mut out = ParseOut {
                keys: WcSet::new(EB),
                recs: WcSet::new(UB),
                ck: WcSet::new(UB),
            };
            let t = Instant::now();
            parse_events_chunk(&c, &ctx, &mut out);
            out.keys.finish();
            out.recs.finish();
            let e = t.elapsed();
            let n: usize = (0..EB).map(|b| out.keys.len(b)).sum();
            best = best.min(e.as_nanos() as f64 / n as f64);
            if it == 14 { eprintln!("min {:.1} ns/line over 15 iters ({n} events)", best); }
            out.keys.release_all();
            out.recs.release_all();
        }
        std::process::exit(0);
    }

    // P1: parse events + clicks
    let etotal: usize = ev_files.iter().map(|d| d.len()).sum();
    let ctotal: usize = ck_files.iter().map(|d| d.len()).sum();
    let target = ((etotal + ctotal) / (nthreads * 24)).clamp(1 << 18, 16 << 20);
    let mut chunks = Vec::new();
    make_chunks(&ev_files, 0, target, &mut chunks);
    make_chunks(&ck_files, 1, target, &mut chunks);
    chunks.sort_by_key(|c| std::cmp::Reverse(c.end - c.start));
    let next = AtomicUsize::new(0);
    let pouts: Vec<ParseOut> = run_threads(nthreads, |_| {
        let mut out = ParseOut {
            keys: WcSet::new(EB),
            recs: WcSet::new(UB),
            ck: WcSet::new(UB),
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
        out.keys.finish();
        out.recs.finish();
        out.ck.finish();
        out
    });
    lap("P1 events+clicks parsed");
    if std::env::var_os("SS_P1ONLY").is_some() {
        std::process::exit(0);
    }

    // P2: dedup per event-id bucket: tombstone every valid copy except the earliest-ingested one
    let block_ptrs = arena().block_ptrs();
    let rec_cap = WcSet::<Rec, 8>::cap() as u32;
    let mark = |h: u32| {
        if h != u32::MAX {
            let p = block_ptrs[(h / rec_cap) as usize] as *mut Rec;
            unsafe { (*p.add((h % rec_cap) as usize)).ts = u32::MAX };
        }
    };
    let next = AtomicUsize::new(0);
    let order = lpt_order(|b| pouts.iter().map(|po| po.keys.len(b)).sum(), EB);
    run_threads(nthreads, |_| {
        let mut keys: Vec<Key> = Vec::new();
        let mut slots: Vec<u32> = Vec::new();
        loop {
            let i = next.fetch_add(1, Relaxed);
            if i >= EB {
                break;
            }
            let b = order[i];
            keys.clear();
            for po in pouts.iter() {
                po.keys.for_slices(b, |x| keys.extend_from_slice(x));
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
                let mut s = (r.hk as usize) & mask;
                loop {
                    let v = unsafe { *slots.get_unchecked(s) };
                    if v == 0 {
                        slots[s] = i as u32 + 1;
                        break;
                    }
                    let o = unsafe { keys.get_unchecked((v - 1) as usize) };
                    if o.hk == r.hk {
                        if r.ingest < o.ingest {
                            mark(o.handle);
                            slots[s] = i as u32 + 1;
                        } else {
                            mark(r.handle);
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
        po.keys.release_all();
    }
    let pouts = pouts;

    // P3: drop losers, map users -> persons per user bucket; collect names of unlinked users
    let next = AtomicUsize::new(0);
    type P3Out = (WcSet<Ev, 16>, WcSet<Ck, 8>, Vec<(usize, Vec<u64>)>);
    let s_round = links.s_round;
    let order = lpt_order(|b| pouts.iter().map(|po| po.recs.len(b) + po.ck.len(b)).sum(), UB);
    let mouts: Vec<P3Out> = run_threads(nthreads, |_| {
        let mut eo: WcSet<Ev, 16> = WcSet::new(UB);
        let mut co: WcSet<Ck, 8> = WcSet::new(UB);
        let mut names = Vec::new();
        let mut seen = SmallMap::new(0);
        loop {
            let i = next.fetch_add(1, Relaxed);
            if i >= UB {
                break;
            }
            let b = order[i];
            let mut nm: Vec<u64> = Vec::new();
            let nb: usize = pouts.iter().map(|po| po.recs.len(b)).sum();
            seen.reset(nb / 8 + 64);
            // all events of the bucket first, so every unlinked user with events has its id
            for po in pouts.iter() {
                po.recs.for_slices(b, |x| {
                    for r in x {
                        if r.ts == u32::MAX {
                            continue;
                        }
                        let pid = match links.lookup(b, r.user) {
                            Some(p) => p,
                            None => {
                                let n = nm.len() as u32;
                                let l = seen.insert_grow(r.user, n, n as usize);
                                if l == n {
                                    nm.push(r.uref);
                                }
                                s_round + ((l << UB_BITS) | b as u32)
                            }
                        };
                        eo.push(pid as usize & (UB - 1), Ev { ts: r.ts, pc: r.pc, pid });
                    }
                });
            }
            assert!(((nm.len() as u64) << UB_BITS) + (s_round as u64) < (1u64 << 32), "too many unlinked users");
            // clicks of unlinked users without events can never credit a session: dropped
            for po in pouts.iter() {
                po.ck.for_slices(b, |x| {
                    for c in x {
                        let pid = match links.lookup(b, c.user) {
                            Some(p) => p,
                            None => match seen.get(c.user) {
                                Some(l) => s_round + ((l << UB_BITS) | b as u32),
                                None => continue,
                            },
                        };
                        co.push(pid as usize & (UB - 1), Ck { cid: c.cid, pid, ts: c.ts, camp: c.camp, _pad: 0 });
                    }
                });
            }
            names.push((b, nm));
        }
        eo.finish();
        co.finish();
        (eo, co, names)
    });
    let mut names_of: Vec<Vec<u64>> = (0..UB).map(|_| Vec::new()).collect();
    let mut mouts = mouts;
    for m in mouts.iter_mut() {
        for (b, v) in m.2.drain(..) {
            names_of[b] = v;
        }
    }
    let mouts = mouts;
    lap("P3 person mapping");
    let mut pouts = pouts;
    for po in pouts.iter_mut() {
        po.recs.release_all();
        po.ck.release_all();
    }

    // P4: person buckets
    let base_slots = (s_round >> UB_BITS) as usize;
    let day0 = ws.div_euclid(DAY);
    let ndays = ((we - 1).div_euclid(DAY) - day0 + 1).max(1) as usize;
    let ncats = cats.len();
    std::fs::create_dir_all(out_dir.join("user_daily")).unwrap();
    std::fs::create_dir_all(out_dir.join("campaign_daily")).unwrap();
    let next = AtomicUsize::new(0);
    let order = lpt_order(|b| mouts.iter().map(|d| d.0.len(b) + d.1.len(b) / 2).sum(), UB);
    if verbose {
        let sz: Vec<usize> = order.iter().map(|&b| mouts.iter().map(|d| d.0.len(b)).sum()).collect();
        eprintln!("      P4 buckets: max {} {} {} median {} min {}", sz[0], sz[1], sz[2], sz[UB / 2], sz[UB - 1]);
    }
    let works: Vec<PersonWork> = run_threads(nthreads, |t| {
        let mut w = PersonWork {
            cnt: vec![0; ncamp],
            pos: vec![0; ncamp],
            active: Vec::new(),
            catsum: vec![0; ncats],
            touched: Vec::new(),
            camp_acc: vec![[0; 4]; ncamp * ndays],
            assist: vec![0; ncamp * ndays],
            rows: Vec::new(),
            namebuf: Vec::new(),
        };
        let mut sorted: Vec<Ev> = Vec::new();
        let mut pname: Vec<u64> = Vec::new();
        let mut plist: Vec<u32> = Vec::new();
        let mut ra: Vec<EvP> = Vec::new();
        let mut rb: Vec<EvP> = Vec::new();
        let mut csorted: Vec<Ck> = Vec::new();
        let maxn = (0..UB).map(|b| mouts.iter().map(|d| d.0.len(b)).sum::<usize>()).max().unwrap_or(0);
        let maxc = (0..UB).map(|b| mouts.iter().map(|d| d.1.len(b)).sum::<usize>()).max().unwrap_or(0);
        sorted.reserve(maxn);
        csorted.reserve(maxc);
        w.rows.reserve(maxn * UB / nthreads / 6);
        w.namebuf.reserve(maxn * UB / nthreads / 2);
        let (mut tgroup, mut tproc) = (0f64, 0f64);
        let (mut pcount, mut pstart, mut pfill, mut cpid, mut ccount, mut cstart): (
            Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>,
        ) = Default::default();
        cpid.reserve(maxc);
        loop {
            let i = next.fetch_add(1, Relaxed);
            if i >= UB {
                break;
            }
            let b = order[i];
            let tb = Instant::now();
            let n: usize = mouts.iter().map(|d| d.0.len(b)).sum::<usize>();
            if n == 0 {
                continue;
            }
            // group by person slot (pid >> UB_BITS, dense per bucket): counting sort after an
            // LSD radix sort of the bucket by ts (3 x 11 bits), so the stable scatter by slot
            // leaves every person's events in ts order.
            let np = base_slots + names_of[b].len();
            pcount.clear();
            pcount.resize(np, 0);
            ra.clear();
            for d in mouts.iter() {
                d.0.for_slices(b, |x| ra.extend_from_slice(x));
            }
            for e in ra.iter_mut() {
                let s = e.pid >> UB_BITS;
                e.pid = s;
                unsafe { *pcount.get_unchecked_mut(s as usize) += 1 };
            }
            radix_ts(&mut ra, &mut rb);
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
            for r in ra.iter() {
                let id = r.pid as usize;
                unsafe {
                    let f = pfill.get_unchecked_mut(id);
                    *sorted.get_unchecked_mut(*f as usize) = *r;
                    *f += 1;
                }
            }
            // clicks grouped by the same slots; clicks of persons with no events are irrelevant
            cpid.clear();
            ccount.clear();
            ccount.resize(np, 0);
            for d in mouts.iter() {
                d.1.for_slices(b, |x| {
                    for c in x {
                        let s = (c.pid >> UB_BITS) as usize;
                        let id = if pcount[s] != 0 {
                            ccount[s] += 1;
                            s as u32
                        } else {
                            u32::MAX
                        };
                        cpid.push(id);
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
                d.1.for_slices(b, |x| {
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
            let tg = Instant::now();
            tgroup += (tg - tb).as_secs_f64();
            // resolve all names first and prefetch them, so the copies in process_person hit cache
            pname.clear();
            plist.clear();
            for s in 0..np {
                if pcount[s] == 0 {
                    continue;
                }
                let r = if s < base_slots {
                    links.dref[(s << UB_BITS) | b]
                } else {
                    names_of[b][s - base_slots]
                };
                unsafe {
                    std::arch::x86_64::_mm_prefetch((r & ((1u64 << 47) - 1)) as *const i8, std::arch::x86_64::_MM_HINT_T0)
                };
                pname.push(r);
                plist.push(s as u32);
            }
            for (k, &s) in plist.iter().enumerate() {
                let s = s as usize;
                let ev = &mut sorted[pstart[s] as usize..pstart[s + 1] as usize];
                debug_assert!(ev.windows(2).all(|w| w[0].ts <= w[1].ts));
                let cs = &mut csorted[cstart[s] as usize..cstart[s + 1] as usize];
                if cs.len() > 1 {
                    cs.sort_unstable_by_key(|c| c.ts);
                }
                process_person(ev, cs, pname[k], &mut w, ws, day0, ndays, organic);
            }
            tproc += tg.elapsed().as_secs_f64();
        }
        if verbose {
            eprintln!("{:>8.1} ms  thread {t} sessions done (group {:.0} ms, sort+process {:.0} ms)", t0.elapsed().as_secs_f64() * 1000.0, tgroup * 1e3, tproc * 1e3);
        }
        let tw = Instant::now();
        if verbose {
            eprintln!("          thread {t} rows {}", w.rows.len());
        }
        write_user_daily(&out_dir.join("user_daily").join(format!("part-{t:03}.parquet")), &w.rows, &w.namebuf, &cats);
        if verbose {
            eprintln!("          thread {t} parquet {:.0} ms", tw.elapsed().as_secs_f64() * 1e3);
        }
        w
    });
    lap("P4 sessions + user_daily written");

    let mut crow = Vec::new();
    for c in 0..ncamp {
        for d in 0..ndays {
            let mut v = [0u64; 4];
            for w in &works {
                let a = &w.camp_acc[c * ndays + d];
                for k in 0..3 {
                    v[k] += a[k];
                }
                v[3] += w.assist[d * ncamp + c] as u64;
            }
            if v[0] > 0 || v[3] > 0 {
                crow.push(CRow { camp: camp_names[c], chan: camp_chan[c], day: (day0 + d as i64) as i32, v });
            }
        }
    }
    write_campaign_daily(&out_dir.join("campaign_daily").join("part-000.parquet"), &crow);
    lap("done");
    if let Some(d) = std::env::var_os("SS_CSV") {
        let users: Vec<(&[URow], &[u8])> = works.iter().map(|w| (&w.rows[..], &w.namebuf[..])).collect();
        dump_csv(std::path::Path::new(&d), &users, &cats, &crow);
    }
    if verbose {
        let urows: usize = works.iter().map(|w| w.rows.len()).sum();
        eprintln!("user rows {urows}, campaign rows {}", crow.len());
    }
    std::process::exit(0);
}
