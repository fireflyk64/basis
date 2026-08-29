use crate::io::{NetDataError, NetDataReader, NetDataWriter, NetResult};

use super::identity::PlayerIdMessage;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteSceneDataMessage {
    pub message_index: u16,
    pub payload: Option<Vec<u8>>,
    pub payload_length: usize,
}

impl RemoteSceneDataMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.message_index = reader
            .try_get_ushort()
            .ok_or_else(|| NetDataError("Failed to read messageIndex.".into()))?;
        let payload_size = reader.available_bytes();
        if payload_size > 0 {
            self.payload = Some(reader.get_bytes_vec(payload_size)?);
            self.payload_length = payload_size;
        }
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        writer.put_ushort(self.message_index);
        if let Some(payload) = &self.payload {
            let len = if self.payload_length > 0 { self.payload_length.min(payload.len()) } else { payload.len() };
            if len > 0 {
                writer.put_bytes_range(payload, 0, len);
            }
        }
    }

    pub fn release(&mut self) {
        self.payload = None;
        self.payload_length = 0;
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SceneDataMessage {
    pub message_index: u16,
    pub recipients_size: u16,
    /// If empty, it's for everyone. Otherwise, send only to the listed entries.
    pub recipients: Option<Vec<u16>>,
    pub payload: Option<Vec<u8>>,
}

impl SceneDataMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.message_index = reader
            .try_get_ushort()
            .ok_or_else(|| NetDataError("Failed to read messageIndex.".into()))?;
        match reader.try_get_ushort() {
            Some(recipients_size) => {
                self.recipients_size = recipients_size;
                // Guard against absurd sizes
                if usize::from(recipients_size) > reader.available_bytes() / 2 {
                    return Err(NetDataError(format!("Invalid recipientsSize: {recipients_size}")));
                }
                let mut recipients = Vec::with_capacity(usize::from(recipients_size));
                for index in 0..recipients_size {
                    let r = reader
                        .try_get_ushort()
                        .ok_or_else(|| NetDataError(format!("Failed to read recipient at index {index}.")))?;
                    recipients.push(r);
                }
                self.recipients = Some(recipients);
                // Read remaining bytes as payload
                if reader.available_bytes() > 0 {
                    self.payload = Some(reader.get_remaining_bytes());
                }
            }
            None => {
                self.recipients = None;
                self.payload = None;
            }
        }
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        writer.put_ushort(self.message_index);
        self.recipients_size = self.recipients.as_ref().map(|r| r.len() as u16).unwrap_or(0);
        writer.put_ushort(self.recipients_size);
        if let Some(recipients) = &self.recipients {
            for r in recipients.iter().take(usize::from(self.recipients_size)) {
                writer.put_ushort(*r);
            }
        }
        if let Some(payload) = &self.payload
            && !payload.is_empty()
        {
            writer.put_bytes(payload);
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerSceneDataMessage {
    pub player_id_message: PlayerIdMessage,
    pub scene_data_message: RemoteSceneDataMessage,
}

impl ServerSceneDataMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id_message.deserialize(reader)?;
        self.scene_data_message.deserialize(reader)
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        self.player_id_message.serialize(writer);
        self.scene_data_message.serialize(writer);
    }
}
