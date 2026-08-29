//! Round-trip and accounting tests for the NetDataWriter/NetDataReader pair. Covers every
//! put/get pair, try_get/peek semantics, position bookkeeping, writer growth/reset, and the ushort
//! length-prefix cap.
//!
//! Differences from the C#: there are no null strings or arrays (the empty cases stand in), the
//! writer always grows (the non-auto-resize constructor has no counterpart), a UTF-16 surrogate is
//! not a Rust `char`, and an over-long length-prefixed write is an error rather than a silent wrap
//! to a zero-length record.

use basis_network_core::{NetDataReader, NetDataWriter};

fn reader_over(w: &NetDataWriter) -> NetDataReader {
    NetDataReader::new(w.copy_data())
}

#[test]
fn primitives_round_trip_including_extremes() {
    let mut w = NetDataWriter::new();
    w.put_bool(true);
    w.put_bool(false);
    w.put_byte(0);
    w.put_byte(255);
    w.put_byte(0x5A);
    w.put_sbyte(i8::MIN);
    w.put_sbyte(i8::MAX);
    w.put_sbyte(-1);
    w.put_short(i16::MIN);
    w.put_short(i16::MAX);
    w.put_short(-12345);
    w.put_ushort(u16::MIN);
    w.put_ushort(u16::MAX);
    w.put_ushort(0xBEEF);
    w.put_int(i32::MIN);
    w.put_int(i32::MAX);
    w.put_int(-123456789);
    w.put_uint(u32::MIN);
    w.put_uint(u32::MAX);
    w.put_uint(0xDEADBEEF);
    w.put_long(i64::MIN);
    w.put_long(i64::MAX);
    w.put_long(-1234567890123456789);
    w.put_ulong(u64::MIN);
    w.put_ulong(u64::MAX);
    w.put_ulong(0x0123456789ABCDEF);
    w.put_char('\0');
    w.put_char('A');
    w.put_char('好');
    let guid: [u8; 16] = [0x44, 0x33, 0x22, 0x11, 0x66, 0x55, 0x88, 0x77, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff, 0x00];
    w.put_guid(&guid);

    let mut r = reader_over(&w);
    assert!(r.get_bool().unwrap());
    assert!(!r.get_bool().unwrap());
    assert_eq!(r.get_byte().unwrap(), 0);
    assert_eq!(r.get_byte().unwrap(), 255);
    assert_eq!(r.get_byte().unwrap(), 0x5A);
    assert_eq!(r.get_sbyte().unwrap(), i8::MIN);
    assert_eq!(r.get_sbyte().unwrap(), i8::MAX);
    assert_eq!(r.get_sbyte().unwrap(), -1);
    assert_eq!(r.get_short().unwrap(), i16::MIN);
    assert_eq!(r.get_short().unwrap(), i16::MAX);
    assert_eq!(r.get_short().unwrap(), -12345);
    assert_eq!(r.get_ushort().unwrap(), u16::MIN);
    assert_eq!(r.get_ushort().unwrap(), u16::MAX);
    assert_eq!(r.get_ushort().unwrap(), 0xBEEF);
    assert_eq!(r.get_int().unwrap(), i32::MIN);
    assert_eq!(r.get_int().unwrap(), i32::MAX);
    assert_eq!(r.get_int().unwrap(), -123456789);
    assert_eq!(r.get_uint().unwrap(), u32::MIN);
    assert_eq!(r.get_uint().unwrap(), u32::MAX);
    assert_eq!(r.get_uint().unwrap(), 0xDEADBEEF);
    assert_eq!(r.get_long().unwrap(), i64::MIN);
    assert_eq!(r.get_long().unwrap(), i64::MAX);
    assert_eq!(r.get_long().unwrap(), -1234567890123456789);
    assert_eq!(r.get_ulong().unwrap(), u64::MIN);
    assert_eq!(r.get_ulong().unwrap(), u64::MAX);
    assert_eq!(r.get_ulong().unwrap(), 0x0123456789ABCDEF);
    assert_eq!(r.get_char().unwrap(), '\0');
    assert_eq!(r.get_char().unwrap(), 'A');
    assert_eq!(r.get_char().unwrap(), '好');
    assert_eq!(r.get_guid().unwrap(), guid);
    assert!(r.end_of_data());
    assert_eq!(r.available_bytes(), 0);
}

#[test]
fn float_round_trips_bit_exact() {
    for value in [0f32, 1.5, -123.456, f32::MIN, f32::MAX, f32::EPSILON, f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.0] {
        let mut w = NetDataWriter::new();
        w.put_float(value);
        let mut r = reader_over(&w);
        assert_eq!(r.get_float().unwrap().to_bits(), value.to_bits());
    }
}

#[test]
fn double_round_trips_bit_exact() {
    for value in [0f64, 2.718281828459045, -98765.4321, f64::MIN, f64::MAX, f64::EPSILON, f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0] {
        let mut w = NetDataWriter::new();
        w.put_double(value);
        let mut r = reader_over(&w);
        assert_eq!(r.get_double().unwrap().to_bits(), value.to_bits());
    }
}

#[test]
fn string_round_trips() {
    for value in ["", "ascii only", "héllo wörld", "世界こんにちは", "emoji 🎈 pair"] {
        let mut w = NetDataWriter::new();
        w.put_string(value).unwrap();
        let mut r = reader_over(&w);
        assert_eq!(r.get_string().unwrap(), value);
    }
}

#[test]
fn string_empty_reads_back_as_empty() {
    let mut w = NetDataWriter::new();
    w.put_string("").unwrap();
    w.put_string("").unwrap();
    assert_eq!(w.length(), 4);
    let mut r = reader_over(&w);
    assert_eq!(r.get_string().unwrap(), "");
    assert_eq!(r.get_string().unwrap(), "");
    assert!(r.end_of_data());
}

#[test]
fn string_writer_max_length_truncates_by_char_count() {
    let mut w = NetDataWriter::new();
    w.put_string_max("abcdefghij", 4).unwrap();
    let mut r = reader_over(&w);
    assert_eq!(r.get_string().unwrap(), "abcd");
}

#[test]
fn string_reader_max_length_returns_empty_but_stays_aligned() {
    let mut w = NetDataWriter::new();
    w.put_string("abcdefghij").unwrap();
    w.put_int(42);
    let mut r = reader_over(&w);
    assert_eq!(r.get_string_max(3).unwrap(), "");
    assert_eq!(r.get_int().unwrap(), 42);
    assert!(r.end_of_data());
}

#[test]
fn large_string_round_trips_including_empty_and_unicode() {
    let big: String = (0..300).map(|i| (b'a' + (i % 26) as u8) as char).collect();
    let unicode = "世界 🎈 mixed";
    let mut w = NetDataWriter::new();
    w.put_large_string(&big).unwrap();
    w.put_large_string("").unwrap();
    w.put_large_string(unicode).unwrap();
    let mut r = reader_over(&w);
    assert_eq!(r.get_large_string().unwrap(), big);
    assert_eq!(r.get_large_string().unwrap(), "");
    assert_eq!(r.get_large_string().unwrap(), unicode);
    assert!(r.end_of_data());
}

#[test]
fn typed_arrays_round_trip() {
    let bools = [true, false, true, true, false];
    let shorts = [i16::MIN, -1, 0, 1, i16::MAX];
    let ushorts = [0u16, 1, 0x8000, u16::MAX];
    let ints = [i32::MIN, -1, 0, 1, i32::MAX];
    let uints = [0u32, 1, 0x80000000, u32::MAX];
    let longs = [i64::MIN, -1, 0, 1, i64::MAX];
    let ulongs = [0u64, 1, 0x8000000000000000, u64::MAX];
    let floats = [0f32, -1.5, f32::MAX, f32::EPSILON, f32::NAN];
    let doubles = [0f64, -2.5, f64::MAX, f64::EPSILON, f64::NAN];

    let mut w = NetDataWriter::new();
    w.put_array_bool(&bools).unwrap();
    w.put_array_short(&shorts).unwrap();
    w.put_array_ushort(&ushorts).unwrap();
    w.put_array_int(&ints).unwrap();
    w.put_array_uint(&uints).unwrap();
    w.put_array_long(&longs).unwrap();
    w.put_array_ulong(&ulongs).unwrap();
    w.put_array_float(&floats).unwrap();
    w.put_array_double(&doubles).unwrap();

    let mut r = reader_over(&w);
    assert_eq!(r.get_bool_array().unwrap(), bools);
    assert_eq!(r.get_short_array().unwrap(), shorts);
    assert_eq!(r.get_ushort_array().unwrap(), ushorts);
    assert_eq!(r.get_int_array().unwrap(), ints);
    assert_eq!(r.get_uint_array().unwrap(), uints);
    assert_eq!(r.get_long_array().unwrap(), longs);
    assert_eq!(r.get_ulong_array().unwrap(), ulongs);
    let f: Vec<u32> = r.get_float_array().unwrap().iter().map(|v| v.to_bits()).collect();
    assert_eq!(f, floats.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
    let d: Vec<u64> = r.get_double_array().unwrap().iter().map(|v| v.to_bits()).collect();
    assert_eq!(d, doubles.iter().map(|v| v.to_bits()).collect::<Vec<_>>());
    assert!(r.end_of_data());
}

#[test]
fn typed_arrays_empty_read_back_as_empty() {
    let mut w = NetDataWriter::new();
    w.put_array_int(&[]).unwrap();
    w.put_array_int(&[]).unwrap();
    w.put_array_double(&[]).unwrap();
    w.put_array_string(&[]).unwrap();
    let mut r = reader_over(&w);
    assert!(r.get_int_array().unwrap().is_empty());
    assert!(r.get_int_array().unwrap().is_empty());
    assert!(r.get_double_array().unwrap().is_empty());
    assert!(r.get_string_array().unwrap().is_empty());
    assert!(r.end_of_data());
}

#[test]
fn string_arrays_round_trip_and_per_element_max_length() {
    let values: Vec<String> = ["", "one", "二 two", "three 🎈"].iter().map(|s| s.to_string()).collect();
    let mut w = NetDataWriter::new();
    w.put_array_string(&values).unwrap();
    w.put_array_string_max(&values, 3).unwrap();
    let mut r = reader_over(&w);
    assert_eq!(r.get_string_array().unwrap(), values);
    assert_eq!(r.get_string_array().unwrap(), ["", "one", "二 t", "thr"]);
    assert!(r.end_of_data());
}

#[test]
fn string_array_reader_max_length_replaces_overlong_entries_with_empty() {
    let values: Vec<String> = ["ok", "toolongvalue", "yes"].iter().map(|s| s.to_string()).collect();
    let mut w = NetDataWriter::new();
    w.put_array_string(&values).unwrap();
    let mut r = reader_over(&w);
    assert_eq!(r.get_string_array_max(5).unwrap(), ["ok", "", "yes"]);
    assert!(r.end_of_data());
}

#[test]
fn bytes_with_length_round_trip_including_zero_length() {
    let payload = [1u8, 2, 3, 250, 251, 252];
    let signed = [i8::MIN, -1, 0, 1, i8::MAX];
    let mut w = NetDataWriter::new();
    w.put_bytes_with_length(&payload).unwrap();
    w.put_bytes_with_length(&[]).unwrap();
    w.put_sbytes_with_length(&signed).unwrap();
    w.put_sbytes_with_length(&[]).unwrap();
    let mut r = reader_over(&w);
    assert_eq!(r.get_bytes_with_length().unwrap(), payload);
    assert!(r.get_bytes_with_length().unwrap().is_empty());
    assert_eq!(r.get_sbytes_with_length().unwrap(), signed);
    assert!(r.get_sbytes_with_length().unwrap().is_empty());
    assert!(r.end_of_data());
}

#[test]
fn put_bytes_with_length_ushort_cap_65535_round_trips() {
    let payload: Vec<u8> = (0..usize::from(u16::MAX)).map(|i| (i * 31) as u8).collect();
    let mut w = NetDataWriter::new();
    w.put_bytes_with_length(&payload).unwrap();
    assert_eq!(w.length(), 2 + usize::from(u16::MAX));
    let mut r = reader_over(&w);
    assert_eq!(r.get_bytes_with_length().unwrap(), payload);
    assert!(r.end_of_data());
}

/// The length prefix is a ushort. The C# wrapped a 65536-byte array to a zero-length record; the
/// Rust refuses it and writes nothing, so a caller cannot ship an empty record by accident.
#[test]
fn put_bytes_with_length_above_64k_is_refused_and_writes_nothing() {
    let payload = vec![0u8; usize::from(u16::MAX) + 1];
    let mut w = NetDataWriter::new();
    assert!(w.put_bytes_with_length(&payload).is_err());
    assert_eq!(w.length(), 0);
    let over: Vec<String> = vec!["x".repeat(70_000)];
    assert!(w.put_string(&over[0]).is_err());
    assert!(w.put_array_string(&over).is_err());
    assert_eq!(w.length(), 0, "a refused write must leave nothing partial behind");
}

#[test]
fn try_get_on_empty_reader_returns_none_without_advancing() {
    let mut r = NetDataReader::from_slice(&[]);
    assert!(r.try_get_byte().is_none());
    assert!(r.try_get_sbyte().is_none());
    assert!(r.try_get_bool().is_none());
    assert!(r.try_get_char().is_none());
    assert!(r.try_get_short().is_none());
    assert!(r.try_get_ushort().is_none());
    assert!(r.try_get_int().is_none());
    assert!(r.try_get_uint().is_none());
    assert!(r.try_get_long().is_none());
    assert!(r.try_get_ulong().is_none());
    assert!(r.try_get_float().is_none());
    assert!(r.try_get_double().is_none());
    assert!(r.try_get_string().is_none());
    assert!(r.try_get_string_array().is_none());
    assert!(r.try_get_bytes_with_length().is_none());
    assert_eq!(r.position(), 0);
    assert!(r.end_of_data());
}

#[test]
fn try_get_on_truncated_fixed_width_value_returns_none_without_advancing() {
    let mut r = NetDataReader::from_slice(&[0x11, 0x22, 0x33]);
    assert!(r.try_get_int().is_none());
    assert!(r.try_get_long().is_none());
    assert!(r.try_get_float().is_none());
    assert!(r.try_get_double().is_none());
    assert_eq!(r.position(), 0);
    assert_eq!(r.try_get_short(), Some(0x2211));
    assert_eq!(r.position(), 2);
    assert_eq!(r.try_get_byte(), Some(0x33));
    assert!(r.try_get_byte().is_none());
    assert_eq!(r.position(), 3);
}

#[test]
fn try_get_string_on_truncated_payload_returns_none_without_advancing() {
    let mut w = NetDataWriter::new();
    w.put_string("hello").unwrap();
    let full = w.copy_data();
    let mut r = NetDataReader::from_slice(&full[..full.len() - 2]);
    assert!(r.try_get_string().is_none());
    assert_eq!(r.position(), 0);
}

#[test]
fn try_get_bytes_with_length_on_truncated_payload_returns_none_without_advancing() {
    let mut w = NetDataWriter::new();
    w.put_bytes_with_length(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10]).unwrap();
    let full = w.copy_data();
    let mut r = NetDataReader::from_slice(&full[..7]);
    assert!(r.try_get_bytes_with_length().is_none());
    assert_eq!(r.position(), 0);
}

#[test]
fn try_get_string_array_missing_entries_returns_none() {
    let mut w = NetDataWriter::new();
    w.put_ushort(3);
    w.put_string("only-one").unwrap();
    let mut r = reader_over(&w);
    assert!(r.try_get_string_array().is_none());
}

#[test]
fn try_get_on_sufficient_data_returns_values() {
    let mut w = NetDataWriter::new();
    w.put_byte(9);
    w.put_sbyte(-9);
    w.put_bool(true);
    w.put_char('q');
    w.put_short(-1000);
    w.put_ushort(1000);
    w.put_int(-100000);
    w.put_uint(100000);
    w.put_long(-10_000_000_000);
    w.put_ulong(10_000_000_000);
    w.put_float(1.25);
    w.put_double(-1.25);
    w.put_string("str").unwrap();
    w.put_array_string(&["a".to_string(), "b".to_string()]).unwrap();
    w.put_bytes_with_length(&[4, 5]).unwrap();

    let mut r = reader_over(&w);
    assert_eq!(r.try_get_byte(), Some(9));
    assert_eq!(r.try_get_sbyte(), Some(-9));
    assert_eq!(r.try_get_bool(), Some(true));
    assert_eq!(r.try_get_char(), Some('q'));
    assert_eq!(r.try_get_short(), Some(-1000));
    assert_eq!(r.try_get_ushort(), Some(1000));
    assert_eq!(r.try_get_int(), Some(-100000));
    assert_eq!(r.try_get_uint(), Some(100000));
    assert_eq!(r.try_get_long(), Some(-10_000_000_000));
    assert_eq!(r.try_get_ulong(), Some(10_000_000_000));
    assert_eq!(r.try_get_float(), Some(1.25));
    assert_eq!(r.try_get_double(), Some(-1.25));
    assert_eq!(r.try_get_string().as_deref(), Some("str"));
    assert_eq!(r.try_get_string_array(), Some(vec!["a".to_string(), "b".to_string()]));
    assert_eq!(r.try_get_bytes_with_length(), Some(vec![4, 5]));
    assert!(r.end_of_data());
}

#[test]
fn peek_does_not_advance_and_matches_get() {
    let mut w = NetDataWriter::new();
    w.put_byte(200);
    let mut r1 = reader_over(&w);
    assert_eq!(r1.peek_byte().unwrap(), 200);
    assert_eq!(r1.peek_sbyte().unwrap(), 200u8 as i8);
    assert!(!r1.peek_bool().unwrap());
    assert_eq!(r1.position(), 0);
    assert_eq!(r1.get_byte().unwrap(), 200);

    let mut w2 = NetDataWriter::new();
    w2.put_ushort(u16::from(b'x'));
    let r2 = reader_over(&w2);
    assert_eq!(r2.peek_char().unwrap(), 'x');
    assert_eq!(r2.peek_ushort().unwrap(), u16::from(b'x'));
    assert_eq!(r2.peek_short().unwrap(), i16::from(b'x'));
    assert_eq!(r2.position(), 0);

    let mut w3 = NetDataWriter::new();
    w3.put_long(-1234567890123456789);
    let mut r3 = reader_over(&w3);
    assert_eq!(r3.peek_long().unwrap(), -1234567890123456789);
    assert_eq!(r3.peek_ulong().unwrap(), (-1234567890123456789i64) as u64);
    assert_eq!(r3.position(), 0);
    assert_eq!(r3.get_long().unwrap(), -1234567890123456789);

    let mut w4 = NetDataWriter::new();
    w4.put_int(-123456789);
    let r4 = reader_over(&w4);
    assert_eq!(r4.peek_int().unwrap(), -123456789);
    assert_eq!(r4.peek_uint().unwrap(), (-123456789i32) as u32);
    assert_eq!(r4.peek_float().unwrap().to_bits(), f32::from_bits((-123456789i32) as u32).to_bits());
    assert_eq!(r4.position(), 0);

    let mut w5 = NetDataWriter::new();
    w5.put_double(3.5);
    let r5 = reader_over(&w5);
    assert_eq!(r5.peek_double().unwrap(), 3.5);
    assert_eq!(r5.position(), 0);

    let mut w6 = NetDataWriter::new();
    w6.put_string("peeked").unwrap();
    let mut r6 = reader_over(&w6);
    assert_eq!(r6.peek_string(), "peeked");
    assert_eq!(r6.peek_string_max(0).unwrap_or_default(), "peeked");
    assert_eq!(r6.peek_string_max(10).unwrap_or_default(), "peeked");
    assert_eq!(r6.peek_string_max(3).unwrap_or_default(), "");
    assert_eq!(r6.position(), 0);
    assert_eq!(r6.get_string().unwrap(), "peeked");
}

#[test]
fn peek_string_on_malformed_buffers_returns_empty() {
    assert_eq!(NetDataReader::from_slice(&[0x07]).peek_string(), "");
    // Prefix claims 4 content bytes that are not present.
    assert_eq!(NetDataReader::from_slice(&[0x05, 0x00]).peek_string(), "");
    assert_eq!(NetDataReader::from_slice(&[0x00, 0x00]).peek_string(), "");
}

#[test]
fn position_accounting_mixed_reads_skip_bytes_set_position() {
    let mut w = NetDataWriter::new();
    w.put_bool(true);
    w.put_short(7);
    w.put_int(11);
    w.put_long(13);
    w.put_float(1.5);
    let data = w.copy_data();
    assert_eq!(data.len(), 1 + 2 + 4 + 8 + 4);

    let mut r = NetDataReader::new(data.clone());
    assert_eq!(r.raw_data(), &data[..]);
    assert!(!r.is_null());
    assert_eq!(r.raw_data_size(), 19);
    assert_eq!(r.user_data_offset(), 0);
    assert_eq!(r.user_data_size(), 19);

    assert!(r.get_bool().unwrap());
    assert_eq!(r.position(), 1);
    assert_eq!(r.available_bytes(), 18);
    assert_eq!(r.get_short().unwrap(), 7);
    assert_eq!(r.position(), 3);
    assert_eq!(r.get_int().unwrap(), 11);
    assert_eq!(r.position(), 7);
    assert_eq!(r.available_bytes(), 12);
    r.skip_bytes(8);
    assert_eq!(r.position(), 15);
    assert_eq!(r.get_float().unwrap(), 1.5);
    assert!(r.end_of_data());
    assert_eq!(r.available_bytes(), 0);

    r.set_position(3);
    assert_eq!(r.get_int().unwrap(), 11);
    assert_eq!(r.get_long().unwrap(), 13);
}

#[test]
fn with_offset_window_reports_user_data_and_reads_slice() {
    let mut payload = NetDataWriter::new();
    payload.put_int(0x61626364);
    payload.put_ushort(0x9876);
    let body = payload.copy_data();

    const PREFIX: usize = 4;
    const SUFFIX: usize = 3;
    let mut full = vec![0xEEu8; PREFIX + body.len() + SUFFIX];
    full[PREFIX..PREFIX + body.len()].copy_from_slice(&body);

    let mut r = NetDataReader::with_offset(full, PREFIX, PREFIX + body.len());
    assert_eq!(r.position(), PREFIX);
    assert_eq!(r.user_data_offset(), PREFIX);
    assert_eq!(r.user_data_size(), body.len());
    assert_eq!(r.raw_data_size(), PREFIX + body.len());
    assert_eq!(r.available_bytes(), body.len());
    assert_eq!(r.get_int().unwrap(), 0x61626364);
    assert_eq!(r.get_ushort().unwrap(), 0x9876);
    assert!(r.end_of_data());
}

#[test]
fn set_source_reuse_resets_state_and_clear_nulls_reader() {
    let mut r = NetDataReader::from_slice(&[1, 0, 0, 0]);
    assert_eq!(r.get_int().unwrap(), 1);
    assert!(r.end_of_data());

    r.set_source(vec![2u8, 0, 0, 0, 9].into());
    assert_eq!(r.position(), 0);
    assert_eq!(r.user_data_offset(), 0);
    assert_eq!(r.available_bytes(), 5);
    assert_eq!(r.get_int().unwrap(), 2);
    assert_eq!(r.get_byte().unwrap(), 9);

    r.clear();
    assert!(r.is_null());
    assert_eq!(r.position(), 0);
    assert_eq!(r.raw_data_size(), 0);
    assert!(r.end_of_data());
}

fn make_string(i: usize) -> String {
    let length = i % 40;
    (0..length).map(|j| if j % 2 == 0 { (b'A' + ((i + j) % 26) as u8) as char } else { char::from_u32(0x4E00 + ((i + j) % 512) as u32).unwrap() }).collect()
}

fn make_bytes(i: usize) -> Vec<u8> {
    (0..i % 33).map(|j| ((i + j * 7) & 0xFF) as u8).collect()
}

#[test]
fn writer_growth_large_deterministic_sequence_reads_back_intact() {
    let mut w = NetDataWriter::new();
    let initial_capacity = w.capacity();

    const COUNT: usize = 500;
    for i in 0..COUNT {
        match i % 7 {
            0 => w.put_int((i as u32).wrapping_mul(2654435761) as i32),
            1 => w.put_double(i as f64 * 1.618033988749895 - 500.0),
            2 => w.put_string(&make_string(i)).unwrap(),
            3 => w.put_ulong(((i as u64) << 32) | 0xDEADBEEF),
            4 => w.put_bytes_with_length(&make_bytes(i)).unwrap(),
            5 => w.put_short((i as i32 * 31 - 16000) as i16),
            _ => w.put_bool(i % 3 == 0),
        }
    }

    assert!(w.length() > initial_capacity, "sequence should have outgrown the initial capacity");
    assert!(w.capacity() >= w.length());

    let mut r = reader_over(&w);
    for i in 0..COUNT {
        match i % 7 {
            0 => assert_eq!(r.get_int().unwrap(), (i as u32).wrapping_mul(2654435761) as i32),
            1 => assert_eq!(r.get_double().unwrap(), i as f64 * 1.618033988749895 - 500.0),
            2 => assert_eq!(r.get_string().unwrap(), make_string(i)),
            3 => assert_eq!(r.get_ulong().unwrap(), ((i as u64) << 32) | 0xDEADBEEF),
            4 => assert_eq!(r.get_bytes_with_length().unwrap(), make_bytes(i)),
            5 => assert_eq!(r.get_short().unwrap(), (i as i32 * 31 - 16000) as i16),
            _ => assert_eq!(r.get_bool().unwrap(), i % 3 == 0),
        }
    }
    assert!(r.end_of_data());
}

#[test]
fn writer_reset_keeps_capacity_and_allows_reuse() {
    let mut w = NetDataWriter::new();
    w.put_int(1234);
    w.put_string("payload").unwrap();
    let capacity_after_writes = w.capacity();
    assert!(w.length() > 0);

    w.reset();
    assert_eq!(w.length(), 0);
    assert_eq!(w.capacity(), capacity_after_writes);

    w.put_byte(77);
    let mut r = reader_over(&w);
    assert_eq!(r.get_byte().unwrap(), 77);
    assert!(r.end_of_data());

    w.reset_with_size(4096);
    assert_eq!(w.length(), 0);
    assert!(w.capacity() >= 4096);
}

#[test]
fn writer_set_position_rewrites_earlier_bytes() {
    let mut w = NetDataWriter::new();
    let header_pos = w.length();
    w.put_int(0);
    w.put_string("body").unwrap();
    let end = w.set_position(header_pos);
    assert_eq!(w.length(), header_pos);
    w.put_int(end as i32);
    let back = w.set_position(end);
    assert_eq!(back, header_pos + 4);
    assert_eq!(w.length(), end);

    let mut r = reader_over(&w);
    assert_eq!(r.get_int().unwrap(), end as i32);
    assert_eq!(r.get_string().unwrap(), "body");
    assert!(r.end_of_data());
}

#[test]
fn writer_ensure_fit_and_resize_if_need_grow_capacity() {
    let mut w = NetDataWriter::new();
    w.ensure_fit(1000);
    assert!(w.capacity() >= 1000);
    w.resize_if_need(5000);
    assert!(w.capacity() >= 5000);
    assert_eq!(w.length(), 0);
}

#[test]
fn writer_from_bytes_from_string_copy_semantics() {
    let source = vec![10u8, 20, 30, 40, 50];

    let borrowed = NetDataWriter::from_bytes(source.clone(), false);
    assert_eq!(borrowed.as_read_only_span(), &source[..]);
    assert_eq!(borrowed.length(), source.len());

    let copied = NetDataWriter::from_bytes(source.clone(), true);
    assert_eq!(copied.copy_data(), source);

    let sliced = NetDataWriter::from_slice(&source[1..4]);
    assert_eq!(sliced.copy_data(), vec![20, 30, 40]);

    let from_string = NetDataWriter::from_string("via string").unwrap();
    assert_eq!(NetDataReader::new(from_string.copy_data()).get_string().unwrap(), "via string");
}

#[test]
fn writer_as_read_only_span_matches_copy_data() {
    let mut w = NetDataWriter::new();
    w.put_int(0x0A0B0C0D);
    w.put_byte(0xFE);
    assert_eq!(w.as_read_only_span(), &w.copy_data()[..]);
    assert_eq!(w.as_read_only_span().len(), w.length());
}

#[test]
fn raw_bytes_put_and_get_bytes_with_offsets() {
    let payload = [9u8, 8, 7, 6, 5, 4];
    let mut w = NetDataWriter::new();
    w.put_bytes(&payload);
    w.put_bytes_range(&payload, 2, 3).unwrap();
    w.put_bytes(&payload[1..3]);

    let mut r = reader_over(&w);
    let mut first = [0u8; 6];
    r.get_bytes(&mut first, payload.len()).unwrap();
    assert_eq!(first, payload);

    let mut second = [0u8; 5];
    r.get_bytes_into(&mut second, 1, 3).unwrap();
    assert_eq!(second, [0, 7, 6, 5, 0]);

    let mut third = [0u8; 2];
    r.get_bytes_into(&mut third, 0, 2).unwrap();
    assert_eq!(third, [8, 7]);
    assert!(r.end_of_data());
}

#[test]
fn segments_spans_and_remaining_bytes_behave_consistently() {
    let mut w = NetDataWriter::new();
    for b in 1..=4u8 {
        w.put_byte(b);
    }
    let mut r = reader_over(&w);

    let empty = r.get_bytes_segment(0).unwrap();
    assert!(empty.is_empty());
    assert_eq!(r.position(), 0);

    let seg = r.get_bytes_segment(2).unwrap();
    assert_eq!(&seg[..], &[1, 2]);
    assert_eq!(r.position(), 2);

    let span = r.get_remaining_bytes_span();
    assert_eq!(span.len(), 2);
    assert_eq!(span, &[3, 4]);
    assert_eq!(r.position(), 2);

    let remaining = r.get_remaining_bytes();
    assert_eq!(remaining, vec![3, 4]);
    assert!(r.end_of_data());
    assert_eq!(r.available_bytes(), 0);

    let mut r2 = reader_over(&w);
    r2.get_byte().unwrap();
    let rest = r2.get_remaining_bytes_segment();
    assert_eq!(&rest[..], &[2, 3, 4]);
    assert!(r2.end_of_data());
}

#[test]
fn overclaimed_lengths_are_errors() {
    let mut array_claim = NetDataWriter::new();
    array_claim.put_ushort(100);
    array_claim.put_int(0);
    assert!(NetDataReader::new(array_claim.copy_data()).get_int_array().is_err());

    let mut string_claim = NetDataWriter::new();
    string_claim.put_ushort(50);
    string_claim.put_byte(65);
    assert!(NetDataReader::new(string_claim.copy_data()).get_string().is_err());
    assert!(NetDataReader::new(string_claim.copy_data()).get_string_max(1000).is_err());

    let mut large_claim = NetDataWriter::new();
    large_claim.put_int(100);
    large_claim.put_byte(65);
    assert!(NetDataReader::new(large_claim.copy_data()).get_large_string().is_err());

    assert!(NetDataReader::from_slice(&[0u8; 8]).get_guid().is_err());
    assert!(NetDataReader::from_slice(&[0u8; 4]).get_bytes_segment(5).is_err());
    assert!(NetDataReader::from_slice(&[0u8; 4]).get_bytes(&mut [0u8; 8], 8).is_err());
    assert!(NetDataReader::from_slice(&[0u8; 4]).get_bytes_into(&mut [0u8; 8], 0, 8).is_err());
    // A destination that cannot hold the request is refused before anything is read.
    let mut r = NetDataReader::from_slice(&[1, 2, 3, 4]);
    assert!(r.get_bytes_into(&mut [0u8; 2], 1, 3).is_err());
    assert_eq!(r.position(), 0);
}

/// Every reader failure names what it was reading and where, so a malformed packet can be traced
/// to the field that broke.
#[test]
fn reader_errors_carry_the_field_and_the_position() {
    let mut r = NetDataReader::from_slice(&[0x01]);
    let err = r.get_int().unwrap_err();
    let text = err.to_string();
    assert!(text.contains("int") || text.contains("i32"), "{text}");
    assert!(text.contains('1') || text.contains("position"), "{text}");
    assert_eq!(r.position(), 0, "a failed read does not consume");
}
