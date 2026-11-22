#![allow(dead_code)]
use crate::*;
use std::path::Path;
#[derive(Clone, Copy)]
pub struct URow {
    pub name: u64,
    pub day: i32,
    pub top: i32,
    pub sessions: u32,
    pub events: u32,
    pub rev: u64,
}

pub struct PersonWork {
    pub cnt: Vec<u32>,
    pub pos: Vec<u32>,
    pub active: Vec<u32>,
    pub catsum: Vec<u64>,
    pub touched: Vec<u16>,
    pub camp_acc: Vec<[u64; 4]>, // sessions, purchasing, revenue, assisted
    pub rows: Vec<URow>,
    pub namebuf: Vec<u8>,
}

pub struct DayAcc {
    pub day: i64,
    pub sessions: u32,
    pub events: u32,
    pub rev: u64,
}

#[cfg_attr(feature = "prof", inline(never))]
pub fn flush_day(w: &mut PersonWork, acc: &DayAcc, name: u64) {
    if acc.sessions == 0 {
        return;
    }
    let mut top: i32 = -1;
    let mut best: u64 = 0;
    for &c in &w.touched {
        let v = w.catsum[c as usize];
        if v > best || (v == best && (c as i32) < top) {
            best = v;
            top = c as i32;
        }
        w.catsum[c as usize] = 0;
    }
    w.touched.clear();
    w.rows.push(URow { name, day: acc.day as i32, top, sessions: acc.sessions, events: acc.events, rev: acc.rev });
}

/// One person's events (sorted by ts) and clicks (sorted by ts, then click_id).
/// Event ts is ms since window start; click ts is ms since window start - 7 days.
#[cfg_attr(feature = "prof", inline(never))]
pub fn process_person(ev: &[Ev], cs: &[Ck], name_ref: u64, w: &mut PersonWork, ws: i64, day0: i64, ndays: usize, organic: u32) {
    // copy the name once into the thread's sequential buffer; rows refer to it by (offset, len)
    let nm = unpack(name_ref);
    let name = ((w.namebuf.len() as u64) << 16) | nm.len() as u64;
    w.namebuf.extend_from_slice(nm);
    let cbase = ws - WEEK;
    let mut lo = 0usize;
    let mut hi = 0usize;
    let mut acc = DayAcc { day: i64::MIN, sessions: 0, events: 0, rev: 0 };
    let mut k = 0;
    while k < ev.len() {
        let s_rel = ev[k].ts as i64;
        let s = ws + s_rel;
        let day = s.div_euclid(DAY);
        if day != acc.day {
            flush_day(w, &acc, name);
            acc = DayAcc { day, sessions: 0, events: 0, rev: 0 };
        }
        let mut last = s_rel;
        let mut m = k;
        let mut rev = 0u64;
        let mut purch = false;
        while m < ev.len() && (ev[m].ts as i64) - last <= GAP {
            let e = &ev[m];
            last = e.ts as i64;
            let price = (e.pc & 0x00FF_FFFF) as u64;
            if price != 0 {
                rev += price;
                purch = true;
                let c = (e.pc >> 24) as usize;
                if w.catsum[c] == 0 {
                    w.touched.push(c as u16);
                }
                w.catsum[c] += price;
            }
            m += 1;
        }
        acc.sessions += 1;
        acc.events += (m - k) as u32;
        acc.rev += rev;
        k = m;
        // clicks with abs ts <= s  <=>  rel ts <= s - cbase
        let up = s - cbase;
        let down = s - WEEK - cbase; // clicks with rel ts < down are older than 7 days
        while hi < cs.len() && (cs[hi].ts as i64) <= up {
            let c = cs[hi].camp as usize;
            if w.cnt[c] == 0 {
                w.pos[c] = w.active.len() as u32;
                w.active.push(c as u32);
            }
            w.cnt[c] += 1;
            hi += 1;
        }
        while lo < hi && (cs[lo].ts as i64) < down {
            let c = cs[lo].camp as usize;
            w.cnt[c] -= 1;
            if w.cnt[c] == 0 {
                let p = w.pos[c] as usize;
                let lastc = *w.active.last().unwrap();
                w.active[p] = lastc;
                w.pos[lastc as usize] = p as u32;
                w.active.pop();
            }
            lo += 1;
        }
        let di = (day - day0) as usize;
        let credited = if hi > lo {
            // latest click; on a ts tie the byte-wise smallest click_id wins. Clicks are sorted by
            // ts only, so resolve the (rare) tie here, comparing strings only if campaigns differ.
            let t = cs[hi - 1].ts;
            let mut q = hi - 1;
            let mut same = true;
            while q > lo && cs[q - 1].ts == t {
                q -= 1;
                same &= cs[q].camp == cs[hi - 1].camp;
            }
            if same {
                cs[hi - 1].camp
            } else {
                let mut best = q;
                for j in q + 1..hi {
                    if unpack(cs[j].cid) < unpack(cs[best].cid) {
                        best = j;
                    }
                }
                cs[best].camp
            }
        } else {
            organic
        };
        let a = &mut w.camp_acc[credited as usize * ndays + di];
        a[0] += 1;
        a[1] += purch as u64;
        a[2] += rev;
        for &c in &w.active {
            w.camp_acc[c as usize * ndays + di][3] += 1;
        }
    }
    flush_day(w, &acc, name);
    for &c in &w.active {
        w.cnt[c as usize] = 0;
    }
    w.active.clear();
}

pub fn write_user_daily(path: &Path, rows: &[URow], namebuf: &[u8], cats: &[&'static [u8]]) {
    let nb: &'static [u8] = unsafe { std::mem::transmute::<&[u8], &'static [u8]>(namebuf) };
    let mut w = pq::StreamWriter::create(path, rows.len()).expect("create user_daily");
    w.col_str("user_id", |i| {
        let r = rows[i].name;
        &nb[(r >> 16) as usize..((r >> 16) + (r & 0xFFFF)) as usize]
    })
    .unwrap();
    w.col_i32("day", true, |i| rows[i].day).unwrap();
    w.col_i32("sessions", false, |i| rows[i].sessions as i32).unwrap();
    w.col_i32("events", false, |i| rows[i].events as i32).unwrap();
    if rows.iter().all(|r| r.rev <= i32::MAX as u64) {
        w.col_i32("revenue_cents", false, |i| rows[i].rev as i32).unwrap();
    } else {
        w.col_i64("revenue_cents", |i| rows[i].rev as i64).unwrap();
    }
    w.col_opt_dict("top_category", cats, |i| if rows[i].top >= 0 { Some(rows[i].top as u32) } else { None }).unwrap();
    w.finish().expect("write user_daily");
}

pub struct CRow {
    pub camp: &'static [u8],
    pub chan: &'static [u8],
    pub day: i32,
    pub v: [u64; 4],
}

pub fn write_campaign_daily(path: &Path, rows: &[CRow]) {
    let camp: Vec<&[u8]> = rows.iter().map(|r| r.camp).collect();
    let chan: Vec<&[u8]> = rows.iter().map(|r| r.chan).collect();
    let days: Vec<i32> = rows.iter().map(|r| r.day).collect();
    let v: Vec<Vec<i64>> = (0..4).map(|k| rows.iter().map(|r| r.v[k] as i64).collect()).collect();
    let cols = [
        pq::Column { name: "campaign_id", data: pq::Data::Str(&camp), date: false },
        pq::Column { name: "channel", data: pq::Data::Str(&chan), date: false },
        pq::Column { name: "day", data: pq::Data::I32(&days), date: true },
        pq::Column { name: "sessions", data: pq::Data::I64(&v[0]), date: false },
        pq::Column { name: "purchasing_sessions", data: pq::Data::I64(&v[1]), date: false },
        pq::Column { name: "revenue_cents", data: pq::Data::I64(&v[2]), date: false },
        pq::Column { name: "assisted_sessions", data: pq::Data::I64(&v[3]), date: false },
    ];
    pq::write(path, &cols, rows.len()).expect("write campaign_daily");
}

/// Sort events by ts: LSD radix (3 x 11 bits) for large slices, comparison sort otherwise.
#[cfg_attr(feature = "prof", inline(never))]
pub fn sort_events(ev: &mut [Ev], tmp: &mut Vec<Ev>) {
    let n = ev.len();
    if n < 2048 {
        ev.sort_unstable_by_key(|e| e.ts);
        return;
    }
    tmp.clear();
    tmp.reserve(n);
    unsafe { tmp.set_len(n) };
    let mut cnt = [[0u32; 2048]; 3];
    for e in ev.iter() {
        let t = e.ts;
        cnt[0][(t & 2047) as usize] += 1;
        cnt[1][((t >> 11) & 2047) as usize] += 1;
        cnt[2][(t >> 22) as usize & 2047] += 1;
    }
    for c in cnt.iter_mut() {
        let mut acc = 0u32;
        for x in c.iter_mut() {
            let v = *x;
            *x = acc;
            acc += v;
        }
    }
    // pass 0: ev -> tmp, pass 1: tmp -> ev, pass 2: ev -> tmp, then copy back
    for e in ev.iter() {
        let d = &mut cnt[0][(e.ts & 2047) as usize];
        tmp[*d as usize] = *e;
        *d += 1;
    }
    for e in tmp.iter() {
        let d = &mut cnt[1][((e.ts >> 11) & 2047) as usize];
        ev[*d as usize] = *e;
        *d += 1;
    }
    for e in ev.iter() {
        let d = &mut cnt[2][(e.ts >> 22) as usize & 2047];
        tmp[*d as usize] = *e;
        *d += 1;
    }
    ev.copy_from_slice(tmp);
}
