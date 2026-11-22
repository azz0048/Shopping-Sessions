//! Diagnostic build: measures the scoring machine, then fails on purpose so the dev-set
//! check returns these logs. Never part of a real entry.
#![allow(dead_code)]
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

fn show_file(p: &str, filter: impl Fn(&str) -> bool, max: usize) {
    match std::fs::read_to_string(p) {
        Ok(t) => {
            for l in t.lines().filter(|l| filter(l)).take(max) {
                eprintln!("  {p}: {l}");
            }
        }
        Err(e) => eprintln!("  {p}: <{e}>"),
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

fn run_child(tag: &str, data: &Path, out: &Path, envs: &[(&str, &str)]) -> f64 {
    let _ = std::fs::remove_dir_all(out);
    std::fs::create_dir_all(out).unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut c = Command::new(&exe);
    c.env("SS_CHILD", "1").env("DATA_DIR", data).env("OUTPUT_DIR", out);
    for (k, v) in envs {
        c.env(k, v);
    }
    let t = Instant::now();
    let st = c.status().unwrap();
    let ms = t.elapsed().as_secs_f64() * 1e3;
    eprintln!("== {tag}: wall {ms:.1} ms, status {st}");
    ms
}

#[cfg(unix)]
fn fault_bench(label: &str, bytes: usize, huge: bool, nthreads: usize) {
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
        let t = Instant::now();
        libc::munmap(p, bytes);
        let tu = t.elapsed().as_secs_f64() * 1e3;
        eprintln!("  fault {label}: {} MB touch {tf:.1} ms ({nthreads} thr), munmap {tu:.1} ms", bytes >> 20);
    }
}

#[cfg(unix)]
fn file_fault_bench(files: &[PathBuf], nthreads: usize) {
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
    let t = Instant::now();
    let next = std::sync::atomic::AtomicUsize::new(0);
    // 4 MB pieces, work-stealing
    let pieces: Vec<(usize, usize)> = maps.iter().flat_map(|&(b, l)| (0..l).step_by(4 << 20).map(move |o| (b + o, (l - o).min(4 << 20)))).collect();
    std::thread::scope(|s| {
        for _ in 0..nthreads {
            s.spawn(|| {
                let mut acc = 0u8;
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if i >= pieces.len() { break; }
                    let (b, l) = pieces[i];
                    let mut o = 0;
                    while o < l {
                        acc = acc.wrapping_add(unsafe { std::ptr::read_volatile((b + o) as *const u8) });
                        o += 4096;
                    }
                }
                std::hint::black_box(acc);
            });
        }
    });
    let tf = t.elapsed().as_secs_f64() * 1e3;
    // full streaming read (memory bandwidth from page cache, now mapped)
    let t = Instant::now();
    let next = std::sync::atomic::AtomicUsize::new(0);
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
                        o += 64;
                    }
                }
                std::hint::black_box(acc);
            });
        }
    });
    let tr = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    for &(m, l) in &maps {
        unsafe { libc::munmap(m as *mut libc::c_void, l) };
    }
    let tu = t.elapsed().as_secs_f64() * 1e3;
    eprintln!("  file map {} MB: first-touch {tf:.1} ms, stream read {tr:.1} ms ({:.1} GB/s), munmap {tu:.1} ms", total >> 20, total as f64 / tr / 1e6);
}

pub fn diag_main() -> ! {
    let t0 = Instant::now();
    let nthreads: usize = std::env::var("NCPU").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
    eprintln!("==== DIAG build (intentional failure) ====");
    for (k, v) in std::env::vars() {
        if ["NCPU", "MEM_GB", "DATA_DIR", "OUTPUT_DIR", "SCRATCH_DIR", "HOME"].contains(&k.as_str()) {
            eprintln!("  env {k}={v}");
        }
    }
    eprintln!("  available_parallelism {:?}", std::thread::available_parallelism());
    show_file("/proc/cpuinfo", |l| l.starts_with("model name") || l.starts_with("cpu MHz") || l.starts_with("cache size") || l.starts_with("siblings") || l.starts_with("cpu cores"), 5);
    show_file("/proc/cpuinfo", |l| l.starts_with("flags"), 1);
    show_file("/proc/version", |_| true, 1);
    show_file("/sys/kernel/mm/transparent_hugepage/enabled", |_| true, 1);
    show_file("/sys/kernel/mm/transparent_hugepage/defrag", |_| true, 1);
    show_file("/sys/kernel/mm/transparent_hugepage/shmem_enabled", |_| true, 1);
    show_file("/proc/meminfo", |l| ["MemTotal", "MemFree", "MemAvailable", "Cached", "AnonHugePages", "HugePages_Total", "Hugepagesize"].iter().any(|k| l.starts_with(k)), 10);
    show_file("/proc/self/status", |l| l.starts_with("Cpus_allowed_list") || l.starts_with("Mems_allowed_list"), 2);
    show_file("/sys/fs/cgroup/cpu.max", |_| true, 1);
    show_file("/sys/fs/cgroup/memory.max", |_| true, 1);
    show_file("/sys/fs/cgroup/cpuset.cpus.effective", |_| true, 1);
    show_file("/proc/mounts", |l| l.contains(" /data ") || l.contains(" /scratch ") || l.contains(" /output ") || l.contains(" / ") || l.contains(" /tmp "), 8);
    show_file("/proc/sys/kernel/numa_balancing", |_| true, 1);
    show_file("/sys/devices/system/cpu/cpu0/topology/thread_siblings_list", |_| true, 1);
    show_file("/sys/devices/system/cpu/smt/active", |_| true, 1);

    let data = PathBuf::from(std::env::var("DATA_DIR").unwrap_or("/data".into()));
    let scratch = PathBuf::from(std::env::var("SCRATCH_DIR").unwrap_or("/scratch".into()));
    let out_dir = PathBuf::from(std::env::var("OUTPUT_DIR").unwrap_or("/output".into()));
    let dev_files: Vec<PathBuf> = ["events", "clicks", "links"].iter().flat_map(|d| list_csv(&data.join(d))).collect();
    let (r, t) = residency(&dev_files);
    eprintln!("  /data page-cache residency at start: {r}/{t} pages");

    // 1) the real pipeline on the dev set (also produces the real output once, then we fail anyway)
    run_child("dev run 1 (verbose)", &data, &scratch.join("o_dev"), &[("SS_VERBOSE", "1")]);
    run_child("dev run 2", &data, &scratch.join("o_dev"), &[]);
    run_child("dev run 3", &data, &scratch.join("o_dev"), &[]);

    // 2) practice-size dataset in /scratch
    let p20 = scratch.join("p20");
    let tg = Instant::now();
    let nev: u64 = std::env::var("SS_DIAG_EVENTS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000_000);
    match crate::datagen::generate(7, nev, nev / 4, &p20, nthreads) {
        Ok(()) => eprintln!("== generated {nev} events / {} clicks, seed 7, in {:.1} s", nev / 4, tg.elapsed().as_secs_f64()),
        Err(e) => {
            eprintln!("== generate failed: {e}");
            std::process::exit(1);
        }
    }
    let p20_files: Vec<PathBuf> = ["events", "clicks", "links"].iter().flat_map(|d| list_csv(&p20.join(d))).collect();
    let (r, t) = residency(&p20_files);
    eprintln!("  p20 residency: {r}/{t} pages; {} files", p20_files.len());
    let o = scratch.join("o20");
    let mut walls = vec![];
    walls.push(run_child("p20 run 1", &p20, &o, &[]));
    walls.push(run_child("p20 run 2 (verbose)", &p20, &o, &[("SS_VERBOSE", "1")]));
    walls.push(run_child("p20 run 3", &p20, &o, &[]));
    walls.push(run_child("p20 run 4", &p20, &o, &[]));
    walls.push(run_child("p20 run 5 (verbose)", &p20, &o, &[("SS_VERBOSE", "1")]));
    walls.sort_by(|a, b| a.partial_cmp(b).unwrap());
    eprintln!("== p20 walls sorted: {walls:.1?}");
    run_child("p20 NCPU=16", &p20, &o, &[("NCPU", "16")]);
    run_child("p20 SS_POP=1", &p20, &o, &[("SS_POP", "1")]);
    run_child("p20 SS_P1ONLY", &p20, &o, &[("SS_P1ONLY", "1"), ("SS_VERBOSE", "1")]);

    // 3) machine microbenchmarks
    #[cfg(unix)]
    {
        fault_bench("anon THP-advised", 1 << 30, true, nthreads);
        fault_bench("anon no-advice", 1 << 30, false, nthreads);
        fault_bench("anon THP-advised 1thr", 1 << 30, true, 1);
        let ev: Vec<PathBuf> = list_csv(&p20.join("events"));
        file_fault_bench(&ev, nthreads);
        file_fault_bench(&ev, nthreads);
    }
    let _ = out_dir;
    eprintln!("==== DIAG total {:.1} s; exiting 1 on purpose ====", t0.elapsed().as_secs_f64());
    std::process::exit(1);
}
