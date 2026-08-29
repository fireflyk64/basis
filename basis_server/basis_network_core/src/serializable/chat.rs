use crate::io::{NetDataReader, NetDataWriter, NetResult};

use super::identity::PlayerIdMessage;
use crate::io::net_data_reader::NetDataError;

/// Client-to-server chat message. Contains UTF-8 encoded text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChatMessage {
    /// The UTF-8 encoded chat message bytes.
    pub payload: Vec<u8>,
    /// Length of the payload in bytes.
    pub payload_size: u16,
    /// Whether receivers should play their chat notification sound.
    pub play_notification_sound: bool,
}

impl ChatMessage {
    /// Maximum allowed message length in bytes.
    pub const MAX_PAYLOAD_BYTES: usize = 512;

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.play_notification_sound = true;
        let payload_size_wire = usize::from(reader.get_ushort()?);
        let read_size = payload_size_wire.min(Self::MAX_PAYLOAD_BYTES);

        if payload_size_wire == 0 {
            self.payload = Vec::new();
            self.payload_size = 0;
        } else if reader.available_bytes() < read_size {
            self.payload = Vec::new();
            self.payload_size = 0;
            reader.skip_bytes(payload_size_wire.min(reader.available_bytes()));
            return Ok(());
        } else {
            self.payload_size = read_size as u16;
            self.payload = reader.get_bytes_vec(read_size)?;
            let excess_size = payload_size_wire - read_size;
            if excess_size > 0 {
                reader.skip_bytes(excess_size.min(reader.available_bytes()));
            }
        }

        if reader.available_bytes() > 0 {
            self.play_notification_sound = reader.get_bool()?;
        }
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        if self.payload.is_empty() {
            writer.put_ushort(0);
            writer.put_bool(self.play_notification_sound);
            return Ok(());
        }
        self.payload_size = self.payload.len().min(Self::MAX_PAYLOAD_BYTES) as u16;
        writer.put_ushort(self.payload_size);
        writer.put_bytes_range(&self.payload, 0, usize::from(self.payload_size))?;
        writer.put_bool(self.play_notification_sound);
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConsoleData {
    pub message_index: u8,
    pub array: Option<Vec<u8>>,
}

impl ConsoleData {
    /// A payload the buffer cannot hold is a fault: the array is left empty and the error names
    /// the claimed and available sizes.
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.message_index = reader.get_byte().map_err(|e| e.for_field("ConsoleData.messageIndex"))?;
        let payload_size = usize::from(reader.get_ushort().map_err(|e| e.for_field("ConsoleData.payloadSize"))?);
        if payload_size > reader.available_bytes() {
            self.array = Some(Vec::new());
            return Err(NetDataError::length_exceeds_data("ConsoleData.array", payload_size, reader.available_bytes()));
        }
        self.array = Some(if payload_size > 0 { reader.get_bytes_vec(payload_size)? } else { Vec::new() });
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_byte(self.message_index);
        match self.array.as_ref() {
            Some(array) if !array.is_empty() => {
                let size = u16::try_from(array.len())
                    .map_err(|_| NetDataError::too_long("chat payload", array.len(), usize::from(u16::MAX)))?;
                writer.put_ushort(size);
                writer.put_bytes(array);
            }
            _ => writer.put_ushort(0),
        }
        Ok(())
    }
}

/// Server-to-client chat message. Wraps the chat payload with the sender's player ID.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerChatMessage {
    pub player_id_message: PlayerIdMessage,
    pub chat_message: ChatMessage,
}

impl ServerChatMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id_message.deserialize(reader)?;
        self.chat_message.deserialize(reader)
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        self.player_id_message.serialize(writer)?;
        self.chat_message.serialize(writer)?;
        Ok(())
    }
}
