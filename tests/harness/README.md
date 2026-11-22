# Test harness (no third-party Python packages needed)

- `gen.py` — synthetic data in the real generator's shape (scrambled base-36
  `u`/`e`/`k` IDs, kiosks, chains, whales, all rules.md dirt). Extra switches:
  `--nasty` (\r endings, odd IDs), `--leap` (window over 2028-02-29),
  `--whale X` (scale whale size), `--intrude P` (drop a duplicate's original so
  the surviving copy names another person), `--safe-dups`.
- `ref.py` — plain, independent implementation of rules.md -> canonical CSV.
- `pq.py` — strict minimal Parquet reader; `compare.py OUT REF`; `dump.py OUT CANON`.
- `fixtures.py DIR` — 32 hand-written edge-case datasets.
- `run.sh NAME [gen args]` — generate + reference + fast and generic runs + compare.

```
cargo build --release --target x86_64-unknown-linux-gnu --bin pipeline
tests/harness/run.sh dev --seed 7 --events 1000000 --clicks 300000
tests/harness/run.sh intr --seed 8 --events 1000000 --clicks 300000 --intrude 0.0002 --whale 5
```
