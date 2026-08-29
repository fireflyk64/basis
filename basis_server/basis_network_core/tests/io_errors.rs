//! Negative tests for the wire reader and writer: every short read, oversized length prefix
//! and bad range must come back as a `NetDataError` — never a panic — and must say where it
//! was detected.

use basis_error::{BasisError, ErrorCode, FaultKind};
use basis_network_core::io::net_data_reader::{NetDataError, NetDataErrorKind, NetDataReader, NetResultExt};
use basis_network_core::io::net_data_writer::NetDataWriter;

fn kind(err: &NetDataError) -> &NetDataErrorKind {
    err.kind()
}

#[test]
fn empty_reader_reports_a_short_read_with_its_position() {
    let mut reader = NetDataReader::from_slice(&[]);
    let err = reader.get_byte().unwrap_err();
    assert_eq!(kind(&err), &NetDataErrorKind::ShortRead { what: "byte", wanted: 1, position: 0, data_size: 0 });
    assert!(err.location().file().ends_with("net_data_reader.rs"), "{}", err.location().file());
    assert!(err.location().line() > 0);
    assert!(err.to_string().contains("Not enough data to read 1 byte(s) for byte"));
}

#[test]
fn every_scalar_getter_fails_cleanly_on_one_byte() {
    let mut reader = NetDataReader::from_slice(&[7]);
    assert!(reader.get_ushort().is_err());
    assert!(reader.get_short().is_err());
    assert!(reader.get_int().is_err());
    assert!(reader.get_uint().is_err());
    assert!(reader.get_long().is_err());
    assert!(reader.get_ulong().is_err());
    assert!(reader.get_float().is_err());
    assert!(reader.get_double().is_err());
    assert!(reader.get_guid().is_err());
    assert!(reader.get_char().is_err());
    // A failed read does not move the cursor past the data.
    assert_eq!(reader.position(), 0);
    assert_eq!(reader.get_byte().unwrap(), 7);
    assert!(reader.end_of_data());
    assert!(reader.get_byte().is_err());
}

#[test]
fn peek_never_moves_the_cursor_and_fails_on_short_data() {
    let reader = NetDataReader::from_slice(&[1, 2, 3]);
    assert_eq!(reader.peek_ushort().unwrap(), 0x0201);
    assert!(reader.peek_int().is_err());
    assert!(reader.peek_long().is_err());
    assert_eq!(reader.position(), 0);
}

#[test]
fn length_prefix_beyond_available_data_is_refused_before_allocating() {
    // Claims 10 bytes, carries 3.
    let mut reader = NetDataReader::from_slice(&[10, 0, 1, 2, 3]);
    let err = reader.get_bytes_with_length().unwrap_err();
    assert_eq!(kind(&err), &NetDataErrorKind::LengthExceedsData { what: "byte array", length: 10, available: 3 });

    // A ushort array claiming 65535 entries with 4 bytes behind it.
    let mut reader = NetDataReader::from_slice(&[0xFF, 0xFF, 1, 2, 3, 4]);
    let err = reader.get_ushort_array().unwrap_err();
    assert!(matches!(kind(&err), NetDataErrorKind::LengthExceedsData { what: "ushort array", length: 131070, .. }));

    // A string array claiming 65535 strings with 2 bytes behind it.
    let mut reader = NetDataReader::from_slice(&[0xFF, 0xFF, 0, 0]);
    assert!(reader.get_string_array().is_err());

    // Every typed array getter refuses the same way.
    for getter in [
        (|r: &mut NetDataReader| r.get_int_array().map(|_| ())) as fn(&mut NetDataReader) -> Result<(), NetDataError>,
        |r| r.get_uint_array().map(|_| ()),
        |r| r.get_float_array().map(|_| ()),
        |r| r.get_double_array().map(|_| ()),
        |r| r.get_long_array().map(|_| ()),
        |r| r.get_ulong_array().map(|_| ()),
        |r| r.get_short_array().map(|_| ()),
        |r| r.get_bool_array().map(|_| ()),
        |r| r.get_sbytes_with_length().map(|_| ()),
    ] {
        let mut reader = NetDataReader::from_slice(&[0xFF, 0xFF, 1]);
        assert!(getter(&mut reader).is_err());
    }
}

#[test]
fn strings_with_bad_prefixes_are_errors_not_panics() {
    // Prefix says 100 chars, 3 bytes follow.
    let mut reader = NetDataReader::from_slice(&[101, 0, b'a', b'b', b'c']);
    let err = reader.get_string().unwrap_err();
    assert!(matches!(kind(&err), NetDataErrorKind::LengthExceedsData { what: "string", length: 100, available: 3 }));

    // Large string with a negative length reads as empty (the C# behaviour) ...
    let mut reader = NetDataReader::from_slice(&(-5i32).to_le_bytes());
    assert_eq!(reader.get_large_string().unwrap(), "");
    // ... and one longer than the data is an error.
    let mut reader = NetDataReader::from_slice(&[0x10, 0, 0, 0, b'x']);
    assert!(reader.get_large_string().is_err());

    // peek_string answers empty for garbage instead of faulting.
    let reader = NetDataReader::from_slice(&[0xFF, 0xFF, 1]);
    assert_eq!(reader.peek_string(), "");

    // try_get_string leaves the cursor alone when the string is truncated.
    let mut reader = NetDataReader::from_slice(&[9, 0, b'a']);
    assert_eq!(reader.try_get_string(), None);
    assert_eq!(reader.position(), 0);

    // A string over the character limit is returned empty but consumed.
    let mut writer = NetDataWriter::new();
    writer.put_string("hello").unwrap();
    writer.put_byte(9);
    let mut reader = NetDataReader::new(writer.copy_data());
    assert_eq!(reader.get_string_max(3).unwrap(), "");
    assert_eq!(reader.get_byte().unwrap(), 9);
}

#[test]
fn copying_into_a_small_destination_is_refused_without_moving() {
    let mut reader = NetDataReader::from_slice(&[1, 2, 3, 4]);
    let mut dest = [0u8; 2];
    let err = reader.get_bytes_into(&mut dest, 1, 2).unwrap_err();
    assert_eq!(kind(&err), &NetDataErrorKind::DestinationTooSmall { start: 1, count: 2, len: 2 });
    assert_eq!(reader.position(), 0);
    assert!(reader.get_bytes(&mut dest, 2).is_ok());
    assert_eq!(dest, [1, 2]);
    assert!(reader.get_bytes(&mut dest, 3).is_err());
}

#[test]
fn cursor_movement_is_clamped_to_the_data() {
    let mut reader = NetDataReader::from_slice(&[1, 2, 3]);
    reader.skip_bytes(100);
    assert_eq!(reader.available_bytes(), 0);
    assert!(reader.end_of_data());
    assert_eq!(reader.get_remaining_bytes_span(), &[]);
    reader.set_position(1);
    assert_eq!(reader.get_remaining_bytes(), vec![2, 3]);
    reader.set_position(50);
    assert_eq!(reader.get_remaining_bytes_segment().len(), 0);

    let reader = NetDataReader::with_offset(vec![1, 2, 3], 10, 99);
    assert_eq!(reader.available_bytes(), 0);
    assert_eq!(reader.raw_data_size(), 3);
    let mut reader = NetDataReader::with_offset(vec![1, 2, 3], 1, 2);
    assert_eq!(reader.get_byte().unwrap(), 2);
    assert!(reader.get_byte().is_err());
}

#[test]
fn zero_copy_segments_check_their_length() {
    let mut reader = NetDataReader::from_slice(&[1, 2, 3]);
    assert!(reader.get_bytes_segment(4).is_err());
    assert_eq!(reader.get_bytes_segment(2).unwrap().as_ref(), &[1, 2]);
    assert_eq!(reader.get_bytes_vec(1).unwrap(), vec![3]);
    assert!(reader.get_bytes_vec(1).is_err());
}

#[test]
fn field_names_travel_with_the_error() {
    let mut reader = NetDataReader::from_slice(&[]);
    let err = reader.get_ushort().field("playerID").unwrap_err();
    assert_eq!(err.field(), Some("playerID"));
    assert!(err.to_string().starts_with("playerID: "));
}

#[test]
fn equality_ignores_where_the_error_was_raised() {
    let a = NetDataError::short_read("byte", 1, 0, 0);
    let b = NetDataError::short_read("byte", 1, 0, 0);
    assert_ne!(a.location().line(), b.location().line());
    assert_eq!(a, b);
    assert_ne!(a, NetDataError::short_read("byte", 2, 0, 0));
}

#[test]
fn a_wire_error_becomes_a_permanent_protocol_fault_with_a_trace() {
    let mut reader = NetDataReader::from_slice(&[1]);
    let err = reader.get_int().unwrap_err();
    let basis: BasisError = err.into(); // this line is the second frame
    assert_eq!(basis.kind(), FaultKind::Permanent);
    assert_eq!(basis.code(), ErrorCode::Protocol);
    assert_eq!(basis.frames().len(), 2);
    assert!(basis.origin().file().ends_with("net_data_reader.rs"));
    assert!(basis.frames()[1].file().ends_with("io_errors.rs"));
    assert!(basis.find_source::<NetDataError>().is_some());
    let report = basis.report();
    assert!(report.contains("[permanent protocol]"), "{report}");
    assert!(report.contains("propagated at"), "{report}");
}

// ── Writer ───────────────────────────────────────────────────────────────────

#[test]
fn oversized_length_prefixed_values_are_refused_without_writing() {
    let mut writer = NetDataWriter::new();
    let big = vec![0u8; 70_000];
    let err = writer.put_bytes_with_length(&big).unwrap_err();
    assert_eq!(kind(&err), &NetDataErrorKind::TooLong { what: "byte array", length: 70_000, max: 65_535 });
    assert_eq!(writer.length(), 0);

    let big_string = "x".repeat(70_000);
    assert!(matches!(writer.put_string(&big_string).unwrap_err().kind(), NetDataErrorKind::TooLong { what: "string", .. }));
    assert_eq!(writer.length(), 0);

    let many = vec![1u16; 65_536];
    assert!(writer.put_array_ushort(&many).is_err());
    let many_strings = vec![String::new(); 65_536];
    assert!(writer.put_array_string(&many_strings).is_err());
    let many_bools = vec![true; 65_536];
    assert!(writer.put_array_bool(&many_bools).is_err());
    let many_sbytes = vec![-1i8; 65_536];
    assert!(writer.put_sbytes_with_length(&many_sbytes).is_err());
    assert_eq!(writer.length(), 0);

    // Exactly at the limit is fine.
    let limit = vec![0u8; 65_535];
    assert!(writer.put_bytes_with_length(&limit).is_ok());
    assert_eq!(writer.length(), 2 + 65_535);
}

#[test]
fn ranges_outside_the_source_are_refused() {
    let mut writer = NetDataWriter::new();
    let data = [1u8, 2, 3];
    let err = writer.put_bytes_range(&data, 2, 5).unwrap_err();
    assert_eq!(kind(&err), &NetDataErrorKind::RangeOutOfBounds { offset: 2, length: 5, len: 3 });
    assert!(writer.put_bytes_range(&data, usize::MAX, 1).is_err());
    assert_eq!(writer.length(), 0);
    assert!(writer.put_bytes_range(&data, 1, 2).is_ok());
    assert_eq!(writer.as_read_only_span(), &[2, 3]);
}

#[test]
fn the_writer_grows_instead_of_overflowing() {
    let mut writer = NetDataWriter::with_capacity(1);
    writer.put_long(-1);
    writer.put_bytes(&[0u8; 1000]);
    assert_eq!(writer.length(), 1008);
    assert!(writer.capacity() >= 1008);

    // Rewinding past the end grows the buffer so the invariant holds.
    let prev = writer.set_position(5000);
    assert_eq!(prev, 1008);
    writer.put_byte(1);
    assert_eq!(writer.length(), 5001);
    assert_eq!(writer.as_read_only_span().len(), 5001);
}

#[test]
fn string_truncation_and_char_handling_do_not_split_utf8() {
    let mut writer = NetDataWriter::new();
    writer.put_string_max("héllo", 2).unwrap();
    writer.put_char('😀');
    writer.put_char('A');
    let mut reader = NetDataReader::new(writer.copy_data());
    assert_eq!(reader.get_string().unwrap(), "hé");
    assert_eq!(reader.get_char().unwrap(), '\u{FFFD}');
    assert_eq!(reader.get_char().unwrap(), 'A');
}

#[test]
fn scalars_and_arrays_round_trip() {
    let mut writer = NetDataWriter::new();
    writer.put_byte(1);
    writer.put_sbyte(-2);
    writer.put_bool(true);
    writer.put_short(-300);
    writer.put_ushort(60_000);
    writer.put_int(-70_000);
    writer.put_uint(4_000_000_000);
    writer.put_long(i64::MIN);
    writer.put_ulong(u64::MAX);
    writer.put_float(1.5);
    writer.put_double(-2.25);
    writer.put_guid(&[9u8; 16]);
    writer.put_array_int(&[1, -1]).unwrap();
    writer.put_array_double(&[0.5]).unwrap();
    writer.put_array_string(&["a".to_string(), String::new()]).unwrap();
    writer.put_large_string("large").unwrap();
    writer.put_sbytes_with_length(&[-1, 1]).unwrap();

    let mut reader = NetDataReader::new(writer.copy_data());
    assert_eq!(reader.get_byte().unwrap(), 1);
    assert_eq!(reader.get_sbyte().unwrap(), -2);
    assert!(reader.get_bool().unwrap());
    assert_eq!(reader.get_short().unwrap(), -300);
    assert_eq!(reader.get_ushort().unwrap(), 60_000);
    assert_eq!(reader.get_int().unwrap(), -70_000);
    assert_eq!(reader.get_uint().unwrap(), 4_000_000_000);
    assert_eq!(reader.get_long().unwrap(), i64::MIN);
    assert_eq!(reader.get_ulong().unwrap(), u64::MAX);
    assert_eq!(reader.get_float().unwrap(), 1.5);
    assert_eq!(reader.get_double().unwrap(), -2.25);
    assert_eq!(reader.get_guid().unwrap(), [9u8; 16]);
    assert_eq!(reader.get_int_array().unwrap(), vec![1, -1]);
    assert_eq!(reader.get_double_array().unwrap(), vec![0.5]);
    assert_eq!(reader.get_string_array().unwrap(), vec!["a".to_string(), String::new()]);
    assert_eq!(reader.get_large_string().unwrap(), "large");
    assert_eq!(reader.get_sbytes_with_length().unwrap(), vec![-1, 1]);
    assert!(reader.end_of_data());
}
