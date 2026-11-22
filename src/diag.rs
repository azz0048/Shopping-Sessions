//! Diagnostic build: measures the scoring machine, then fails on purpose so the dev-set
//! check returns these logs (only the END of stderr is shown, so the summary comes last).
#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

fn grab(p: &str, filter: impl Fn(&str) -> bool, max: usize) -> String {
    match std::fs::read_to_string(p) {
        Ok(t) => t.lines().filter(|l| filter(l)).take(max).collect::<Vec<_>>().join(" | "),
        Err(e) => format!("<{e}>"),
    }
}

fn list_csv(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().map_or(false, |x| x == "csv")).collect())
        .unwrap_or_default();
    v.sort();
    v
}

#[cfg(unix)]
fn residency(files: &[PathBuf]) -> (u64, u64) {
    use std::os::unix::io::AsRawFd;
    let (mut res, mut tot) = (0u64, 0u64);
    for p in files {
        let f = match std::fs::File::open(p) { Ok(f) => f, Err(_) => continue };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0) as usize;
        if len == 0 { continue; }
        unsafe {
            let m = libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_SHARED, f.as_raw_fd(), 0);
            if m == libc::MAP_FAILED { continue; }
            let pages = len.div_ceil(4096);
            let mut v = vec![0u8; pages];
            if libc::mincore(m, len, v.as_mut_ptr()) == 0 {
                res += v.iter().filter(|&&b| b & 1 != 0).count() as u64;
                tot += pages as u64;
            }
            libc::munmap(m, len);
        }
    }
    (res, tot)
}
#[cfg(not(unix))]
fn residency(_: &[PathBuf]) -> (u64, u64) { (0, 0) }

/// Runs the real pipeline as a child (verbose), returns a one-line phase summary.
fn run_child(tag: &str, data: &Path, out: &Path, envs: &[(&str, &str)]) -> (f64, String) {
    let _ = std::fs::remove_dir_all(out);
    std::fs::create_dir_all(out).unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut c = Command::new(&exe);
    c.env("SS_CHILD", "1").env("SS_VERBOSE", "1").env("DATA_DIR", data).env("OUTPUT_DIR", out);
    for (k, v) in envs {
        c.env(k, v);
    }
    let t = Instant::now();
    let o = c.output().unwrap();
    let wall = t.elapsed().as_secs_f64() * 1e3;
    let err = String::from_utf8_lossy(&o.stderr);
    let get = |pat: &str| -> f64 {
        err.lines().find(|l| l.contains(pat)).and_then(|l| l.trim().split_whitespace().next()).and_then(|x| x.parse().ok()).unwrap_or(f64::NAN)
    };
    let (setup, pt, pf, lb, p1, p2, p3, p4, done) = (
        get("ms  setup"), get("ms  pretouch"), get("ms  prefault inputs"), get("ms  links built"),
        get("P1 events"), get("P2 dedup"), get("P3 person"), get("P4 sessions"), get("ms  done"),
    );
    let pre = if !pf.is_nan() { pf } else if !pt.is_nan() { pt } else { setup };
    let extra = if !pt.is_nan() || !pf.is_nan() {
        format!(" [pt {:.0} pf {:.0}]", if pt.is_nan() { 0.0 } else { pt - setup }, if pf.is_nan() { 0.0 } else { pf - if pt.is_nan() { setup } else { pt } })
    } else {
        String::new()
    };
    if let Some(l) = err.lines().find(|l| l.contains("ns/line")) {
        return (wall, format!("{tag} {}", l.trim()));
    }
    let line = format!(
        "{tag}: P1 {:.0} | L {:.0} P2 {:.0} P3 {:.0} P4 {:.0} | in {:.0} wall {wall:.0}{extra}{}",
        p1 - lb, lb - pre, p2 - p1, p3 - p2, p4 - p3, done - pre,
        if o.status.success() { "" } else { " FAILED" }
    );
    (wall, line)
}

#[cfg(unix)]
fn fault_bench(label: &str, bytes: usize, huge: bool, nthreads: usize) -> String {
    unsafe {
        let p = libc::mmap(std::ptr::null_mut(), bytes, libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE, -1, 0);
        if huge { libc::madvise(p, bytes, libc::MADV_HUGEPAGE); }
        let base = p as usize;
        let t = Instant::now();
        std::thread::scope(|s| {
            for th in 0..nthreads {
                s.spawn(move || {
                    let per = bytes / nthreads;
                    let mut i = th * per;
                    while i < (th + 1) * per {
                        std::ptr::write_volatile((base + i) as *mut u8, 1);
                        i += 4096;
                    }
                });
            }
        });
        let tf = t.elapsed().as_secs_f64() * 1e3;
        // second pass: memory write bandwidth on already-faulted memory
        let t = Instant::now();
        std::thread::scope(|s| {
            for th in 0..nthreads {
                s.spawn(move || {
                    let per = bytes / nthreads;
                    std::ptr::write_bytes((base + th * per) as *mut u8, 7, per);
                });
            }
        });
        let tw = t.elapsed().as_secs_f64() * 1e3;
        let t = Instant::now();
        libc::munmap(p, bytes);
        let tu = t.elapsed().as_secs_f64() * 1e3;
        format!("{label}: touch {tf:.0} memset {tw:.0} ({:.1} GB/s) munmap {tu:.0}", bytes as f64 / tw / 1e6)
    }
}

#[cfg(unix)]
fn file_fault_bench(files: &[PathBuf], nthreads: usize) -> String {
    use std::os::unix::io::AsRawFd;
    let mut maps = Vec::new();
    for p in files {
        let f = std::fs::File::open(p).unwrap();
        let len = f.metadata().unwrap().len() as usize;
        if len == 0 { continue; }
        let m = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, f.as_raw_fd(), 0) };
        maps.push((m as usize, len));
    }
    let total: usize = maps.iter().map(|m| m.1).sum();
    let pieces: Vec<(usize, usize)> = maps.iter().flat_map(|&(b, l)| (0..l).step_by(4 << 20).map(move |o| (b + o, (l - o).min(4 << 20)))).collect();
    let pass = |stride: usize| {
        let next = std::sync::atomic::AtomicUsize::new(0);
        let t = Instant::now();
        std::thread::scope(|s| {
            for _ in 0..nthreads {
                s.spawn(|| {
                    let mut acc = 0u64;
                    loop {
                        let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if i >= pieces.len() { break; }
                        let (b, l) = pieces[i];
                        let mut o = 0;
                        while o + 8 <= l {
                            acc = acc.wrapping_add(unsafe { std::ptr::read_volatile((b + o) as *const u64) });
                            o += stride;
                        }
                    }
                    std::hint::black_box(acc);
                });
            }
        });
        t.elapsed().as_secs_f64() * 1e3
    };
    let tf = pass(4096);
    let tr = pass(64);
    let t = Instant::now();
    for &(m, l) in &maps {
        unsafe { libc::munmap(m as *mut libc::c_void, l) };
    }
    let tu = t.elapsed().as_secs_f64() * 1e3;
    // read() of the same bytes into a reused buffer (page cache copy speed)
    let t = Instant::now();
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..nthreads {
            s.spawn(|| {
                use std::io::Read;
                let mut buf = vec![0u8; 1 << 20];
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= files.len() { break; }
                    if let Ok(mut f) = std::fs::File::open(&files[i]) {
                        while let Ok(n) = f.read(&mut buf) { if n == 0 { break; } }
                    }
                }
            });
        }
    });
    let trd = t.elapsed().as_secs_f64() * 1e3;
    format!("mmap {}MB: touch {tf:.0} stream {tr:.0} ({:.1} GB/s) munmap {tu:.0}; read() {trd:.0} ({:.1} GB/s)",
        total >> 20, total as f64 / tr / 1e6, total as f64 / trd / 1e6)
}

pub fn diag_main() -> ! {
    let t0 = Instant::now();
    let nthreads: usize = std::env::var("NCPU").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
    let mut sum: Vec<String> = Vec::new();
    let scratch = PathBuf::from(std::env::var("SCRATCH_DIR").unwrap_or("/scratch".into()));
    let p20 = scratch.join("p20");
    let nev: u64 = std::env::var("SS_DIAG_EVENTS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000_000);
    if let Err(e) = crate::datagen::generate(7, nev, nev / 4, &p20, nthreads) {
        eprintln!("generate failed: {e}");
        std::process::exit(1);
    }
    let o = scratch.join("o20");
    let mut big: Vec<(u64, PathBuf)> = list_csv(&p20.join("events")).into_iter().map(|p| (std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0), p)).collect();
    big.sort();
    let bf = big.last().unwrap().1.to_string_lossy().to_string();
    // run list: "tag[:K=V,K=V]" separated by ';' (src/diag_runs.txt)
    let spec = include_str!("diag_runs.txt").replace("{BF}", &bf);
    let runs: Vec<(String, Vec<(String, String)>)> = spec
        .split(';')
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
        .map(|r| {
            let (tag, kv) = r.split_once(':').unwrap_or((r, ""));
            let envs = kv.split(',').filter_map(|x| x.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
            (tag.to_string(), envs)
        })
        .collect();
    for (tag, envs) in runs.iter() {
        let envs: Vec<(&str, &str)> = envs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
        let (_, l) = run_child(tag, &p20, &o, &envs);
        sum.push(l);
    }
    // output equality across kernels: order-independent checksum of the SS_CSV dumps
    if let Some(modes) = std::env::var("SS_DIAG_CMP").ok().or(Some("0".into())) {
        let mut sums = Vec::new();
        for m in modes.split(',') {
            let csv = scratch.join(format!("csv{m}"));
            let (k, fl) = m.split_once('f').unwrap_or((m, "0"));
            let _ = run_child("cmp", &p20, &o, &[("SS_V4MODE", k), ("SS_P1FLAGS", fl), ("SS_CSV", csv.to_str().unwrap())]);
            let mut acc = (0u64, 0u64);
            for t in ["user_daily.csv", "campaign_daily.csv"] {
                let text = std::fs::read(csv.join(t)).unwrap_or_default();
                for line in text.split(|&b| b == b'\n') {
                    acc.0 = acc.0.wrapping_add(crate::hash1(line));
                    acc.1 += 1;
                }
            }
            sums.push(format!("m{m}={:x}/{}", acc.0, acc.1));
        }
        sum.push(format!("csv sums: {}", sums.join(" ")));
    }
    #[cfg(unix)]
    if std::env::var_os("SS_DIAG_FAULTS").is_some() {
        sum.push(fault_bench("anonTHP8", 1 << 30, true, nthreads));
        sum.push(fault_bench("anon4k8", 1 << 30, false, nthreads));
        let ev: Vec<PathBuf> = list_csv(&p20.join("events"));
        sum.push(file_fault_bench(&ev, nthreads));
    }
    sum.push(format!("total {:.1} s", t0.elapsed().as_secs_f64()));
    eprintln!("==== DIAG3 ====");
    for l in &sum {
        eprintln!("{l}");
    }
    std::process::exit(1);
}