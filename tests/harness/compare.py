#!/usr/bin/env python3
"""compare.py OUTPUT_DIR REF_DIR  -> exit 0 if identical."""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pq import read_table

UD = ["user_id", "day", "sessions", "events", "revenue_cents", "top_category"]
CD = ["campaign_id", "channel", "day", "sessions", "purchasing_sessions", "revenue_cents", "assisted_sessions"]
STR = {"user_id", "top_category", "campaign_id", "channel"}


def check_schema(name, cols, info):
    assert sorted(cols) == sorted(info.keys()), (name, sorted(info.keys()))
    for c in cols:
        e = info[c]
        if c == "day":
            is_date = e.get(6) == 6 or (10 in e and 6 in e[10])
            assert e[1] == 1 and is_date, "%s.day not DATE: %r" % (name, e)
        elif c in STR:
            assert e[1] == 6, "%s.%s not BYTE_ARRAY" % (name, c)
        else:
            assert e[1] in (1, 2), "%s.%s not int" % (name, c)
        if c != "top_category":
            pass


def canon(rows, cols):
    out = []
    for r in rows:
        f = []
        for c in cols:
            v = r[c]
            if v is None:
                f.append(b"\\N")
            elif isinstance(v, bytes):
                f.append(v)
            else:
                f.append(str(v).encode())
        out.append(b"\t".join(f))
    out.sort()
    return out


def main(outdir, refdir):
    ok = True
    for tname, cols, fname in (("user_daily", UD, "user_daily.csv"), ("campaign_daily", CD, "campaign_daily.csv")):
        files, schema, rows = read_table(os.path.join(outdir, tname))
        if not files:
            got = []
        else:
            check_schema(tname, cols, schema[1])
            for c in cols:
                if c != "top_category":
                    assert all(r[c] is not None for r in rows), "null in %s.%s" % (tname, c)
            got = canon(rows, cols)
        want = open(os.path.join(refdir, fname), "rb").read().split(b"\n")[:-1]
        if got != want:
            ok = False
            gs, wsset = set(got), set(want)
            extra = sorted(gs - wsset)
            miss = sorted(wsset - gs)
            print("%s MISMATCH: got %d want %d, extra %d missing %d" % (tname, len(got), len(want), len(extra), len(miss)))
            if len(got) != len(gs):
                print("  duplicate rows in output:", len(got) - len(gs))
            for x in extra[:8]:
                print("  +", x.decode(errors="replace"))
            for x in miss[:8]:
                print("  -", x.decode(errors="replace"))
        else:
            print("%s OK (%d rows)" % (tname, len(got)))
    return ok


if __name__ == "__main__":
    sys.exit(0 if main(sys.argv[1], sys.argv[2]) else 1)
