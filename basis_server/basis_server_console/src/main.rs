//! Port of `BasisServerConsole/Program.cs`: the server executable.
//!
//! Boot order is load-bearing and mirrors the C# exactly: predecessor wait → config load →
//! tuning profile → environment overrides → logging → first-boot wizard/tuning → health check →
//! REST API → network server → resource loaders → console → wait for shutdown.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]
#![deny(unused_must_use)]

mod basis_console_commands;
mod basis_console_driver;
mod basis_first_boot_tuning;
mod basis_setup_wizard;

use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};

use basis_console_commands::BasisConsoleCommands;
use basis_console_driver::BasisConsoleDriver;
use basis_first_boot_tuning::BasisFirstBootTuning;
use basis_network_core::BNL;
use basis_network_core::configuration::{BasisTuningProfile, Configuration};
use basis_network_core::transport::iroh_network_impl::IrohRuntime;
use basis_network_server::NetworkServer;
use basis_network_server::diagnostics::{BasisNetworkHealthCheck, BasisServerSideLogging, BasisStatistics};
use basis_network_server::networking::initial_data::{BasisDefaultLibraryLoader, BasisLoadableLoader};
use basis_network_server::reduction::basis_server_reduction_system_events::BasisServerReductionSystemEvents;
use basis_network_server::rest_api::BasisRestApiHandler;
use basis_network_server::BasisServerControl;
use basis_setup_wizard::BasisSetupWizard;
use parking_lot::{Condvar, Mutex};

/// The C# `Program` statics: `isRunning` and the shutdown event the main thread waits on.
pub struct Program;

static IS_RUNNING: AtomicBool = AtomicBool::new(true);
static SHUTDOWN: (Mutex<bool>, Condvar) = (Mutex::new(false), Condvar::new());

impl Program {
    pub fn is_running() -> bool {
        IS_RUNNING.load(Ordering::Acquire)
    }

    /// `Program.isRunning = false; shutdownEvent.Set()` — wakes the main thread, which runs the
    /// orderly shutdown. Idempotent: every path that asks the process to stop lands here.
    pub fn request_shutdown() {
        IS_RUNNING.store(false, Ordering::Release);
        *SHUTDOWN.0.lock() = true;
        SHUTDOWN.1.notify_all();
    }

    fn wait_for_shutdown() {
        let mut flag = SHUTDOWN.0.lock();
        while !*flag {
            SHUTDOWN.1.wait(&mut flag);
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    BasisConsoleCommands::wait_for_predecessor_exit(&args);

    let base_dir = Configuration::base_directory();
    let config_dir = base_dir.join(Configuration::CONFIG_FOLDER_NAME);
    if let Err(e) = std::fs::create_dir_all(&config_dir) {
        eprintln!("Could not create the config directory {}: {e}", config_dir.display());
        return ExitCode::from(1);
    }
    let config_file_path = config_dir.join("config.xml");
    // Capture this before load_from_xml, which creates config.xml when it's missing.
    let is_first_boot = !config_file_path.exists();
    let mut config = match Configuration::load_from_xml(&config_file_path) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Could not load {}: {e}", config_file_path.display());
            return ExitCode::from(1);
        }
    };

    // Settings the benchmark fitted to this machine, if it left any. Applied once and folded into
    // config.xml, so it never shadows a later hand edit.
    //
    // Before the environment overrides, not after, and the order is load-bearing in both
    // directions. Applying this persists the config, and an override is a per-run pin — so running
    // it second would write whatever was in the environment permanently into config.xml. Going
    // first also leaves the overrides applied last, which is what makes them still win for this run.
    BasisTuningProfile::apply_if_present(&config_dir, &mut config);
    config.process_environmental_overrides();

    let log_dir = base_dir.join(Configuration::LOGS_FOLDER_NAME);
    if let Err(e) = BasisServerSideLogging::initialize(&config, &log_dir) {
        // A server that cannot write its log file is still a server; the screen output stays.
        eprintln!("Log files are unavailable ({}); logging to the console only.", e.report());
    }

    // Brand-new server: walk the operator through core settings and force them to designate an
    // admin before anything boots.
    if is_first_boot {
        BasisSetupWizard::run(&mut config, &config_file_path);

        // Offer to fit the settings to this machine before it ever serves anyone. Runs the
        // benchmark as a separate process, so nothing from it is ever loaded here.
        if BasisFirstBootTuning::run(&base_dir, &config_dir) {
            // Re-read from disc rather than applying onto the object in hand: that object has
            // already had this run's environment overrides folded into it, and applying a profile
            // persists the config. Loading fresh also picks up the transport sidecars the
            // benchmark's own server runs rewrote underneath us.
            match Configuration::load_from_xml(&config_file_path) {
                Ok(fresh) => {
                    config = fresh;
                    BasisTuningProfile::apply_if_present(&config_dir, &mut config);
                    config.process_environmental_overrides();
                }
                Err(e) => BNL::log_warning(format!("[Tuning] Could not re-read {} after tuning ({e}); keeping the settings already in hand.", config_file_path.display())),
            }
        }
    }

    BNL::log("Server Booting");
    let mut check = match BasisNetworkHealthCheck::new(&config) {
        Ok(check) => Some(check),
        Err(e) => {
            BNL::log_error(format!("The health check endpoint could not start: {}", e.report()));
            None
        }
    };
    let mut api = if config.api_enabled && !config.api_key.is_empty() {
        match BasisRestApiHandler::new(&config, Some(BasisServerControl::shared())) {
            Ok(api) => Some(api),
            Err(e) => {
                BNL::log_error(format!("The REST API could not start: {}", e.report()));
                None
            }
        }
    } else {
        None
    };

    if let Err(e) = NetworkServer::start_server(config.clone()) {
        BNL::log_error(format!("The server could not start: {}", e.report()));
        if let Some(api) = api.as_mut() {
            api.stop();
        }
        if let Some(check) = check.as_mut() {
            check.stop();
        }
        BasisServerSideLogging::shutdown();
        return ExitCode::from(1);
    }

    migrate_legacy_resource_directories(&base_dir);
    BasisLoadableLoader::load_xml(Configuration::INITIAL_RESOURCES_FOLDER_NAME);
    BasisDefaultLibraryLoader::load_xml(Configuration::DEFAULT_LIBRARY_FOLDER_NAME);

    install_signal_handlers();

    if config.enable_console {
        BasisConsoleCommands::register_command("/players", "Lists all connected players.", BasisConsoleCommands::handle_show_players);
        BasisConsoleCommands::register_command("/status", "Shows the current server status.", BasisConsoleCommands::handle_status);
        BasisConsoleCommands::register_command("/shutdown", "Shuts down the server.", BasisConsoleCommands::handle_shutdown);
        BasisConsoleCommands::register_command("/restart", "Restarts the server, applying settings that need a restart.", BasisConsoleCommands::handle_restart);
        BasisConsoleCommands::register_command("/help", "Displays all available commands.", BasisConsoleCommands::handle_help);
        BasisConsoleCommands::register_command("/clear", "Clears the console", BasisConsoleCommands::handle_clear);
        BasisConsoleCommands::register_permission_commands();
        BasisConsoleCommands::register_configuration_commands();
        BasisConsoleCommands::start_console_listener();
    }

    // Wait for shutdown signal
    Program::wait_for_shutdown();

    BNL::log("Shutting down server...");
    BasisConsoleDriver::restore();
    if let Some(api) = api.as_mut() {
        api.stop();
    }
    if let Some(check) = check.as_mut() {
        check.stop();
    }
    BasisServerReductionSystemEvents::shutdown();
    if config.enable_statistics {
        BasisStatistics::stop_worker_thread();
    }
    NetworkServer::stop_server();
    BNL::log("Server shut down successfully.");
    BasisServerSideLogging::shutdown();
    ExitCode::SUCCESS
}

/// Handle legacy resource directory name migrations and similar. After a version bump or two this
/// should be removed.
fn migrate_legacy_resource_directories(base_dir: &Path) {
    let legacy_paths = [
        "initalresources",   // dooly spelling
        "initialressources", // if you're french
        "intialresources",   // another common typo
    ];
    let correct_path = base_dir.join(Configuration::INITIAL_RESOURCES_FOLDER_NAME);
    for legacy_name in legacy_paths {
        let legacy_full_path = base_dir.join(legacy_name);
        if legacy_full_path.is_dir() && !correct_path.is_dir() {
            BNL::log(format!("Found legacy '{legacy_name}' directory, migrating to '{}'...", Configuration::INITIAL_RESOURCES_FOLDER_NAME));
            match std::fs::rename(&legacy_full_path, &correct_path) {
                Ok(()) => {
                    BNL::log("Directory migration completed successfully");
                    break; // Exit after first successful migration
                }
                Err(e) => BNL::log_error(format!("Failed to migrate legacy directory '{legacy_name}': {e}")),
            }
        }
    }
}

/// Ctrl-C and SIGTERM both end in the orderly shutdown the main thread runs, the way the C#
/// `ProcessExit` handler did. Registered on the transport runtime so no extra thread is needed.
fn install_signal_handlers() {
    let handler = async {
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
        Program::request_shutdown();
    };
    if let Err(e) = IrohRuntime::spawn(handler) {
        BNL::log_warning(format!("Signal handling is unavailable ({}); use /shutdown to stop the server.", e.report()));
    }
}
