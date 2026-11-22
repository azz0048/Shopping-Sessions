#!/usr/bin/env python3
"""Write small edge-case fixtures to ROOT/<name>/ (one rule each, mostly)."""
import json
import os
import sys

ROOT = sys.argv[1]
WS = "2026-10-01T00:00:00Z"
WE = "2026-10-15T00:00:00Z"
EH = "event_id,user_id,ts,ingest_ts,event_type,product_id,price_cents"
CH = "click_id,user_id,campaign_id,ts"
LH = "user_id_a,user_id_b"
PROD = "product_id,category\np1,books\np2,toys\np3,apparel\n"
PRODX = PROD + "P1,zeta\n"
CAMP = "campaign_id,channel\nc_1,search\nc_2,social\nc_3,email\n"
CAMPX = CAMP + "C_1,display\n"
WSMS = 1790812800000  # 2026-10-01T00:00:00Z

FX = {}


def fx(name, events="", clicks="", links="", extra=None, ws=WS, we=WE, prod=PROD, camp=CAMP):
    FX[name] = dict(events=events, clicks=clicks, links=links, extra=extra or {}, ws=ws, we=we, prod=prod, camp=camp)


def ev(*rows):
    return "".join(r + "\n" for r in rows)


I = "2026-10-03T10:00:00Z"  # generic ingest
fx("basic", ev("e1,u1,2026-10-03T10:00:00Z,%s,view,p1," % I, "e2,u1,2026-10-03T10:10:00Z,%s,purchase,p2,500" % I),
   ev("k1,u1,c_1,2026-10-03T09:00:00Z"))
fx("gap_exact_30m", ev("e1,u1,2026-10-03T10:00:00Z,%s,view,p1," % I, "e2,u1,2026-10-03T10:30:00Z,%s,view,p1," % I,
                       "e3,u1,1791027000001,%s,view,p1," % I, "e4,u1,1791030600002,%s,view,p1," % I))
fx("equal_ts", ev("e1,u1,1791020000000,%s,view,p1," % I, "e2,u1,1791021800000,%s,view,p1," % I,
                  "e3,u1,1791021800000,%s,view,p1," % I, "e4,u1,1791023600001,%s,purchase,p1,7" % I))
fx("ts_forms", ev("e1,u1,2026-10-03T19:58:10-04:00,2026-10-04T00:00:00Z,view,p1,",
                  "e2,u2,0001791071890000,2026-10-04T00:00:00Z,view,p1,",
                  "e3,u3,2026-10-03T23:58:10-00:00,2026-10-04T00:00:00Z,view,p1,",
                  "e4,u4,2026-10-04T05:28:10+05:30,2026-10-04T00:00:00Z,view,p1,",
                  "e5,u5,2026-10-03T23:58:10+0200,2026-10-04T00:00:00Z,view,p1,",
                  "e6,u6,2026-10-03T23:58:10.000Z,2026-10-04T00:00:00Z,view,p1,",
                  "e7,u7,2026-10-03T23:58:10z,2026-10-04T00:00:00Z,view,p1,",
                  "e8,u8,2026-10-03 23:58:10Z,2026-10-04T00:00:00Z,view,p1,",
                  "e9,u9,2026-10-03T23:58:10,2026-10-04T00:00:00Z,view,p1,",
                  "e10,u10,1791071890000.0,2026-10-04T00:00:00Z,view,p1,",
                  "e11,u11,1.79e12,2026-10-04T00:00:00Z,view,p1,",
                  "e12,u12,-5,2026-10-04T00:00:00Z,view,p1,",
                  "e13,u13,2026-02-29T10:00:00Z,2026-10-04T00:00:00Z,view,p1,",
                  "e14,u14,2026-10-03T24:00:00Z,2026-10-04T00:00:00Z,view,p1,",
                  "e15,u15,2026-10-03T23:59:60Z,2026-10-04T00:00:00Z,view,p1,",
                  "e16,u16,2026-10-03T23:58:10+24:00,2026-10-04T00:00:00Z,view,p1,",
                  "e17,u17,2026-10-03T23:58:10+05:60,2026-10-04T00:00:00Z,view,p1,",
                  "e18,u18,2026-10-03T23:58:10+23:59,2026-10-04T00:00:00Z,view,p1,",
                  "e19,u19,2026-13-03T23:58:10Z,2026-10-04T00:00:00Z,view,p1,",
                  "e20,u20,99999999999999999999999999,2026-10-04T00:00:00Z,view,p1,",
                  "e21,u21,١٧٩١٠٧١٨٩٠٠٠٠,2026-10-04T00:00:00Z,view,p1,",
                  "e22,u22,2026-10-03T23:58:10Z\r,2026-10-04T00:00:00Z,view,p1,",
                  "e23,u23,2026-09-30T23:59:59Z,2026-10-01T00:00:00Z,view,p1,",
                  "e24,u24,2026-10-01T00:00:00Z,2026-10-01T00:00:00Z,view,p1,",
                  "e25,u25,2026-10-15T00:00:00Z,2026-10-15T00:00:00Z,view,p1,",
                  "e26,u26,1792022399999,2026-10-14T23:59:59Z,view,p1,",
                  "e27,u27,2026-10-03T23:58:10+00:00,2026-10-04T00:00:00Z,view,p1,",
                  "e28,u28,2026-10-3T23:58:10Z,2026-10-04T00:00:00Z,view,p1,",
                  "e29,u29,+1791071890000,2026-10-04T00:00:00Z,view,p1,",
                  "e30,u30,2026-10-03T23:58:10-10:00,2026-10-04T23:00:00Z,view,p1,"))
fx("dedup", ev("e1,u1,2026-10-03T10:00:00Z,2026-10-03T10:05:00Z,view,p1,",
               "e1,u2,2026-10-03T10:00:00Z,2026-10-03T10:01:00Z,purchase,p1,100",
               "e2,u1,bad,2026-10-03T10:00:00Z,view,p1,",
               "e2,u1,2026-10-03T10:00:00Z,2026-10-03T10:05:00Z,view,p1,",
               "e3,u1,2026-10-03T10:00:00Z,2026-10-03T10:05:00Z,view,p1,,x",
               "e3,u3,2026-10-03T10:00:00Z,2026-10-03T10:06:00Z,view,p1,",
               '"e4",u4,2026-10-03T10:00:00Z,2026-10-03T10:06:00Z,view,p1,',
               'e4,u4,2026-10-03T10:00:00Z,"2026-10-03T10:06:00Z",view,p1,',
               ",u5,2026-10-03T10:00:00Z,2026-10-03T10:06:00Z,view,p1,"))
fx("late", ev("e1,u1,2026-10-03T10:00:00Z,2026-10-04T10:00:00Z,view,p1,",
              "e2,u2,2026-10-03T10:00:00Z,2026-10-04T10:00:01Z,view,p1,",
              "e3,u3,1791021599999,2026-10-04T10:00:00Z,view,p1,",
              "e4,u4,2026-10-03T10:00:00Z,2026-10-01T10:00:00Z,view,p1,",
              "e5,u5,1791021600001,2026-10-04T10:00:00Z,view,p1,"))
fx("validate", ev("e1,u1,2026-10-03T10:00:00Z,%s,Purchase,p1,100" % I, "e2,u1,2026-10-03T10:00:00Z,%s,cart ,p1," % I,
                  "e3,u1,2026-10-03T10:00:00Z,%s,purchase,p1,0250" % I, "e4,u1,2026-10-03T10:00:00Z,%s,purchase,p1,0" % I,
                  "e5,u1,2026-10-03T10:00:00Z,%s,purchase,p1,10000000" % I, "e6,u1,2026-10-03T10:00:00Z,%s,purchase,p1,10000001" % I,
                  "e7,u1,2026-10-03T10:00:00Z,%s,purchase,p1,12.5" % I, "e8,u1,2026-10-03T10:00:00Z,%s,purchase,p1,-1" % I,
                  "e9,u1,2026-10-03T10:00:00Z,%s,purchase,p1," % I, "e10,u1,2026-10-03T10:00:00Z,%s,purchase,p1,+5" % I,
                  "e11,u1,2026-10-03T10:00:00Z,%s,view,p1,garbage" % I, "e12,u1,2026-10-03T10:00:00Z,%s,view,,1" % I,
                  "e13,,2026-10-03T10:00:00Z,%s,view,p1," % I, "e14,u1,2026-10-03T10:00:00Z,%s,cart,p9,abc" % I,
                  "e15,u1,2026-10-03T10:00:00Z,%s,purchase,p1,100\r" % I, "e16,u1,2026-10-03T10:00:00Z,%s,purchase,p1,٢٥٠" % I,
                  "e17,u1,2026-10-03T10:00:00Z,%s,purchase,p1,00000000000000000000000000042" % I,
                  "e18,u1,2026-10-03T10:00:00Z,%s,purchase,p1,99999999999999999999999" % I,
                  "e19,u1,2026-10-03T10:00:00Z,%s,purchase,p1,１００" % I))
fx("csv_dialect", ev('"e1","u1","2026-10-03T10:00:00Z","%s","view","p1",""' % I, 'e2,u1,2026-10-03T10:00:00Z,%s,view,p1,"' % I,
                     'e3,u1,2026-10-03T10:00:00Z,%s,vi"ew,p1,' % I, '', 'e4," u1",2026-10-03T10:00:00Z,%s,view,p1,' % I,
                     'e5, u1,2026-10-03T10:05:00Z,%s,view,p1,' % I, 'e6,u1 ,2026-10-03T10:00:00Z,%s,view,p1,' % I,
                     'e7,"u1,2026-10-03T10:00:00Z,%s,view,p1,' % I, 'e8,""u1"",2026-10-03T10:00:00Z,%s,view,p1,' % I,
                     'e9,u9,2026-10-03T10:00:00Z,%s,view,p1' % I, 'e10,u10,2026-10-03T10:00:00Z,%s,view,p1,,' % I,
                     '"e11,u11,2026-10-03T10:00:00Z,%s,view,p1,"' % I, 'e12,"",2026-10-03T10:00:00Z,%s,view,p1,' % I,
                     'e13,u13,2026-10-03T10:00:00Z,%s,view,"p1",' % I, 'e14,u14,2026-10-03T10:00:00Z,%s,view,p1,"x' % I,
                     'e15,u15,2026-10-03T10:00:00Z,%s,view,"",1' % I, 'e16,u16,2026-10-03T10:00:00Z,%s,"",p1,1' % I,
                     'e17,u17,2026-10-03T10:00:00Z,%s,view,p1,"""' % I, 'e18,"u18"x,2026-10-03T10:00:00Z,%s,view,p1,' % I))
fx("categories", ev("e1,u1,2026-10-03T10:00:00Z,%s,purchase,p1,100" % I, "e2,u1,2026-10-03T10:01:00Z,%s,purchase,p2,100" % I,
                    "e3,u2,2026-10-03T10:00:00Z,%s,purchase,p9,300" % I, "e4,u2,2026-10-03T10:01:00Z,%s,purchase,p3,299" % I,
                    "e5,u3,2026-10-03T10:00:00Z,%s,purchase,P1,5" % I, "e6,u3,2026-10-03T10:01:00Z,%s,purchase,p01,5" % I,
                    "e7,u4,2026-10-03T10:00:00Z,%s,purchase,p1 ,5" % I, "e8,u4,2026-10-03T10:01:00Z,%s,purchase,p3,5" % I,
                    "e9,u5,2026-10-03T10:00:00Z,%s,view,p1," % I,
                    "e10,u6,2026-10-03T10:00:00Z,%s,purchase,p1,10" % I, "e11,u6,2026-10-03T12:00:00Z,%s,purchase,p2,20" % I))
fx("midnight", ev("e1,u1,2026-10-03T23:50:00Z,2026-10-04T00:00:00Z,view,p1,",
                  "e2,u1,2026-10-04T00:10:00Z,2026-10-04T00:10:00Z,purchase,p1,50",
                  "e3,u1,2026-10-04T00:50:00Z,2026-10-04T00:50:00Z,purchase,p1,60"),
   ev("k1,u1,c_2,2026-10-03T23:00:00Z"))
fx("links_chain", ev("e1,ub,2026-10-03T10:00:00Z,%s,view,p1," % I, "e2,uc,2026-10-03T10:10:00Z,%s,view,p1," % I,
                     "e3,ud,2026-10-03T10:20:00Z,%s,view,p1," % I, "e4,U1,2026-10-03T10:00:00Z,%s,view,p1," % I,
                     "e5,ux,2026-10-03T10:00:00Z,%s,view,p1," % I, "e6, ub,2026-10-03T10:00:00Z,%s,view,p1," % I),
   ev("k1,ud,c_1,2026-10-03T09:00:00Z"),
   ev("ub,uc", "ud,uc", "uc,ub", "ud,ud", "zz,ua", "ua,ub", "ux,", ",ux", "uy,uy", 'ux,"uq', "U1,u1"))
fx("links_unused_smallest", ev("e1,u5,2026-10-03T10:00:00Z,%s,view,p1," % I), "", ev("u5,a0"))
fx("click_credit", ev("e1,u1,2026-10-10T10:00:00Z,%s,view,p1," % I, "e2,u2,2026-10-10T10:00:00Z,%s,view,p1," % I,
                      "e3,u3,2026-10-10T10:00:00Z,%s,purchase,p1,9" % I, "e4,u4,2026-10-10T10:00:00Z,%s,view,p1," % I,
                      "e5,u5,2026-10-10T10:00:00Z,%s,view,p1," % I),
   ev("k1,u1,c_1,2026-10-03T10:00:00Z", "k2,u1,c_2,2026-10-03T09:59:59Z",  # exactly 7d -> c_1
      "k3,u2,c_1,1790935199999", "k4,u2,c_2,2026-10-10T10:00:00Z", "k5,u2,c_3,2026-10-10T10:00:00.001Z",
      "kb,u3,c_2,2026-10-09T10:00:00Z", "ka,u3,c_3,2026-10-09T10:00:00Z", "kc,u3,c_1,2026-10-09T09:00:00Z",
      "k9,u4,c_1,1791626400001", "k8,u4,c_9,2026-10-10T09:00:00Z", "k7,u4,C_1,2026-10-10T09:00:00Z",
      "k66,,c_1,2026-10-10T09:00:00Z", "k6,u5,c_2,2026-10-10T09:00:00Z", "k6,u5,c_2,2026-10-10T09:00:00Z",
      ",u5,c_3,2026-10-10T09:30:00Z", "k10,u5,c_3,2026-10-10T09:30:00z", "k11,u5,c_3,2026-10-10t09:30:00Z",
      '"k12","u5","c_1","1791626000000"', 'k12,u5,c_1,"1791626000000"'))
fx("click_window", ev("e1,u1,2026-10-01T00:00:00Z,2026-10-01T00:00:00Z,view,p1,",
                      "e2,u2,2026-10-01T05:00:00Z,2026-10-01T05:00:00Z,view,p1,"),
   ev("k1,u1,c_1,2026-09-24T00:00:00Z", "k2,u2,c_1,2026-09-23T23:59:59Z", "k3,u2,c_2,2026-09-24T05:00:00Z",
      "k4,u1,c_3,2026-10-15T00:00:00Z", "k5,u1,c_2,1790208000000"))
fx("assists", ev("e1,u1,2026-10-10T10:00:00Z,%s,purchase,p1,100" % I, "e2,u1,2026-10-12T10:00:00Z,%s,view,p1," % I),
   ev("k1,u1,c_1,2026-10-04T10:00:00Z", "k2,u1,c_1,2026-10-05T10:00:00Z", "k3,u1,c_2,2026-10-06T10:00:00Z",
      "k4,u1,c_3,2026-10-10T10:00:00Z", "k5,u1,c_3,2026-10-11T10:00:00Z"))
fx("shared_device", ev("e1,ua,2026-10-03T10:00:00Z,%s,view,p1," % I, "e2,ub,2026-10-03T10:20:00Z,%s,view,p1," % I,
                       "e3,dev,2026-10-03T10:40:00Z,%s,purchase,p2,10" % I, "e4,uc,2026-10-03T12:00:00Z,%s,view,p1," % I),
   ev("k1,ub,c_2,2026-10-03T10:00:00Z"), ev("ua,dev", "dev,ub", "uc,dev"))
fx("empty_all", "", "", "")
fx("crlf_lines", "e1,u1,2026-10-03T10:00:00Z,%s,view,p1,\r\ne2,u1,2026-10-03T10:00:00Z\r,%s,view,p1,\r\ne3,u1,2026-10-03T10:01:00Z,%s,purchase,p1,100\r\n" % (I, I, I),
   "k1,u1,c_1,2026-10-03T09:00:00Z\r\nk2,u1,c_2\r,2026-10-03T09:00:00Z\n", "u1,u2\r\n")
fx("no_trailing_nl", "e1,u1,2026-10-03T10:00:00Z,%s,view,p1," % I, "k1,u1,c_1,2026-10-03T09:00:00Z", "u1,u2")
fx("utf8_ids", ev("e1,ü1,2026-10-03T10:00:00Z,%s,view,p1," % I, "e2,u1,2026-10-03T10:00:00Z,%s,view,p1," % I,
                  "e3,~,2026-10-03T10:00:00Z,%s,view,p1," % I), "", ev("ü1,~"))
fx("multi_files", ev("e1,u1,2026-10-03T10:00:00Z,%s,view,p1," % I), "", "",
   extra={"events/b.csv": EH + "\n" + ev("e1,u1,2026-10-03T10:00:00Z,2026-10-03T09:00:00Z,purchase,p1,5", "e2,u2,2026-10-04T10:00:00Z,%s,view,p1," % I),
          "events/c.csv": EH + "\n", "events/notcsv.txt": EH + "\ne9,u9,2026-10-03T10:00:00Z,%s,view,p1,\n" % I,
          "links/x.csv": LH + "\nu1,u2\n", "clicks/z.csv": CH + "\n"})
fx("leap_window", ev("e1,u1,2028-02-29T10:00:00Z,2028-02-29T10:00:00Z,view,p1,", "e2,u2,2027-02-29T10:00:00Z,2028-02-29T10:00:00Z,view,p1,",
                     "e3,u3,2028-02-28T23:59:59-05:00,2028-02-29T10:00:00Z,purchase,p1,3"),
   ev("k1,u1,c_1,2028-02-29T00:00:00+01:00"), ws="2028-02-20T00:00:00Z", we="2028-03-05T00:00:00Z")
fx("quoted_lookups", ev("e1,u1,2026-10-03T10:00:00Z,%s,purchase,p1,5" % I), ev("k1,u1,c_1,2026-10-03T09:00:00Z"),
   prod='product_id,category\n"p1","books"\np2,toys\n', camp='campaign_id,channel\n"c_1","search"\n')
fx("weird_lookups", ev("e1,u1,2026-10-03T10:00:00Z,%s,purchase,sku-1,5" % I, "e2,u1,2026-10-03T10:01:00Z,%s,purchase,p7,5" % I),
   ev("k1,u1,promo-x,2026-10-03T09:00:00Z"),
   prod="product_id,category\nsku-1,Unknown\np7,unknownx\n", camp="campaign_id,channel\npromo-x,tv\n")
fx("unknown_cat_tie", ev("e1,u1,2026-10-03T10:00:00Z,%s,purchase,p9,50" % I, "e2,u1,2026-10-03T10:01:00Z,%s,purchase,p3,50" % I,
                         "e3,u2,2026-10-03T10:00:00Z,%s,purchase,p9,50" % I, "e4,u2,2026-10-03T10:01:00Z,%s,purchase,p2,50" % I),
   prod="product_id,category\np2,video\np3,apparel\n")
fx("window_lookback_events", ev("e1,u1,2026-09-30T23:59:59Z,2026-10-01T00:00:00Z,view,p1,"), ev("k1,u1,c_1,2026-09-30T00:00:00Z"))
fx("big_f3", ev("e1,u1,%s,2026-10-03T10:00:00Z,view,p1," % ("0" * 30 + "1791021600000"),
                "e2,u2,18446744073709551617,2026-10-03T10:00:00Z,view,p1,", "e3,u3,9223372036854775808,2026-10-03T10:00:00Z,view,p1,"))
fx("header_only_files", "", "", "", extra={"events/h1.csv": EH + "\n", "events/h2.csv": EH, "clicks/h.csv": CH})
fx("many_sessions_one_click", ev(*["e%d,u1,%d,2026-10-05T00:00:00Z,view,p1," % (i, 1791100000000 + i * 3_600_000) for i in range(30)]),
   ev("k1,u1,c_1,1791100000000", "k2,u1,c_2,1791150000000"))
fx("ingest_bad", ev("e1,u1,2026-10-03T10:00:00Z,%s,view,p1," % I))

for base in ("click_credit", "categories"):
    FX[base + "_xlookup"] = dict(FX[base], prod=PRODX, camp=CAMPX)


def write(name, f):
    d = os.path.join(ROOT, name)
    for sub in ("events", "clicks", "links"):
        os.makedirs(os.path.join(d, sub), exist_ok=True)
    json.dump({"window_start": f["ws"], "window_end": f["we"]}, open(os.path.join(d, "manifest.json"), "w"))
    open(os.path.join(d, "products.csv"), "w").write(f["prod"])
    open(os.path.join(d, "campaigns.csv"), "w").write(f["camp"])
    for sub, h in (("events", EH), ("clicks", CH), ("links", LH)):
        with open(os.path.join(d, sub, "part-0.csv"), "w", newline="", encoding="utf-8") as fo:
            fo.write(h + "\n" + f[sub])
    for p, body in f["extra"].items():
        with open(os.path.join(d, p), "w", newline="", encoding="utf-8") as fo:
            fo.write(body)


for n, f in FX.items():
    write(n, f)
print(len(FX), "fixtures")
