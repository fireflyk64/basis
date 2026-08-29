use crate::io::{NetDataReader, NetDataWriter, NetResult};
use crate::BNL;

use super::identity::PlayerIdMessage;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioSegmentDataMessage {
    pub sequence_number: u8,
    pub total_played_in_silence: u8,
    /// Retained, grow-only buffer; consumers gate on `length_used`.
    pub buffer: Vec<u8>,
    pub total_length: usize,
    pub length_used: usize,
}

impl AudioSegmentDataMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.sequence_number = reader.get_byte()?;
        self.total_played_in_silence = reader.get_byte()?;
        if reader.end_of_data() {
            // Keep the retained buffer; consumers gate on length_used.
            self.total_length = 0;
            self.length_used = 0;
        } else {
            let available = reader.available_bytes();
            if self.buffer.len() < available {
                self.buffer.resize(available, 0);
            }
            reader.get_bytes(&mut self.buffer, available)?;
            self.total_length = available;
            self.length_used = available;
        }
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_byte(self.sequence_number);
        writer.put_byte(self.total_played_in_silence);
        if self.length_used != 0 {
            writer.put_bytes_range(&self.buffer, 0, self.length_used)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerAudioSegmentMessage {
    pub player_id_message: PlayerIdMessage,
    pub audio_segment_data: AudioSegmentDataMessage,
}

impl ServerAudioSegmentMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id_message.deserialize(reader)?;
        self.audio_segment_data.deserialize(reader)
    }

    pub fn deserialize_sized(&mut self, reader: &mut NetDataReader, large_id: bool) -> NetResult<()> {
        self.player_id_message.deserialize_sized(reader, large_id)?;
        self.audio_segment_data.deserialize(reader)
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        self.player_id_message.serialize(writer)?;
        self.audio_segment_data.serialize(writer)?;
        Ok(())
    }

    pub fn serialize_sized(&mut self, writer: &mut NetDataWriter, large_id: bool) -> NetResult<()> {
        self.player_id_message.serialize_sized(writer, large_id)?;
        self.audio_segment_data.serialize(writer)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VoiceReceiversMessage {
    /// `None` means "couldn't parse / corrupt" and is deliberately left for the consumer to
    /// ignore; `Some(empty)` is an explicit "no recipients".
    pub users: Option<Vec<u16>>,
    /// Actual count (the C# rented array could be larger).
    pub users_length: usize,
}

impl VoiceReceiversMessage {
    // Hard cap to avoid giant allocations if data is corrupted
    const MAX_USERS: usize = u16::MAX as usize;

    /// `large_count`: false = byte count (AudioRecipientsChannel), true = ushort count
    /// (AudioRecipientsLargeChannel).
    pub fn deserialize(&mut self, reader: &mut NetDataReader, large_count: bool) -> NetResult<()> {
        let remaining_bytes = reader.available_bytes();
        if remaining_bytes == 0 {
            self.users = Some(Vec::new());
            return Ok(());
        }
        let count_size = if large_count { 2 } else { 1 };
        if remaining_bytes < count_size {
            BNL::log_error(format!("VoiceReceiversMessage: not enough bytes for length. Remaining={remaining_bytes}"));
            Self::skip_remaining(reader);
            self.users = Some(Vec::new());
            return Ok(());
        }
        let count = if large_count { usize::from(reader.get_ushort()?) } else { usize::from(reader.get_byte()?) };
        if count == 0 {
            self.return_pool();
            self.users = Some(Vec::new());
            self.users_length = 0;
            return Ok(());
        }
        if count > Self::MAX_USERS {
            BNL::log_error(format!("VoiceReceiversMessage: reported count={count} exceeds MaxUsers={}. Possible protocol mismatch or corrupted packet.", Self::MAX_USERS));
            Self::skip_remaining(reader);
            self.return_pool();
            self.users = None;
            self.users_length = 0;
            return Ok(());
        }
        let bytes_needed = count * 2;
        if reader.available_bytes() < bytes_needed {
            BNL::log_error(format!("VoiceReceiversMessage: count={count} needs {bytes_needed} bytes, but only {} available. Protocol mismatch?", reader.available_bytes()));
            Self::skip_remaining(reader);
            self.return_pool();
            self.users = None;
            self.users_length = 0;
            return Ok(());
        }
        self.return_pool();
        let mut users = Vec::with_capacity(count);
        for _ in 0..count {
            users.push(reader.get_ushort()?);
        }
        self.users = Some(users);
        self.users_length = count;
        Ok(())
    }

    /// Equivalent to `serialize_sized(writer, true)` — always writes a 2-byte count.
    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        self.serialize_sized(writer, true)?;
        Ok(())
    }

    /// `large_count` must match the channel the packet will be sent on.
    pub fn serialize_sized(&mut self, writer: &mut NetDataWriter, large_count: bool) -> NetResult<()> {
        let users_length = self.users.as_ref().map(|u| u.len()).unwrap_or(0);
        let max_count = if large_count { u16::MAX as usize } else { u8::MAX as usize };
        if users_length == 0 {
            // Still write a 0-length so read side stays in sync
            if large_count { writer.put_ushort(0) } else { writer.put_byte(0) }
            return Ok(());
        }
        if users_length > max_count {
            BNL::log_error(format!(
                "VoiceReceiversMessage: Users.Length={users_length} exceeds {} for this channel. Truncating.",
                if large_count { "ushort.MaxValue" } else { "byte.MaxValue" }
            ));
        }
        let count = users_length.min(max_count);
        if large_count { writer.put_ushort(count as u16) } else { writer.put_byte(count as u8) }
        let Some(users) = self.users.as_ref() else {
            return Ok(());
        };
        for u in users.iter().take(count) {
            writer.put_ushort(*u);
        }
        Ok(())
    }

    /// The C# returned a rented array to the pool; here it just clears.
    pub fn return_pool(&mut self) {
        self.users = None;
        self.users_length = 0;
    }

    fn skip_remaining(reader: &mut NetDataReader) {
        let n = reader.available_bytes();
        if n > 0 {
            reader.skip_bytes(n);
        }
    }
}
