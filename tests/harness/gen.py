#!/usr/bin/env python3
"""Synthetic dataset generator modelled on rules.md.

gen.py --seed N --events E --clicks C --output DIR [--nasty]

IDs follow the scheme the fast path expects ('u'/'e'/'k' + base36 of a
40-bit scramble), so the optimized path is exercised. --nasty adds extra
traps (\r endings, odd IDs, unquoted oddities) beyond what the real
generator writes, to hit fallback/odd-row paths.
"""
import argparse
import datetime as dt
import json
import os
import random

MASK = (1 << 40) - 1
A = 0x9E3779B97F4B
B = 0xD6E8FEB86659
KIOSK = 1 << 39
DAY = 86_400_000
WEEK = 7 * DAY
D36 = "0123456789abcdefghijklmnopqrstuvwxyz"
CATS = ["apparel", "beauty", "books", "electronics", "garden", "grocery", "home", "jewelry", "kids", "kitchen",
        "music", "office", "outdoors", "pets", "shoes", "sports", "tools", "toys", "travel", "video"]
CHANNELS = ["affiliate", "display", "email", "search", "social"]


def scramble(v, key):
    m = v & MASK
    m = ((m * A) ^ key) & MASK
    m ^= m >> 17
    return (m * B) & MASK


def b36(v):
    if v == 0:
        return "0"
    s = []
    while v:
        v, r = divmod(v, 36)
        s.append(D36[r])
    return "".join(reversed(s))


def iso(ms):
    return dt.datetime.fromtimestamp(ms // 1000, dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


class Gen:
    def __init__(self, a):
        self.r = random.Random(a.seed)
        self.a = a
        r = self.r
        self.ukey = r.getrandbits(40)
        self.ekey = r.getrandbits(40)
        self.ckey = r.getrandbits(40)
        y = r.randint(2025, 2028)
        start = dt.datetime(y, 1, 1, tzinfo=dt.timezone.utc) + dt.timedelta(days=r.randint(0, 350))
        if a.leap:
            start = dt.datetime(2028, 2, 20, tzinfo=dt.timezone.utc)
        self.ws = int(start.timestamp()) * 1000
        self.we = self.ws + 14 * DAY
        self.rates = dict(
            dup=r.uniform(0.02, 0.04), cdup=r.uniform(0.01, 0.03), badts=r.uniform(0.003, 0.007),
            late=r.uniform(0.005, 0.015),
        )

    # ---- formatting helpers
    def uid(self, v):
        return "u" + b36(scramble(v, self.ukey))

    def fmt_ts(self, ms):
        r = self.r
        if ms % 1000:
            return str(ms) if r.random() > 0.05 else "000" + str(ms)
        x = r.random()
        if x < 0.58:
            return iso(ms)
        if x < 0.68:
            off = r.randint(-40, 48) * 15  # minutes, -10:00..+12:00
            t = dt.datetime.fromtimestamp(ms // 1000, dt.timezone(dt.timedelta(minutes=off)))
            sg = "+" if off >= 0 else "-"
            if off == 0 and r.random() < 0.5:
                sg = "-"
            return t.strftime("%Y-%m-%dT%H:%M:%S") + "%s%02d:%02d" % (sg, abs(off) // 60, abs(off) % 60)
        return str(ms) if r.random() > 0.1 else "0" * r.randint(1, 4) + str(ms)

    def bad_ts(self, ms, window):
        r = self.r
        s = iso(ms)
        ws, we = window
        opts = [
            "", s[:-1] + ".123Z", s[:-1] + "z", s.replace("T", "t"), s.replace("T", " "), s[:-1],
            str(ms) + ".0", "1.79e12", "-5", s[:5] + "13" + s[7:], "2027-02-30T10:00:00Z", "2027-02-29T10:00:00Z",
            s[:11] + "24" + s[13:], s[:17] + "60Z", s[:-1] + "+24:00", s[:-1] + "+05:60", s[:-1] + "+0200",
            iso(ws - 1000) if window[0] % 1000 == 0 else str(ws - 1), iso(we), str(we), str(ws - 1),
            "99999999999999999999999", "86400000", "٢٥٠", "２０２６", s[:-1] + "+5:00",
        ]
        if self.a.nasty:
            opts += [s + "\r", str(ms) + "\r", " " + s]
        return r.choice(opts)

    def quote(self, fields):
        r = self.r
        return ",".join('"%s"' % f if r.random() < 0.2 else f for f in fields)

    def run(self):
        a, r = self.a, self.r
        os.makedirs(a.output, exist_ok=True)
        for d in ("events", "clicks", "links"):
            os.makedirs(os.path.join(a.output, d), exist_ok=True)
        with open(os.path.join(a.output, "manifest.json"), "w") as f:
            json.dump({"window_start": iso(self.ws), "window_end": iso(self.we)}, f)
        # products
        nprod = a.products
        prices = [r.randint(100, 50000) for _ in range(nprod)]
        catw = [1 / (i + 1) for i in range(len(CATS))]
        pcat = r.choices(CATS, catw, k=nprod)
        with open(os.path.join(a.output, "products.csv"), "w") as f:
            f.write("product_id,category\n")
            for i in range(nprod):
                f.write("p%d,%s\n" % (i + 1, pcat[i]))
        with open(os.path.join(a.output, "campaigns.csv"), "w") as f:
            f.write("campaign_id,channel\n")
            for i in range(a.campaigns):
                f.write("c_%d,%s\n" % (i + 1, r.choice(CHANNELS)))
        pw = list(__import__('itertools').accumulate(1 / (i + 1) ** 0.9 for i in range(nprod)))

        npeople = max(30, a.events // 30)
        # IDs per person
        people = []
        kiosks = [scramble(KIOSK + k, self.ukey) for k in range(r.randint(3, 6))]
        kiosks = ["u" + b36(k) for k in kiosks]
        nchain = 3 + npeople // 20000
        chainers = set(r.sample(range(5, npeople), min(nchain, npeople - 5)))
        for p in range(npeople):
            if p < 5:
                n = r.randint(3, 6)
            elif p in chainers:
                n = r.randint(50, min(400, 500))
            else:
                n = r.choices([1, 2, 3, 4, 5, 6, 7, 8], [45, 30, 13, 5, 3, 2, 1, 1])[0]
            aliases = list(range(n))
            if p in chainers:
                r.shuffle(aliases)
            ids = [self.uid(p << 9 | al) for al in aliases]
            unused = []
            if r.random() < 0.05:
                unused.append(self.uid(p << 9 | (n + r.randint(0, 3))))
            kio = r.choice(kiosks) if r.random() < 0.004 else None
            people.append((ids, unused, kio, p in chainers))

        # links
        links = []
        for ids, unused, kio, chain in people:
            allids = ids + unused + ([kio] if kio else [])
            for j in range(1, len(allids)):
                other = allids[j - 1] if chain else allids[r.randrange(j)]
                links.append([allids[j], other])
            if r.random() < 0.01:
                links.append([ids[0], ids[0]])
        extra = []
        for l in links:
            if r.random() < 0.08:
                extra.append(l[::-1] if r.random() < 0.5 else l[:])
        links += extra
        for l in links:
            for s in range(2):
                x = r.random()
                if x < 0.002:
                    l[s] = ""
                elif x < 0.0025:
                    l[s] = l[s].upper()
        r.shuffle(links)
        lk_lines = [self.quote(l) for l in links]
        lk_lines = self.malform(lk_lines, 2, 0.0005)

        # events
        weights = [1 / (i + 1) ** 0.7 for i in range(npeople)]
        tot = sum(weights[5:])
        whale_share = [r.uniform(0.01, 0.02) * self.a.whale for _ in range(5)]
        rest = 1 - sum(whale_share)
        counts = [max(1, int(a.events * whale_share[i])) for i in range(5)]
        counts += [max(1, int(a.events * rest * weights[i] / tot)) for i in range(5, npeople)]
        ev_lines = []
        sess_starts = []  # (person, start_ms)
        eseq = 0
        span = self.we - self.ws
        for p, cnt in enumerate(counts):
            ids, unused, kio, chain = people[p]
            useids = ids + ([kio] if kio else [])
            left = cnt
            while left > 0:
                k = min(left, max(1, int(r.expovariate(1 / 6))))
                left -= k
                x = r.random()
                if x < 0.02:  # cross midnight
                    day0 = self.ws + r.randrange(14) * DAY
                    t = day0 + DAY - r.randint(1, 600) * 1000
                else:
                    t = self.ws + r.randrange(span)
                    t -= t % 1000 if r.random() < 0.7 else 0
                if r.random() < 0.003:
                    t = self.ws if r.random() < 0.5 else self.we - 1
                sess_starts.append((p, t))
                for i in range(k):
                    if i:
                        g = r.random()
                        if g < 0.001:
                            t += 1_800_000
                        elif g < 0.002:
                            t += 1_800_001
                        elif g < 0.03:
                            t += 0  # equal timestamps
                        elif g < 0.05:
                            t += r.randint(1_800_000, 1_800_002) - r.randint(0, 2)
                        else:
                            t += r.randint(1, 300) * 1000 + (r.randrange(1000) if r.random() < 0.3 else 0)
                    if t >= self.we + DAY:
                        break
                    u = ids[0] if r.random() < 0.5 else r.choice(useids)
                    ev_lines += self.event(eseq, u, t, people, pw, prices, nprod)
                    eseq += 1
        r.shuffle(ev_lines)
        ev_lines = self.malform(ev_lines, 7, 0.0005)

        # clicks
        ck_lines = []
        cseq = 0
        nwhale = r.randint(3, 5)
        cw = list(__import__('itertools').accumulate(1 / (i + 1) for i in range(a.campaigns)))
        per_sess = max(1, a.clicks // max(1, len(sess_starts)))
        target = a.clicks
        while len(ck_lines) < target:
            p, st = r.choice(sess_starts)
            ids, unused, kio, chain = people[p]
            n = 0 if r.random() < 0.4 else r.randint(1, 2 * per_sess + 1)
            for _ in range(n):
                x = r.random()
                if x < 0.03:
                    t = st - WEEK
                elif x < 0.06:
                    t = st - WEEK - 1
                elif x < 0.09:
                    t = st
                elif x < 0.12:
                    t = st + 1
                else:
                    t = st - r.randrange(WEEK + DAY)
                camp = r.randint(1, nwhale) if r.random() < 0.3 else r.choices(range(1, a.campaigns + 1), cum_weights=cw)[0]
                u = ids[0] if r.random() < 0.5 else r.choice(ids + ([kio] if kio else []))
                ck_lines += self.click(cseq, u, "c_%d" % camp, t)
                cseq += 1
                if r.random() < 0.02:  # same-ms pair
                    camp2 = r.randint(1, a.campaigns)
                    ck_lines += self.click(cseq, u, "c_%d" % camp2, t)
                    cseq += 1
        # some clicks from people with no session
        for _ in range(target // 50):
            p = r.randrange(npeople)
            t = self.ws - WEEK + r.randrange(span + WEEK)
            ck_lines += self.click(cseq, people[p][0][0], "c_%d" % r.randint(1, a.campaigns), t)
            cseq += 1
        r.shuffle(ck_lines)
        ck_lines = self.malform(ck_lines, 4, 0.0005)

        self.write("events", "event_id,user_id,ts,ingest_ts,event_type,product_id,price_cents", ev_lines, a.evfiles)
        self.write("clicks", "click_id,user_id,campaign_id,ts", ck_lines, a.ckfiles)
        self.write("links", "user_id_a,user_id_b", lk_lines, a.lkfiles)

    def event(self, seq, u, t, people, pw, prices, nprod):
        r = self.r
        eid = "e" + b36(scramble(seq, self.ekey))
        x = r.random()
        if x < 0.03:
            et = "purchase"
        elif x < 0.13:
            et = "cart"
        else:
            et = "view"
        pidx = r.choices(range(nprod), cum_weights=pw)[0]
        pid = "p%d" % (pidx + 1)
        price = str(prices[pidx]) if et == "purchase" else ("" if r.random() < 0.9 else str(r.randint(0, 999)))
        if et == "purchase" and r.random() < 0.05:
            price = r.choice(["12.50", "-100", "0", "", "10000001", "+100", "1e3", "100 ", "0250", "00001", "10000000", "٢٥٠"])
        if r.random() < 0.001:
            et = r.choice(["Purchase", "cart ", "VIEW", "click", " view", ""])
        if r.random() < 0.005:
            pid = r.choice(["", "p99999999", "P%d" % (pidx + 1), "p0%d" % (pidx + 1), "p%d " % (pidx + 1), "prod1", "p"])
        if r.random() < 0.002:
            u = ""
        elif r.random() < 0.00002 * (50 if self.a.nasty else 1):
            u = u.upper()
        if self.a.nasty and r.random() < 0.001:
            u = r.choice(["user-odd", "u", "u0", " " + u, u + " ", "uzzzzzzzzzzz", "x" + u[1:]])
        late = r.random() < self.rates["late"]
        if late:
            y = r.random()
            if y < 0.2 and t % 1000 == 0:
                ing = t + DAY
            elif y < 0.4 and t % 1000 == 0:
                ing = t + DAY + 1000
            else:
                ing = t + r.randint(DAY // 1000, 5 * DAY // 1000) * 1000
        else:
            ing = t - (t % 1000) + r.randint(0, 120) * 1000
        ing -= ing % 1000
        tss = self.fmt_ts(t)
        if r.random() < self.rates["badts"]:
            tss = self.bad_ts(t, (self.ws, self.we))
        rows = [[eid, u, tss, iso(ing), et, pid, price]]
        if r.random() < self.rates["dup"]:
            badfirst = r.random() < 0.02
            otheru = r.random() < 1 / 64
            if badfirst:
                rows[0][2] = self.bad_ts(t, (self.ws, self.we))
            for _ in range(r.randint(1, 3)):
                c = list(rows[0])
                c[3] = iso(ing + r.randint(1, 600) * 1000)
                c[2] = self.fmt_ts(t)
                if otheru:
                    c[1] = self.uid(r.randrange(len(people)) << 9)
                rows.append(c)
            if self.a.nasty and r.random() < 0.05:
                c = list(rows[0])  # equal-ingest identical copy
                rows.append(c)
        if self.a.intrude and r.random() < self.a.intrude:
            # the original is lost and the only copy names someone else
            rows = [list(rows[0])]
            rows[0][1] = self.uid(r.randrange(len(people)) << 9 | r.randrange(2))
            rows[0][3] = iso(ing + r.randint(1, 600) * 1000)
        out = [self.quote(x) for x in rows]
        if self.a.nasty and r.random() < 0.0005:
            out = [o + "\r" for o in out]
        return out

    def click(self, seq, u, camp, t):
        r = self.r
        cid = "k" + b36(scramble(seq, self.ckey))
        if r.random() < 0.005:
            camp = r.choice(["c_999999", "C_1", "c_01", "c_1 ", "", "c_", "camp"])
        if r.random() < 0.002:
            u = ""
        elif r.random() < 0.00002 * (50 if self.a.nasty else 1):
            u = u.upper()
        if self.a.nasty and r.random() < 0.001:
            u = r.choice(["user-odd", "u", " " + u, "x" + u[1:]])
        tss = self.fmt_ts(t)
        if r.random() < self.rates["badts"]:
            tss = self.bad_ts(t, (self.ws - WEEK, self.we))
        f = [cid, u, camp, tss]
        out = [self.quote(f)]
        if r.random() < self.rates["cdup"]:
            for _ in range(r.randint(1, 2)):
                out.append(self.quote(f))
        return out

    def malform(self, lines, nf, rate):
        r = self.r
        out = []
        for l in lines:
            if r.random() < rate:
                k = r.randrange(5)
                if k == 0:
                    out.append(l + ",x")
                elif k == 1:
                    out.append(l.rsplit(",", 1)[0])
                elif k == 2:
                    out.append('"' + l)
                elif k == 3:
                    out.append("")
                else:
                    out.append("," + l.split(",", 1)[1])  # empty first ID
                if r.random() < 0.5 or self.a.safe_dups:
                    out.append(l)
            else:
                out.append(l)
        if self.a.nasty:
            out.append('a"b' + "," * (nf - 1))
            out.append('""' + "," * (nf - 1))
        return out

    def write(self, sub, header, lines, nfiles):
        r = self.r
        n = nfiles
        w = [1 / (i + 1) for i in range(n)]
        r.shuffle(w)
        nh = r.randint(1, min(3, n - 1)) if n > 1 else 0
        for i in range(nh):
            w[i] = 0
        tot = sum(w)
        bounds = [0]
        acc = 0
        for x in w:
            acc += x
            bounds.append(int(len(lines) * acc / tot))
        bounds[-1] = len(lines)
        for i in range(n):
            chunk = lines[bounds[i]:bounds[i + 1]]
            body = header + "\n" + "".join(l + "\n" for l in chunk)
            if self.a.nasty and i == n - 1 and chunk:
                body = body[:-1]  # no trailing newline on last file
            with open(os.path.join(self.a.output, sub, "part-%05d.csv" % i), "w", encoding="utf-8", newline="") as f:
                f.write(body)


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--events", type=int, default=100000)
    ap.add_argument("--clicks", type=int, default=30000)
    ap.add_argument("--products", type=int, default=50000)
    ap.add_argument("--campaigns", type=int, default=2000)
    ap.add_argument("--evfiles", type=int, default=24)
    ap.add_argument("--ckfiles", type=int, default=8)
    ap.add_argument("--lkfiles", type=int, default=4)
    ap.add_argument("--nasty", action="store_true")
    ap.add_argument("--leap", action="store_true")
    ap.add_argument("--whale", type=float, default=1.0)
    ap.add_argument("--safe-dups", action="store_true")
    ap.add_argument("--intrude", type=float, default=0.0)
    ap.add_argument("--output", required=True)
    Gen(ap.parse_args()).run()
