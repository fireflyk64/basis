use std::io::{IsTerminal, Write};
use std::sync::Arc;

use parking_lot::RwLock;

pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

static LOG_OUTPUT: RwLock<Option<LogSink>> = RwLock::new(None);
static LOG_WARNING_OUTPUT: RwLock<Option<LogSink>> = RwLock::new(None);
static LOG_ERROR_OUTPUT: RwLock<Option<LogSink>> = RwLock::new(None);

/// Basis Network Logger with console colors.
///
/// The three `*_output` sinks are the C# delegates: when one is installed it receives the
/// message instead of the console. `BasisServerSideLogging` installs all three.
pub struct BNL;

impl BNL {
    pub fn log(message: impl AsRef<str>) {
        let message = message.as_ref();
        if let Some(sink) = LOG_OUTPUT.read().clone() {
            sink(message);
        } else {
            Self::write_with_color(message, "37"); // Info is white
        }
    }

    pub fn log_warning(message: impl AsRef<str>) {
        let message = message.as_ref();
        if let Some(sink) = LOG_WARNING_OUTPUT.read().clone() {
            sink(message);
        } else {
            Self::write_with_color(message, "33"); // Warning is yellow
        }
    }

    pub fn log_error(message: impl AsRef<str>) {
        let message = message.as_ref();
        if let Some(sink) = LOG_ERROR_OUTPUT.read().clone() {
            sink(message);
        } else {
            Self::write_with_color(message, "31"); // Error is red
        }
    }

    pub fn set_log_output(sink: Option<LogSink>) {
        *LOG_OUTPUT.write() = sink;
    }

    pub fn set_log_warning_output(sink: Option<LogSink>) {
        *LOG_WARNING_OUTPUT.write() = sink;
    }

    pub fn set_log_error_output(sink: Option<LogSink>) {
        *LOG_ERROR_OUTPUT.write() = sink;
    }

    pub fn log_output() -> Option<LogSink> {
        LOG_OUTPUT.read().clone()
    }

    pub fn log_warning_output() -> Option<LogSink> {
        LOG_WARNING_OUTPUT.read().clone()
    }

    pub fn log_error_output() -> Option<LogSink> {
        LOG_ERROR_OUTPUT.read().clone()
    }

    fn write_with_color(message: &str, ansi: &str) {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let colored = out.is_terminal();
        let _ = if colored {
            writeln!(out, "\x1b[{ansi}m{message}\x1b[0m")
        } else {
            writeln!(out, "{message}")
        };
    }

    pub fn clear_console() {
        let mut out = std::io::stdout().lock();
        let _ = write!(out, "\x1b[2J\x1b[H");
        let _ = out.flush();
    }
}
