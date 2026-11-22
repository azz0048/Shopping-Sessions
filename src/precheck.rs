//! Run-contiguity pre-check for the v2b event stage.
//!
//! v2b buckets events by event-ID sequence number and assumes every person's
//! kept events form one contiguous run (stitched across bucket boundaries).
//! That breaks when the only surviving copy of a duplicated event names a
//! different person than the dropped original did (rules.md: "the later
//! copies name a different user"; the original can be lost as a malformed
//! line). Stage 3 then sees one person in two runs and falls back.
//!
//! This pass finds such users before stage 3 and repairs the layout without
//! changing the answer: for each user with more than one run, the largest run
//! group stays in place; the records of the other runs are moved to the
//! existing EXTRA side list (`g.extra`, merged by time into the user's main
//! run, or processed alone, or routed to the complex path, exactly like odd
//! events from stage 2) and every copy of those slots is invalidated in place
//! so stage 3 ignores it. Per rules.md a person's result depends only on the
//! set of their kept events, never on where they sat in the files.
//!
//! The dedup here mirrors the stage 3 scatter (smallest ingest wins, first
//! seen on ties) and runs are formed over the same kept records (occupied,
//! valid ts, not odd). If anything still disagrees, stage 3's own check
//! remains and the fallback goes to v3b.
use crate::*;
use std::cell::UnsafeCell;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[derive(Clone, Copy)]
struct Run {
    user: u32,
    n: u32,
    s0: u16,
    s1: u16,
}

struct Shared {
    runs: Vec<UnsafeCell<Vec<Run>>>,
    conflicts: std::sync::Mutex<Vec<u32>>,
}
unsafe impl Sync for Shared {}

pub static ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
pub static N_FIXED: AtomicUsize = AtomicUsize::new(0);

#[inline(always)]
fn user_of(w1: u64) -> u32 {
    ((w1 >> 37) & 0x7FF_FFFF) as u32
}
#[inline(always)]
fn kept(w0: u64, w1: u64) -> bool {
    (w0 >> 33) != INVALID_TS && (w1 >> 63) == 0
}

/// Calls `f(ptr, rec)` for every raw record of bucket `b`, in stage 3 order.
/// `ptr` points at the record in the parsed chunk; `wide` tells its format.
#[inline(always)]
unsafe fn each_rec(g: &G, b: usize, mut f: impl FnMut(*mut u8, bool, Rec)) {
    for th in 0..g.nt {
        let part = &g.t(th).ev;
        part.for_each_raw(b, |p, n| {
            for i in 0..n {
                let e = p.add(crate::parse::EV_REC * i) as *mut u8;
                let r = if crate::parse::R12 { crate::parse::dec12(e) } else { *(e as *const Rec) };
                f(e, !crate::parse::R12, r);
            }
        });
        part.for_each_esc_raw(b, |p, n| {
            for i in 0..n {
                let e = p.add(i);
                f(e as *mut u8, true, e.read());
            }
        });
    }
}

/// Dedup bucket `b` into `tag/ing/rec` (generation `gen`); returns true if any record.
#[inline(always)]
unsafe fn dedup(g: &G, b: usize, gen: u32, tag: &mut [u32], rec: &mut [Rec]) -> bool {
    let mut any = false;
    each_rec(g, b, |_, _, r| {
        any = true;
        let s = (r.w1 & (BSIZE as u64 - 1)) as usize;
        let t = tag.get_unchecked_mut(s);
        let o = rec.get_unchecked_mut(s);
        if *t != gen || ((r.w1 >> 16) & ING_MAX) < ((o.w1 >> 16) & ING_MAX) {
            *t = gen;
            *o = r;
        }
    });
    any
}

// Packed slot for detect(): (0xFFFF - gen) << 48 | ingest << 24 | (user + 1 if
// kept, else 0). The generation is stored descending so one `min` both
// replaces a stale slot and keeps the smallest ingest. (Equal ingest: rules.md
// says such copies are identical; if not, detection may differ from stage 3,
// which only costs a missed or a harmless extra diversion, both still exact.)
const PK_USER: u64 = (1 << 24) - 1;

/// Same winner as `dedup` (smallest ingest, first seen on ties), but only
/// what run detection needs, in one u64 per slot (256 KB, stays in L2).
#[inline(always)]
unsafe fn dedup_packed(g: &G, b: usize, gen: u64, sl: &mut [u64]) -> bool {
    let mut any = false;
    let sp = sl.as_mut_ptr();
    let mut put = |slot: usize, ing: u64, ukept: u64| {
        let p = sp.add(slot);
        *p = (*p).min((0xFFFF - gen) << 48 | ing << 24 | ukept);
    };
    for th in 0..g.nt {
        let part = &g.t(th).ev;
        part.for_each_raw(b, |p, n| {
            any |= n > 0;
            for i in 0..n {
                let e = p.add(crate::parse::EV_REC * i);
                if crate::parse::R12 {
                    let q = (e as *const u64).read_unaligned();
                    let h = (e.add(8) as *const u32).read_unaligned() as u64;
                    let w1 = q & crate::parse::R12_M59;
                    let kept = (h >> 1) != INVALID_TS && (w1 >> 63) == 0;
                    let u = user_of(w1) as u64;
                    put((w1 & (BSIZE as u64 - 1)) as usize, (w1 >> 16) & ING_MAX, if kept && u < PK_USER { u + 1 } else { 0 });
                } else {
                    let r = *(e as *const Rec);
                    let u = user_of(r.w1) as u64;
                    put((r.w1 & (BSIZE as u64 - 1)) as usize, (r.w1 >> 16) & ING_MAX, if kept(r.w0, r.w1) && u < PK_USER { u + 1 } else { 0 });
                }
            }
        });
        part.for_each_esc_raw(b, |p, n| {
            any |= n > 0;
            for i in 0..n {
                let r = p.add(i).read();
                let u = user_of(r.w1) as u64;
                put((r.w1 & (BSIZE as u64 - 1)) as usize, (r.w1 >> 16) & ING_MAX, if kept(r.w0, r.w1) && u < PK_USER { u + 1 } else { 0 });
            }
        });
    }
    any
}

/// Lists every bucket's runs and the users claimed by more than one run.
fn detect(g: &G) -> (Vec<Vec<Run>>, HashSet<u32>) {
    let nb = g.nb;
    let words = (g.ucap as usize >> 6) + 1;
    let claim: Vec<AtomicU64> = (0..words).map(|_| AtomicU64::new(0)).collect();
    let sh = Shared { runs: (0..nb).map(|_| UnsafeCell::new(Vec::new())).collect(), conflicts: std::sync::Mutex::new(Vec::new()) };
    let next = AtomicUsize::new(0);
    let shr: &Shared = &sh;
    let utab = g.utab as usize;
    let complex = |u: u32| unsafe { *(utab as *const u16).add(u as usize) } & COMPLEX != 0;
    let do_claim = |u: u32| {
        if complex(u) {
            return; // complex users never claim; any number of runs is fine
        }
        let bit = 1u64 << (u & 63);
        if claim[u as usize >> 6].fetch_or(bit, Ordering::Relaxed) & bit != 0 {
            shr.conflicts.lock().unwrap().push(u);
        }
    };
    sys::run_par(g.nt, |_tid| unsafe {
        use std::arch::x86_64::*;
        let mut sl = vec![u64::MAX; BSIZE];
        let mut gen = 0u64;
        loop {
            let b = next.fetch_add(1, Ordering::Relaxed);
            if b >= nb {
                break;
            }
            gen += 1;
            if gen == 0xFFFF {
                sl.iter_mut().for_each(|x| *x = u64::MAX);
                gen = 1;
            }
            if !dedup_packed(g, b, gen, &mut sl) {
                continue;
            }

            let runs = &mut *shr.runs[b].get();
            // Scan 8 slots at a time: a slot matters if it was written for
            // this bucket and holds a kept record whose user differs from
            // the current run's user (a run start), or continues the run.
            let genv = _mm512_set1_epi64(((0xFFFF - gen) << 48) as i64);
            let hi16 = _mm512_set1_epi64((0xFFFFu64 << 48) as i64);
            let um = _mm512_set1_epi64(PK_USER as i64);
            let zero = _mm512_setzero_si512();
            let iota = _mm512_setr_epi64(0, 1, 2, 3, 4, 5, 6, 7);
            let mut cur = Run { user: u32::MAX, n: 0, s0: 0, s1: 0 };
            let mut cu1 = 0u64; // user + 1 of the current run (0: none)
            let mut s = 0usize;
            while s < BSIZE {
                let v = _mm512_loadu_si512(sl.as_ptr().add(s) as *const _);
                let mine = _mm512_cmpeq_epi64_mask(_mm512_and_si512(v, hi16), genv);
                let u1 = _mm512_and_si512(v, um);
                let live = mine & _mm512_cmpneq_epi64_mask(u1, zero);
                if live != 0 {
                    // Dense list of this vector's live users and slots; a run
                    // starts wherever a user differs from its predecessor.
                    let cnt = live.count_ones() as usize;
                    let du = _mm512_maskz_compress_epi64(live, u1);
                    let prevu = _mm512_alignr_epi64::<7>(du, _mm512_set1_epi64(cu1 as i64));
                    let chg = _mm512_cmpneq_epi64_mask(du, prevu) as u32 & ((1u32 << cnt) - 1);
                    if chg == 0 {
                        cur.n += cnt as u32;
                        cur.s1 = (s + 7 - live.leading_zeros() as usize) as u16;
                    } else {
                        let mut ub = [0u64; 8];
                        let mut sb = [0u64; 8];
                        _mm512_storeu_si512(ub.as_mut_ptr() as *mut _, du);
                        _mm512_storeu_si512(sb.as_mut_ptr() as *mut _, _mm512_maskz_compress_epi64(live, _mm512_add_epi64(_mm512_set1_epi64(s as i64), iota)));
                        let mut at = 0usize; // dense index where `cur` resumes in this vector
                        let mut m = chg;
                        while m != 0 {
                            let i = m.trailing_zeros() as usize;
                            m &= m - 1;
                            if i > at {
                                cur.n += (i - at) as u32;
                                cur.s1 = sb[i - 1] as u16;
                            }
                            if cu1 != 0 {
                                runs.push(cur);
                            }
                            cu1 = ub[i];
                            cur = Run { user: (ub[i] - 1) as u32, n: 1, s0: sb[i] as u16, s1: sb[i] as u16 };
                            at = i + 1;
                        }
                        if cnt > at {
                            cur.n += (cnt - at) as u32;
                            cur.s1 = sb[cnt - 1] as u16;
                        }
                    }
                }
                s += 8;
            }
            if cu1 != 0 {
                runs.push(cur);
            }
            for r in runs.iter().skip(1) {
                do_claim(r.user);
            }
        }
    });
    // First runs, in bucket order: a run continuing the previous non-empty
    // bucket's last run is the same run (stage 3 stitches it), so no claim.
    let mut prev: Option<u32> = None;
    for b in 0..nb {
        let runs = unsafe { &*sh.runs[b].get() };
        let Some(first) = runs.first() else { continue };
        if prev != Some(first.user) {
            do_claim(first.user);
        }
        prev = Some(runs.last().unwrap().user);
    }
    let conflicts: HashSet<u32> = sh.conflicts.into_inner().unwrap().into_iter().collect();
    (sh.runs.into_iter().map(|c| c.into_inner()).collect(), conflicts)
}

/// For each conflicted user: its run groups (stitched across buckets); all
/// but the largest are diverted, if their size is at most `limit`.
fn plan(runs: &[Vec<Run>], conflicts: &HashSet<u32>, limit: u64) -> Vec<(usize, Vec<Run>)> {
    let mut groups: HashMap<u32, Vec<(u64, Vec<(usize, Run)>)>> = HashMap::new();
    let mut prev: Option<u32> = None;
    for (b, rs) in runs.iter().enumerate() {
        for (i, r) in rs.iter().enumerate() {
            if conflicts.contains(&r.user) {
                let gs = groups.entry(r.user).or_default();
                let cont = i == 0 && prev == Some(r.user);
                if !cont || gs.is_empty() {
                    gs.push((0, Vec::new()));
                }
                let gl = gs.last_mut().unwrap();
                gl.0 += r.n as u64;
                gl.1.push((b, *r));
            }
        }
        if let Some(l) = rs.last() {
            prev = Some(l.user);
        }
    }
    let mut divert: HashMap<usize, Vec<Run>> = HashMap::new();
    for gs in groups.values() {
        let main = (0..gs.len()).max_by_key(|&i| (gs[i].0, std::cmp::Reverse(i))).unwrap();
        for (i, (n, rs)) in gs.iter().enumerate() {
            if i != main && *n <= limit {
                for &(b, r) in rs {
                    divert.entry(b).or_default().push(r);
                }
            }
        }
    }
    let mut v: Vec<(usize, Vec<Run>)> = divert.into_iter().collect();
    v.sort_unstable_by_key(|x| x.0);
    v
}

/// For each conflicted user, the runs (of any users) lying between two of
/// its run groups are diverted when that stretch holds at most `limit`
/// records: removing them rejoins the user's run.
fn plan_gaps(runs: &[Vec<Run>], conflicts: &HashSet<u32>, limit: u64) -> Vec<(usize, Vec<Run>)> {
    let flat: Vec<(usize, Run)> = runs.iter().enumerate().flat_map(|(b, rs)| rs.iter().map(move |r| (b, *r))).collect();
    let mut last: HashMap<u32, usize> = HashMap::new();
    let mut take = vec![false; flat.len()];
    for (i, &(_, r)) in flat.iter().enumerate() {
        if !conflicts.contains(&r.user) {
            continue;
        }
        if let Some(&j) = last.get(&r.user) {
            if j + 1 < i {
                let n: u64 = flat[j + 1..i].iter().map(|x| x.1.n as u64).sum();
                if n <= limit {
                    take[j + 1..i].iter_mut().for_each(|t| *t = true);
                }
            }
        }
        last.insert(r.user, i);
    }
    let mut divert: HashMap<usize, Vec<Run>> = HashMap::new();
    for (i, &(b, r)) in flat.iter().enumerate() {
        if take[i] {
            divert.entry(b).or_default().push(r);
        }
    }
    let mut v: Vec<(usize, Vec<Run>)> = divert.into_iter().collect();
    v.sort_unstable_by_key(|x| x.0);
    v
}

/// Moves the diverted runs' winning records to the EXTRA list and
/// invalidates every copy of their slots. Returns the number moved.
fn apply(g: &mut G, divert: &[(usize, Vec<Run>)]) -> usize {
    let mut tag = vec![0u32; BSIZE];
    let mut rec = vec![Rec { w0: 0, w1: 0 }; BSIZE];
    let mut kill = vec![false; BSIZE];
    let mut nfix = 0usize;
    for (k, (b, runs)) in divert.iter().enumerate() {
        let (b, gen) = (*b, k as u32 + 1);
        unsafe { dedup(g, b, gen, &mut tag, &mut rec) };
        kill.iter_mut().for_each(|x| *x = false);
        for r in runs {
            for s in r.s0 as usize..=r.s1 as usize {
                let x = rec[s];
                if tag[s] == gen && kept(x.w0, x.w1) && user_of(x.w1) == r.user {
                    kill[s] = true;
                    unsafe { *g.utab.add(r.user as usize) |= EXTRA };
                    g.extra.entry(r.user).or_default().push(x.w0);
                    nfix += 1;
                }
            }
        }
        unsafe {
            each_rec(g, b, |p, wide, r| {
                if kill[(r.w1 & (BSIZE as u64 - 1)) as usize] {
                    if wide {
                        let w0 = p as *mut u64;
                        w0.write(w0.read() | INVALID_TS << 33);
                    } else {
                        let h = p.add(8) as *mut u32;
                        h.write_unaligned(h.read_unaligned() | 0xFFFF_FFFE);
                    }
                }
            });
        }
    }
    nfix
}

pub fn run(g: &mut G) {
    // Off unless asked for: on data without strays it is pure overhead. When
    // stage 3 meets a user in two runs, the process re-executes itself with
    // XL_PRECHECK=1 (see sys::fallback), so the cost is only paid when needed.
    if !ON.load(Ordering::Relaxed) || !std::env::var("XL_PRECHECK").is_ok_and(|v| v == "1") || g.nb == 0 {
        return;
    }
    let verbose = g.check || std::env::var("XL_TIMING").is_ok_and(|v| v == "1");
    let mut moved = 0usize;
    // Round 0 removes small strays sitting inside a user's run (typically one
    // record), which rejoins it. Round 1 moves small leftover groups (a stray
    // at a run boundary), round 2 everything else; round 3 only checks.
    for round in 0..4 {
        let (runs, conflicts) = detect(g);
        if verbose {
            eprintln!("[precheck] round {round}: {} users in more than one run", conflicts.len());
        }
        if conflicts.is_empty() {
            break;
        }
        if round == 3 {
            break; // stage 3 will notice and retry in v3b
        }
        let divert = match round {
            0 => plan_gaps(&runs, &conflicts, 64),
            1 => plan(&runs, &conflicts, 64),
            _ => plan(&runs, &conflicts, u64::MAX),
        };
        moved += apply(g, &divert);
    }
    if moved > 0 {
        for v in g.extra.values_mut() {
            v.sort_unstable();
        }
    }
    N_FIXED.store(moved, Ordering::Relaxed);
    if verbose {
        eprintln!("[precheck] moved {moved} records to the extra list");
    }
}
