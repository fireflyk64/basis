//! The error type every fallible operation in the Basis Rust port propagates.
//!
//! The C# server leans on exceptions: a bad packet, a missing file or a refused socket unwinds
//! to the nearest `catch` and is logged there. A production Rust server cannot panic, so every
//! one of those paths is a `Result` here — and a bare `Result<T, String>` would throw away the
//! two things an operator needs at 3 am: *where* the fault was raised and *whether retrying will
//! help*. [`BasisError`] keeps both:
//!
//! * a [`FaultKind`] — [`Transient`](FaultKind::Transient) faults (a timeout, a refused dial, a
//!   busy resource) are worth retrying; [`Permanent`](FaultKind::Permanent) ones (malformed input,
//!   a bad config value, a missing file, a violated invariant) fail the same way until something
//!   outside the process changes;
//! * an [`ErrorCode`] — the category, for metrics and for callers that branch on it;
//! * a message plus the wrapped source error, reachable through [`std::error::Error::source`];
//! * a trace of every boundary the error crossed: the `?` that converted it plus every
//!   [`context`](ResultExt::context) call, each with its `file:line:column`;
//! * a [`std::backtrace::Backtrace`], captured at creation when `RUST_BACKTRACE` or
//!   `RUST_LIB_BACKTRACE` is set (and free otherwise).
//!
//! `{}` prints the message and its cause chain on one line for a log entry; `{:?}` prints the
//! full report with the trace, which is what goes in the log when a request fails.
//!
//! ```
//! use basis_error::{BasisError, BasisResult, ErrorCode, FaultKind, ResultExt};
//!
//! fn parse_port(text: &str) -> BasisResult<u16> {
//!     let port: u16 = text.trim().parse().context("parsing the port number")?;
//!     if port == 0 {
//!         return Err(BasisError::permanent(ErrorCode::Config, "port 0 is not listenable"));
//!     }
//!     Ok(port)
//! }
//!
//! let err = parse_port("nope").unwrap_err();
//! assert_eq!(err.kind(), FaultKind::Permanent);
//! assert!(err.report().contains("parsing the port number"));
//! ```

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::backtrace::{Backtrace, BacktraceStatus};
use std::borrow::Cow;
use std::error::Error as StdError;
use std::fmt;
use std::panic::Location;
use std::time::Duration;

pub mod retry;

/// Result alias used throughout the Basis crates.
pub type BasisResult<T> = Result<T, BasisError>;

/// Whether retrying the failed operation can succeed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaultKind {
    /// Retrying the same operation later can succeed: a timeout, a refused or reset connection,
    /// a busy resource, an interrupted syscall, a name server that did not answer.
    Transient,
    /// Retrying will fail the same way until something outside the process changes: malformed
    /// input, a bad configuration value, a missing file, a permission problem, a violated
    /// invariant.
    Permanent,
}

impl FaultKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FaultKind::Transient => "transient",
            FaultKind::Permanent => "permanent",
        }
    }
}

impl fmt::Display for FaultKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The category of a fault. Coarse on purpose: this is what a metric is labelled with and what
/// a caller matches on, not a substitute for the message.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// File or socket I/O.
    Io,
    /// Configuration files, environment overrides, tuning profiles.
    Config,
    /// A peer sent something the protocol does not allow.
    Protocol,
    /// A message could not be encoded or decoded.
    Serialization,
    /// A compressed payload could not be packed or unpacked.
    Compression,
    /// Signing, verification, key exchange, AEAD.
    Crypto,
    /// Authentication or authorization failed.
    Auth,
    /// DIDs, handles, player identity records.
    Identity,
    /// DNS resolution.
    Dns,
    /// The transport (iroh / QUIC) refused, dropped or lost something.
    Transport,
    /// An operation did not complete in time.
    Timeout,
    /// The operation was cancelled or the other side of a channel went away.
    Cancelled,
    /// A resource (file, object, peer) could not be found or loaded.
    NotFound,
    /// A limit or quota was exceeded.
    Limit,
    /// The caller passed an argument the operation cannot act on.
    InvalidArgument,
    /// The operation conflicts with existing state (already exists, already running).
    Conflict,
    /// Not supported on this platform or build.
    Unsupported,
    /// A bug: an invariant the code relies on did not hold.
    Internal,
}

impl ErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::Io => "io",
            ErrorCode::Config => "config",
            ErrorCode::Protocol => "protocol",
            ErrorCode::Serialization => "serialization",
            ErrorCode::Compression => "compression",
            ErrorCode::Crypto => "crypto",
            ErrorCode::Auth => "auth",
            ErrorCode::Identity => "identity",
            ErrorCode::Dns => "dns",
            ErrorCode::Transport => "transport",
            ErrorCode::Timeout => "timeout",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::NotFound => "not-found",
            ErrorCode::Limit => "limit",
            ErrorCode::InvalidArgument => "invalid-argument",
            ErrorCode::Conflict => "conflict",
            ErrorCode::Unsupported => "unsupported",
            ErrorCode::Internal => "internal",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One boundary an error crossed: where, and what the code there was doing.
#[derive(Clone, Debug)]
pub struct Frame {
    note: Cow<'static, str>,
    location: &'static Location<'static>,
}

impl Frame {
    /// What the code at this frame was doing ("loading the ban list"), or `raised` for the
    /// frame where the error was created.
    pub fn note(&self) -> &str {
        &self.note
    }

    pub fn location(&self) -> &'static Location<'static> {
        self.location
    }

    pub fn file(&self) -> &'static str {
        self.location.file()
    }

    pub fn line(&self) -> u32 {
        self.location.line()
    }

    pub fn column(&self) -> u32 {
        self.location.column()
    }
}

impl fmt::Display for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at {}:{}:{}", self.note, self.location.file(), self.location.line(), self.location.column())
    }
}

struct Inner {
    kind: FaultKind,
    code: ErrorCode,
    message: Cow<'static, str>,
    source: Option<Box<dyn StdError + Send + Sync + 'static>>,
    /// Innermost first: `frames[0]` is where the error was raised.
    frames: Vec<Frame>,
    backtrace: Backtrace,
}

/// A fault with a kind, a code, a message, its cause and the trail of code that propagated it.
///
/// One pointer wide, so a `Result<T, BasisError>` costs nothing extra on the `Ok` path.
pub struct BasisError {
    inner: Box<Inner>,
}

impl BasisError {
    const RAISED: &'static str = "raised";

    /// Creates an error at the caller's location.
    #[track_caller]
    pub fn new(kind: FaultKind, code: ErrorCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self::build(kind, code, message.into(), None, Location::caller())
    }

    /// A fault that is worth retrying.
    #[track_caller]
    pub fn transient(code: ErrorCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self::build(FaultKind::Transient, code, message.into(), None, Location::caller())
    }

    /// A fault that will not go away by itself.
    #[track_caller]
    pub fn permanent(code: ErrorCode, message: impl Into<Cow<'static, str>>) -> Self {
        Self::build(FaultKind::Permanent, code, message.into(), None, Location::caller())
    }

    /// A fault caused by another error, keeping that error reachable through
    /// [`source`](StdError::source).
    #[track_caller]
    pub fn with_source(
        kind: FaultKind,
        code: ErrorCode,
        message: impl Into<Cow<'static, str>>,
        source: impl StdError + Send + Sync + 'static,
    ) -> Self {
        Self::build(kind, code, message.into(), Some(Box::new(source)), Location::caller())
    }

    /// Wraps another error, using its own message.
    #[track_caller]
    pub fn wrap(kind: FaultKind, code: ErrorCode, source: impl StdError + Send + Sync + 'static) -> Self {
        let message = source.to_string();
        Self::build(kind, code, message.into(), Some(Box::new(source)), Location::caller())
    }

    /// Wraps a boxed error, using its own message.
    #[track_caller]
    pub fn wrap_boxed(kind: FaultKind, code: ErrorCode, source: Box<dyn StdError + Send + Sync + 'static>) -> Self {
        let message = source.to_string();
        Self::build(kind, code, message.into(), Some(source), Location::caller())
    }

    /// Creates an error whose origin frame is `location` rather than the caller — for errors
    /// that already know where they were detected (a reader that records the short read).
    pub fn at(
        kind: FaultKind,
        code: ErrorCode,
        message: impl Into<Cow<'static, str>>,
        location: &'static Location<'static>,
    ) -> Self {
        Self::build(kind, code, message.into(), None, location)
    }

    /// [`at`](Self::at) with a source error.
    pub fn at_with_source(
        kind: FaultKind,
        code: ErrorCode,
        message: impl Into<Cow<'static, str>>,
        source: impl StdError + Send + Sync + 'static,
        location: &'static Location<'static>,
    ) -> Self {
        Self::build(kind, code, message.into(), Some(Box::new(source)), location)
    }

    /// [`context`](Self::context) with an explicit location.
    pub fn context_at(mut self, note: impl Into<Cow<'static, str>>, location: &'static Location<'static>) -> Self {
        self.push_frame(note.into(), location);
        self
    }

    fn build(
        kind: FaultKind,
        code: ErrorCode,
        message: Cow<'static, str>,
        source: Option<Box<dyn StdError + Send + Sync + 'static>>,
        location: &'static Location<'static>,
    ) -> Self {
        Self {
            inner: Box::new(Inner {
                kind,
                code,
                message,
                source,
                frames: vec![Frame { note: Cow::Borrowed(Self::RAISED), location }],
                backtrace: Backtrace::capture(),
            }),
        }
    }

    /// Records what the caller was doing when the error passed through it.
    ///
    /// A note at the same location as the previous frame (the usual `.context()` right after a
    /// `?` conversion) replaces the `raised` marker rather than adding a duplicate line.
    #[track_caller]
    pub fn context(mut self, note: impl Into<Cow<'static, str>>) -> Self {
        self.push_frame(note.into(), Location::caller());
        self
    }

    fn push_frame(&mut self, note: Cow<'static, str>, location: &'static Location<'static>) {
        if let Some(last) = self.inner.frames.last_mut()
            && last.location == location
        {
            if last.note == Self::RAISED {
                last.note = note;
            } else {
                last.note = Cow::Owned(format!("{}; {}", last.note, note));
            }
            return;
        }
        self.inner.frames.push(Frame { note, location });
    }

    /// Reclassifies the fault; a caller that knows an I/O error came from a socket that will be
    /// reopened can mark it transient, one that knows a retry is pointless can mark it permanent.
    pub fn with_kind(mut self, kind: FaultKind) -> Self {
        self.inner.kind = kind;
        self
    }

    pub fn with_code(mut self, code: ErrorCode) -> Self {
        self.inner.code = code;
        self
    }

    pub fn kind(&self) -> FaultKind {
        self.inner.kind
    }

    pub fn code(&self) -> ErrorCode {
        self.inner.code
    }

    pub fn is_transient(&self) -> bool {
        self.inner.kind == FaultKind::Transient
    }

    pub fn is_permanent(&self) -> bool {
        self.inner.kind == FaultKind::Permanent
    }

    /// The message this error was raised with, without its cause chain.
    pub fn message(&self) -> &str {
        &self.inner.message
    }

    /// Every boundary the error crossed, innermost first.
    pub fn frames(&self) -> &[Frame] {
        &self.inner.frames
    }

    /// Where the error was raised.
    pub fn origin(&self) -> &Frame {
        // `frames` is created with one element and only ever grows.
        self.inner.frames.first().unwrap_or_else(|| unreachable_frame())
    }

    pub fn backtrace(&self) -> &Backtrace {
        &self.inner.backtrace
    }

    /// The innermost error in the cause chain (this error if it has no source).
    pub fn root_cause(&self) -> &(dyn StdError + 'static) {
        let mut current: &(dyn StdError + 'static) = self;
        while let Some(next) = current.source() {
            current = next;
        }
        current
    }

    /// Finds an error of a concrete type anywhere in the cause chain.
    pub fn find_source<E: StdError + 'static>(&self) -> Option<&E> {
        let mut current: Option<&(dyn StdError + 'static)> = self.source();
        while let Some(err) = current {
            if let Some(found) = err.downcast_ref::<E>() {
                return Some(found);
            }
            current = err.source();
        }
        None
    }

    /// The full multi-line report: kind, code, message, cause chain, trace and — when captured —
    /// the backtrace. This is what belongs in the log when an operation fails.
    pub fn report(&self) -> String {
        let mut out = format!("[{} {}] {}", self.inner.kind, self.inner.code, self.inner.message);
        let mut source = self.source();
        while let Some(err) = source {
            out.push_str("\n  caused by: ");
            out.push_str(&err.to_string());
            source = err.source();
        }
        out.push_str("\n  trace:");
        for frame in &self.inner.frames {
            out.push_str("\n    ");
            out.push_str(&frame.to_string());
        }
        if self.inner.backtrace.status() == BacktraceStatus::Captured {
            out.push_str("\n  backtrace:\n");
            out.push_str(&self.inner.backtrace.to_string());
        }
        out
    }
}

fn unreachable_frame() -> &'static Frame {
    static FRAME: Frame = Frame { note: Cow::Borrowed("raised"), location: Location::caller() };
    &FRAME
}

impl fmt::Display for BasisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.inner.message)?;
        let mut source = self.source();
        while let Some(err) = source {
            write!(f, ": {err}")?;
            source = err.source();
        }
        Ok(())
    }
}

impl fmt::Debug for BasisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.report())
    }
}

impl StdError for BasisError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.inner.source.as_deref().map(|s| s as &(dyn StdError + 'static))
    }
}

/// Adds fault classification and context to any `Result` whose error converts into
/// [`BasisError`].
pub trait ResultExt<T> {
    /// Records what the caller was doing; the location of this call joins the trace.
    #[track_caller]
    fn context(self, note: impl Into<Cow<'static, str>>) -> BasisResult<T>;

    /// Like [`context`](Self::context) but the note is only built on the error path.
    #[track_caller]
    fn with_context<S: Into<Cow<'static, str>>>(self, note: impl FnOnce() -> S) -> BasisResult<T>;

    /// Converts and marks the fault transient.
    #[track_caller]
    fn transient(self) -> BasisResult<T>;

    /// Converts and marks the fault permanent.
    #[track_caller]
    fn permanent(self) -> BasisResult<T>;

    /// Converts and sets the category.
    #[track_caller]
    fn code(self, code: ErrorCode) -> BasisResult<T>;
}

impl<T, E: Into<BasisError>> ResultExt<T> for Result<T, E> {
    #[track_caller]
    fn context(self, note: impl Into<Cow<'static, str>>) -> BasisResult<T> {
        match self {
            Ok(value) => Ok(value),
            Err(err) => {
                let mut err: BasisError = err.into();
                err.push_frame(note.into(), Location::caller());
                Err(err)
            }
        }
    }

    #[track_caller]
    fn with_context<S: Into<Cow<'static, str>>>(self, note: impl FnOnce() -> S) -> BasisResult<T> {
        match self {
            Ok(value) => Ok(value),
            Err(err) => {
                let mut err: BasisError = err.into();
                err.push_frame(note().into(), Location::caller());
                Err(err)
            }
        }
    }

    #[track_caller]
    fn transient(self) -> BasisResult<T> {
        self.map_err(|err| err.into().with_kind(FaultKind::Transient))
    }

    #[track_caller]
    fn permanent(self) -> BasisResult<T> {
        self.map_err(|err| err.into().with_kind(FaultKind::Permanent))
    }

    #[track_caller]
    fn code(self, code: ErrorCode) -> BasisResult<T> {
        self.map_err(|err| err.into().with_code(code))
    }
}

/// Turns an absent value into a classified error.
pub trait OptionExt<T> {
    /// `None` becomes a permanent fault with the given code.
    #[track_caller]
    fn ok_or_permanent(self, code: ErrorCode, message: impl Into<Cow<'static, str>>) -> BasisResult<T>;

    /// `None` becomes a transient fault with the given code.
    #[track_caller]
    fn ok_or_transient(self, code: ErrorCode, message: impl Into<Cow<'static, str>>) -> BasisResult<T>;

    /// `None` becomes a permanent [`NotFound`](ErrorCode::NotFound) fault: "`what` is missing".
    #[track_caller]
    fn required(self, what: &str) -> BasisResult<T>;
}

impl<T> OptionExt<T> for Option<T> {
    #[track_caller]
    fn ok_or_permanent(self, code: ErrorCode, message: impl Into<Cow<'static, str>>) -> BasisResult<T> {
        match self {
            Some(value) => Ok(value),
            None => Err(BasisError::build(FaultKind::Permanent, code, message.into(), None, Location::caller())),
        }
    }

    #[track_caller]
    fn ok_or_transient(self, code: ErrorCode, message: impl Into<Cow<'static, str>>) -> BasisResult<T> {
        match self {
            Some(value) => Ok(value),
            None => Err(BasisError::build(FaultKind::Transient, code, message.into(), None, Location::caller())),
        }
    }

    #[track_caller]
    fn required(self, what: &str) -> BasisResult<T> {
        match self {
            Some(value) => Ok(value),
            None => Err(BasisError::build(
                FaultKind::Permanent,
                ErrorCode::NotFound,
                Cow::Owned(format!("{what} is missing")),
                None,
                Location::caller(),
            )),
        }
    }
}

/// Classifies an error by the first `std::io::Error` in its cause chain (itself included);
/// `None` when the chain holds no I/O error. Lets a wrapped socket or bind error keep the
/// transient/permanent distinction its `ErrorKind` carries.
pub fn fault_kind_from_chain(err: &(dyn StdError + 'static)) -> Option<FaultKind> {
    let mut current: Option<&(dyn StdError + 'static)> = Some(err);
    while let Some(e) = current {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            return Some(io_fault_kind(io.kind()));
        }
        current = e.source();
    }
    None
}

/// Classifies an I/O error kind: the ones a retry can plausibly clear are transient.
pub fn io_fault_kind(kind: std::io::ErrorKind) -> FaultKind {
    use std::io::ErrorKind as K;
    match kind {
        K::Interrupted
        | K::WouldBlock
        | K::TimedOut
        | K::ConnectionReset
        | K::ConnectionAborted
        | K::ConnectionRefused
        | K::NotConnected
        | K::BrokenPipe
        | K::AddrInUse
        | K::HostUnreachable
        | K::NetworkUnreachable
        | K::NetworkDown
        | K::ResourceBusy
        | K::Deadlock => FaultKind::Transient,
        _ => FaultKind::Permanent,
    }
}

fn io_error_code(kind: std::io::ErrorKind) -> ErrorCode {
    use std::io::ErrorKind as K;
    match kind {
        K::TimedOut => ErrorCode::Timeout,
        K::NotFound => ErrorCode::NotFound,
        K::Unsupported => ErrorCode::Unsupported,
        K::InvalidInput => ErrorCode::InvalidArgument,
        K::InvalidData => ErrorCode::Serialization,
        _ => ErrorCode::Io,
    }
}

impl From<std::io::Error> for BasisError {
    #[track_caller]
    fn from(err: std::io::Error) -> Self {
        let kind = io_fault_kind(err.kind());
        let code = io_error_code(err.kind());
        let message = err.to_string();
        Self::build(kind, code, message.into(), Some(Box::new(err)), Location::caller())
    }
}

/// Implements `From<$ty> for BasisError` with a fixed kind and code, capturing the location of
/// the `?` that performed the conversion.
#[macro_export]
macro_rules! impl_from_error {
    ($($ty:ty => $kind:ident, $code:ident;)+) => {
        $(
            impl From<$ty> for $crate::BasisError {
                #[track_caller]
                fn from(err: $ty) -> Self {
                    $crate::BasisError::wrap($crate::FaultKind::$kind, $crate::ErrorCode::$code, err)
                }
            }
        )+
    };
}

impl_from_error! {
    std::num::ParseIntError => Permanent, InvalidArgument;
    std::num::ParseFloatError => Permanent, InvalidArgument;
    std::str::ParseBoolError => Permanent, InvalidArgument;
    std::char::ParseCharError => Permanent, InvalidArgument;
    std::num::TryFromIntError => Permanent, InvalidArgument;
    std::array::TryFromSliceError => Permanent, Serialization;
    std::str::Utf8Error => Permanent, Serialization;
    std::string::FromUtf8Error => Permanent, Serialization;
    std::string::FromUtf16Error => Permanent, Serialization;
    std::net::AddrParseError => Permanent, Config;
    std::env::VarError => Permanent, Config;
    std::time::SystemTimeError => Transient, Internal;
    std::fmt::Error => Permanent, Internal;
}

impl From<Box<dyn StdError + Send + Sync + 'static>> for BasisError {
    #[track_caller]
    fn from(err: Box<dyn StdError + Send + Sync + 'static>) -> Self {
        Self::wrap_boxed(FaultKind::Permanent, ErrorCode::Internal, err)
    }
}

impl From<std::convert::Infallible> for BasisError {
    fn from(err: std::convert::Infallible) -> Self {
        match err {}
    }
}

/// Returns early with a [`BasisError`] built from a format string:
/// `basis_bail!(Permanent, Protocol, "channel {channel} is out of range")`.
#[macro_export]
macro_rules! basis_bail {
    ($kind:ident, $code:ident, $($arg:tt)*) => {
        return ::core::result::Result::Err($crate::BasisError::new(
            $crate::FaultKind::$kind,
            $crate::ErrorCode::$code,
            ::std::format!($($arg)*),
        ))
    };
}

/// Returns early with a [`BasisError`] unless the condition holds:
/// `basis_ensure!(len <= MAX, Permanent, Limit, "{len} exceeds {MAX}")`.
#[macro_export]
macro_rules! basis_ensure {
    ($cond:expr, $kind:ident, $code:ident, $($arg:tt)*) => {
        if !($cond) {
            $crate::basis_bail!($kind, $code, $($arg)*);
        }
    };
}

/// Backoff schedule for retrying transient faults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RetryPolicy {
    /// Total attempts, including the first. `1` means no retry.
    pub max_attempts: u32,
    /// Delay after the first failure.
    pub initial_delay: Duration,
    /// Delays grow geometrically by `multiplier` up to this ceiling.
    pub max_delay: Duration,
    pub multiplier: f64,
    /// Fraction of the delay randomised away (0.2 = ±20%) so retrying peers do not stampede.
    pub jitter: f64,
}

impl RetryPolicy {
    /// No retries at all.
    pub const NONE: RetryPolicy = RetryPolicy {
        max_attempts: 1,
        initial_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
        multiplier: 1.0,
        jitter: 0.0,
    };

    /// Geometric backoff doubling from `initial_delay` to `max_delay` with ±20% jitter.
    pub const fn new(max_attempts: u32, initial_delay: Duration, max_delay: Duration) -> Self {
        Self { max_attempts: if max_attempts == 0 { 1 } else { max_attempts }, initial_delay, max_delay, multiplier: 2.0, jitter: 0.2 }
    }

    /// Delay to wait after the given 1-based failed attempt. `seed` only spreads the jitter.
    pub fn delay_for(&self, failed_attempt: u32, seed: u64) -> Duration {
        let exponent = failed_attempt.saturating_sub(1).min(62);
        let base = self.initial_delay.as_secs_f64() * self.multiplier.max(1.0).powi(exponent as i32);
        let capped = base.min(self.max_delay.as_secs_f64()).max(0.0);
        let jitter = self.jitter.clamp(0.0, 1.0);
        let unit = xorshift(seed ^ u64::from(failed_attempt).wrapping_mul(0x9E37_79B9_7F4A_7C15)) as f64
            / u64::MAX as f64; // 0..1
        let factor = 1.0 - jitter + 2.0 * jitter * unit;
        let seconds = (capped * factor).max(0.0);
        if seconds.is_finite() {
            Duration::try_from_secs_f64(seconds).unwrap_or(self.max_delay)
        } else {
            self.max_delay
        }
    }
}

fn xorshift(mut x: u64) -> u64 {
    if x == 0 {
        x = 0x2545_F491_4F6C_DD1D;
    }
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_frame_points_at_the_question_mark() -> Result<(), String> {
        fn failing() -> BasisResult<()> {
            let n: i32 = "x".parse()?; // this line is the origin
            let _ = n;
            Ok(())
        }
        let err = failing().err().ok_or("expected an error")?;
        let origin = err.origin();
        assert!(origin.file().ends_with("lib.rs"));
        assert_eq!(origin.note(), "raised");
        assert_eq!(err.code(), ErrorCode::InvalidArgument);
        assert!(err.is_permanent());
        // The `?` is 2 lines below the `fn failing` line; check it is not inside core::result.
        assert!(!origin.file().contains("core/src/result.rs"), "location was {}", origin.file());
        Ok(())
    }

    #[test]
    fn context_frames_are_recorded_in_order() -> Result<(), String> {
        fn inner() -> BasisResult<()> {
            Err(BasisError::transient(ErrorCode::Transport, "dial refused"))
        }
        fn middle() -> BasisResult<()> {
            inner().context("connecting to the relay")
        }
        fn outer() -> BasisResult<()> {
            middle().with_context(|| format!("joining server {}", 7))
        }
        let err = outer().err().ok_or("expected an error")?;
        let notes: Vec<&str> = err.frames().iter().map(Frame::note).collect();
        assert_eq!(notes, vec!["raised", "connecting to the relay", "joining server 7"]);
        assert!(err.is_transient());
        let report = err.report();
        assert!(report.starts_with("[transient transport] dial refused"));
        assert!(report.contains("joining server 7 at "));
        Ok(())
    }

    #[test]
    fn context_at_the_same_location_replaces_the_raised_marker() -> Result<(), String> {
        let err = "nope".parse::<u16>().context("parsing the port").err().ok_or("expected an error")?;
        assert_eq!(err.frames().len(), 1);
        assert_eq!(err.origin().note(), "parsing the port");
        assert!(err.find_source::<std::num::ParseIntError>().is_some());
        Ok(())
    }

    #[test]
    fn io_errors_are_classified() {
        let timeout: BasisError = std::io::Error::new(std::io::ErrorKind::TimedOut, "slow").into();
        assert!(timeout.is_transient());
        assert_eq!(timeout.code(), ErrorCode::Timeout);

        let missing: BasisError = std::io::Error::new(std::io::ErrorKind::NotFound, "gone").into();
        assert!(missing.is_permanent());
        assert_eq!(missing.code(), ErrorCode::NotFound);

        let reset: BasisError = std::io::Error::from(std::io::ErrorKind::ConnectionReset).into();
        assert!(reset.is_transient());
        assert_eq!(reset.code(), ErrorCode::Io);
    }

    #[test]
    fn display_is_the_cause_chain_on_one_line() {
        let err = BasisError::with_source(
            FaultKind::Permanent,
            ErrorCode::Config,
            "bad PeerLimit",
            std::io::Error::new(std::io::ErrorKind::InvalidData, "not a number"),
        );
        assert_eq!(err.to_string(), "bad PeerLimit: not a number");
        assert_eq!(err.root_cause().to_string(), "not a number");
    }

    #[test]
    fn option_ext_classifies_absence() {
        let missing: Option<u8> = None;
        let err = missing.required("the ban list").err();
        assert!(err.as_ref().is_some_and(|e| e.code() == ErrorCode::NotFound && e.is_permanent()));
        assert_eq!(err.map(|e| e.to_string()), Some("the ban list is missing".to_string()));

        let waiting: Option<u8> = None;
        let err = waiting.ok_or_transient(ErrorCode::Transport, "no relay yet").err();
        assert!(err.is_some_and(|e| e.is_transient()));

        assert_eq!(Some(3u8).required("x").ok(), Some(3));
    }

    #[test]
    fn bail_and_ensure_return_early() {
        fn check(n: u32) -> BasisResult<u32> {
            basis_ensure!(n < 10, Permanent, Limit, "{n} is too large");
            if n == 7 {
                basis_bail!(Transient, Internal, "seven is unlucky");
            }
            Ok(n)
        }
        assert_eq!(check(3).ok(), Some(3));
        assert!(check(12).err().is_some_and(|e| e.code() == ErrorCode::Limit && e.to_string() == "12 is too large"));
        assert!(check(7).err().is_some_and(|e| e.is_transient()));
    }

    #[test]
    fn reclassification_helpers() {
        let err: BasisResult<()> = Err(BasisError::permanent(ErrorCode::Io, "x"));
        assert!(err.transient().err().is_some_and(|e| e.is_transient()));
        let err: BasisResult<()> = Err(BasisError::transient(ErrorCode::Io, "x"));
        assert!(err.permanent().err().is_some_and(|e| e.is_permanent()));
        let err: BasisResult<()> = Err(BasisError::transient(ErrorCode::Io, "x"));
        assert!(err.code(ErrorCode::Dns).err().is_some_and(|e| e.code() == ErrorCode::Dns));
    }

    #[test]
    fn retry_policy_backs_off_and_caps() {
        let policy = RetryPolicy { jitter: 0.0, ..RetryPolicy::new(5, Duration::from_millis(100), Duration::from_millis(350)) };
        assert_eq!(policy.delay_for(1, 1), Duration::from_millis(100));
        assert_eq!(policy.delay_for(2, 1), Duration::from_millis(200));
        assert_eq!(policy.delay_for(3, 1), Duration::from_millis(350));
        assert_eq!(policy.delay_for(60, 1), Duration::from_millis(350));
        let jittered = RetryPolicy::new(5, Duration::from_millis(100), Duration::from_secs(1));
        let d = jittered.delay_for(1, 42);
        assert!(d >= Duration::from_millis(80) && d <= Duration::from_millis(120), "{d:?}");
        assert_eq!(RetryPolicy::NONE.max_attempts, 1);
        assert_eq!(RetryPolicy::new(0, Duration::ZERO, Duration::ZERO).max_attempts, 1);
    }

    #[test]
    fn error_is_one_pointer_wide() {
        assert_eq!(std::mem::size_of::<BasisError>(), std::mem::size_of::<usize>());
        assert_eq!(std::mem::size_of::<BasisResult<()>>(), std::mem::size_of::<usize>());
    }
}
