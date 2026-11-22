#!/usr/bin/env python3
"""dump.py PARQUET_OUT_DIR CANON_DIR: write canonical CSVs (same form as ref.py)."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from pq import read_table
from compare import UD, CD, canon
o, d = sys.argv[1], sys.argv[2]
os.makedirs(d, exist_ok=True)
for t, cols, f in (("user_daily", UD, "user_daily.csv"), ("campaign_daily", CD, "campaign_daily.csv")):
    _, _, rows = read_table(os.path.join(o, t))
    open(os.path.join(d, f), "wb").write(b"".join(r + b"\n" for r in canon(rows, cols)))
