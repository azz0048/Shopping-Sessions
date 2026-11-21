# Shopping Sessions — native Rust pipeline

A single static Rust binary in a `FROM scratch` image. No engine, no JVM, no Python.

## How it works

All phases run on `NCPU` threads with work-stealing over chunks or buckets.

| Phase | Work |
| --- | --- |
| L  | Parse `links/`, intern IDs per hash bucket, union-find, and name each person by the byte-smallest ID |
| P1 | Parse events and clicks with an AVX2 line splitter. Each event copy writes a 16-byte dedup key to an event-ID bucket and, if valid, a record to a user bucket |
| P2 | Dedup each event-ID bucket. The copy with the smallest `ingest_ts` wins, and all other copies are marked in a bitmap |
| P3 | For each user bucket, drop the marked copies and map user → person with that bucket's cache-resident slice of the link table, then scatter to person buckets |
| P4 | For each person bucket, group by person and sort by `ts`. Sessionize, attribute with a sliding 7-day click window, aggregate, and write Parquet |

Notes:
- Bucket storage is chains of blocks from a huge-page arena. Blocks freed by one phase are reused by the next.
- Parquet is written by a small built-in writer: PLAIN encoding, uncompressed, one row group.
- Strings are never copied. Every ID is a pointer into the mmapped input.

## Build and test

```sh
cargo build --release
cargo test --release                       # differential fuzz tests (CSV splitter, timestamps)
python3 scripts/run_fixtures.py fixtures -- target/x86_64-unknown-linux-gnu/release/ss
docker build -t shopping-sessions .
```

Set `SS_VERBOSE=1` to print phase timings to stderr.
