// SPDX-License-Identifier: MIT
// Copyright (c) 2020 Ruslan Pyrch
// Port of LiteNetLib's NetDataReader as vendored into BasisNetworkCore/Io/NetDataReader.cs.

use bytes::Bytes;

/// The C# reader throws (`InvalidOperationException`, `ArgumentException`) on a short read;
/// every `get_*` here returns this instead so a handler propagates it with `?` and the message
/// processor counts it against the peer exactly as the catch block did.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct NetDataError(pub String);

pub type NetResult<T> = Result<T, NetDataError>;

/// Little-endian cursor over a received buffer.
///
/// The buffer is a [`Bytes`] so a datagram or stream frame straight off the transport is read
/// without copying, and `get_remaining_bytes_segment` hands out zero-copy views of it.
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
        self.data_size - self.offset
    }

    pub fn is_null(&self) -> bool {
        self.data.is_empty() && self.data_size == 0
    }

    pub fn position(&self) -> usize {
        self.position
    }

    pub fn end_of_data(&self) -> bool {
        self.position == self.data_size
    }

    pub fn available_bytes(&self) -> usize {
        self.data_size.saturating_sub(self.position)
    }

    pub fn skip_bytes(&mut self, count: usize) {
        self.position += count;
    }

    pub fn set_position(&mut self, position: usize) {
        self.position = position;
    }

    pub fn set_source(&mut self, source: Bytes) {
        self.data_size = source.len();
        self.data = source;
        self.position = 0;
        self.offset = 0;
    }

    pub fn set_source_with_offset(&mut self, source: Bytes, offset: usize, max_size: usize) {
        self.data = source;
        self.position = offset;
        self.offset = offset;
        self.data_size = max_size;
    }

    /// See [`NetPacketReader`]. `_is_ok_to_have_empty_data` mirrors the C# parameter and is
    /// unused: the leftover-bytes warning it gated was an editor-only build feature.
    pub fn recycle(&mut self) {}

    pub fn recycle_with(&mut self, _is_ok_to_have_empty_data: bool) {}

    fn short(&self, wanted: usize, what: &str) -> NetDataError {
        NetDataError(format!(
            "Not enough data to read {wanted} byte(s) for {what}. Position={}, DataSize={}",
            self.position, self.data_size
        ))
    }

    fn take(&mut self, count: usize, what: &str) -> NetResult<&[u8]> {
        if count > self.available_bytes() {
            return Err(self.short(count, what));
        }
        let start = self.position;
        self.position += count;
        Ok(&self.data[start..start + count])
    }

    // ── Get methods ────────────────────────────────────────────────────────

    pub fn get_byte(&mut self) -> NetResult<u8> {
        if self.position >= self.data_size {
            return Err(NetDataError(format!(
                "Not enough data to read 1 byte. Position={}, DataSize={}",
                self.position, self.data_size
            )));
        }
        let res = self.data[self.position];
        self.position += 1;
        Ok(res)
    }

    pub fn get_sbyte(&mut self) -> NetResult<i8> {
        Ok(self.get_byte()? as i8)
    }

    pub fn get_bool(&mut self) -> NetResult<bool> {
        Ok(self.get_byte()? == 1)
    }

    pub fn get_char(&mut self) -> NetResult<char> {
        let v = self.get_ushort()?;
        Ok(char::from_u32(u32::from(v)).unwrap_or('\u{FFFD}'))
    }

    pub fn get_ushort(&mut self) -> NetResult<u16> {
        let b = self.take(2, "ushort")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn get_short(&mut self) -> NetResult<i16> {
        let b = self.take(2, "short")?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }

    pub fn get_long(&mut self) -> NetResult<i64> {
        let b = self.take(8, "long")?;
        Ok(i64::from_le_bytes(b.try_into().unwrap()))
    }

    pub fn get_ulong(&mut self) -> NetResult<u64> {
        let b = self.take(8, "ulong")?;
        Ok(u64::from_le_bytes(b.try_into().unwrap()))
    }

    pub fn get_int(&mut self) -> NetResult<i32> {
        let b = self.take(4, "int")?;
        Ok(i32::from_le_bytes(b.try_into().unwrap()))
    }

    pub fn get_uint(&mut self) -> NetResult<u32> {
        let b = self.take(4, "uint")?;
        Ok(u32::from_le_bytes(b.try_into().unwrap()))
    }

    pub fn get_float(&mut self) -> NetResult<f32> {
        let b = self.take(4, "float")?;
        Ok(f32::from_le_bytes(b.try_into().unwrap()))
    }

    pub fn get_double(&mut self) -> NetResult<f64> {
        let b = self.take(8, "double")?;
        Ok(f64::from_le_bytes(b.try_into().unwrap()))
    }

    /// Reads the `[ushort count][count * size bytes]` array framing shared by every typed array.
    fn get_array_raw(&mut self, size: usize) -> NetResult<(usize, &[u8])> {
        let length = usize::from(self.get_ushort()?);
        let byte_count = length * size;
        if byte_count > self.available_bytes() {
            return Err(NetDataError(format!(
                "Array length {byte_count} exceeds available data ({} bytes).",
                self.available_bytes()
            )));
        }
        let start = self.position;
        self.position += byte_count;
        Ok((length, &self.data[start..start + byte_count]))
    }

    pub fn get_bool_array(&mut self) -> NetResult<Vec<bool>> {
        let (_, raw) = self.get_array_raw(1)?;
        Ok(raw.iter().map(|b| *b == 1).collect())
    }

    pub fn get_ushort_array(&mut self) -> NetResult<Vec<u16>> {
        let (_, raw) = self.get_array_raw(2)?;
        Ok(raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect())
    }

    pub fn get_short_array(&mut self) -> NetResult<Vec<i16>> {
        let (_, raw) = self.get_array_raw(2)?;
        Ok(raw.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect())
    }

    pub fn get_int_array(&mut self) -> NetResult<Vec<i32>> {
        let (_, raw) = self.get_array_raw(4)?;
        Ok(raw.chunks_exact(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect())
    }

    pub fn get_uint_array(&mut self) -> NetResult<Vec<u32>> {
        let (_, raw) = self.get_array_raw(4)?;
        Ok(raw.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect())
    }

    pub fn get_float_array(&mut self) -> NetResult<Vec<f32>> {
        let (_, raw) = self.get_array_raw(4)?;
        Ok(raw.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect())
    }

    pub fn get_double_array(&mut self) -> NetResult<Vec<f64>> {
        let (_, raw) = self.get_array_raw(8)?;
        Ok(raw.chunks_exact(8).map(|c| f64::from_le_bytes(c.try_into().unwrap())).collect())
    }

    pub fn get_long_array(&mut self) -> NetResult<Vec<i64>> {
        let (_, raw) = self.get_array_raw(8)?;
        Ok(raw.chunks_exact(8).map(|c| i64::from_le_bytes(c.try_into().unwrap())).collect())
    }

    pub fn get_ulong_array(&mut self) -> NetResult<Vec<u64>> {
        let (_, raw) = self.get_array_raw(8)?;
        Ok(raw.chunks_exact(8).map(|c| u64::from_le_bytes(c.try_into().unwrap())).collect())
    }

    pub fn get_string_array(&mut self) -> NetResult<Vec<String>> {
        let length = self.get_ushort()?;
        (0..length).map(|_| self.get_string()).collect()
    }

    /// Note that `max_string_length` only limits the number of characters in a string, not
    /// its size in bytes. Strings that exceed this parameter are returned as empty.
    pub fn get_string_array_max(&mut self, max_string_length: usize) -> NetResult<Vec<String>> {
        let length = self.get_ushort()?;
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
        if actual_size > self.available_bytes() {
            return Err(NetDataError(format!(
                "String length {actual_size} exceeds available data ({} bytes).",
                self.available_bytes()
            )));
        }
        let raw = &self.data[self.position..self.position + actual_size];
        let result = String::from_utf8_lossy(raw);
        // C# counts UTF-16 code units; a char over the BMP counts twice there and once here,
        // which only matters for a limit sitting exactly on a surrogate pair.
        let result = if max_length > 0 && result.chars().count() > max_length {
            String::new()
        } else {
            result.into_owned()
        };
        self.position += actual_size;
        Ok(result)
    }

    pub fn get_string(&mut self) -> NetResult<String> {
        self.get_string_max(0)
    }

    pub fn get_large_string(&mut self) -> NetResult<String> {
        let size = self.get_int()?;
        if size <= 0 {
            return Ok(String::new());
        }
        let size = size as usize;
        if size > self.available_bytes() {
            return Err(NetDataError(format!(
                "String length {size} exceeds available data ({} bytes).",
                self.available_bytes()
            )));
        }
        let raw = &self.data[self.position..self.position + size];
        let result = String::from_utf8_lossy(raw).into_owned();
        self.position += size;
        Ok(result)
    }

    /// A .NET `Guid`'s 16 wire bytes, in the mixed-endian layout `Guid.ToByteArray` produces.
    pub fn get_guid(&mut self) -> NetResult<[u8; 16]> {
        if 16 > self.available_bytes() {
            return Err(NetDataError(format!(
                "Guid read exceeds available data ({} bytes).",
                self.available_bytes()
            )));
        }
        let b = self.take(16, "guid")?;
        Ok(b.try_into().unwrap())
    }

    /// Zero-copy view of the next `count` bytes.
    pub fn get_bytes_segment(&mut self, count: usize) -> NetResult<Bytes> {
        if count > self.available_bytes() {
            return Err(NetDataError(format!(
                "Segment length {count} exceeds available data ({} bytes).",
                self.available_bytes()
            )));
        }
        let segment = self.data.slice(self.position..self.position + count);
        self.position += count;
        Ok(segment)
    }

    /// Zero-copy view of everything left. Advances to the end like the C# version.
    pub fn get_remaining_bytes_segment(&mut self) -> Bytes {
        let segment = self.data.slice(self.position..self.data_size);
        self.position = self.data_size;
        segment
    }

    pub fn get_remaining_bytes_span(&self) -> &[u8] {
        &self.data[self.position..self.data_size]
    }

    pub fn get_remaining_bytes(&mut self) -> Vec<u8> {
        let out = self.data[self.position..self.data_size].to_vec();
        self.position = self.data_size;
        out
    }

    pub fn get_bytes_into(&mut self, destination: &mut [u8], start: usize, count: usize) -> NetResult<()> {
        if count > self.available_bytes() {
            return Err(NetDataError(format!(
                "Byte read {count} exceeds available data ({} bytes).",
                self.available_bytes()
            )));
        }
        destination[start..start + count].copy_from_slice(&self.data[self.position..self.position + count]);
        self.position += count;
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
        let (_, raw) = self.get_array_raw(1)?;
        Ok(raw.iter().map(|b| *b as i8).collect())
    }

    pub fn get_bytes_with_length(&mut self) -> NetResult<Vec<u8>> {
        let (_, raw) = self.get_array_raw(1)?;
        Ok(raw.to_vec())
    }

    // ── Peek methods ───────────────────────────────────────────────────────

    fn peek_slice(&self, count: usize, what: &str) -> NetResult<&[u8]> {
        if count > self.available_bytes() {
            return Err(self.short(count, what));
        }
        Ok(&self.data[self.position..self.position + count])
    }

    pub fn peek_byte(&self) -> NetResult<u8> {
        Ok(self.peek_slice(1, "byte")?[0])
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
        let b = self.peek_slice(2, "ushort")?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn peek_short(&self) -> NetResult<i16> {
        let b = self.peek_slice(2, "short")?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }

    pub fn peek_long(&self) -> NetResult<i64> {
        Ok(i64::from_le_bytes(self.peek_slice(8, "long")?.try_into().unwrap()))
    }

    pub fn peek_ulong(&self) -> NetResult<u64> {
        Ok(u64::from_le_bytes(self.peek_slice(8, "ulong")?.try_into().unwrap()))
    }

    pub fn peek_int(&self) -> NetResult<i32> {
        Ok(i32::from_le_bytes(self.peek_slice(4, "int")?.try_into().unwrap()))
    }

    pub fn peek_uint(&self) -> NetResult<u32> {
        Ok(u32::from_le_bytes(self.peek_slice(4, "uint")?.try_into().unwrap()))
    }

    pub fn peek_float(&self) -> NetResult<f32> {
        Ok(f32::from_le_bytes(self.peek_slice(4, "float")?.try_into().unwrap()))
    }

    pub fn peek_double(&self) -> NetResult<f64> {
        Ok(f64::from_le_bytes(self.peek_slice(8, "double")?.try_into().unwrap()))
    }

    /// Note that `max_length` only limits the number of characters in a string, not its size in bytes.
    pub fn peek_string_max(&self, max_length: usize) -> NetResult<String> {
        let size = usize::from(self.peek_ushort()?);
        if size == 0 {
            return Ok(String::new());
        }
        let actual_size = size - 1;
        let raw = self.peek_slice(2 + actual_size, "string")?;
        let s = String::from_utf8_lossy(&raw[2..]);
        if max_length > 0 && s.chars().count() > max_length {
            Ok(String::new())
        } else {
            Ok(s.into_owned())
        }
    }

    /// Defensive: callers (e.g. HandleDisconnectionReason) sometimes hand us a buffer that
    /// doesn't actually contain a length-prefixed string — a version-mismatch reject or any
    /// other malformed additional-data payload would otherwise tip `get_string` into an error.
    /// Validate before reading.
    pub fn peek_string(&self) -> String {
        if self.available_bytes() < 2 {
            return String::new();
        }
        let Ok(size) = self.peek_ushort() else {
            return String::new();
        };
        if size == 0 {
            return String::new();
        }
        let actual_size = usize::from(size) - 1;
        if self.position + 2 + actual_size > self.data.len() {
            return String::new();
        }
        String::from_utf8_lossy(&self.data[self.position + 2..self.position + 2 + actual_size]).into_owned()
    }

    // ── TryGet methods ─────────────────────────────────────────────────────

    pub fn try_get_byte(&mut self) -> Option<u8> {
        if self.available_bytes() >= 1 { self.get_byte().ok() } else { None }
    }

    pub fn try_get_sbyte(&mut self) -> Option<i8> {
        if self.available_bytes() >= 1 { self.get_sbyte().ok() } else { None }
    }

    pub fn try_get_bool(&mut self) -> Option<bool> {
        if self.available_bytes() >= 1 { self.get_bool().ok() } else { None }
    }

    pub fn try_get_char(&mut self) -> Option<char> {
        self.try_get_ushort().map(|v| char::from_u32(u32::from(v)).unwrap_or('\0'))
    }

    pub fn try_get_short(&mut self) -> Option<i16> {
        if self.available_bytes() >= 2 { self.get_short().ok() } else { None }
    }

    pub fn try_get_ushort(&mut self) -> Option<u16> {
        if self.available_bytes() >= 2 { self.get_ushort().ok() } else { None }
    }

    pub fn try_get_int(&mut self) -> Option<i32> {
        if self.available_bytes() >= 4 { self.get_int().ok() } else { None }
    }

    pub fn try_get_uint(&mut self) -> Option<u32> {
        if self.available_bytes() >= 4 { self.get_uint().ok() } else { None }
    }

    pub fn try_get_long(&mut self) -> Option<i64> {
        if self.available_bytes() >= 8 { self.get_long().ok() } else { None }
    }

    pub fn try_get_ulong(&mut self) -> Option<u64> {
        if self.available_bytes() >= 8 { self.get_ulong().ok() } else { None }
    }

    pub fn try_get_float(&mut self) -> Option<f32> {
        if self.available_bytes() >= 4 { self.get_float().ok() } else { None }
    }

    pub fn try_get_double(&mut self) -> Option<f64> {
        if self.available_bytes() >= 8 { self.get_double().ok() } else { None }
    }

    pub fn try_get_string(&mut self) -> Option<String> {
        if self.available_bytes() >= 2 {
            let str_size = usize::from(self.peek_ushort().ok()?);
            if self.available_bytes() >= str_size + 1 {
                return self.get_string().ok();
            }
        }
        None
    }

    pub fn try_get_string_array(&mut self) -> Option<Vec<String>> {
        let length = self.try_get_ushort()?;
        let mut result = Vec::with_capacity(usize::from(length));
        for _ in 0..length {
            result.push(self.try_get_string()?);
        }
        Some(result)
    }

    pub fn try_get_bytes_with_length(&mut self) -> Option<Vec<u8>> {
        if self.available_bytes() >= 2 {
            let length = usize::from(self.peek_ushort().ok()?);
            if self.available_bytes() >= 2 + length {
                return self.get_bytes_with_length().ok();
            }
        }
        None
    }

    pub fn clear(&mut self) {
        self.position = 0;
        self.data_size = 0;
        self.data = Bytes::new();
    }
}
