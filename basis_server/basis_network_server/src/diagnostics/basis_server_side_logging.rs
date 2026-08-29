//! Port of `Diagnostics/BasisServerSideLogging.cs`: BNL sink that writes the console and a
//! daily log file through a bounded queue drained by one writer thread.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use basis_error::{BasisResult, ResultExt};
use basis_network_core::BNL;
use basis_network_core::configuration::Configuration;
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use parking_lot::{Mutex, RwLock};

use crate::util::{local_hour_minute, utc_today};

struct Writer {
    sender: Sender<String>,
    handle: Option<std::thread::JoinHandle<()>>,
    stop: Arc<AtomicBool>,
}

static LOG_DIRECTORY: Mutex<Option<PathBuf>> = Mutex::new(None);
static USE_LOGGING: AtomicBool = AtomicBool::new(false);
static WRITE_TO_SCREEN: AtomicBool = AtomicBool::new(true);
static WRITER: LazyLock<Mutex<Option<Writer>>> = LazyLock::new(|| Mutex::new(None));
static SCREEN_LOCK: Mutex<()> = Mutex::new(());
/// An interactive console can take over screen output (to keep its input line intact); the
/// sink receives one fully rendered line (ANSI colours included, no trailing newline).
pub type ConsoleSink = Arc<dyn Fn(&str) + Send + Sync>;
static CONSOLE_SINK: RwLock<Option<ConsoleSink>> = RwLock::new(None);

#[derive(Clone, Copy)]
enum Level {
    Info,
    Warning,
    Error,
}

impl Level {
    fn name(self) -> &'static str {
        match self {
            Level::Info => "INFO",
            Level::Warning => "WARNING",
            Level::Error => "ERROR",
        }
    }

    fn console_label(self) -> &'static str {
        match self {
            Level::Info => "[INFO] ",
            Level::Warning => "[WARNING] ",
            Level::Error => "[ERROR] ",
        }
    }

    /// ANSI colour matching the C# console colours (DarkMagenta / DarkYellow / DarkRed).
    fn ansi(self) -> &'static str {
        match self {
            Level::Info => "\x1b[35m",
            Level::Warning => "\x1b[33m",
            Level::Error => "\x1b[31m",
        }
    }
}

pub struct BasisServerSideLogging;

impl BasisServerSideLogging {
    /// Bound on queued lines; a full queue drops the oldest line, as the C# did.
    pub const QUEUE_CAPACITY: usize = 200;

    pub fn use_logging() -> bool {
        USE_LOGGING.load(Ordering::Acquire)
    }

    pub fn set_use_logging(value: bool) {
        USE_LOGGING.store(value, Ordering::Release);
    }

    pub fn write_to_screen() -> bool {
        WRITE_TO_SCREEN.load(Ordering::Acquire)
    }

    pub fn set_write_to_screen(value: bool) {
        WRITE_TO_SCREEN.store(value, Ordering::Release);
    }

    pub fn log_directory() -> Option<PathBuf> {
        LOG_DIRECTORY.lock().clone()
    }

    /// `<logDirectory>/<yyyy-MM-dd>.log`.
    pub fn current_log_file_name() -> Option<PathBuf> {
        Self::log_directory().map(|dir| dir.join(format!("{}.log", utc_today())))
    }

    /// Hooks BNL and, when file support is on, starts the writer thread. A log directory that
    /// cannot be created is returned; the console sink is installed either way.
    pub fn initialize(config: &Configuration, log_directory: impl Into<PathBuf>) -> BasisResult<()> {
        let log_directory = log_directory.into();
        Self::set_use_logging(config.has_file_support);
        *LOG_DIRECTORY.lock() = Some(log_directory.clone());
        BNL::set_log_output(Some(Arc::new(|m: &str| Self::log(m))));
        BNL::set_log_warning_output(Some(Arc::new(|m: &str| Self::log_warning(m))));
        BNL::set_log_error_output(Some(Arc::new(|m: &str| Self::log_error(m))));

        if Self::use_logging() {
            std::fs::create_dir_all(&log_directory).with_context(|| format!("creating the log folder '{}'", log_directory.display()))?;
            if let Some(file) = Self::current_log_file_name() {
                Self::log(&format!("Logs are saved to {}", file.display()));
            }
            Self::start_logging_task()?;
        } else {
            Self::log("no logs will be saved");
        }
        Ok(())
    }

    fn start_logging_task() -> BasisResult<()> {
        let mut writer = WRITER.lock();
        if writer.is_some() {
            return Ok(());
        }
        let (sender, receiver) = bounded::<String>(Self::QUEUE_CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("BasisServerLogWriter".to_string())
            .spawn(move || Self::drain(receiver, thread_stop))
            .context("starting the log writer thread")?;
        *writer = Some(Writer { sender, handle: Some(handle), stop });
        Ok(())
    }

    /// Owned by the writer thread. Whatever queued up while the previous write was in flight
    /// goes out in a single open/write/close.
    fn drain(receiver: Receiver<String>, stop: Arc<AtomicBool>) {
        loop {
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(first) => {
                    let mut batch = first;
                    batch.push('\n');
                    while let Ok(queued) = receiver.try_recv() {
                        batch.push_str(&queued);
                        batch.push('\n');
                    }
                    Self::write_to_file(&batch);
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    if stop.load(Ordering::Acquire) && receiver.is_empty() {
                        return;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn write_to_file(text: &str) {
        use std::io::Write;
        let Some(file) = Self::current_log_file_name() else {
            return;
        };
        let written = std::fs::OpenOptions::new().create(true).append(true).open(&file).and_then(|mut f| f.write_all(text.as_bytes()));
        if let Err(e) = written {
            // The console is the only place left to say so; do not recurse into BNL.
            eprintln!("[ERROR] could not append to '{}': {e}", file.display());
        }
    }

    /// Stops the writer after flushing queued lines and unhooks BNL.
    pub fn shutdown() {
        let taken = WRITER.lock().take();
        if let Some(mut writer) = taken {
            writer.stop.store(true, Ordering::Release);
            drop(writer.sender);
            if let Some(handle) = writer.handle.take() {
                let _ = handle.join();
            }
        }
        BNL::set_log_output(None);
        BNL::set_log_warning_output(None);
        BNL::set_log_error_output(None);
    }

    /// Newlines and control characters would break the one-record-per-line shape of the log
    /// file. Almost every message is already clean, so scan before copying.
    pub fn sanitize(message: &str) -> String {
        if !message.chars().any(|c| (c as u32) < 0x20 && c != '\t') {
            return message.to_string();
        }
        message
            .chars()
            .map(|c| match c {
                '\n' | '\r' => ' ',
                c if (c as u32) < 0x20 && c != '\t' => '?',
                c => c,
            })
            .collect()
    }

    fn stamp() -> String {
        let (h, m) = local_hour_minute();
        format!("{h:02}:{m:02}")
    }

    pub fn log(message: &str) {
        Self::emit(Level::Info, message);
    }

    pub fn log_warning(message: &str) {
        Self::emit(Level::Warning, message);
    }

    pub fn log_error(message: &str) {
        Self::emit(Level::Error, message);
    }

    fn emit(level: Level, message: &str) {
        let to_screen = Self::write_to_screen();
        let to_file = Self::use_logging();
        if !to_screen && !to_file {
            return;
        }
        let message = Self::sanitize(message);
        let stamp = Self::stamp();
        if to_screen {
            Self::write_screen_line(&stamp, level, &message);
        }
        if to_file {
            let formatted = format!("[{stamp}] [{}] {message}", level.name());
            Self::enqueue(formatted);
        }
    }

    fn enqueue(line: String) {
        let writer = WRITER.lock();
        let Some(writer) = writer.as_ref() else {
            return;
        };
        match writer.sender.try_send(line) {
            Ok(()) => {}
            Err(TrySendError::Full(line)) => {
                // Drop the oldest line if the queue is full, then retry once.
                let _ = writer.sender.try_send(line);
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }

    /// Routes screen output through `sink` instead of stdout (the interactive console driver).
    pub fn set_console_sink(sink: Option<ConsoleSink>) {
        *CONSOLE_SINK.write() = sink;
    }

    /// One rendered console line: coloured stamp, level label and message.
    pub fn render_console_line(stamp: &str, level_label: &str, level_ansi: &str, message: &str) -> String {
        format!("\x1b[36m[{stamp}] {level_ansi}{level_label}\x1b[37m{message}\x1b[0m")
    }

    /// Writes one whole log line. The parts land together under the screen lock so two threads
    /// cannot interleave colours and text.
    fn write_screen_line(stamp: &str, level: Level, message: &str) {
        use std::io::Write;
        let line = Self::render_console_line(stamp, level.console_label(), level.ansi(), message);
        let sink = CONSOLE_SINK.read().clone();
        if let Some(sink) = sink {
            sink(&line);
            return;
        }
        let _guard = SCREEN_LOCK.lock();
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = writeln!(out, "{line}");
    }
}
