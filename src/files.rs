#![allow(dead_code)]
use crate::*;
use std::fs::File;
use std::path::{Path, PathBuf};
pub fn list_csv(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                name.ends_with(".csv") && !name.starts_with('.') && p.is_file()
            })
            .collect(),
        Err(_) => vec![],
    };
    v.sort();
    v
}

pub fn map_file(p: &Path) -> &'static [u8] {
    let f = File::open(p).expect("open");
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len == 0 {
        return &[];
    }
    let m = unsafe { memmap2::Mmap::map(&f).expect("mmap") };
    let m: &'static memmap2::Mmap = Box::leak(Box::new(m));
    &m[..]
}

pub fn make_chunks(files: &[&'static [u8]], kind: u8, target: usize, out: &mut Vec<Chunk>) {
    for &d in files {
        if d.is_empty() {
            continue;
        }
        let mut s = 0;
        while s < d.len() {
            let mut e = (s + target).min(d.len());
            if e < d.len() {
                e = match memchr::memchr(b'\n', &d[e..]) {
                    Some(q) => e + q + 1,
                    None => d.len(),
                };
            }
            out.push(Chunk { data: d, start: s, end: e, kind });
            s = e;
        }
    }
}

pub fn read_small_table(path: &Path) -> Vec<[&'static [u8]; 2]> {
    let d = if path.exists() { map_file(path) } else { &[] };
    let mut v = Vec::new();
    if d.is_empty() {
        return v;
    }
    let mut p = chunk_start(d, 0, d.len());
    let mut f: [&'static [u8]; 2] = [&[]; 2];
    while p < d.len() {
        let (np, ok) = split_at::<2>(d, p, &mut f);
        p = np;
        if ok {
            v.push(f);
        }
    }
    v
}

pub fn manifest_ts(text: &str, key: &str) -> i64 {
    let i = text.find(key).expect("manifest key") + key.len();
    let rest = &text[i..];
    let rest = &rest[rest.find(':').unwrap() + 1..];
    let a = rest.find('"').unwrap() + 1;
    let b = a + rest[a..].find('"').unwrap();
    parse_ts(rest[a..b].as_bytes()).expect("manifest ts")
}

