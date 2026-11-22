//! The published dataset generator (generator.zip), embedded for the diagnostic build only.
#![allow(dead_code, clippy::all)]
mod plan;
mod random;
mod text;
mod user;

use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use plan::{CATEGORIES, CHANNELS, Dataset};
use random::{Random, Stream};
use user::{Parts, Shard, generate_shard};

pub fn generate(seed: u64, events: u64, clicks: u64, output: &Path, threads: usize) -> io::Result<()> {
    let dataset = Dataset::plan(seed, events, clicks);
    let shards: Vec<Mutex<Option<Shard>>> = dataset.shards.iter().map(|_| Mutex::new(None)).collect();
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| loop {
                let at = next.fetch_add(1, Ordering::Relaxed);
                let Some(users) = dataset.shards.get(at) else { break };
                *shards[at].lock().unwrap() = Some(generate_shard(&dataset, users.clone()));
            });
        }
    });
    let shards: Vec<Shard> = shards.into_iter().map(|s| s.into_inner().unwrap().unwrap()).collect();
    fs::create_dir_all(output.join("events"))?;
    fs::create_dir_all(output.join("clicks"))?;
    fs::create_dir_all(output.join("links"))?;
    let event_parts: Vec<&Parts> = shards.iter().map(|s| &s.events).collect();
    let click_parts: Vec<&Parts> = shards.iter().map(|s| &s.clicks).collect();
    let link_parts: Vec<&Parts> = shards.iter().map(|s| &s.links).collect();
    let mut jobs: Vec<(&[&Parts], usize, &str, Stream, &str)> = Vec::new();
    for part in 0..dataset.event_parts.len() {
        jobs.push((event_parts.as_slice(), part, "events", Stream::ShuffleEvents, EVENT_HEADER));
    }
    for part in 0..dataset.click_parts.len() {
        jobs.push((click_parts.as_slice(), part, "clicks", Stream::ShuffleClicks, CLICK_HEADER));
    }
    for part in 0..dataset.link_parts.len() {
        jobs.push((link_parts.as_slice(), part, "links", Stream::ShuffleLinks, LINK_HEADER));
    }
    let next = AtomicUsize::new(0);
    let failure: Mutex<Option<io::Error>> = Mutex::new(None);
    thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                while let Some(&(parts, part, folder, stream, header)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                    let path = output.join(folder).join(format!("part-{part:05}.csv"));
                    let mut random = Random::new(dataset.seed, stream, part as u64);
                    if let Err(e) = write_part(&path, header, parts, part, &mut random) {
                        failure.lock().unwrap().get_or_insert(e);
                    }
                }
            });
        }
    });
    if let Some(e) = failure.into_inner().unwrap() {
        return Err(e);
    }
    write_lookups(output, &dataset)
}

const EVENT_HEADER: &str = "event_id,user_id,ts,ingest_ts,event_type,product_id,price_cents";
const CLICK_HEADER: &str = "click_id,user_id,campaign_id,ts";
const LINK_HEADER: &str = "user_id_a,user_id_b";

fn write_part(path: &Path, header: &str, shards: &[&Parts], part: usize, random: &mut Random) -> io::Result<()> {
    let mut lines: Vec<&[u8]> = Vec::new();
    for shard in shards {
        let bytes = &shard.bytes[part];
        let starts = &shard.starts[part];
        for (at, &start) in starts.iter().enumerate() {
            let end = starts.get(at + 1).map_or(bytes.len(), |&n| n as usize);
            lines.push(&bytes[start as usize..end]);
        }
    }
    random.shuffle(&mut lines);
    let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
    writeln!(w, "{header}")?;
    for line in lines {
        w.write_all(line)?;
    }
    w.flush()
}

fn write_lookups(out: &Path, dataset: &Dataset) -> io::Result<()> {
    let mut products = BufWriter::new(File::create(out.join("products.csv"))?);
    writeln!(products, "product_id,category")?;
    for (i, c) in dataset.product_category.iter().enumerate() {
        writeln!(products, "p{},{}", i + 1, CATEGORIES[*c as usize])?;
    }
    products.flush()?;
    let mut campaigns = BufWriter::new(File::create(out.join("campaigns.csv"))?);
    writeln!(campaigns, "campaign_id,channel")?;
    for (i, c) in dataset.campaign_channel.iter().enumerate() {
        writeln!(campaigns, "c_{},{}", i + 1, CHANNELS[*c as usize])?;
    }
    campaigns.flush()?;
    let mut start = Vec::new();
    text::write_utc(&mut start, dataset.window_start);
    let mut end = Vec::new();
    text::write_utc(&mut end, dataset.window_end());
    fs::write(
        out.join("manifest.json"),
        format!(
            "{{\n  \"window_start\": \"{}\",\n  \"window_end\": \"{}\"\n}}\n",
            String::from_utf8_lossy(&start),
            String::from_utf8_lossy(&end)
        ),
    )
}
