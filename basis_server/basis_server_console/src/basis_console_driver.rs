//! Port of `BasisConsoleDriver.cs`.
//!
//! Keeps the command being typed intact while the server logs from its own threads. Console
//! output is funnelled through here, so a log line erases the input line, prints above it, then
//! the input line is redrawn underneath with the caret back where it was. Without this a log
//! arriving mid-keystroke lands in the middle of the typed text and the terminal's own echo is
//! left in pieces.
//!
//! Redirected stdin/stdout (docker -d, pipes, service hosts) keeps plain line-oriented behaviour.
//! Positioning uses relative ANSI moves, because a cursor query is answered through stdin and the
//! reader thread is parked on stdin.

use std::io::{Read, Write};
use std::sync::Arc;

use basis_network_core::BNL;
use basis_network_server::diagnostics::BasisServerSideLogging;
use parking_lot::Mutex;

const PROMPT: &str = "> ";
const PROMPT_COLOR: &str = "\x1b[32m";
const RESET_COLOR: &str = "\x1b[0m";
const HISTORY_LIMIT: usize = 100;
const FALLBACK_WIDTH: usize = 80;

struct State {
    line: Vec<char>,
    history: Vec<String>,
    installed: bool,
    interactive: bool,
    input_active: bool,
    line_shown: bool,
    caret: usize,
    history_cursor: usize,
    history_draft: String,
    #[cfg(unix)]
    saved_termios: Option<libc::termios>,
}

static GATE: Mutex<State> = Mutex::new(State {
    line: Vec::new(),
    history: Vec::new(),
    installed: false,
    interactive: false,
    input_active: false,
    line_shown: false,
    caret: 0,
    history_cursor: 0,
    history_draft: String::new(),
    #[cfg(unix)]
    saved_termios: None,
});

enum Key {
    Enter,
    Backspace,
    Delete,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
    Escape,
    Char(char),
    Eof,
    /// A control byte with no meaning here.
    Ignored,
}

pub struct BasisConsoleDriver;

impl BasisConsoleDriver {
    /// False when the console is redirected, which leaves every path here a plain passthrough.
    pub fn interactive() -> bool {
        GATE.lock().interactive
    }

    /// Takes over stdout. Call once the interactive console is wanted, and after any plain
    /// prompting (the first boot wizard) has finished.
    pub fn initialize() {
        let mut state = GATE.lock();
        if state.installed {
            return;
        }
        state.interactive = Self::stdin_is_terminal() && Self::stdout_is_terminal();
        if !state.interactive {
            return;
        }
        if !Self::enter_raw_mode(&mut state) {
            state.interactive = false;
            return;
        }
        state.installed = true;
        drop(state);
        BasisServerSideLogging::set_console_sink(Some(Arc::new(|line: &str| Self::commit(line))));
    }

    /// Hands the terminal back: cooked mode again, no more output interception. Safe to call
    /// more than once and when nothing was installed.
    pub fn restore() {
        let mut state = GATE.lock();
        if !state.installed {
            return;
        }
        Self::erase(&mut state);
        state.input_active = false;
        state.installed = false;
        state.interactive = false;
        Self::leave_raw_mode(&mut state);
        drop(state);
        BasisServerSideLogging::set_console_sink(None);
    }

    /// Reads one command, redrawing the input line whenever the server logs underneath it.
    /// Returns `None` at end of input.
    pub fn read_line() -> Option<String> {
        if !Self::interactive() {
            return Self::read_plain_line();
        }

        {
            let mut state = GATE.lock();
            state.input_active = true;
            state.history_cursor = state.history.len();
            Self::draw(&mut state);
        }

        loop {
            let key = Self::read_key();
            let mut state = GATE.lock();
            if !state.interactive {
                // The terminal went away mid-line (restore() ran): fall back to plain reads.
                state.input_active = false;
                drop(state);
                return Self::read_plain_line();
            }
            match key {
                Key::Eof => {
                    Self::erase(&mut state);
                    state.input_active = false;
                    return None;
                }
                key => {
                    if let Some(entered) = Self::handle(&mut state, key) {
                        return Some(entered);
                    }
                }
            }
        }
    }

    fn read_plain_line() -> Option<String> {
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(0) | Err(_) => None,
            Ok(_) => Some(line.trim_end_matches(['\r', '\n']).to_string()),
        }
    }

    /// Clears the screen and puts the input line back.
    pub fn clear() {
        if !Self::interactive() {
            BNL::clear_console();
            return;
        }
        let mut state = GATE.lock();
        state.line_shown = false;
        Self::write_raw("\x1b[2J\x1b[H");
        Self::draw(&mut state);
    }

    fn handle(state: &mut State, key: Key) -> Option<String> {
        match key {
            Key::Enter => {
                let end = state.line.len();
                Self::move_caret(state, end);
                Self::write_raw("\r\n");
                state.line_shown = false;
                state.input_active = false;
                let entered: String = state.line.iter().collect();
                Self::remember(state, &entered);
                state.line.clear();
                state.caret = 0;
                Some(entered)
            }
            Key::Backspace => {
                if state.caret > 0 {
                    Self::erase(state);
                    state.caret -= 1;
                    let caret = state.caret;
                    state.line.remove(caret);
                    Self::draw(state);
                }
                None
            }
            Key::Delete => {
                if state.caret < state.line.len() {
                    Self::erase(state);
                    let caret = state.caret;
                    state.line.remove(caret);
                    Self::draw(state);
                }
                None
            }
            Key::Left => {
                if state.caret > 0 {
                    let target = state.caret - 1;
                    Self::move_caret(state, target);
                }
                None
            }
            Key::Right => {
                if state.caret < state.line.len() {
                    let target = state.caret + 1;
                    Self::move_caret(state, target);
                }
                None
            }
            Key::Home => {
                Self::move_caret(state, 0);
                None
            }
            Key::End => {
                let end = state.line.len();
                Self::move_caret(state, end);
                None
            }
            Key::Up => {
                Self::recall(state, -1);
                None
            }
            Key::Down => {
                Self::recall(state, 1);
                None
            }
            Key::Escape => {
                if !state.line.is_empty() {
                    Self::erase(state);
                    state.line.clear();
                    state.caret = 0;
                    Self::draw(state);
                }
                None
            }
            Key::Char(c) => {
                if c.is_control() {
                    return None;
                }
                if state.caret == state.line.len() {
                    state.line.push(c);
                    state.caret += 1;
                    Self::write_raw(&c.to_string());
                    Self::settle_wrap(PROMPT.len() + state.caret);
                } else {
                    Self::erase(state);
                    let caret = state.caret;
                    state.line.insert(caret, c);
                    state.caret += 1;
                    Self::draw(state);
                }
                None
            }
            Key::Eof | Key::Ignored => None,
        }
    }

    fn recall(state: &mut State, direction: i32) {
        let target = state.history_cursor as i64 + direction as i64;
        if target < 0 || target > state.history.len() as i64 {
            return;
        }
        let target = target as usize;
        if state.history_cursor == state.history.len() {
            state.history_draft = state.line.iter().collect();
        }
        Self::erase(state);
        state.line.clear();
        let text = if target == state.history.len() { state.history_draft.clone() } else { state.history[target].clone() };
        state.line.extend(text.chars());
        state.caret = state.line.len();
        state.history_cursor = target;
        Self::draw(state);
    }

    fn remember(state: &mut State, line: &str) {
        if !line.is_empty() && state.history.last().is_none_or(|last| last != line) {
            state.history.push(line.to_string());
            if state.history.len() > HISTORY_LIMIT {
                state.history.remove(0);
            }
        }
        state.history_cursor = state.history.len();
        state.history_draft.clear();
    }

    fn draw(state: &mut State) {
        if !state.input_active || state.line_shown {
            return;
        }
        let mut text = String::with_capacity(PROMPT.len() + state.line.len() + 16);
        text.push_str(PROMPT_COLOR);
        text.push_str(PROMPT);
        text.push_str(RESET_COLOR);
        text.extend(state.line.iter());
        Self::write_raw(&text);

        let painted = PROMPT.len() + state.line.len();
        Self::settle_wrap(painted);
        state.line_shown = true;
        Self::move_between(painted, PROMPT.len() + state.caret);
    }

    fn erase(state: &mut State) {
        if !state.line_shown {
            return;
        }
        Self::move_between(PROMPT.len() + state.caret, 0);
        Self::write_raw("\x1b[J");
        state.line_shown = false;
    }

    fn move_caret(state: &mut State, target: usize) {
        if state.line_shown {
            Self::move_between(PROMPT.len() + state.caret, PROMPT.len() + target);
        }
        state.caret = target;
    }

    /// Walks the cursor between two offsets, both counted in characters from the first cell of
    /// the prompt so that wrapping falls out of the arithmetic.
    fn move_between(from: usize, to: usize) {
        if from == to {
            return;
        }
        let width = Self::width();
        let rows = (from / width) as i64 - (to / width) as i64;
        let mut text = String::new();
        if rows > 0 {
            text.push_str(&format!("\x1b[{rows}A"));
        } else if rows < 0 {
            text.push_str(&format!("\x1b[{}B", -rows));
        }
        text.push('\r');
        let column = to % width;
        if column > 0 {
            text.push_str(&format!("\x1b[{column}C"));
        }
        Self::write_raw(&text);
    }

    /// Terminals hold the wrap until the next character is written, so text ending exactly at
    /// the margin leaves the cursor ambiguous and every later relative move a row out.
    fn settle_wrap(offset: usize) {
        if offset == 0 || !offset.is_multiple_of(Self::width()) {
            return;
        }
        Self::write_raw(" \r");
    }

    fn write_raw(text: &str) {
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
    }

    /// One finished log line from the server: drawn above the input line, which is put back
    /// underneath it.
    pub fn commit(line: &str) {
        let mut state = GATE.lock();
        Self::erase(&mut state);
        let mut text = String::with_capacity(line.len() + 2);
        text.push_str(line);
        text.push_str("\r\n");
        Self::write_raw(&text);
        Self::draw(&mut state);
    }

    fn width() -> usize {
        #[cfg(unix)]
        {
            let mut size: libc::winsize = unsafe { std::mem::zeroed() };
            // SAFETY: TIOCGWINSZ fills a winsize the kernel owns the layout of; a failure leaves
            // the zeroed struct in place and is reported through the return value.
            let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) } == 0;
            if ok && size.ws_col > 1 {
                return size.ws_col as usize;
            }
        }
        FALLBACK_WIDTH
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

    fn stdout_is_terminal() -> bool {
        #[cfg(unix)]
        {
            // SAFETY: isatty only inspects a descriptor.
            unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    #[cfg(unix)]
    fn enter_raw_mode(state: &mut State) -> bool {
        // SAFETY: termios is plain data the kernel fills; both calls report failure in the return.
        unsafe {
            let mut original: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut original) != 0 {
                return false;
            }
            let mut raw = original;
            // Keys arrive one at a time and unechoed; ISIG stays on so Ctrl-C still interrupts,
            // OPOST stays on so "\n" keeps working for everything that writes to stdout.
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            raw.c_cc[libc::VMIN] = 1;
            raw.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                return false;
            }
            state.saved_termios = Some(original);
        }
        true
    }

    #[cfg(not(unix))]
    fn enter_raw_mode(_state: &mut State) -> bool {
        false
    }

    #[cfg(unix)]
    fn leave_raw_mode(state: &mut State) {
        if let Some(original) = state.saved_termios.take() {
            // SAFETY: restoring the attributes we read earlier.
            unsafe {
                let _ = libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &original);
            }
        }
    }

    #[cfg(not(unix))]
    fn leave_raw_mode(_state: &mut State) {}

    fn read_byte() -> Option<u8> {
        let mut byte = [0u8; 1];
        match std::io::stdin().lock().read(&mut byte) {
            Ok(1) => Some(byte[0]),
            _ => None,
        }
    }

    /// True when another byte is waiting on stdin within `timeout_ms`, which is how a lone
    /// Escape is told apart from the first byte of an arrow-key sequence.
    fn byte_pending(timeout_ms: i32) -> bool {
        #[cfg(unix)]
        {
            let mut fds = libc::pollfd { fd: libc::STDIN_FILENO, events: libc::POLLIN, revents: 0 };
            // SAFETY: poll reads one pollfd we own for the duration of the call.
            unsafe { libc::poll(&mut fds, 1, timeout_ms) > 0 }
        }
        #[cfg(not(unix))]
        {
            let _ = timeout_ms;
            false
        }
    }

    fn read_key() -> Key {
        let Some(byte) = Self::read_byte() else {
            return Key::Eof;
        };
        match byte {
            b'\r' | b'\n' => Key::Enter,
            0x7f | 0x08 => Key::Backspace,
            0x04 => Key::Eof,
            0x1b => Self::read_escape_sequence(),
            b if b < 0x20 => Key::Ignored,
            b if b < 0x80 => Key::Char(b as char),
            lead => Self::read_utf8_tail(lead),
        }
    }

    fn read_escape_sequence() -> Key {
        if !Self::byte_pending(50) {
            return Key::Escape;
        }
        let Some(second) = Self::read_byte() else {
            return Key::Eof;
        };
        match second {
            b'[' => {
                let Some(third) = Self::read_byte() else {
                    return Key::Eof;
                };
                match third {
                    b'A' => Key::Up,
                    b'B' => Key::Down,
                    b'C' => Key::Right,
                    b'D' => Key::Left,
                    b'H' => Key::Home,
                    b'F' => Key::End,
                    b'1'..=b'9' => {
                        // CSI <digits> ~ : read through the terminator.
                        let mut digits = vec![third];
                        loop {
                            let Some(next) = Self::read_byte() else {
                                return Key::Eof;
                            };
                            if next == b'~' {
                                break;
                            }
                            if next.is_ascii_digit() || next == b';' {
                                digits.push(next);
                                continue;
                            }
                            return Key::Ignored;
                        }
                        match digits.as_slice() {
                            b"1" | b"7" => Key::Home,
                            b"3" => Key::Delete,
                            b"4" | b"8" => Key::End,
                            _ => Key::Ignored,
                        }
                    }
                    _ => Key::Ignored,
                }
            }
            b'O' => match Self::read_byte() {
                Some(b'H') => Key::Home,
                Some(b'F') => Key::End,
                Some(_) => Key::Ignored,
                None => Key::Eof,
            },
            _ => Key::Ignored,
        }
    }

    fn read_utf8_tail(lead: u8) -> Key {
        let extra = if lead & 0xE0 == 0xC0 {
            1
        } else if lead & 0xF0 == 0xE0 {
            2
        } else if lead & 0xF8 == 0xF0 {
            3
        } else {
            return Key::Ignored;
        };
        let mut bytes = vec![lead];
        for _ in 0..extra {
            let Some(next) = Self::read_byte() else {
                return Key::Eof;
            };
            bytes.push(next);
        }
        match std::str::from_utf8(&bytes).ok().and_then(|s| s.chars().next()) {
            Some(c) => Key::Char(c),
            None => Key::Ignored,
        }
    }
}
