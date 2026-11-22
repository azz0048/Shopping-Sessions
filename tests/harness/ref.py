#!/usr/bin/env python3
"""Independent, deliberately plain reference implementation of rules.md.

Usage: ref.py DATA_DIR OUT_DIR
Writes OUT_DIR/user_daily.csv and OUT_DIR/campaign_daily.csv in canonical
(sorted, tab-separated) form for comparison.
"""
import glob
import json
import os
import sys
from bisect import bisect_right
from collections import defaultdict

DAY = 86_400_000
WEEK = 604_800_000
GAP = 1_800_000
DIG = set(b"0123456789")


def lines_of(path):
    with open(path, "rb") as f:
        data = f.read()
    parts = data.split(b"\n")
    if parts and parts[-1] == b"":
        parts.pop()  # trailing newline terminates the last line
    return parts[1:]  # header row


def split(line, n):
    if line == b"":
        return None
    fs = line.split(b",")
    if len(fs) != n:
        return None
    out = []
    for f in fs:
        if len(f) >= 2 and f[0:1] == b'"' and f[-1:] == b'"':
            f = f[1:-1]
        if b'"' in f:
            return None
        out.append(f)
    return out


def isdig(b):
    return len(b) > 0 and all(c in DIG for c in b)


def dim(y, m):
    if m in (1, 3, 5, 7, 8, 10, 12):
        return 31
    if m in (4, 6, 9, 11):
        return 30
    leap = y % 4 == 0 and (y % 100 != 0 or y % 400 == 0)
    return 29 if leap else 28


def civil_days(y, m, d):
    y -= m <= 2
    era = y // 400
    yoe = y - era * 400
    doy = (153 * ((m + 9) % 12) + 2) // 5 + d - 1
    doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
    return era * 146097 + doe - 719468


def parse_ts(s):
    """Return epoch ms or None for a bad timestamp."""
    if s == b"":
        return None
    if isdig(s):
        return int(s)  # may be huge -> simply out of window
    if len(s) not in (20, 25):
        return None
    if not (s[4:5] == b"-" and s[7:8] == b"-" and s[10:11] == b"T" and s[13:14] == b":" and s[16:17] == b":"):
        return None
    parts = [s[0:4], s[5:7], s[8:10], s[11:13], s[14:16], s[17:19]]
    if not all(isdig(p) for p in parts):
        return None
    y, mo, d, h, mi, se = (int(p) for p in parts)
    if not (1 <= mo <= 12) or d < 1 or d > dim(y, mo) or h > 23 or mi > 59 or se > 59:
        return None
    t = ((civil_days(y, mo, d) * 24 + h) * 60 + mi) * 60 + se
    if len(s) == 20:
        if s[19:20] != b"Z":
            return None
    else:
        sg = s[19:20]
        if sg not in (b"+", b"-") or s[22:23] != b":":
            return None
        oh, om = s[20:22], s[23:25]
        if not (isdig(oh) and isdig(om)):
            return None
        oh, om = int(oh), int(om)
        if oh > 23 or om > 59:
            return None
        off = oh * 3600 + om * 60
        t -= off if sg == b"+" else -off
    return t * 1000


def csvs(d, sub):
    return sorted(glob.glob(os.path.join(d, sub, "*.csv")))


def find(par, x):
    while par[x] != x:
        par[x] = par[par[x]]
        x = par[x]
    return x


def main(data, out):
    man = json.load(open(os.path.join(data, "manifest.json")))
    ws = parse_ts(man["window_start"].encode())
    we = parse_ts(man["window_end"].encode())

    # lookups
    prod = {}
    for ln in lines_of(os.path.join(data, "products.csv")):
        f = split(ln, 2)
        if f:
            prod[f[0]] = f[1]
    camp = {}
    for ln in lines_of(os.path.join(data, "campaigns.csv")):
        f = split(ln, 2)
        if f:
            camp[f[0]] = f[1]

    # links -> union find
    par = {}
    for p in csvs(data, "links"):
        for ln in lines_of(p):
            f = split(ln, 2)
            if not f or f[0] == b"" or f[1] == b"":
                continue
            a, b = f
            par.setdefault(a, a)
            par.setdefault(b, b)
            ra, rb = find(par, a), find(par, b)
            if ra != rb:
                par[ra] = rb
    name = {}
    for x in par:
        r = find(par, x)
        if r not in name or x < name[r]:
            name[r] = x

    def person(u):
        if u in par:
            return name[find(par, u)]
        return u

    # events: parse + dedup (smallest ingest)
    kept = {}
    for p in csvs(data, "events"):
        for ln in lines_of(p):
            f = split(ln, 7)
            if not f or f[0] == b"":
                continue
            ing = parse_ts(f[3])
            if ing is None:
                # rules: ingest_ts always valid F1 on well-formed lines
                raise SystemExit("bad ingest_ts %r" % ln)
            cur = kept.get(f[0])
            if cur is None or ing < cur[0]:
                kept[f[0]] = (ing, f)
    per = defaultdict(list)  # person -> [(ts, type, price, cat)]
    for eid, (ing, f) in kept.items():
        _, uid, tss, _, et, pid, pr = f
        if uid == b"" or pid == b"":
            continue
        ts = parse_ts(tss)
        if ts is None or not (ws <= ts < we):
            continue
        if et not in (b"view", b"cart", b"purchase"):
            continue
        price = 0
        if et == b"purchase":
            if not isdig(pr):
                continue
            price = int(pr)
            if not (1 <= price <= 10_000_000):
                continue
        if ing - ts > DAY:
            continue
        per[person(uid)].append((ts, et == b"purchase", price, prod.get(pid, b"unknown")))

    # clicks
    clicks = {}
    for p in csvs(data, "clicks"):
        for ln in lines_of(p):
            f = split(ln, 4)
            if not f or f[0] == b"":
                continue
            clicks.setdefault(f[0], f)
    pclicks = defaultdict(list)  # person -> [(ts, click_id, campaign)]
    for cid, f in clicks.items():
        _, uid, cmp, tss = f
        if uid == b"":
            continue
        ts = parse_ts(tss)
        if ts is None or not (ws - WEEK <= ts < we):
            continue
        if cmp not in camp:
            continue
        pclicks[person(uid)].append((ts, cid, cmp))
    for v in pclicks.values():
        v.sort()

    ud = {}
    cd = defaultdict(lambda: [0, 0, 0, 0])
    for pn, evs in per.items():
        evs.sort(key=lambda e: e[0])
        sessions = []
        prev = None
        for e in evs:
            if prev is None or e[0] - prev > GAP:
                sessions.append([])
            sessions[-1].append(e)
            prev = e[0]
        cl = pclicks.get(pn, [])
        cts = [c[0] for c in cl]
        for s in sessions:
            start = s[0][0]
            day = start // DAY
            hi = bisect_right(cts, start)
            cands = [c for c in cl[:hi] if start - c[0] <= WEEK]
            rev = sum(e[2] for e in s)
            purch = any(e[1] for e in s)
            if cands:
                best_ts = max(c[0] for c in cands)
                win = min(c[1] for c in cands if c[0] == best_ts)
                cred = [c for c in cands if c[1] == win][0][2]
            else:
                cred = b"organic"
            r = cd[(cred, day)]
            r[0] += 1
            r[1] += purch
            r[2] += rev
            for a in set(c[2] for c in cands):
                cd[(a, day)][3] += 1
            u = ud.setdefault((pn, day), [0, 0, 0, defaultdict(int)])
            u[0] += 1
            u[1] += len(s)
            u[2] += rev
            for e in s:
                if e[1]:
                    u[3][e[3]] += e[2]

    os.makedirs(out, exist_ok=True)
    rows = []
    for (pn, day), (ns, ne, rv, cats) in ud.items():
        top = b"\\N"
        if cats:
            best = max(cats.values())
            top = min(c for c, v in cats.items() if v == best)
        rows.append(b"\t".join([pn, str(day).encode(), str(ns).encode(), str(ne).encode(), str(rv).encode(), top]))
    rows.sort()
    with open(os.path.join(out, "user_daily.csv"), "wb") as fo:
        fo.write(b"".join(r + b"\n" for r in rows))
    rows = []
    for (c, day), (ns, npur, rv, asst) in cd.items():
        ch = b"organic" if c == b"organic" else camp[c]
        rows.append(b"\t".join([c, ch, str(day).encode(), str(ns).encode(), str(npur).encode(), str(rv).encode(), str(asst).encode()]))
    rows.sort()
    with open(os.path.join(out, "campaign_daily.csv"), "wb") as fo:
        fo.write(b"".join(r + b"\n" for r in rows))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
