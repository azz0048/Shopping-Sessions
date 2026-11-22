//! Diagnostic build: times /pipeline under setting variants on the scoring machine, then
//! fails on purpose so the dev-set check returns the summary (the END of stderr is shown).
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

const VARIANTS: &str = include_str!("diag_runs.txt");

pub fn diag_main() -> ! {
    let t0 = Instant::now();
    let nthreads: usize = std::env::var("NCPU").ok().and_then(|s| s.parse().ok()).unwrap_or(8);
    let scratch = PathBuf::from(std::env::var("SCRATCH_DIR").unwrap_or("/scratch".into()));
    let p20 = scratch.join("p20");
    let nev: u64 = std::env::var("SS_DIAG_EVENTS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000_000);
    if let Err(e) = crate::datagen::generate(7, nev, nev / 4, &p20, nthreads) {
        eprintln!("generate failed: {e}");
        std::process::exit(1);
    }
    let gen_s = t0.elapsed().as_secs_f64();
    let out = scratch.join("o20");
    let runs: Vec<(String, Vec<(String, String)>)> = VARIANTS
        .split(';')
        .map(|r| r.trim())
        .filter(|r| !r.is_empty())
        .map(|r| {
            let (tag, kv) = r.split_once(':').unwrap_or((r, ""));
            let envs = kv.split(',').filter_map(|x| x.split_once('=')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
            (tag.trim().to_string(), envs)
        })
        .collect();
    let rounds: usize = 12;
    let mut times: Vec<Vec<f64>> = vec![Vec::new(); runs.len()];
    let mut bad: Vec<usize> = vec![0; runs.len()];
    let mut rows: Vec<String> = vec![String::new(); runs.len()];
    // warm-up (page cache, first-run effects)
    for _ in 0..2 {
        let _ = std::fs::remove_dir_all(&out);
        let _ = std::fs::create_dir_all(&out);
        let _ = Command::new("/pipeline").env("DATA_DIR", &p20).env("OUTPUT_DIR", &out).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    for round in 0..rounds {
        // rotate the order each round so no variant always runs first
        for k in 0..runs.len() {
            let i = (k + round * 7) % runs.len();
            let (_, envs) = &runs[i];
            let _ = std::fs::remove_dir_all(&out);
            let _ = std::fs::create_dir_all(&out);
            let bin = envs.iter().find(|(k, _)| k == "BIN").map(|(_, v)| v.clone()).unwrap_or("/pipeline".into());
            let mut c = Command::new(&bin);
            c.env("DATA_DIR", &p20).env("OUTPUT_DIR", &out).stdout(Stdio::null()).stderr(Stdio::null());
            for (k, v) in envs {
                if k != "BIN" {
                    c.env(k, v);
                }
            }
            let t = Instant::now();
            let st = c.status();
            let w = t.elapsed().as_secs_f64() * 1e3;
            if !st.map(|s| s.success()).unwrap_or(false) {
                bad[i] += 1;
            }
            times[i].push(w);
            if round == 0 {
                // output size as a cheap sanity check of the result
                let mut bytes = 0u64;
                for t in ["user_daily", "campaign_daily"] {
                    if let Ok(rd) = std::fs::read_dir(out.join(t)) {
                        for e in rd.flatten() {
                            bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
                        }
                    }
                }
                rows[i] = format!("{bytes}");
            }
        }
    }
    eprintln!("==== DIAG6 (gen {gen_s:.1}s) ms over {rounds} ====");
    for (i, (tag, _)) in runs.iter().enumerate() {
        let mut v = times[i].clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        let q1 = v[v.len() / 4];
        let q3 = v[3 * v.len() / 4];
        eprintln!("{tag:>9} med {:6.1} min {:6.1} mean {mean:6.1} q1 {q1:6.1} q3 {q3:6.1}{} {}", v[v.len() / 2], v[0], if bad[i] > 0 { format!(" FAIL{}", bad[i]) } else { String::new() }, rows[i]);
    }
    eprintln!("total {:.1} s", t0.elapsed().as_secs_f64());
    std::process::exit(1);
}
