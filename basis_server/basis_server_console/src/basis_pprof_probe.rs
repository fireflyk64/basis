//! In-process sampling profiler, compiled only with `--features pprof` and armed only by
//! environment variables — the production build never contains it, and a build that does
//! contain it does nothing unless asked. Exists because this sandbox blocks `perf_event_open`
//! even for root, so the only way to see inside the server under load is to sample from within.
//!
//! Contract:
//!   BASIS_PPROF="<duration_s>:<out_prefix>"   — sample for that long, write
//!       <out_prefix>.folded   collapsed stacks ("thread;root;...;leaf count"), and
//!       <out_prefix>.svg      a flamegraph of the same samples.
//!   BASIS_PPROF_TRIGGER="<path>"              — optional: wait (up to 10 min) until this file
//!       exists before sampling, so a harness can start the window at exactly the right moment.
//!   BASIS_PPROF_DELAY_S="<n>"                 — optional fallback when no trigger file: wait
//!       n seconds after boot (default 5).
//!
//! Sampling is SIGPROF-based (pprof crate), 199 Hz, with the libc/pthread frames blocklisted
//! from unwinding as that crate recommends. The probe thread logs what it does and every
//! failure; it never panics and never touches the server's state.

use basis_network_core::BNL;
use std::fmt::Write as _;
use std::io::Write as _;
use std::time::{Duration, Instant};

pub fn arm_from_env() {
    let Ok(spec) = std::env::var("BASIS_PPROF") else { return };
    let Some((duration_s, prefix)) = parse_spec(&spec) else {
        BNL::log_warning(format!("[pprof] BASIS_PPROF=\"{spec}\" is not <duration_s>:<out_prefix>; profiler not armed"));
        return;
    };
    let trigger = std::env::var("BASIS_PPROF_TRIGGER").ok();
    let delay_s = std::env::var("BASIS_PPROF_DELAY_S").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(5);
    let spawned = std::thread::Builder::new()
        .name("basis-pprof".to_string())
        .spawn(move || run(duration_s, prefix, trigger, delay_s));
    match spawned {
        Ok(_) => BNL::log(format!("[pprof] armed: {duration_s}s of 199 Hz sampling; trigger={}", std::env::var("BASIS_PPROF_TRIGGER").unwrap_or_else(|_| format!("boot+{delay_s}s")))),
        Err(e) => BNL::log_warning(format!("[pprof] probe thread could not start: {e}")),
    }
}

fn parse_spec(spec: &str) -> Option<(u64, String)> {
    let (dur, prefix) = spec.split_once(':')?;
    let dur = dur.trim().parse::<u64>().ok().filter(|d| (1..=600).contains(d))?;
    if prefix.trim().is_empty() {
        return None;
    }
    Some((dur, prefix.trim().to_string()))
}

fn run(duration_s: u64, prefix: String, trigger: Option<String>, delay_s: u64) {
    match trigger {
        Some(path) => {
            let deadline = Instant::now() + Duration::from_secs(600);
            while !std::path::Path::new(&path).exists() {
                if Instant::now() >= deadline {
                    BNL::log_warning(format!("[pprof] trigger {path} never appeared within 10 min; profiler giving up"));
                    return;
                }
                std::thread::sleep(Duration::from_millis(250));
            }
            BNL::log(format!("[pprof] trigger {path} seen; sampling {duration_s}s"));
        }
        None => std::thread::sleep(Duration::from_secs(delay_s)),
    }
    let guard = match pprof::ProfilerGuardBuilder::default()
        .frequency(199)
        .blocklist(&["libc", "libgcc", "pthread", "vdso"])
        .build()
    {
        Ok(g) => g,
        Err(e) => {
            BNL::log_warning(format!("[pprof] sampler could not start: {e}"));
            return;
        }
    };
    std::thread::sleep(Duration::from_secs(duration_s));
    let report = match guard.report().build() {
        Ok(r) => r,
        Err(e) => {
            BNL::log_warning(format!("[pprof] report failed: {e}"));
            return;
        }
    };
    drop(guard);

    // Collapsed stacks, root first, one line per unique (thread, stack): the format every
    // flamegraph and aggregation tool reads, and small enough to commit next to the results.
    let mut folded = String::new();
    let mut per_thread: std::collections::HashMap<String, isize> = std::collections::HashMap::new();
    let mut total: isize = 0;
    for (frames, count) in report.data.iter() {
        let mut line = String::new();
        line.push_str(&frames.thread_name);
        for frame in frames.frames.iter().rev() {
            for symbol in frame.iter().rev() {
                let _ = write!(line, ";{symbol}");
            }
        }
        let _ = writeln!(folded, "{line} {count}");
        *per_thread.entry(frames.thread_name.clone()).or_insert(0) += *count;
        total += *count;
    }
    let mut threads: Vec<(String, isize)> = per_thread.into_iter().collect();
    threads.sort_by(|a, b| b.1.cmp(&a.1));
    let mut summary = format!("total_samples {total}\n");
    for (name, count) in &threads {
        let _ = writeln!(summary, "thread {name} {count}");
    }

    write_out(&format!("{prefix}.folded"), folded.as_bytes());
    write_out(&format!("{prefix}.threads"), summary.as_bytes());
    match std::fs::File::create(format!("{prefix}.svg")) {
        Ok(mut f) => {
            if let Err(e) = report.flamegraph(&mut f) {
                BNL::log_warning(format!("[pprof] flamegraph failed: {e}"));
            }
        }
        Err(e) => BNL::log_warning(format!("[pprof] cannot create {prefix}.svg: {e}")),
    }
    BNL::log(format!("[pprof] wrote {prefix}.folded/.threads/.svg ({total} samples)"));
}

fn write_out(path: &str, bytes: &[u8]) {
    match std::fs::File::create(path) {
        Ok(mut f) => {
            if let Err(e) = f.write_all(bytes) {
                BNL::log_warning(format!("[pprof] writing {path} failed: {e}"));
            }
        }
        Err(e) => BNL::log_warning(format!("[pprof] cannot create {path}: {e}")),
    }
}
