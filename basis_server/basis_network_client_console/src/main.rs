//! Port of `BasisNetworkClientConsole`: the headless load-test client.
//!
//! Spawns `ClientCount` simulated players that connect to a server, move, talk and reconnect,
//! and reports whether the harness itself kept up so a server measurement is never quietly a
//! measurement of the load generator instead.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]
#![deny(unused_must_use)]
// The modules mirror the C# public surface; the harness itself uses a subset of it.
#![allow(dead_code)]

mod audio;
mod avatar;
mod client;
mod diagnostics;
mod util;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use audio::microphone_capture::MicrophoneCapture;
use audio::voice_delivery_stats::VoiceDeliveryStats;
use basis_network_core::BNL;
use basis_network_core::transport::basis_network_shell::NetDebug;
use basis_network_core::transport::iroh_network_impl::IrohRuntime;
use client::client_manager::{ClientManager, ClientSlot};
use client::config_manager::ConfigManager;
use client::message_handler::MessageHandler;
use client::movement_sender::{MovementSender, VoiceSender};
use diagnostics::basis_client_logger::BasisClientLogger;
use diagnostics::bundle_capture_sink::BundleCaptureSink;
use util::error_handlers::ErrorHandlers;

pub struct Program;

const DRIVER_TICK_MS: f64 = 15.0;
const MOVEMENT_INTERVAL_MS: f64 = 90.0;
const MAX_VOICE_CATCH_UP_FRAMES: i32 = 5;

static RUNNING: AtomicBool = AtomicBool::new(true);
static SHUTDOWN_STARTED: AtomicBool = AtomicBool::new(false);
/// Driver iterations that took longer than DRIVER_TICK_MS — the harness falling behind.
static DRIVER_OVERRUNS: AtomicI64 = AtomicI64::new(0);
/// Worst driver iteration seen, in ms (f64 bits).
static DRIVER_PEAK_MS: AtomicU64 = AtomicU64::new(0);

impl Program {
    pub fn is_running() -> bool {
        RUNNING.load(Ordering::Acquire)
    }
}

fn env_flag(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn main() {
    ErrorHandlers::attach_global_handlers();
    ConfigManager::load_or_create_config_xml("ClientSimConfig.xml");
    NetDebug::set_logger(Some(Arc::new(BasisClientLogger)));

    // Face-data test mode: BASIS_EMIT_FACE=1 attaches a synthetic AdditionalAvatarData to every
    // avatar send and logs when other clients' additional data arrives — an end-to-end probe of
    // the face-tracking transport. Companions:
    //   BASIS_FACE_SPACING=<m>  pin client i at (i*m,1,0), no random walk (distance tiers)
    //   BASIS_UPLINK_DELTAS=0   legacy all-keyframe uploads (no v42 uplink deltas)
    //   BASIS_PACKET_LOSS=<pct> simulated loss (LiteNetLib only; the iroh transport has none)
    //   BASIS_BUNDLE_CAPTURE=<path>  harvest decoded avatar-bundle bodies for Zstd
    //                                dictionary training (see BundleCaptureSink)
    //   BASIS_BUNDLE_CAPTURE_EVERY=<n>   keep 1 bundle in n (default 200)
    //   BASIS_BUNDLE_CAPTURE_MAX=<n>     stop after n samples (default 20000)
    if let Some(capture_path) = env_flag("BASIS_BUNDLE_CAPTURE") {
        let capture_every = env_flag("BASIS_BUNDLE_CAPTURE_EVERY").and_then(|v| v.parse::<i32>().ok()).filter(|v| *v >= 1).unwrap_or(200);
        let capture_max = env_flag("BASIS_BUNDLE_CAPTURE_MAX").and_then(|v| v.parse::<i32>().ok()).filter(|v| *v >= 1).unwrap_or(20000);
        match BundleCaptureSink::configure(&capture_path, capture_max, capture_every) {
            Ok(()) => BNL::log(format!("[BundleCapture] Capturing 1 bundle in {capture_every} (max {capture_max}) to {capture_path}.")),
            Err(e) => BNL::log_error(format!("[BundleCapture] Could not open {capture_path}: {e}")),
        }
    }
    if env_flag("BASIS_EMIT_FACE").as_deref() == Some("1") {
        MovementSender::set_emit_face_data(true);
        BNL::log("[FaceObserver] EmitFaceData enabled — every avatar send carries additional data.");
    }
    if let Some(spacing) = env_flag("BASIS_FACE_SPACING").and_then(|v| v.parse::<f32>().ok()).filter(|s| *s > 0.0) {
        MovementSender::set_pin_spacing_meters(spacing);
        BNL::log(format!("[FaceObserver] Positions pinned at {spacing}m spacing."));
    }
    if env_flag("BASIS_UPLINK_DELTAS").as_deref() == Some("0") {
        MovementSender::set_use_uplink_deltas(false);
        BNL::log("[FaceObserver] Uplink deltas disabled — legacy all-keyframe uploads.");
    }
    // Spectator mode: join a live server (e.g. during a Unity-client repro) and report whether
    // OTHER senders' additional data reaches the wire, without emitting any.
    if env_flag("BASIS_FACE_OBSERVE_ONLY").as_deref() == Some("1") {
        MessageHandler::set_observe_only(true);
        BNL::log("[FaceObserver] Observe-only: reporting additional data from other clients.");
    }

    let mut client_manager = ClientManager::new();
    client_manager.prepare();
    let client_manager = Arc::new(client_manager);

    // Every way this process is asked to stop ends in the same place, and all of them are
    // reachable: Ctrl-C interactively, SIGTERM from docker stop or systemd, a "stop" line from a
    // harness driving it. A population of a few thousand has to finish announcing its departure,
    // so nothing here relies on an exit-time budget.
    install_signal_handlers(client_manager.clone());
    start_stop_request_watcher(client_manager.clone());

    MovementSender::initialize(client_manager.client_count());
    VoiceSender::initialize(client_manager.client_count());

    // Drive all clients from one worker per CPU core
    start_client_driver_loops(client_manager.slots());

    if let Some(loss_pct) = env_flag("BASIS_PACKET_LOSS").and_then(|v| v.parse::<i32>().ok()).filter(|v| *v > 0) {
        BNL::log_warning(format!("[FaceObserver] BASIS_PACKET_LOSS={loss_pct} is a LiteNetLib simulation switch; the iroh transport carries no loss simulation, so it is ignored."));
    }

    client_manager.start_clients();

    // Voice delivery accounting. On whenever voice is simulated: it is a per-frame dictionary
    // touch on the receive path, which is nothing against the avatar traffic beside it, and
    // without it a run can only report what the server chose to drop rather than what a listener
    // would actually have heard.
    if ConfigManager::current().simulate_voice {
        VoiceDeliveryStats::set_enabled(true);
        spawn_reporter("VoiceReport", Duration::from_secs(5), || BNL::log(VoiceDeliveryStats::describe()));
    }

    // Periodic observer summary so a timed run ends with machine-readable totals.
    if MovementSender::emit_face_data() || MessageHandler::observe_only() {
        spawn_reporter("FaceReport", Duration::from_secs(5), || BNL::log(MessageHandler::summary()));
    }

    // Whether audio actually reaches the virtual cable is invisible otherwise.
    if MicrophoneCapture::active() {
        let mut last_frames = 0i64;
        let mut last_speech = 0i64;
        spawn_reporter("MicReport", Duration::from_secs(5), move || {
            let frames = MicrophoneCapture::frames_captured();
            let speech = MicrophoneCapture::frames_speech();
            let d_f = frames - last_frames;
            let d_s = speech - last_speech;
            last_frames = frames;
            last_speech = speech;
            let peak = MicrophoneCapture::take_peak();
            if d_s > 0 {
                BNL::log(format!("[Mic] {d_f} frames/5s, {d_s} with speech, peak {peak:.3} — transmitting."));
            } else if peak <= 0.0 {
                BNL::log(format!("[Mic] {d_f} frames/5s, peak 0.000 (digital silence) — nothing is routed into CABLE Input."));
            } else {
                BNL::log(format!("[Mic] {d_f} frames/5s, peak {peak:.4} but under the transmit threshold — signal is arriving, just too quiet."));
            }
        });
    }

    // Report whether the harness itself is keeping up. Without this a driver that cannot hit its
    // tick looks identical to a server that cannot keep up, and every number the run produces is
    // quietly a measurement of the load generator instead.
    let mut last_overruns = 0i64;
    spawn_reporter("DriverReport", Duration::from_secs(10), move || {
        let overruns = DRIVER_OVERRUNS.load(Ordering::Relaxed);
        let delta = overruns - last_overruns;
        last_overruns = overruns;
        let peak = f64::from_bits(DRIVER_PEAK_MS.swap(0, Ordering::Relaxed));
        if delta > 0 {
            BNL::log(format!("[Driver] BEHIND: {delta} slice overruns in 10s (peak {peak:.0}ms vs {DRIVER_TICK_MS}ms tick) — harness is limiting, not the server."));
        } else {
            BNL::log(format!("[Driver] healthy: 0 overruns in 10s ({DRIVER_TICK_MS}ms tick met)."));
        }
        BNL::log(MessageHandler::sender_fairness());
    });

    // Start random reconnects
    start_random_reconnect_loop(client_manager);

    // keep main alive
    loop {
        std::thread::park();
    }
}

/// Runs the shutdown once, whoever asks for it first. Ctrl-C, SIGTERM and the stop watcher can
/// all fire for one stop, so this has to be idempotent or the population is torn down twice.
fn shutdown(client_manager: &ClientManager) {
    if SHUTDOWN_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    println!("Shutting down...");
    RUNNING.store(false, Ordering::Release);
    MicrophoneCapture::stop();
    client_manager.stop_clients();
    // Close the capture file here rather than relying on a destructor: a run is normally ended
    // with Ctrl-C, and a half-written last record would make the whole capture unreadable.
    if let Some(capture_summary) = BundleCaptureSink::finish() {
        println!("{capture_summary}");
    }
}

fn install_signal_handlers(client_manager: Arc<ClientManager>) {
    let handler = async move {
        let ctrl_c = tokio::signal::ctrl_c();
        #[cfg(unix)]
        {
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(mut terminate) => {
                    tokio::select! {
                        _ = ctrl_c => {}
                        _ = terminate.recv() => {}
                    }
                }
                Err(_) => {
                    let _ = ctrl_c.await;
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = ctrl_c.await;
        }
        shutdown(&client_manager);
        std::process::exit(0);
    };
    if let Err(e) = IrohRuntime::spawn(handler) {
        BNL::log_warning(format!("Signal handling is unavailable ({}); type 'stop' to leave cleanly.", e.report()));
    }
}

/// Lets whatever started this process ask it to leave cleanly.
///
/// A harness cannot send SIGTERM on Windows, and killing the process runs no shutdown code at all
/// — exactly the case that leaves a server holding several thousand peers until they time out.
/// Watching stdin gives every platform one graceful stop: a "stop" or "quit" line means leave now.
/// End of stream is NOT a stop request: a process started with stdin closed reads EOF immediately.
fn start_stop_request_watcher(client_manager: Arc<ClientManager>) {
    let spawned = std::thread::Builder::new().name("StopRequestWatcher".into()).spawn(move || {
        let stdin = std::io::stdin();
        loop {
            let mut line = String::new();
            match stdin.read_line(&mut line) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let line = line.trim();
            if line.eq_ignore_ascii_case("stop") || line.eq_ignore_ascii_case("quit") || line.eq_ignore_ascii_case("exit") {
                break;
            }
        }
        shutdown(&client_manager);
        std::process::exit(0);
    });
    if let Err(e) = spawned {
        BNL::log_warning(format!("The stop watcher could not start: {e}"));
    }
}

fn spawn_reporter(name: &str, every: Duration, mut report: impl FnMut() + Send + 'static) {
    let spawned = std::thread::Builder::new().name(name.into()).spawn(move || {
        while Program::is_running() {
            std::thread::sleep(every);
            if !Program::is_running() {
                break;
            }
            report();
        }
    });
    if let Err(e) = spawned {
        BNL::log_warning(format!("The {name} thread could not start: {e}"));
    }
}

pub fn stop_client(manager: &ClientManager, index: usize) {
    if let Some(peer) = manager.slots().get(index).and_then(|slot| slot.peer()) {
        peer.disconnect();
    }
}

fn start_client_driver_loops(slots: Arc<Vec<ClientSlot>>) {
    let count = slots.len();
    let worker_count = num_cpus::get().min(count);
    if worker_count == 0 {
        return;
    }
    let chunk_size = count.div_ceil(worker_count);
    for w in 0..worker_count {
        let start = w * chunk_size;
        let end = (start + chunk_size).min(count);
        if start >= end {
            break;
        }
        let phase_offset_ms = MOVEMENT_INTERVAL_MS * w as f64 / worker_count as f64;
        let slots = slots.clone();
        let spawned = std::thread::Builder::new().name(format!("ClientDriver({start}-{end})")).spawn(move || drive_slice(&slots, start, end, phase_offset_ms));
        if let Err(e) = spawned {
            BNL::log_error(format!("Driver thread for clients {start}-{end} could not start: {e}"));
        }
    }
}

fn drive_slice(slots: &[ClientSlot], start: usize, end: usize, phase_offset_ms: f64) {
    let sw = Instant::now();
    let mut last_tick_ms = 0.0f64;
    let mut last_movement_ms = phase_offset_ms - MOVEMENT_INTERVAL_MS;
    let mut last_voice_ms = 0.0f64;

    // Amortized voice-recipient sweep state: a cursor over this worker's slice plus the fractional
    // number of rebuilds owed, so the sweep runs at a steady rate rather than in bursts.
    let slice_count = end - start;
    let mut refresh_cursor = start;
    let mut refresh_debt = 0.0f64;
    let mut last_refresh_ms = 0.0f64;

    while Program::is_running() {
        let now_ms = sw.elapsed().as_secs_f64() * 1000.0;
        let dt = (now_ms - last_tick_ms) as f32;
        last_tick_ms = now_ms;

        for slot in &slots[start..end] {
            if let Some(client) = slot.client() {
                client.poll();
                client.update(dt);
            }
        }

        if now_ms - last_movement_ms >= MOVEMENT_INTERVAL_MS {
            last_movement_ms = now_ms;
            for (i, slot) in slots.iter().enumerate().take(end).skip(start) {
                if let Some(peer) = slot.peer()
                    && slot.is_authenticated()
                {
                    MovementSender::process_single(&peer, i);
                }
            }
        }

        let config = ConfigManager::current();
        if config.simulate_voice {
            let mut voice_frame_ms = config.voice_frame_ms as f64;
            if voice_frame_ms <= 0.0 {
                voice_frame_ms = 20.0;
            }
            let mut due_frames = ((now_ms - last_voice_ms) / voice_frame_ms) as i32;
            if due_frames > MAX_VOICE_CATCH_UP_FRAMES {
                due_frames = MAX_VOICE_CATCH_UP_FRAMES;
                last_voice_ms = now_ms;
            } else if due_frames > 0 {
                last_voice_ms += due_frames as f64 * voice_frame_ms;
            }

            // Amortized recipient sweep: the whole slice is swept once per window at a steady
            // rate, so the per-tick cost is set by the window, not by the population.
            let mut window_ms = config.voice_recipient_refresh_ms as f64;
            if window_ms <= 0.0 {
                window_ms = 5000.0;
            }
            refresh_debt += slice_count as f64 * (now_ms - last_refresh_ms) / window_ms;
            last_refresh_ms = now_ms;
            let mut due_rebuilds = refresh_debt as usize;
            if due_rebuilds > 0 {
                refresh_debt -= due_rebuilds as f64;
                // Never let a stall turn into a burst that stalls the next tick too.
                due_rebuilds = due_rebuilds.min(slice_count);
                for _ in 0..due_rebuilds {
                    let idx = refresh_cursor;
                    refresh_cursor += 1;
                    if refresh_cursor >= end {
                        refresh_cursor = start;
                    }
                    let slot = &slots[idx];
                    if let Some(sweep_peer) = slot.peer()
                        && slot.is_authenticated()
                    {
                        let _ = VoiceSender::rebuild_recipients(&sweep_peer, slots, idx);
                    }
                }
            }

            if due_frames > 0 {
                for (i, slot) in slots.iter().enumerate().take(end).skip(start) {
                    let Some(peer) = slot.peer() else { continue };
                    if !slot.is_authenticated() {
                        continue;
                    }
                    // A client that has never been swept builds once immediately, so a joiner can
                    // transmit without waiting out a window. After that the sweep owns it.
                    let mut ready = VoiceSender::has_recipients(i);
                    if !ready {
                        ready = VoiceSender::rebuild_recipients(&peer, slots, i);
                    }
                    if ready {
                        let talking = VoiceSender::is_talking(i, now_ms);
                        let mic = VoiceSender::is_mic_client(i);
                        if talking && mic {
                            let _ = VoiceSender::send_mic_frames(&peer, i, due_frames);
                        } else if talking {
                            for _ in 0..due_frames {
                                VoiceSender::send_frame(&peer, i);
                            }
                        } else {
                            // Idle mic clients track the live edge, so a burst opens on current
                            // audio instead of replaying whatever was buffered when they went quiet.
                            if mic {
                                VoiceSender::sync_mic_cursor(i);
                            }
                            for _ in 0..due_frames {
                                VoiceSender::note_silence(i);
                            }
                        }
                    }
                }
            }
        }

        // A slice that takes longer than the tick silently degrades the whole simulation. Track it
        // so harness limits can never be mistaken for server results.
        let iteration_ms = sw.elapsed().as_secs_f64() * 1000.0 - now_ms;
        let sleep_ms = DRIVER_TICK_MS - iteration_ms;
        if sleep_ms > 0.0 {
            std::thread::sleep(Duration::from_secs_f64(sleep_ms / 1000.0));
        } else {
            DRIVER_OVERRUNS.fetch_add(1, Ordering::Relaxed);
            let mut peak = DRIVER_PEAK_MS.load(Ordering::Relaxed);
            while f64::from_bits(peak) < iteration_ms {
                match DRIVER_PEAK_MS.compare_exchange(peak, iteration_ms.to_bits(), Ordering::Relaxed, Ordering::Relaxed) {
                    Ok(_) => break,
                    Err(current) => peak = current,
                }
            }
        }
    }
}

fn start_random_reconnect_loop(client_manager: Arc<ClientManager>) {
    let total_clients = client_manager.client_count();
    if total_clients == 0 {
        return;
    }
    let spawned = std::thread::Builder::new().name("RandomReconnect".into()).spawn(move || {
        use rand::RngExt;
        loop {
            let wait_minutes = rand::rng().random_range(1..21u64); // 1–20 minutes
            std::thread::sleep(Duration::from_secs(wait_minutes * 60));
            if !Program::is_running() {
                return;
            }
            let index_to_restart = rand::rng().random_range(0..total_clients);
            BNL::log(format!("Randomly restarting client at index {index_to_restart}"));
            client_manager.reconnect_client(index_to_restart);
        }
    });
    if let Err(e) = spawned {
        BNL::log_warning(format!("The reconnect thread could not start: {e}"));
    }
}
