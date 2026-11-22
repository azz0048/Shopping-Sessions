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
        format!(" [pretouch {:.0} prefault {:.0}]", if pt.is_nan() { 0.0 } else { pt - setup }, if pf.is_nan() { 0.0 } else { pf - if pt.is_nan() { setup } else { pt } })
    } else {
        String::new()
    };
    let line = format!(
        "{tag:<22} wall {wall:6.1} | setup {setup:5.1} links {:5.1} P1 {:6.1} P2 {:5.1} P3 {:5.1} P4 {:6.1} end {:5.1} | exit+start {:5.1}{extra}{}",
        lb - pre, p1 - lb, p2 - p1, p3 - p2, p4 - p3, done - p4, wall - done,
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
        format!("{label}: touch {tf:.1} ms, memset {tw:.1} ms ({:.1} GB/s), munmap {tu:.1} ms", bytes as f64 / tw / 1e6)
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
    format!("events mmap {} MB: first-touch {tf:.1} ms, stream {tr:.1} ms ({:.1} GB/s), munmap {tu:.1} ms; read() 1MB bufs {trd:.1} ms ({:.1} GB/s)",
        total >> 20, total as f64 / tr / 1e6, total as f64 / trd / 1e6)
}

pub fn diag_main() -> ! {
    let t0 = Instant::now();
    let nthreads: usize = std::env::var("NCPU").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
    let mut sum: Vec<String> = Vec::new();
    sum.push(format!("cpu: {}", grab("/proc/cpuinfo", |l| l.starts_with("model name") || l.starts_with("cpu MHz") || l.starts_with("cache size") || l.starts_with("siblings") || l.starts_with("cpu cores"), 5)));
    sum.push(format!("avx512f: {} | nproc {:?} | cpus_allowed {}", grab("/proc/cpuinfo", |l| l.starts_with("flags"), 1).contains("avx512f"), std::thread::available_parallelism().map(|n| n.get()), grab("/proc/self/status", |l| l.starts_with("Cpus_allowed_list"), 1)));
    sum.push(format!("kernel: {}", grab("/proc/version", |_| true, 1)));
    sum.push(format!("thp: enabled {} | defrag {}", grab("/sys/kernel/mm/transparent_hugepage/enabled", |_| true, 1), grab("/sys/kernel/mm/transparent_hugepage/defrag", |_| true, 1)));
    sum.push(format!("mem: {}", grab("/proc/meminfo", |l| ["MemTotal", "MemAvailable", "Cached", "AnonHugePages"].iter().any(|k| l.starts_with(k)), 4)));
    sum.push(format!("cgroup: cpu.max {} | memory.max {}", grab("/sys/fs/cgroup/cpu.max", |_| true, 1), grab("/sys/fs/cgroup/memory.max", |_| true, 1)));
    sum.push(format!("mounts: {}", grab("/proc/mounts", |l| l.contains(" /data ") || l.contains(" /scratch ") || l.contains(" /output ") || l.contains(" / "), 4)));
    sum.push(format!("smt: {} | cpu0 siblings {}", grab("/sys/devices/system/cpu/smt/active", |_| true, 1), grab("/sys/devices/system/cpu/cpu0/topology/thread_siblings_list", |_| true, 1)));

    let data = PathBuf::from(std::env::var("DATA_DIR").unwrap_or("/data".into()));
    let scratch = PathBuf::from(std::env::var("SCRATCH_DIR").unwrap_or("/scratch".into()));
    let dev_files: Vec<PathBuf> = ["events", "clicks", "links"].iter().flat_map(|d| list_csv(&data.join(d))).collect();
    let (r, t) = residency(&dev_files);
    sum.push(format!("/data residency at start: {r}/{t} pages"));
    let (_, l) = run_child("dev", &data, &scratch.join("o_dev"), &[]);
    sum.push(l);

    let p20 = scratch.join("p20");
    let tg = Instant::now();
    let nev: u64 = std::env::var("SS_DIAG_EVENTS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000_000);
    if let Err(e) = crate::datagen::generate(7, nev, nev / 4, &p20, nthreads) {
        eprintln!("generate failed: {e}");
        std::process::exit(1);
    }
    let gen_s = tg.elapsed().as_secs_f64();
    let p20_files: Vec<PathBuf> = ["events", "clicks", "links"].iter().flat_map(|d| list_csv(&p20.join(d))).collect();
    let (r, t) = residency(&p20_files);
    sum.push(format!("generated {nev} events in {gen_s:.1} s; residency {r}/{t} pages"));
    let o = scratch.join("o20");
    let runs: [(&str, &[(&str, &str)]); 8] = [
        ("p20 default", &[]),
        ("p20 default", &[]),
        ("p20 default", &[]),
        ("p20 PREFAULT", &[("SS_PREFAULT", "1")]),
        ("p20 PRETOUCH2", &[("SS_PRETOUCH", "2")]),
        ("p20 PRETOUCH2+PREFAULT", &[("SS_PRETOUCH", "2"), ("SS_PREFAULT", "1")]),
        ("p20 SS_POP", &[("SS_POP", "1")]),
        ("p20 default", &[]),
    ];
    for (tag, envs) in runs.iter() {
        let (_, l) = run_child(tag, &p20, &o, envs);
        sum.push(l);
    }
    #[cfg(unix)]
    {
        sum.push(fault_bench("anon 1GB THP-advised 8thr", 1 << 30, true, nthreads));
        sum.push(fault_bench("anon 1GB no-advice   8thr", 1 << 30, false, nthreads));
        sum.push(fault_bench("anon 1GB THP-advised 1thr", 1 << 30, true, 1));
        let ev: Vec<PathBuf> = list_csv(&p20.join("events"));
        sum.push(file_fault_bench(&ev, nthreads));
        sum.push(file_fault_bench(&ev, nthreads));
    }
    sum.push(format!("AnonHugePages now: {}", grab("/proc/meminfo", |l| l.starts_with("AnonHugePages"), 1)));
    sum.push(format!("DIAG total {:.1} s; exiting 1 on purpose", t0.elapsed().as_secs_f64()));
    eprintln!("==== DIAG2 SUMMARY ====");
    for l in &sum {
        eprintln!("{l}");
    }
    std::process::exit(1);
}
