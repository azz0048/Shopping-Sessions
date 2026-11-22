//! Portable safety-net binary: only the generic (reference-speed) pipeline,
//! compiled for baseline x86-64 so it runs on CPUs without AVX-512.
//! `pipeline` execs this when the CPU lacks the features it was built for.
#![allow(dead_code, unused_imports, static_mut_refs, unexpected_cfgs)]

#[path = "pqw.rs"]
mod pqw;
#[path = "generic/mod.rs"]
mod generic;

pub const fn env_u32(s: Option<&str>, d: u32) -> u32 {
    match s {
        None => d,
        Some(s) => {
            let b = s.as_bytes();
            let (mut v, mut i) = (0u32, 0);
            while i < b.len() {
                v = v * 10 + (b[i] - b'0') as u32;
                i += 1;
            }
            v
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().filter(|a| a != "--generic").collect();
    let (data, out) = if args.len() >= 3 {
        (args[args.len() - 2].clone(), args[args.len() - 1].clone())
    } else {
        (std::env::var("DATA_DIR").unwrap_or_else(|_| "/data".into()), std::env::var("OUTPUT_DIR").unwrap_or_else(|_| "/output".into()))
    };
    let ncpu = std::env::var("NCPU").ok().and_then(|s| s.trim().parse().ok()).filter(|&n: &usize| n > 0).unwrap_or(8);
    eprintln!("[portable] generic pipeline");
    match generic::run(std::path::Path::new(&data), std::path::Path::new(&out), ncpu) {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("[portable] error: {e}");
            std::process::exit(1)
        }
    }
}
