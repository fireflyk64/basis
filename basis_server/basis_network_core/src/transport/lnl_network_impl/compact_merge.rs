//! Port of `LiteNetLib/CompactMerge.cs`: entry framing for `CompactMerged` datagrams.
//!
//! Bit 7 selects the extended 16-bit length form, bit 6 marks an opaque raw Ack/Channeled
//! packet, and bits 0-5 carry the application channel for compact unreliable entries. LiteNetLib
//! caps configured application channels at 64, so all valid channels fit in six bits.

use super::net_constants::NetConstants;
use super::net_packet::PacketProperty;

/// One decoded entry header; the payload starts at the offset `try_read_entry` left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompactEntry {
    pub is_raw_packet: bool,
    pub channel: u8,
    pub payload_length: usize,
}

pub struct CompactMerge;

impl CompactMerge {
    pub const LONG_LENGTH_FLAG: u8 = 0x80;
    pub const RAW_PACKET_FLAG: u8 = 0x40;
    pub const CHANNEL_MASK: u8 = 0x3F;
    pub const MAX_SHORT_LENGTH: usize = u8::MAX as usize;
    pub const SHORT_ENTRY_OVERHEAD: usize = 2;
    pub const LONG_ENTRY_OVERHEAD: usize = 3;

    pub fn entry_overhead(payload_length: usize) -> usize {
        if payload_length > Self::MAX_SHORT_LENGTH { Self::LONG_ENTRY_OVERHEAD } else { Self::SHORT_ENTRY_OVERHEAD }
    }

    pub fn entry_size(payload_length: usize) -> usize {
        payload_length + Self::entry_overhead(payload_length)
    }

    pub fn can_carry_channel(channel: u8) -> bool {
        channel <= Self::CHANNEL_MASK
    }

    fn write_header(destination: &mut [u8], offset: usize, tag: u8, payload_length: usize) -> usize {
        if payload_length > Self::MAX_SHORT_LENGTH {
            destination[offset] = tag | Self::LONG_LENGTH_FLAG;
            let length = u16::try_from(payload_length).unwrap_or(u16::MAX).to_le_bytes();
            destination[offset + 1] = length[0];
            destination[offset + 2] = length[1];
            return Self::LONG_ENTRY_OVERHEAD;
        }
        destination[offset] = tag;
        destination[offset + 1] = u8::try_from(payload_length).unwrap_or(u8::MAX);
        Self::SHORT_ENTRY_OVERHEAD
    }

    /// Writes one compact unreliable entry and returns the bytes written. The destination must
    /// have `entry_size(payload.len())` bytes from `offset`.
    pub fn write_unreliable_entry(destination: &mut [u8], offset: usize, channel: u8, payload: &[u8]) -> usize {
        let overhead = Self::write_header(destination, offset, channel, payload.len());
        destination[offset + overhead..offset + overhead + payload.len()].copy_from_slice(payload);
        overhead + payload.len()
    }

    /// Writes one complete raw Ack/Channeled packet and returns the bytes written.
    pub fn write_raw_entry(destination: &mut [u8], offset: usize, packet: &[u8]) -> usize {
        let overhead = Self::write_header(destination, offset, Self::RAW_PACKET_FLAG, packet.len());
        destination[offset + overhead..offset + overhead + packet.len()].copy_from_slice(packet);
        overhead + packet.len()
    }

    /// Reads the entry header at `offset`, leaving it on the first payload byte. Raw entries are
    /// accepted only for Ack/Channeled packets, preventing recursive CompactMerged nesting and
    /// keeping the raw escape canonical. `None` for anything ragged — the bytes are the remote
    /// side's, so a truncated or non-canonical entry is refused rather than read past.
    pub fn try_read_entry(source: &[u8], size: usize, offset: &mut usize) -> Option<CompactEntry> {
        if size > source.len() || *offset > size || size - *offset < Self::SHORT_ENTRY_OVERHEAD {
            return None;
        }
        let mut pos = *offset;
        let tag = source[pos];
        pos += 1;
        let is_raw_packet = tag & Self::RAW_PACKET_FLAG != 0;
        let channel = tag & Self::CHANNEL_MASK;

        // Raw entries have no application-channel field. Non-zero low bits would create
        // alternate encodings of the same packet, so reject them.
        if is_raw_packet && channel != 0 {
            return None;
        }

        let payload_length = if tag & Self::LONG_LENGTH_FLAG != 0 {
            if size - pos < 2 {
                return None;
            }
            let length = usize::from(u16::from_le_bytes([source[pos], source[pos + 1]]));
            pos += 2;
            // Keep one canonical encoding: <=255 always uses the short form.
            if length <= Self::MAX_SHORT_LENGTH {
                return None;
            }
            length
        } else {
            let length = usize::from(source[pos]);
            pos += 1;
            length
        };

        if payload_length > size - pos {
            return None;
        }

        if is_raw_packet {
            if !(NetConstants::CHANNELED_HEADER_SIZE..=NetConstants::MAX_PACKET_SIZE).contains(&payload_length) {
                return None;
            }
            match PacketProperty::from_byte(source[pos] & 0x1F) {
                Some(PacketProperty::Ack) | Some(PacketProperty::Channeled) => {}
                _ => return None,
            }
        } else if payload_length + NetConstants::UNRELIABLE_HEADER_SIZE > NetConstants::MAX_PACKET_SIZE {
            return None;
        }

        *offset = pos;
        Some(CompactEntry { is_raw_packet, channel, payload_length })
    }
}

#[cfg(test)]
mod tests {
    //! Port of `CompactMergeFramingTests.cs`.
    use super::*;

    const LEGACY_ENTRY_OVERHEAD: usize = 4;

    fn payload(length: usize) -> Vec<u8> {
        (0..length).map(|i| (i * 7 + 3) as u8).collect()
    }

    #[test]
    fn entry_overhead_is_two_bytes_up_to_255_three_above() {
        for (length, expected) in [(0, 2), (1, 2), (200, 2), (255, 2), (256, 3), (1200, 3)] {
            assert_eq!(CompactMerge::entry_overhead(length), expected);
            assert_eq!(CompactMerge::entry_size(length), length + expected);
        }
    }

    #[test]
    fn entry_overhead_beats_legacy_framing_by_half_then_a_quarter() {
        assert_eq!(CompactMerge::entry_overhead(200), LEGACY_ENTRY_OVERHEAD / 2);
        assert_eq!(CompactMerge::entry_overhead(900), LEGACY_ENTRY_OVERHEAD - 1);
    }

    #[test]
    fn can_carry_channel_stops_at_the_bit_the_length_flag_owns() {
        assert!(CompactMerge::can_carry_channel(0));
        assert!(CompactMerge::can_carry_channel(63));
        assert!(CompactMerge::can_carry_channel(CompactMerge::CHANNEL_MASK));
        assert!(!CompactMerge::can_carry_channel(128));
        assert!(!CompactMerge::can_carry_channel(255));
    }

    #[test]
    fn write_entry_round_trips_through_try_read_entry() {
        for (channel, length) in [(0u8, 0usize), (0, 1), (63, 32), (62, 255), (5, 256), (1, 1200)] {
            let data = payload(length);
            let mut buffer = vec![0u8; CompactMerge::entry_size(length) + 8];
            let written = CompactMerge::write_unreliable_entry(&mut buffer, 0, channel, &data);
            assert_eq!(written, CompactMerge::entry_size(length));
            let mut offset = 0;
            let entry = CompactMerge::try_read_entry(&buffer, written, &mut offset).unwrap();
            assert!(!entry.is_raw_packet);
            assert_eq!(entry.channel, channel);
            assert_eq!(entry.payload_length, length);
            assert_eq!(&buffer[offset..offset + length], &data[..]);
        }
    }

    #[test]
    fn write_entry_packs_entries_back_to_back() {
        let lengths = [1usize, 300, 0, 40];
        let channels = [0u8, 7, 63, 2];
        let mut buffer = vec![0u8; 4096];
        let mut written = 0;
        for i in 0..lengths.len() {
            written += CompactMerge::write_unreliable_entry(&mut buffer, written, channels[i], &payload(lengths[i]));
        }
        let mut offset = 0;
        for i in 0..lengths.len() {
            let entry = CompactMerge::try_read_entry(&buffer, written, &mut offset).unwrap();
            assert_eq!(entry.channel, channels[i]);
            assert_eq!(entry.payload_length, lengths[i]);
            assert_eq!(&buffer[offset..offset + lengths[i]], &payload(lengths[i])[..]);
            offset += lengths[i];
        }
        assert_eq!(written, offset);
    }

    #[test]
    fn try_read_entry_rejects_truncated_entries() {
        let mut buffer = vec![0u8; 512];
        let written = CompactMerge::write_unreliable_entry(&mut buffer, 0, 9, &payload(300));
        // Every prefix short of the whole entry has to be refused rather than read past.
        for size in 0..written {
            let mut offset = 0;
            assert!(CompactMerge::try_read_entry(&buffer, size, &mut offset).is_none(), "prefix {size} was accepted");
            assert_eq!(offset, 0);
        }
        let mut whole = 0;
        assert_eq!(CompactMerge::try_read_entry(&buffer, written, &mut whole).unwrap().payload_length, 300);
    }

    #[test]
    fn raw_entries_are_only_ack_or_channeled_with_zero_channel_bits() {
        let mut buffer = vec![0u8; 64];
        let mut packet = vec![0u8; 8];
        packet[0] = PacketProperty::Channeled as u8;
        let written = CompactMerge::write_raw_entry(&mut buffer, 0, &packet);
        assert_eq!(written, 10);
        let mut offset = 0;
        let entry = CompactMerge::try_read_entry(&buffer, written, &mut offset).unwrap();
        assert!(entry.is_raw_packet);
        assert_eq!(entry.payload_length, 8);
        // Nonzero channel bits on a raw tag are an alternate encoding: refused.
        buffer[0] |= 1;
        assert!(CompactMerge::try_read_entry(&buffer, written, &mut 0).is_none());
        buffer[0] = CompactMerge::RAW_PACKET_FLAG;
        // A raw entry that is not Ack/Channeled (an Unreliable, a nested CompactMerged) is refused.
        buffer[2] = PacketProperty::Unreliable as u8;
        assert!(CompactMerge::try_read_entry(&buffer, written, &mut 0).is_none());
        buffer[2] = PacketProperty::CompactMerged as u8;
        assert!(CompactMerge::try_read_entry(&buffer, written, &mut 0).is_none());
        // Shorter than a channeled header cannot be a packet.
        let mut short = [CompactMerge::RAW_PACKET_FLAG, 3, PacketProperty::Ack as u8, 0, 0];
        assert!(CompactMerge::try_read_entry(&short, short.len(), &mut 0).is_none());
        short[1] = 3;
        assert!(CompactMerge::try_read_entry(&short, short.len(), &mut 0).is_none());
    }

    #[test]
    fn non_canonical_long_form_and_oversized_claims_are_refused() {
        // Long form claiming a length that fits the short form.
        let body = [CompactMerge::LONG_LENGTH_FLAG | 5, 1, 0, 0x10];
        assert!(CompactMerge::try_read_entry(&body, body.len(), &mut 0).is_none());
        // Long form claiming more than fits in a datagram at all.
        let mut huge = vec![0u8; 3 + 1500];
        huge[0] = CompactMerge::LONG_LENGTH_FLAG;
        huge[1] = 0xDC;
        huge[2] = 0x05; // 1500
        assert!(CompactMerge::try_read_entry(&huge, huge.len(), &mut 0).is_none());
        // Short form claiming 200 bytes with 4 present.
        let body = [3, 200, 1, 2, 3, 4];
        assert!(CompactMerge::try_read_entry(&body, body.len(), &mut 0).is_none());
        // Long-form flag set with the length field itself cut short.
        let body = [3 | CompactMerge::LONG_LENGTH_FLAG, 0x10];
        assert!(CompactMerge::try_read_entry(&body, body.len(), &mut 0).is_none());
    }
}
