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
}

pub struct DayAcc {
    pub day: i64,
    pub sessions: u32,
    pub events: u32,
    pub rev: u64,
}

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
pub fn process_person(ev: &[Ev], cs: &[Ck], w: &mut PersonWork, ws: i64, day0: i64, ndays: usize, organic: u32) {
    let name = ev[0].name;
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
            let t = cs[hi - 1].ts;
            let mut q = hi - 1;
            while q > lo && cs[q - 1].ts == t {
                q -= 1;
            }
            cs[q].camp
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

pub fn write_user_daily(path: &Path, rows: &[URow], cats: &[&'static [u8]]) {
    let names: Vec<&[u8]> = rows.iter().map(|r| unpack(r.name)).collect();
    let days: Vec<i32> = rows.iter().map(|r| r.day).collect();
    let sess: Vec<i32> = rows.iter().map(|r| r.sessions as i32).collect();
    let evs: Vec<i32> = rows.iter().map(|r| r.events as i32).collect();
    let rev: Vec<i64> = rows.iter().map(|r| r.rev as i64).collect();
    let top: Vec<Option<&[u8]>> = rows.iter().map(|r| if r.top >= 0 { Some(cats[r.top as usize]) } else { None }).collect();
    let cols = [
        pq::Column { name: "user_id", data: pq::Data::Str(&names), date: false },
        pq::Column { name: "day", data: pq::Data::I32(&days), date: true },
        pq::Column { name: "sessions", data: pq::Data::I32(&sess), date: false },
        pq::Column { name: "events", data: pq::Data::I32(&evs), date: false },
        pq::Column { name: "revenue_cents", data: pq::Data::I64(&rev), date: false },
        pq::Column { name: "top_category", data: pq::Data::OptStr(&top), date: false },
    ];
    pq::write(path, &cols, rows.len()).expect("write user_daily");
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
