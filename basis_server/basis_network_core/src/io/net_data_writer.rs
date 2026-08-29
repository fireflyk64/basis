// SPDX-License-Identifier: MIT
// Copyright (c) 2020 Ruslan Pyrch
// Port of LiteNetLib's NetDataWriter as vendored into BasisNetworkCore/Io/NetDataWriter.cs.

use super::net_data_reader::{NetDataError, NetResult};

/// Little-endian append-only buffer. `data()` is the whole backing store (what the C# `Data`
/// property exposed) and `length()` how much of it has been written; `as_read_only_span()`
/// is the written prefix, which is what every send takes.
///
/// The buffer always grows to fit: the C# writer's non-auto-resize mode threw
/// `IndexOutOfRangeException` on overflow, and the server never used it. Scalar `put_*` calls
/// therefore cannot fail. The length-prefixed puts return a [`NetResult`] instead of silently
/// truncating a count to its `ushort` prefix the way the C# casts did.
///
/// Invariant: `position <= data.len()`.
#[derive(Clone, Debug)]
pub struct NetDataWriter {
    data: Vec<u8>,
    position: usize,
}

impl Default for NetDataWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl NetDataWriter {
    const INITIAL_SIZE: usize = 64;

    pub fn new() -> Self {
        Self::with_capacity(Self::INITIAL_SIZE)
    }

    pub fn with_capacity(initial_size: usize) -> Self {
        Self { data: vec![0; initial_size], position: 0 }
    }

    /// Creates a writer over `bytes`. `copy` = false adopts the vector as-is, already "written".
    pub fn from_bytes(bytes: Vec<u8>, copy: bool) -> Self {
        if copy {
            return Self::from_slice(&bytes);
        }
        let position = bytes.len();
        Self { data: bytes, position }
    }

    pub fn from_slice(bytes: &[u8]) -> Self {
        let mut w = Self::with_capacity(bytes.len());
        w.put_bytes(bytes);
        w
    }

    pub fn from_string(value: &str) -> NetResult<Self> {
        let mut w = Self::new();
        w.put_string(value)?;
        Ok(w)
    }

    pub fn capacity(&self) -> usize {
        self.data.len()
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    pub fn length(&self) -> usize {
        self.position
    }

    pub fn as_read_only_span(&self) -> &[u8] {
        self.data.get(..self.position).unwrap_or(&self.data)
    }

    pub fn resize_if_need(&mut self, new_size: usize) {
        if self.data.len() < new_size {
            let grown = new_size.max(self.data.len().saturating_mul(2));
            self.data.resize(grown, 0);
        }
    }

    pub fn ensure_fit(&mut self, additional_size: usize) {
        self.resize_if_need(self.position.saturating_add(additional_size));
    }

    pub fn reset_with_size(&mut self, size: usize) {
        self.resize_if_need(size);
        self.position = 0;
    }

    pub fn reset(&mut self) {
        self.position = 0;
    }

    pub fn copy_data(&self) -> Vec<u8> {
        self.as_read_only_span().to_vec()
    }

    /// Sets the position to rewrite previous values, growing the buffer to keep it in range.
    /// Returns the previous position.
    pub fn set_position(&mut self, position: usize) -> usize {
        let prev = self.position;
        self.resize_if_need(position);
        self.position = position;
        prev
    }

    /// Grows the buffer so `n` more bytes fit at the cursor and returns that window.
    #[inline]
    fn window(&mut self, n: usize) -> &mut [u8] {
        let start = self.position;
        let end = start.saturating_add(n);
        self.resize_if_need(end);
        self.position = end;
        // `resize_if_need` guarantees `end <= data.len()`.
        self.data.get_mut(start..end).unwrap_or(&mut [])
    }

    #[inline]
    fn write_raw(&mut self, bytes: &[u8]) {
        let window = self.window(bytes.len());
        if window.len() == bytes.len() {
            window.copy_from_slice(bytes);
        }
    }

    pub fn put_float(&mut self, value: f32) {
        self.write_raw(&value.to_le_bytes());
    }

    pub fn put_double(&mut self, value: f64) {
        self.write_raw(&value.to_le_bytes());
    }

    pub fn put_long(&mut self, value: i64) {
        self.write_raw(&value.to_le_bytes());
    }

    pub fn put_ulong(&mut self, value: u64) {
        self.write_raw(&value.to_le_bytes());
    }

    pub fn put_int(&mut self, value: i32) {
        self.write_raw(&value.to_le_bytes());
    }

    pub fn put_uint(&mut self, value: u32) {
        self.write_raw(&value.to_le_bytes());
    }

    /// Writes the UTF-16 code unit of `value`; a character outside the BMP is written as U+FFFD
    /// rather than truncated to its low half.
    pub fn put_char(&mut self, value: char) {
        let unit = u16::try_from(u32::from(value)).unwrap_or(0xFFFD);
        self.put_ushort(unit);
    }

    pub fn put_ushort(&mut self, value: u16) {
        self.write_raw(&value.to_le_bytes());
    }

    pub fn put_short(&mut self, value: i16) {
        self.write_raw(&value.to_le_bytes());
    }

    pub fn put_sbyte(&mut self, value: i8) {
        self.put_byte(value as u8);
    }

    pub fn put_byte(&mut self, value: u8) {
        self.write_raw(&[value]);
    }

    pub fn put_guid(&mut self, value: &[u8; 16]) {
        self.write_raw(value);
    }

    /// Writes `data[offset..offset + length]`.
    pub fn put_bytes_range(&mut self, data: &[u8], offset: usize, length: usize) -> NetResult<()> {
        let end = offset.checked_add(length);
        match end.and_then(|end| data.get(offset..end)) {
            Some(slice) => {
                self.write_raw(slice);
                Ok(())
            }
            None => Err(NetDataError::range_out_of_bounds(offset, length, data.len())),
        }
    }

    pub fn put_bytes(&mut self, data: &[u8]) {
        self.write_raw(data);
    }

    fn put_ushort_count(&mut self, what: &'static str, count: usize) -> NetResult<()> {
        let count = u16::try_from(count).map_err(|_| NetDataError::too_long(what, count, usize::from(u16::MAX)))?;
        self.put_ushort(count);
        Ok(())
    }

    pub fn put_sbytes_with_length(&mut self, data: &[i8]) -> NetResult<()> {
        self.put_ushort_count("sbyte array", data.len())?;
        let window = self.window(data.len());
        for (dst, src) in window.iter_mut().zip(data) {
            *dst = *src as u8;
        }
        Ok(())
    }

    pub fn put_bytes_with_length(&mut self, data: &[u8]) -> NetResult<()> {
        self.put_ushort_count("byte array", data.len())?;
        self.write_raw(data);
        Ok(())
    }

    pub fn put_bool(&mut self, value: bool) {
        self.put_byte(if value { 1 } else { 0 });
    }

    fn put_array_le<T: Copy, const N: usize>(
        &mut self,
        what: &'static str,
        arr: &[T],
        to_le: impl Fn(T) -> [u8; N],
    ) -> NetResult<()> {
        self.put_ushort_count(what, arr.len())?;
        let window = self.window(arr.len().saturating_mul(N));
        for (dst, src) in window.as_chunks_mut::<N>().0.iter_mut().zip(arr) {
            dst.copy_from_slice(&to_le(*src));
        }
        Ok(())
    }

    pub fn put_array_float(&mut self, value: &[f32]) -> NetResult<()> {
        self.put_array_le("float array", value, f32::to_le_bytes)
    }

    pub fn put_array_double(&mut self, value: &[f64]) -> NetResult<()> {
        self.put_array_le("double array", value, f64::to_le_bytes)
    }

    pub fn put_array_long(&mut self, value: &[i64]) -> NetResult<()> {
        self.put_array_le("long array", value, i64::to_le_bytes)
    }

    pub fn put_array_ulong(&mut self, value: &[u64]) -> NetResult<()> {
        self.put_array_le("ulong array", value, u64::to_le_bytes)
    }

    pub fn put_array_int(&mut self, value: &[i32]) -> NetResult<()> {
        self.put_array_le("int array", value, i32::to_le_bytes)
    }

    pub fn put_array_uint(&mut self, value: &[u32]) -> NetResult<()> {
        self.put_array_le("uint array", value, u32::to_le_bytes)
    }

    pub fn put_array_ushort(&mut self, value: &[u16]) -> NetResult<()> {
        self.put_array_le("ushort array", value, u16::to_le_bytes)
    }

    pub fn put_array_short(&mut self, value: &[i16]) -> NetResult<()> {
        self.put_array_le("short array", value, i16::to_le_bytes)
    }

    pub fn put_array_bool(&mut self, value: &[bool]) -> NetResult<()> {
        self.put_array_le("bool array", value, |b| [if b { 1u8 } else { 0u8 }])
    }

    pub fn put_array_string(&mut self, value: &[String]) -> NetResult<()> {
        self.put_array_string_max(value, 0)
    }

    /// A refused entry leaves nothing behind: the count prefix and the entries already written
    /// are rolled back, so a caller never ships a truncated array under a full count.
    pub fn put_array_string_max(&mut self, value: &[String], str_max_length: usize) -> NetResult<()> {
        let start = self.position;
        self.put_ushort_count("string array", value.len())?;
        for s in value {
            if let Err(e) = self.put_string_max(s, str_max_length) {
                self.position = start;
                return Err(e);
            }
        }
        Ok(())
    }

    pub fn put_large_string(&mut self, value: &str) -> NetResult<()> {
        if value.is_empty() {
            self.put_int(0);
            return Ok(());
        }
        let bytes = value.as_bytes();
        let length =
            i32::try_from(bytes.len()).map_err(|_| NetDataError::too_long("large string", bytes.len(), i32::MAX as usize))?;
        self.put_int(length);
        self.write_raw(bytes);
        Ok(())
    }

    pub fn put_string(&mut self, value: &str) -> NetResult<()> {
        self.put_string_max(value, 0)
    }

    /// Note that `max_length` only limits the number of characters in a string, not its size
    /// in bytes. A longer string is truncated to `max_length` characters, as the C# did.
    pub fn put_string_max(&mut self, value: &str, max_length: usize) -> NetResult<()> {
        if value.is_empty() {
            self.put_ushort(0);
            return Ok(());
        }
        let truncated: &str = if max_length > 0 && value.chars().count() > max_length {
            let end = value.char_indices().nth(max_length).map(|(i, _)| i).unwrap_or(value.len());
            value.get(..end).unwrap_or(value)
        } else {
            value
        };
        let bytes = truncated.as_bytes();
        if bytes.is_empty() {
            self.put_ushort(0);
            return Ok(());
        }
        let size = bytes.len() + 1;
        let prefix =
            u16::try_from(size).map_err(|_| NetDataError::too_long("string", bytes.len(), usize::from(u16::MAX) - 1))?;
        self.put_ushort(prefix);
        self.write_raw(bytes);
        Ok(())
    }
}
