// SPDX-License-Identifier: MIT
// Copyright (c) 2020 Ruslan Pyrch
// Port of LiteNetLib's NetDataReader as vendored into BasisNetworkCore/Io/NetDataReader.cs.

use std::fmt;
use std::panic::Location;

use basis_error::{BasisError, ErrorCode, FaultKind};
use bytes::Bytes;

/// Why a wire read or write failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetDataErrorKind {
    /// Fewer bytes remain than the value needs.
    ShortRead { what: &'static str, wanted: usize, position: usize, data_size: usize },
    /// A length prefix claims more bytes than remain.
    LengthExceedsData { what: &'static str, length: usize, available: usize },
    /// A value read from the wire is not valid for the field.
    InvalidValue { what: &'static str, detail: String },
    /// The caller's destination buffer cannot hold the requested bytes.
    DestinationTooSmall { start: usize, count: usize, len: usize },
    /// A value cannot be encoded with the wire format's length prefix.
    TooLong { what: &'static str, length: usize, max: usize },
    /// A caller-supplied range does not fit its buffer.
    RangeOutOfBounds { offset: usize, length: usize, len: usize },
    /// A field that must be set before serializing was not.
    MissingField { what: &'static str },
}

/// The C# reader threw (`InvalidOperationException`, `ArgumentException`) on a short read; every
/// `get_*` here returns this instead so a handler propagates it with `?` and the message
/// processor counts it against the peer exactly as the catch block did.
///
/// Carries the location that detected the fault, so a malformed packet can be traced to the
/// exact read without allocating anything on the hot path. Equality ignores the location.
#[derive(Debug, Clone)]
pub struct NetDataError {
    kind: NetDataErrorKind,
    field: Option<&'static str>,
    location: &'static Location<'static>,
}

impl PartialEq for NetDataError {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind && self.field == other.field
    }
}

impl Eq for NetDataError {}

impl NetDataError {
    #[track_caller]
    pub fn new(kind: NetDataErrorKind) -> Self {
        Self { kind, field: None, location: Location::caller() }
    }

    #[track_caller]
    pub fn short_read(what: &'static str, wanted: usize, position: usize, data_size: usize) -> Self {
        Self::new(NetDataErrorKind::ShortRead { what, wanted, position, data_size })
    }

    #[track_caller]
    pub fn length_exceeds_data(what: &'static str, length: usize, available: usize) -> Self {
        Self::new(NetDataErrorKind::LengthExceedsData { what, length, available })
    }

    #[track_caller]
    pub fn invalid(what: &'static str, detail: impl Into<String>) -> Self {
        Self::new(NetDataErrorKind::InvalidValue { what, detail: detail.into() })
    }

    #[track_caller]
    pub fn too_long(what: &'static str, length: usize, max: usize) -> Self {
        Self::new(NetDataErrorKind::TooLong { what, length, max })
    }

    #[track_caller]
    pub fn range_out_of_bounds(offset: usize, length: usize, len: usize) -> Self {
        Self::new(NetDataErrorKind::RangeOutOfBounds { offset, length, len })
    }

    #[track_caller]
    pub fn missing_field(what: &'static str) -> Self {
        Self::new(NetDataErrorKind::MissingField { what })
    }

    pub fn kind(&self) -> &NetDataErrorKind {
        &self.kind
    }

    /// The message field being read when the fault was detected, if the caller named it.
    pub fn field(&self) -> Option<&'static str> {
        self.field
    }

    /// Names the message field being read, for the log line.
    pub fn for_field(mut self, field: &'static str) -> Self {
        self.field = Some(field);
        self
    }

    pub fn location(&self) -> &'static Location<'static> {
        self.location
    }
}

impl fmt::Display for NetDataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(field) = self.field {
            write!(f, "{field}: ")?;
        }
        match &self.kind {
            NetDataErrorKind::ShortRead { what, wanted, position, data_size } => write!(
                f,
                "Not enough data to read {wanted} byte(s) for {what}. Position={position}, DataSize={data_size}"
            ),
            NetDataErrorKind::LengthExceedsData { what, length, available } => {
                write!(f, "{what} length {length} exceeds available data ({available} bytes).")
            }
            NetDataErrorKind::InvalidValue { what, detail } => write!(f, "Invalid {what}: {detail}"),
            NetDataErrorKind::DestinationTooSmall { start, count, len } => {
                write!(f, "Destination of {len} byte(s) cannot take {count} byte(s) at {start}.")
            }
            NetDataErrorKind::TooLong { what, length, max } => {
                write!(f, "{what} of {length} byte(s) exceeds the wire limit of {max}.")
            }
            NetDataErrorKind::RangeOutOfBounds { offset, length, len } => {
                write!(f, "Range {offset}..{} is outside a buffer of {len} byte(s).", offset.saturating_add(*length))
            }
            NetDataErrorKind::MissingField { what } => write!(f, "{what} must be set before serializing."),
        }
    }
}

impl std::error::Error for NetDataError {}

impl From<NetDataError> for BasisError {
    #[track_caller]
    fn from(err: NetDataError) -> Self {
        let location = err.location;
        let message = err.to_string();
        BasisError::at_with_source(FaultKind::Permanent, ErrorCode::Protocol, message, err, location)
            .context_at("propagated", Location::caller())
    }
}

pub type NetResult<T> = Result<T, NetDataError>;

/// Names the field a failed read belonged to: `reader.get_ushort().field("playerID")?`.
pub trait NetResultExt<T> {
    fn field(self, name: &'static str) -> NetResult<T>;
}

impl<T> NetResultExt<T> for NetResult<T> {
    fn field(self, name: &'static str) -> NetResult<T> {
        self.map_err(|e| e.for_field(name))
    }
}

/// Little-endian cursor over a received buffer.
///
/// The buffer is a [`Bytes`] so a datagram or stream frame straight off the transport is read
/// without copying, and `get_remaining_bytes_segment` hands out zero-copy views of it.
///
/// Invariants: `offset <= position <= data_size <= data.len()`. Every read checks against
/// `data_size` and fails with a [`NetDataError`] rather than reading past it.
#[derive(Clone, Debug, Default)]
pub struct NetDataReader {
    data: Bytes,
    position: usize,
    data_size: usize,
    offset: usize,
}

/// A received packet. LiteNetLib's variant carried a pooled buffer that had to be recycled; the
/// Rust transport hands out reference-counted [`Bytes`], so [`NetPacketReader::recycle`] is a
/// no-op kept so ported handlers read the same as their C# originals.
pub type NetPacketReader = NetDataReader;

impl NetDataReader {
    pub fn new(source: impl Into<Bytes>) -> Self {
        let mut reader = Self::default();
        reader.set_source(source.into());
        reader
    }

    pub fn from_slice(source: &[u8]) -> Self {
        Self::new(Bytes::copy_from_slice(source))
    }

    /// Reads `source[offset..max_size]`. Both bounds are clamped to the buffer.
    pub fn with_offset(source: impl Into<Bytes>, offset: usize, max_size: usize) -> Self {
        let mut reader = Self::default();
        reader.set_source_with_offset(source.into(), offset, max_size);
        reader
    }

    pub fn raw_data(&self) -> &[u8] {
        &self.data
    }

    pub fn raw_data_size(&self) -> usize {
        self.data_size
    }

    pub fn user_data_offset(&self) -> usize {
        self.offset
    }

    pub fn user_data_size(&self) -> usize {
        self.data_size.saturating_sub(self.offset)
    }

    pub fn is_null(&self) -> bool {
        self.data.is_empty() && self.data_size == 0
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn end_of_data(&self) -> bool {
        self.position >= self.data_size
    }

    pub fn available_bytes(&self) -> usize {
        self.data_size.saturating_sub(self.position)
    }

    /// Advances by `count`, stopping at the end of the data.
    pub fn skip_bytes(&mut self, count: usize) {
        self.position = self.position.saturating_add(count).min(self.data_size);
    }

    /// Moves the cursor, clamped to the end of the data.
    pub fn set_position(&mut self, position: usize) {
        self.position = position.min(self.data_size);
    }

    pub fn set_source(&mut self, source: Bytes) {
        self.data_size = source.len();
        self.data = source;
        self.position = 0;
        self.offset = 0;
    }

    pub fn set_source_with_offset(&mut self, source: Bytes, offset: usize, max_size: usize) {
        let data_size = max_size.min(source.len());
        let offset = offset.min(data_size);
        self.data = source;
        self.position = offset;
        self.offset = offset;
        self.data_size = data_size;
    }

    /// See [`NetPacketReader`]. `_is_ok_to_have_empty_data` mirrors the C# parameter and is
    /// unused: the leftover-bytes warning it gated was an editor-only build feature.
    pub fn recycle(&mut self) {}

    pub fn recycle_with(&mut self, _is_ok_to_have_empty_data: bool) {}

    #[track_caller]
    fn short(&self, wanted: usize, what: &'static str) -> NetDataError {
        NetDataError::short_read(what, wanted, self.position, self.data_size)
    }

    /// The next `count` bytes, without advancing.
    #[track_caller]
    fn peek_slice(&self, count: usize, what: &'static str) -> NetResult<&[u8]> {
        let end = self.position.checked_add(count).filter(|end| *end <= self.data_size);
        match end.and_then(|end| self.data.get(self.position..end)) {
            Some(slice) => Ok(slice),
            None => Err(self.short(count, what)),
        }
    }

    /// The next `count` bytes, advancing past them.
    #[track_caller]
    fn take(&mut self, count: usize, what: &'static str) -> NetResult<&[u8]> {
        let end = self.position.checked_add(count).filter(|end| *end <= self.data_size);
        let Some(end) = end else {
            return Err(self.short(count, what));
        };
        let start = self.position;
        match self.data.get(start..end) {
            Some(slice) => {
                self.position = end;
                Ok(slice)
            }
            None => Err(self.short(count, what)),
        }
    }

    #[track_caller]
    fn take_array<const N: usize>(&mut self, what: &'static str) -> NetResult<[u8; N]> {
        let position = self.position;
        let data_size = self.data_size;
        let slice = self.take(N, what)?;
        <[u8; N]>::try_from(slice).map_err(|_| NetDataError::short_read(what, N, position, data_size))
    }

    #[track_caller]
    fn peek_array<const N: usize>(&self, what: &'static str) -> NetResult<[u8; N]> {
        let slice = self.peek_slice(N, what)?;
        <[u8; N]>::try_from(slice).map_err(|_| self.short(N, what))
    }

    /// Checks that a length prefix read from the wire fits the remaining data before anything
    /// is allocated for it.
    #[track_caller]
    fn check_length(&self, what: &'static str, length: usize) -> NetResult<()> {
        if length > self.available_bytes() {
            return Err(NetDataError::length_exceeds_data(what, length, self.available_bytes()));
        }
        Ok(())
    }

    // ── Get methods ────────────────────────────────────────────────────────

    pub fn get_byte(&mut self) -> NetResult<u8> {
        Ok(self.take_array::<1>("byte")?[0])
    }

    pub fn get_sbyte(&mut self) -> NetResult<i8> {
        Ok(self.get_byte()? as i8)
    }

    pub fn get_bool(&mut self) -> NetResult<bool> {
        Ok(self.get_byte()? == 1)
    }

    /// A UTF-16 code unit; a lone surrogate decodes to U+FFFD.
    pub fn get_char(&mut self) -> NetResult<char> {
        let v = self.get_ushort()?;
        Ok(char::from_u32(u32::from(v)).unwrap_or('\u{FFFD}'))
    }

    pub fn get_ushort(&mut self) -> NetResult<u16> {
        Ok(u16::from_le_bytes(self.take_array("ushort")?))
    }

    pub fn get_short(&mut self) -> NetResult<i16> {
        Ok(i16::from_le_bytes(self.take_array("short")?))
    }

    pub fn get_long(&mut self) -> NetResult<i64> {
        Ok(i64::from_le_bytes(self.take_array("long")?))
    }

    pub fn get_ulong(&mut self) -> NetResult<u64> {
        Ok(u64::from_le_bytes(self.take_array("ulong")?))
    }

    pub fn get_int(&mut self) -> NetResult<i32> {
        Ok(i32::from_le_bytes(self.take_array("int")?))
    }

    pub fn get_uint(&mut self) -> NetResult<u32> {
        Ok(u32::from_le_bytes(self.take_array("uint")?))
    }

    pub fn get_float(&mut self) -> NetResult<f32> {
        Ok(f32::from_le_bytes(self.take_array("float")?))
    }

    pub fn get_double(&mut self) -> NetResult<f64> {
        Ok(f64::from_le_bytes(self.take_array("double")?))
    }

    /// Reads the `[ushort count][count * size bytes]` array framing shared by every typed array.
    /// The count is checked against the remaining data before anything is allocated.
    #[track_caller]
    fn get_array_raw(&mut self, size: usize, what: &'static str) -> NetResult<&[u8]> {
        let length = usize::from(self.get_ushort()?);
        let byte_count = length.saturating_mul(size);
        self.check_length(what, byte_count)?;
        self.take(byte_count, what)
    }

    fn collect_le<T, const N: usize>(raw: &[u8], from_le: impl Fn([u8; N]) -> T) -> Vec<T> {
        raw.as_chunks::<N>().0.iter().map(|chunk| from_le(*chunk)).collect()
    }

    pub fn get_bool_array(&mut self) -> NetResult<Vec<bool>> {
        let raw = self.get_array_raw(1, "bool array")?;
        Ok(raw.iter().map(|b| *b == 1).collect())
    }

    pub fn get_ushort_array(&mut self) -> NetResult<Vec<u16>> {
        let raw = self.get_array_raw(2, "ushort array")?;
        Ok(Self::collect_le(raw, u16::from_le_bytes))
    }

    pub fn get_short_array(&mut self) -> NetResult<Vec<i16>> {
        let raw = self.get_array_raw(2, "short array")?;
        Ok(Self::collect_le(raw, i16::from_le_bytes))
    }

    pub fn get_int_array(&mut self) -> NetResult<Vec<i32>> {
        let raw = self.get_array_raw(4, "int array")?;
        Ok(Self::collect_le(raw, i32::from_le_bytes))
    }

    pub fn get_uint_array(&mut self) -> NetResult<Vec<u32>> {
        let raw = self.get_array_raw(4, "uint array")?;
        Ok(Self::collect_le(raw, u32::from_le_bytes))
    }

    pub fn get_float_array(&mut self) -> NetResult<Vec<f32>> {
        let raw = self.get_array_raw(4, "float array")?;
        Ok(Self::collect_le(raw, f32::from_le_bytes))
    }

    pub fn get_double_array(&mut self) -> NetResult<Vec<f64>> {
        let raw = self.get_array_raw(8, "double array")?;
        Ok(Self::collect_le(raw, f64::from_le_bytes))
    }

    pub fn get_long_array(&mut self) -> NetResult<Vec<i64>> {
        let raw = self.get_array_raw(8, "long array")?;
        Ok(Self::collect_le(raw, i64::from_le_bytes))
    }

    pub fn get_ulong_array(&mut self) -> NetResult<Vec<u64>> {
        let raw = self.get_array_raw(8, "ulong array")?;
        Ok(Self::collect_le(raw, u64::from_le_bytes))
    }

    pub fn get_string_array(&mut self) -> NetResult<Vec<String>> {
        let length = self.get_ushort()?;
        // Every string costs at least its 2-byte prefix, so the count bounds the allocation.
        self.check_length("string array", usize::from(length).saturating_mul(2))?;
        (0..length).map(|_| self.get_string()).collect()
    }

    /// Note that `max_string_length` only limits the number of characters in a string, not
    /// its size in bytes. Strings that exceed this parameter are returned as empty.
    pub fn get_string_array_max(&mut self, max_string_length: usize) -> NetResult<Vec<String>> {
        let length = self.get_ushort()?;
        self.check_length("string array", usize::from(length).saturating_mul(2))?;
        (0..length).map(|_| self.get_string_max(max_string_length)).collect()
    }

    /// Note that `max_length` only limits the number of characters in a string, not its size
    /// in bytes. Returns an empty string if the value is longer than `max_length`.
    pub fn get_string_max(&mut self, max_length: usize) -> NetResult<String> {
        let size = usize::from(self.get_ushort()?);
        if size == 0 {
            return Ok(String::new());
        }
        let actual_size = size - 1;
        self.check_length("string", actual_size)?;
        let raw = self.take(actual_size, "string")?;
        let result = String::from_utf8_lossy(raw);
        // C# counts UTF-16 code units; a char over the BMP counts twice there and once here,
        // which only matters for a limit sitting exactly on a surrogate pair.
        if max_length > 0 && result.chars().count() > max_length {
            Ok(String::new())
        } else {
            Ok(result.into_owned())
        }
    }

    pub fn get_string(&mut self) -> NetResult<String> {
        self.get_string_max(0)
    }

    pub fn get_large_string(&mut self) -> NetResult<String> {
        let size = self.get_int()?;
        let Ok(size) = usize::try_from(size) else {
            return Ok(String::new());
        };
        if size == 0 {
            return Ok(String::new());
        }
        self.check_length("large string", size)?;
        let raw = self.take(size, "large string")?;
        Ok(String::from_utf8_lossy(raw).into_owned())
    }

    /// A .NET `Guid`'s 16 wire bytes, in the mixed-endian layout `Guid.ToByteArray` produces.
    pub fn get_guid(&mut self) -> NetResult<[u8; 16]> {
        self.take_array("guid")
    }

    /// Zero-copy view of the next `count` bytes.
    pub fn get_bytes_segment(&mut self, count: usize) -> NetResult<Bytes> {
        self.check_length("segment", count)?;
        let start = self.position;
        let end = start.saturating_add(count);
        if end > self.data.len() {
            return Err(self.short(count, "segment"));
        }
        let segment = self.data.slice(start..end);
        self.position = end;
        Ok(segment)
    }

    /// Zero-copy view of everything left. Advances to the end like the C# version.
    pub fn get_remaining_bytes_segment(&mut self) -> Bytes {
        let start = self.position.min(self.data_size);
        let segment = self.data.slice(start..self.data_size);
        self.position = self.data_size;
        segment
    }

    pub fn get_remaining_bytes_span(&self) -> &[u8] {
        self.data.get(self.position.min(self.data_size)..self.data_size).unwrap_or(&[])
    }

    pub fn get_remaining_bytes(&mut self) -> Vec<u8> {
        let out = self.get_remaining_bytes_span().to_vec();
        self.position = self.data_size;
        out
    }

    /// Copies `count` bytes into `destination[start..start + count]`.
    pub fn get_bytes_into(&mut self, destination: &mut [u8], start: usize, count: usize) -> NetResult<()> {
        let end = start.checked_add(count).filter(|end| *end <= destination.len());
        let Some(dest) = end.and_then(|end| destination.get_mut(start..end)) else {
            return Err(NetDataError::new(NetDataErrorKind::DestinationTooSmall { start, count, len: destination.len() }));
        };
        let src = self.take(count, "bytes")?;
        dest.copy_from_slice(src);
        Ok(())
    }

    pub fn get_bytes(&mut self, destination: &mut [u8], count: usize) -> NetResult<()> {
        self.get_bytes_into(destination, 0, count)
    }

    /// `count` bytes as a fresh vector.
    pub fn get_bytes_vec(&mut self, count: usize) -> NetResult<Vec<u8>> {
        Ok(self.take(count, "bytes")?.to_vec())
    }

    pub fn get_sbytes_with_length(&mut self) -> NetResult<Vec<i8>> {
        let raw = self.get_array_raw(1, "sbyte array")?;
        Ok(raw.iter().map(|b| *b as i8).collect())
    }

    pub fn get_bytes_with_length(&mut self) -> NetResult<Vec<u8>> {
        let raw = self.get_array_raw(1, "byte array")?;
        Ok(raw.to_vec())
    }

    // ── Peek methods ───────────────────────────────────────────────────────

    pub fn peek_byte(&self) -> NetResult<u8> {
        Ok(self.peek_array::<1>("byte")?[0])
    }

    pub fn peek_sbyte(&self) -> NetResult<i8> {
        Ok(self.peek_byte()? as i8)
    }

    pub fn peek_bool(&self) -> NetResult<bool> {
        Ok(self.peek_byte()? == 1)
    }

    pub fn peek_char(&self) -> NetResult<char> {
        let v = self.peek_ushort()?;
        Ok(char::from_u32(u32::from(v)).unwrap_or('\u{FFFD}'))
    }

    pub fn peek_ushort(&self) -> NetResult<u16> {
        Ok(u16::from_le_bytes(self.peek_array("ushort")?))
    }

    pub fn peek_short(&self) -> NetResult<i16> {
        Ok(i16::from_le_bytes(self.peek_array("short")?))
    }

    pub fn peek_long(&self) -> NetResult<i64> {
        Ok(i64::from_le_bytes(self.peek_array("long")?))
    }

    pub fn peek_ulong(&self) -> NetResult<u64> {
        Ok(u64::from_le_bytes(self.peek_array("ulong")?))
    }

    pub fn peek_int(&self) -> NetResult<i32> {
        Ok(i32::from_le_bytes(self.peek_array("int")?))
    }

    pub fn peek_uint(&self) -> NetResult<u32> {
        Ok(u32::from_le_bytes(self.peek_array("uint")?))
    }

    pub fn peek_float(&self) -> NetResult<f32> {
        Ok(f32::from_le_bytes(self.peek_array("float")?))
    }

    pub fn peek_double(&self) -> NetResult<f64> {
        Ok(f64::from_le_bytes(self.peek_array("double")?))
    }

    /// Note that `max_length` only limits the number of characters in a string, not its size in bytes.
    pub fn peek_string_max(&self, max_length: usize) -> NetResult<String> {
        let size = usize::from(self.peek_ushort()?);
        if size == 0 {
            return Ok(String::new());
        }
        let actual_size = size - 1;
        let raw = self.peek_slice(2 + actual_size, "string")?;
        let s = String::from_utf8_lossy(raw.get(2..).unwrap_or(&[]));
        if max_length > 0 && s.chars().count() > max_length {
            Ok(String::new())
        } else {
            Ok(s.into_owned())
        }
    }

    /// Defensive: callers (e.g. HandleDisconnectionReason) sometimes hand us a buffer that
    /// doesn't actually contain a length-prefixed string — a version-mismatch reject or any
    /// other malformed additional-data payload would otherwise tip `get_string` into an error.
    /// Validates before reading and answers an empty string for anything that does not fit.
    pub fn peek_string(&self) -> String {
        self.peek_string_max(0).unwrap_or_default()
    }

    // ── TryGet methods ─────────────────────────────────────────────────────

    pub fn try_get_byte(&mut self) -> Option<u8> {
        self.get_byte().ok()
    }

    pub fn try_get_sbyte(&mut self) -> Option<i8> {
        self.get_sbyte().ok()
    }

    pub fn try_get_bool(&mut self) -> Option<bool> {
        self.get_bool().ok()
    }

    pub fn try_get_char(&mut self) -> Option<char> {
        self.try_get_ushort().map(|v| char::from_u32(u32::from(v)).unwrap_or('\0'))
    }

    pub fn try_get_short(&mut self) -> Option<i16> {
        self.get_short().ok()
    }

    pub fn try_get_ushort(&mut self) -> Option<u16> {
        self.get_ushort().ok()
    }

    pub fn try_get_int(&mut self) -> Option<i32> {
        self.get_int().ok()
    }

    pub fn try_get_uint(&mut self) -> Option<u32> {
        self.get_uint().ok()
    }

    pub fn try_get_long(&mut self) -> Option<i64> {
        self.get_long().ok()
    }

    pub fn try_get_ulong(&mut self) -> Option<u64> {
        self.get_ulong().ok()
    }

    pub fn try_get_float(&mut self) -> Option<f32> {
        self.get_float().ok()
    }

    pub fn try_get_double(&mut self) -> Option<f64> {
        self.get_double().ok()
    }

    /// Reads a string only if the whole of it is present; the cursor does not move otherwise.
    pub fn try_get_string(&mut self) -> Option<String> {
        let size = usize::from(self.peek_ushort().ok()?);
        let needed = if size == 0 { 2 } else { 2 + size - 1 };
        if self.available_bytes() < needed {
            return None;
        }
        self.get_string().ok()
    }

    pub fn try_get_string_array(&mut self) -> Option<Vec<String>> {
        let length = self.try_get_ushort()?;
        let mut result = Vec::with_capacity(usize::from(length).min(self.available_bytes() / 2));
        for _ in 0..length {
            result.push(self.try_get_string()?);
        }
        Some(result)
    }

    /// Reads a byte array only if the whole of it is present; the cursor does not move otherwise.
    pub fn try_get_bytes_with_length(&mut self) -> Option<Vec<u8>> {
        let length = usize::from(self.peek_ushort().ok()?);
        if self.available_bytes() < 2 + length {
            return None;
        }
        self.get_bytes_with_length().ok()
    }

    pub fn clear(&mut self) {
        self.position = 0;
        self.data_size = 0;
        self.offset = 0;
        self.data = Bytes::new();
    }
}
