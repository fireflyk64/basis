//! Port of `BasisFirstBootTuning.cs`.
//!
//! Offers to fit the server's settings to this machine the first time it boots, by running the
//! benchmark that ships beside it. A child process, never an in-process call: the benchmark
//! measures the server by starting and stopping it, repeatedly, with different settings — and
//! something that has to restart the server cannot be running inside it. Going through a process
//! also means the only thing this costs a normal boot is one file probe.

use std::io::Write;
use std::path::{Path, PathBuf};

use basis_network_core::BNL;

pub struct BasisFirstBootTuning;

impl BasisFirstBootTuning {
    /// Older layout: the tooling used to be published into a benchmark/ subfolder with the load
    /// client under benchmark/loadclient/. It now sits flat beside the server, but an install
    /// unpacked before that keeps working.
    const BENCHMARK_FOLDER: &'static str = "benchmark";

    /// The benchmark and load client, under their Rust binary names and the C# ones an older
    /// install may still carry.
    const BENCHMARK_NAMES: [&'static str; 2] = ["basis_server_benchmark", "BasisServerBenchmark"];
    const LOAD_CLIENT_NAMES: [&'static str; 2] = ["basis_network_client_console", "BasisNetworkClientConsole"];

    /// Set to 1/true to tune without prompting, or 0/false to skip it. Provisioning scripts have
    /// nobody to answer the question and must not be left blocked on it.
    const ENVIRONMENT_SWITCH: &'static str = "BASIS_AUTOTUNE";

    /// Runs the benchmark if this is a first boot, the tool is present, and the operator wants
    /// it. Returns true when a tuning profile was produced and is waiting to be applied.
    pub fn run(base_directory: &Path, config_directory: &Path) -> bool {
        let profile_path = config_directory.join("tuning-profile.xml");
        if profile_path.exists() {
            // Already tuned — a profile is sitting here waiting to be applied, so there is nothing
            // to measure and the caller will pick it up.
            return true;
        }

        let benchmark_directory = base_directory.join(Self::BENCHMARK_FOLDER);
        let Some(benchmark) = Self::find_any(base_directory, &Self::BENCHMARK_NAMES).or_else(|| Self::find_any(&benchmark_directory, &Self::BENCHMARK_NAMES)) else {
            BNL::log("[Tuning] No benchmark beside the server, so first-boot tuning is unavailable. The server is starting on its shipped defaults, which is a supported way to run it.");
            return false;
        };

        // The benchmark needs a crowd to measure, and the crowd is a separate binary.
        let Some(load_client) = Self::find_any(base_directory, &Self::LOAD_CLIENT_NAMES)
            .or_else(|| Self::find_any(&benchmark_directory.join("loadclient"), &Self::LOAD_CLIENT_NAMES))
            .or_else(|| Self::find_any(&benchmark_directory, &Self::LOAD_CLIENT_NAMES))
        else {
            BNL::log("[Tuning] The benchmark is present but the load client it needs is not, so there is nothing to generate load with. Starting on the shipped defaults.");
            return false;
        };

        let Some(mode) = Self::choose_mode() else {
            return false;
        };
        Self::announce_start(mode);

        let benchmark_dir = benchmark.parent().map(Path::to_path_buf).unwrap_or_else(|| base_directory.to_path_buf());
        let load_client_dir = load_client.parent().map(Path::to_path_buf).unwrap_or_else(|| base_directory.to_path_buf());
        // Inherited stdio rather than captured: this runs for hours and the operator is sitting in
        // front of it. Redirecting would hide the ladder and the sweep behind a silent prompt.
        let status = std::process::Command::new(&benchmark)
            .current_dir(&benchmark_dir)
            .arg("--auto")
            .arg(mode)
            .arg("--server")
            .arg(base_directory)
            .arg("--client")
            .arg(&load_client_dir)
            .status();
        match status {
            Ok(status) if !status.success() => BNL::log_warning(format!("[Tuning] The benchmark exited with {status}.")),
            Ok(_) => {}
            Err(e) => {
                BNL::log_warning(format!("[Tuning] The benchmark failed to run ({e}). Starting on the shipped defaults."));
                return false;
            }
        }

        if !profile_path.exists() {
            BNL::log("[Tuning] The benchmark produced no profile - it found nothing worth changing on this machine, which is a real result. Starting on the shipped defaults.");
            return false;
        }

        Self::say("");
        Self::say("  ----------------------------------------------------------------------------");
        Self::say("   Tuning finished. Applying what it measured, then starting the server.");
        Self::say("   The full report is under 'benchmark-results', beside the server.");
        Self::say("  ----------------------------------------------------------------------------");
        Self::say("");
        true
    }

    /// Which depth of tuning to run, or None to skip.
    ///
    /// Three choices rather than yes/no, because the honest answer to "how long does this take"
    /// ranges from five minutes to a couple of hours. The default when nobody answers is skip: a
    /// server started by systemd or a container runtime that silently disappeared on its first
    /// boot would look like a failed deploy.
    fn choose_mode() -> Option<&'static str> {
        if let Some(configured) = std::env::var(Self::ENVIRONMENT_SWITCH).ok().filter(|v| !v.is_empty()) {
            let chosen = Self::normalise_mode(&configured);
            BNL::log(match chosen {
                None => format!("[Tuning] {}={configured}, so tuning is skipped.", Self::ENVIRONMENT_SWITCH),
                Some(mode) => format!("[Tuning] {}={configured}, so a '{mode}' run is starting.", Self::ENVIRONMENT_SWITCH),
            });
            return chosen;
        }

        if !Self::stdin_is_terminal() {
            BNL::log(format!(
                "[Tuning] This machine has never been tuned, and there is no terminal to ask. Set {} to quick, medium or long to tune on first boot, or run {} beside the server yourself later. Starting on the shipped defaults.",
                Self::ENVIRONMENT_SWITCH,
                Self::BENCHMARK_NAMES[0]
            ));
            return None;
        }

        Self::say("");
        Self::say("  This machine has not been tuned yet.");
        Self::say("");
        Self::say("  The benchmark measures what this host actually does under load and fits the settings");
        Self::say("  to it. The server is not reachable while it runs.");
        Self::say("");
        Self::say("    1  quick    ~5 minutes    codec settings, parallel pass width, auth window");
        Self::say("    2  medium   ~15 minutes   adds the player cap and what limits this box  (recommended)");
        Self::say("    3  long     ~2 hours      adds the A/B setting sweep");
        Self::say("    s  skip                   start now on the shipped defaults");
        Self::say("");
        Self::say(&format!("  Skipping is fine, and you can run {} beside the server whenever it", Self::BENCHMARK_NAMES[0]));
        Self::say("  suits - it is the same tool, and it will offer the same choices.");
        Self::say("");
        Self::ask("  Which? [2] ");

        let answer = Self::read_answer();
        if answer.is_empty() {
            return Some("medium");
        }
        // Menu digits are positional and mean something different from the environment
        // variable's "1", which predates these modes and meant "yes".
        match answer.to_lowercase().as_str() {
            "1" => return Some("quick"),
            "2" => return Some("medium"),
            "3" => return Some("long"),
            _ => {}
        }
        let mode = Self::normalise_mode(&answer);
        if mode.is_none() {
            BNL::log("[Tuning] Skipped. Starting on the shipped defaults.");
        }
        mode
    }

    /// Rough wall time per mode, so the wait is a stated expectation rather than a surprise.
    fn expected_duration(mode: &str) -> &'static str {
        match mode {
            "quick" => "about 5 minutes",
            "long" => "a couple of hours",
            _ => "about 15 minutes",
        }
    }

    /// Says clearly that the benchmark is running and roughly how long it will be. The child
    /// inherits stdout so its progress is visible, which is useful once you know what you are
    /// looking at and alarming when you do not.
    fn announce_start(mode: &str) {
        Self::say("");
        Self::say("  ============================================================================");
        Self::say(&format!("   TUNING THIS MACHINE - {mode}, {}", Self::expected_duration(mode)));
        Self::say("  ============================================================================");
        Self::say("");
        Self::say("   The benchmark is running now. It starts and stops copies of this server under");
        Self::say("   load to find out what this hardware actually does, so THE SERVER IS NOT UP YET");
        Self::say("   and nobody can connect until it finishes.");
        Self::say("");
        Self::say("   Progress appears below as it works through each population. When it is done the");
        Self::say("   settings it measured are applied and the server starts on its own - there is");
        Self::say("   nothing else for you to do.");
        Self::say("");
        Self::say("   Ctrl-C stops the benchmark; the server then starts on the shipped defaults.");
        Self::say("");
        Self::say("  ----------------------------------------------------------------------------");
        Self::say("");
        BNL::log(format!("[Tuning] Benchmark started ({mode}, {}). Server start is deferred until it finishes.", Self::expected_duration(mode)));
    }

    /// Maps an environment value to a mode word, or None to skip.
    pub fn normalise_mode(value: &str) -> Option<&'static str> {
        match value.trim().to_lowercase().as_str() {
            "quick" => Some("quick"),
            // "1"/"true" predate the three modes and meant "tune". Kept working, and pointed at
            // the recommended depth rather than the longest one.
            "1" | "true" | "medium" => Some("medium"),
            "long" | "full" => Some("long"),
            _ => None,
        }
    }

    fn find_any(directory: &Path, names: &[&str]) -> Option<PathBuf> {
        names.iter().find_map(|name| Self::find_executable(directory, name))
    }

    /// Finds a published executable by base name, whatever the platform calls it.
    pub fn find_executable(directory: &Path, base_name: &str) -> Option<PathBuf> {
        if !directory.is_dir() {
            return None;
        }
        let windows = directory.join(format!("{base_name}.exe"));
        if windows.is_file() {
            return Some(windows);
        }
        let unix = directory.join(base_name);
        if unix.is_file() { Some(unix) } else { None }
    }

    fn stdin_is_terminal() -> bool {
        #[cfg(unix)]
        {
            // SAFETY: isatty only inspects a descriptor.
            unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    fn say(line: &str) {
        let mut out = std::io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }

    fn ask(prompt: &str) {
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "{prompt}");
        let _ = out.flush();
    }

    fn read_answer() -> String {
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        line.trim().to_string()
    }
}
