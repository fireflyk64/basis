// SPDX-License-Identifier: MIT
// Copyright (c) 2020 Ruslan Pyrch
// Port of LiteNetLib's NetDataWriter as vendored into BasisNetworkCore/Io/NetDataWriter.cs.

/// Little-endian append-only buffer. `data()` is the whole backing store (what the C# `Data`
/// property exposed) and `length()` how much of it has been written; `as_read_only_span()`
/// is the written prefix, which is what every send takes.
#[derive(Clone, Debug)]
pub struct NetDataWriter {
    data: Vec<u8>,
    position: usize,
    auto_resize: bool,
}

impl Default for NetDataWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl NetDataWriter {
    const INITIAL_SIZE: usize = 64;

    pub fn new() -> Self {
        Self::with_capacity(true, Self::INITIAL_SIZE)
    }

    pub fn with_auto_resize(auto_resize: bool) -> Self {
        Self::with_capacity(auto_resize, Self::INITIAL_SIZE)
    }

    pub fn with_capacity(auto_resize: bool, initial_size: usize) -> Self {
        Self {
            data: vec![0; initial_size],
            position: 0,
            auto_resize,
        }
    }

    /// Creates a writer over `bytes`. `copy` = false adopts the vector as-is, already "written".
    pub fn from_bytes(bytes: Vec<u8>, copy: bool) -> Self {
        if copy {
            let mut w = Self::with_capacity(true, bytes.len());
            w.put_bytes(&bytes);
            return w;
        }
        let position = bytes.len();
        Self {
            data: bytes,
            position,
            auto_resize: true,
        }
    }

    pub fn from_slice(bytes: &[u8]) -> Self {
        let mut w = Self::with_capacity(true, bytes.len());
        w.put_bytes(bytes);
        w
    }

    pub fn from_string(value: &str) -> Self {
        let mut w = Self::new();
        w.put_string(value);
        w
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
        &self.data[..self.position]
    }

    pub fn resize_if_need(&mut self, new_size: usize) {
        if self.data.len() < new_size {
            let grown = new_size.max(self.data.len() * 2);
            self.data.resize(grown, 0);
        }
    }

    pub fn ensure_fit(&mut self, additional_size: usize) {
        if self.data.len() < self.position + additional_size {
            let grown = (self.position + additional_size).max(self.data.len() * 2);
            self.data.resize(grown, 0);
        }
    }

    pub fn reset_with_size(&mut self, size: usize) {
        self.resize_if_need(size);
        self.position = 0;
    }

    pub fn reset(&mut self) {
        self.position = 0;
    }

    pub fn copy_data(&self) -> Vec<u8> {
        self.data[..self.position].to_vec()
    }

    /// Sets the position to rewrite previous values. Returns the previous position.
    pub fn set_position(&mut self, position: usize) -> usize {
        let prev = self.position;
        self.position = position;
        prev
    }

    #[inline]
    fn reserve(&mut self, n: usize) {
        if self.auto_resize {
            self.resize_if_need(self.position + n);
        } else if self.data.len() < self.position + n {
            panic!("NetDataWriter overflow: {} + {n} > {}", self.position, self.data.len());
        }
    }

    #[inline]
    fn write_raw(&mut self, bytes: &[u8]) {
        self.reserve(bytes.len());
        self.data[self.position..self.position + bytes.len()].copy_from_slice(bytes);
        self.position += bytes.len();
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

    pub fn put_char(&mut self, value: char) {
        self.put_ushort(value as u32 as u16);
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
        self.reserve(1);
        self.data[self.position] = value;
        self.position += 1;
    }

    pub fn put_guid(&mut self, value: &[u8; 16]) {
        self.write_raw(value);
    }

    pub fn put_bytes_range(&mut self, data: &[u8], offset: usize, length: usize) {
        self.write_raw(&data[offset..offset + length]);
    }

    pub fn put_bytes(&mut self, data: &[u8]) {
        self.write_raw(data);
    }

    pub fn put_sbytes_with_length(&mut self, data: &[i8]) {
        self.put_ushort(data.len() as u16);
        self.reserve(data.len());
        for &b in data {
            self.data[self.position] = b as u8;
            self.position += 1;
        }
    }

    pub fn put_bytes_with_length(&mut self, data: &[u8]) {
        self.put_ushort(data.len() as u16);
        self.write_raw(data);
    }

    pub fn put_bool(&mut self, value: bool) {
        self.put_byte(if value { 1 } else { 0 });
    }

    fn put_array_le<T: Copy, const N: usize>(&mut self, arr: &[T], to_le: impl Fn(T) -> [u8; N]) {
        self.put_ushort(arr.len() as u16);
        self.reserve(arr.len() * N);
        for &v in arr {
            let b = to_le(v);
            self.data[self.position..self.position + N].copy_from_slice(&b);
            self.position += N;
        }
    }

    pub fn put_array_float(&mut self, value: &[f32]) {
        self.put_array_le(value, f32::to_le_bytes);
    }

    pub fn put_array_double(&mut self, value: &[f64]) {
        self.put_array_le(value, f64::to_le_bytes);
    }

    pub fn put_array_long(&mut self, value: &[i64]) {
        self.put_array_le(value, i64::to_le_bytes);
    }

    pub fn put_array_ulong(&mut self, value: &[u64]) {
        self.put_array_le(value, u64::to_le_bytes);
    }

    pub fn put_array_int(&mut self, value: &[i32]) {
        self.put_array_le(value, i32::to_le_bytes);
    }

    pub fn put_array_uint(&mut self, value: &[u32]) {
        self.put_array_le(value, u32::to_le_bytes);
    }

    pub fn put_array_ushort(&mut self, value: &[u16]) {
        self.put_array_le(value, u16::to_le_bytes);
    }

    pub fn put_array_short(&mut self, value: &[i16]) {
        self.put_array_le(value, i16::to_le_bytes);
    }

    pub fn put_array_bool(&mut self, value: &[bool]) {
        self.put_array_le(value, |b| [if b { 1u8 } else { 0u8 }]);
    }

    pub fn put_array_string(&mut self, value: &[String]) {
        self.put_ushort(value.len() as u16);
        for s in value {
            self.put_string(s);
        }
    }

    pub fn put_array_string_max(&mut self, value: &[String], str_max_length: usize) {
        self.put_ushort(value.len() as u16);
        for s in value {
            self.put_string_max(s, str_max_length);
        }
    }

    pub fn put_large_string(&mut self, value: &str) {
        if value.is_empty() {
            self.put_int(0);
            return;
        }
        let bytes = value.as_bytes();
        self.put_int(bytes.len() as i32);
        self.write_raw(bytes);
    }

    pub fn put_string(&mut self, value: &str) {
        self.put_string_max(value, 0);
    }

    /// Note that `max_length` only limits the number of characters in a string, not its size in bytes.
    pub fn put_string_max(&mut self, value: &str, max_length: usize) {
        if value.is_empty() {
            self.put_ushort(0);
            return;
        }
        let truncated: &str = if max_length > 0 && value.chars().count() > max_length {
            let end = value.char_indices().nth(max_length).map(|(i, _)| i).unwrap_or(value.len());
            &value[..end]
        } else {
            value
        };
        let bytes = truncated.as_bytes();
        if bytes.is_empty() {
            self.put_ushort(0);
            return;
        }
        let size = bytes.len() + 1;
        assert!(size <= usize::from(u16::MAX), "string too long for the ushort length prefix");
        self.put_ushort(size as u16);
        self.write_raw(bytes);
    }
}
