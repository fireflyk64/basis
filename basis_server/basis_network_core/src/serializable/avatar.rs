use crate::compression::{BasisAvatarBitPacking, BasisRangedUshortFloatData, BitQuality};
use crate::io::{NetDataError, NetDataReader, NetDataWriter, NetResult};
use crate::BNL;

use super::identity::PlayerIdMessage;
use crate::io::net_data_reader::NetResultExt;

/// Wire form: [PayloadSize:1][messageIndex:1][data:PayloadSize]. The 2-byte header is written
/// for EVERY entry, including empty/suppressed ones (PayloadSize 0).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdditionalAvatarData {
    pub payload_size: u8,
    pub message_index: u8,
    pub array: Option<Vec<u8>>,
}

impl AdditionalAvatarData {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        let Some(payload_size) = reader.try_get_byte() else {
            BNL::log_error("trying to write data that does not exist! PayloadSize");
            self.array = None;
            return Ok(());
        };
        self.payload_size = payload_size;
        let Some(message_index) = reader.try_get_byte() else {
            BNL::log_error("trying to write data that does not exist! messageIndex");
            self.array = None;
            return Ok(());
        };
        self.message_index = message_index;
        if payload_size == 0 {
            self.array = None;
            return Ok(());
        }
        if usize::from(payload_size) > reader.available_bytes() {
            BNL::log_error("AdditionalAvatarData payload exceeds available data!");
            self.array = None;
            return Ok(());
        }
        // Deserialize in place: the retained buffer is reused when the size matches.
        let size = usize::from(payload_size);
        let array = match self.array.as_mut() {
            Some(a) if a.len() == size => a,
            _ => self.array.insert(vec![0u8; size]),
        };
        reader.get_bytes(array, size)?;
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        if let Some(array) = &self.array
            && array.len() > 255
        {
            BNL::log_error("Larger than 255 cannot send this Additional Avatar Data");
            self.payload_size = 0;
            writer.put_byte(self.payload_size);
            writer.put_byte(self.message_index);
            return Ok(());
        }
        match self.array.as_ref() {
            Some(array) if !array.is_empty() => {
                self.payload_size = u8::try_from(array.len()).unwrap_or(u8::MAX);
                writer.put_byte(self.payload_size);
                writer.put_byte(self.message_index);
                writer.put_bytes_range(array, 0, usize::from(self.payload_size))?;
            }
            _ => {
                self.payload_size = 0;
                writer.put_byte(0);
                writer.put_byte(self.message_index);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AvatarDataMessage {
    pub player_id_message: PlayerIdMessage,
    pub message_index: u8,
    pub avatar_link_index: u8,
    pub recipients_size: u16,
    /// If empty, it's for everyone. Otherwise, send only to the listed entries.
    pub recipients: Option<Vec<u16>>,
    pub payload: Option<Vec<u8>>,
}

impl AvatarDataMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id_message.deserialize(reader)?;
        self.avatar_link_index = reader.get_byte().field("AvatarLinkIndex")?;
        self.message_index = reader.get_byte().field("messageIndex")?;
        match reader.try_get_ushort() {
            Some(recipients_size) => {
                self.recipients_size = recipients_size;
                if usize::from(recipients_size) > reader.available_bytes() / 2 {
                    return Err(NetDataError::invalid("recipientsSize", format!("{recipients_size}")));
                }
                let mut recipients = Vec::with_capacity(usize::from(recipients_size));
                for _ in 0..recipients_size {
                    recipients.push(
                        reader.get_ushort().field("recipients")?,
                    );
                }
                self.recipients = Some(recipients);
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

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        self.player_id_message.serialize(writer)?;
        writer.put_byte(self.avatar_link_index);
        writer.put_byte(self.message_index);
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
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AvatarLoadDataMessage {
    pub message_index: u8,
    pub payload_size: u16,
    pub payload: Option<Vec<u8>>,
    pub who_sent_us_this: u16,
}

impl AvatarLoadDataMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.message_index = reader.get_byte().field("messageIndex")?;
        self.who_sent_us_this = reader.get_ushort().field("who sent us this")?;
        self.payload_size = reader.get_ushort().field("payloadSize")?;
        if self.payload_size == 0 {
            self.payload = None;
            return Ok(());
        }
        if usize::from(self.payload_size) > reader.available_bytes() {
            return Err(NetDataError::invalid("payloadSize", format!("{}", self.payload_size)));
        }
        self.payload = Some(reader.get_bytes_vec(usize::from(self.payload_size))?);
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_byte(self.message_index);
        writer.put_ushort(self.who_sent_us_this);
        self.payload_size = self.payload.as_ref().map(|p| p.len() as u16).unwrap_or(0);
        writer.put_ushort(self.payload_size);
        if let Some(payload) = &self.payload
            && !payload.is_empty()
        {
            writer.put_bytes(payload);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BasisAvatarCloneRequest {
    pub requesting_user: u16,
}

impl BasisAvatarCloneRequest {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.requesting_user = reader.get_ushort()?;
        Ok(())
    }
    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_ushort(self.requesting_user);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BasisAvatarCloneResponse {
    pub requesting_user: u16,
}

impl BasisAvatarCloneResponse {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.requesting_user = reader.get_ushort()?;
        Ok(())
    }
    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_ushort(self.requesting_user);
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClientAvatarChangeMessage {
    /// Downloading - attempts to download from a URL; BuiltIn - loads as an addressable.
    pub load_mode: u8,
    pub byte_array: Option<Vec<u8>>,
    /// Incremented and wrapped around when > 255.
    pub local_avatar_index: u8,
    // Body fit (per-segment stretch/collapse). 1 = no deformation.
    pub arm_scale: f32,
    pub leg_scale: f32,
    pub torso_scale: f32,
}

impl ClientAvatarChangeMessage {
    /// All three fit scales live in [0.5, 1.5]. KEEP IN STEP WITH BasisBodyFitCore.MaxDeviationCeiling.
    pub const FIT_SCALE_MIN: f32 = 0.5;
    pub const FIT_SCALE_MAX: f32 = 1.5;
    const FIT_SCALE_RANGE: BasisRangedUshortFloatData =
        BasisRangedUshortFloatData::new(Self::FIT_SCALE_MIN, Self::FIT_SCALE_MAX, 1.0 / 65535.0);

    /// Normalises a scale before it is quantized. A default-constructed message carries 0, which is
    /// what makes "unset" mean "no deformation".
    pub fn sanitize_fit_scale(value: f32) -> f32 {
        if value.is_nan() || value.is_infinite() || value <= 0.0 {
            return 1.0;
        }
        value.clamp(Self::FIT_SCALE_MIN, Self::FIT_SCALE_MAX)
    }

    /// Quantizes a fit scale to its 2-byte wire form.
    pub fn compress_fit_scale(value: f32) -> u16 {
        Self::FIT_SCALE_RANGE.compress(Self::sanitize_fit_scale(value))
    }

    /// Rebuilds a fit scale from the wire. The result is always within the valid band.
    pub fn decompress_fit_scale(value: u16) -> f32 {
        Self::FIT_SCALE_RANGE.decompress(value)
    }

    pub fn sanitize_fit(&mut self) {
        self.arm_scale = Self::sanitize_fit_scale(self.arm_scale);
        self.leg_scale = Self::sanitize_fit_scale(self.leg_scale);
        self.torso_scale = Self::sanitize_fit_scale(self.torso_scale);
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.load_mode = reader.get_byte()?;
        let length = usize::from(reader.get_ushort()?);
        if length == 0 {
            self.byte_array = None;
        } else {
            if length > reader.available_bytes() {
                self.byte_array = None;
                return Err(NetDataError::invalid("value", format!(
                    "Avatar change length {length} exceeds available data ({} bytes).",
                    reader.available_bytes()
                )));
            }
            self.byte_array = Some(reader.get_bytes_vec(length)?);
        }
        self.local_avatar_index = reader.get_byte()?;
        self.arm_scale = Self::decompress_fit_scale(reader.get_ushort()?);
        self.leg_scale = Self::decompress_fit_scale(reader.get_ushort()?);
        self.torso_scale = Self::decompress_fit_scale(reader.get_ushort()?);
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_byte(self.load_mode);
        match &self.byte_array {
            None => writer.put_ushort(0),
            Some(bytes) => {
                writer.put_ushort(bytes.len() as u16);
                writer.put_bytes(bytes);
            }
        }
        writer.put_byte(self.local_avatar_index);
        writer.put_ushort(Self::compress_fit_scale(self.arm_scale));
        writer.put_ushort(Self::compress_fit_scale(self.leg_scale));
        writer.put_ushort(Self::compress_fit_scale(self.torso_scale));
        Ok(())
    }
}

/// A body-fit change with no avatar change behind it.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClientBodyFitMessage {
    pub arm_scale: f32,
    pub leg_scale: f32,
    pub torso_scale: f32,
}

impl ClientBodyFitMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.arm_scale = ClientAvatarChangeMessage::decompress_fit_scale(reader.get_ushort()?);
        self.leg_scale = ClientAvatarChangeMessage::decompress_fit_scale(reader.get_ushort()?);
        self.torso_scale = ClientAvatarChangeMessage::decompress_fit_scale(reader.get_ushort()?);
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_ushort(ClientAvatarChangeMessage::compress_fit_scale(self.arm_scale));
        writer.put_ushort(ClientAvatarChangeMessage::compress_fit_scale(self.leg_scale));
        writer.put_ushort(ClientAvatarChangeMessage::compress_fit_scale(self.torso_scale));
        Ok(())
    }
}

/// Server->client form of [`ClientBodyFitMessage`], stamped with whose fit changed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ServerBodyFitMessage {
    pub ushort_player_id: PlayerIdMessage,
    pub body_fit: ClientBodyFitMessage,
}

impl ServerBodyFitMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.ushort_player_id.deserialize(reader)?;
        self.body_fit.deserialize(reader)
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        self.ushort_player_id.serialize(writer)?;
        self.body_fit.serialize(writer)?;
        Ok(())
    }
}

/// On-wire contract:
/// Client→Server (channel 2):  [DataQualityLevel:1][PayloadBytes:FixedByQuality][AdditionalSize:1][LinkedAvatarIndex?][Additional...]
/// Server→Client (even ch):    [PayloadBytes:FixedByQuality]
/// Server→Client (odd ch):     [PayloadBytes:FixedByQuality][AdditionalSize:1][LinkedAvatarIndex:1][Additional...]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalAvatarSyncMessage {
    /// 0=VeryLow, 1=Low, 2=Medium, 3=High
    pub data_quality_level: u8,
    /// Payload bytes (length must match convert_to_size(quality)).
    pub array: Option<Vec<u8>>,
    pub additional_avatar_datas: Option<Vec<AdditionalAvatarData>>,
    pub additional_avatar_data_size: u8,
    pub linked_avatar_index: u8,
}

impl LocalAvatarSyncMessage {
    pub fn new(array: Vec<u8>) -> Self {
        Self { array: Some(array), ..Default::default() }
    }

    fn try_get_expected_payload_length(data_quality_level: u8) -> Option<usize> {
        let q = BitQuality::from_byte(data_quality_level)?;
        let expected = BasisAvatarBitPacking::convert_to_size(q);
        if expected == 0 { None } else { Some(expected) }
    }

    /// Deserialize when DataQualityLevel is in the payload (client→server path).
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        let Some(level) = reader.try_get_byte() else {
            BNL::log_error("Missing DataQualityLevel!");
            return Ok(());
        };
        self.data_quality_level = level;
        self.deserialize_payload(reader)
    }

    /// Deserialize when quality and additional-data presence are derived from the channel
    /// (server→client path).
    pub fn deserialize_for_channel(&mut self, reader: &mut NetDataReader, channel_derived_quality: u8, has_additional_data: bool) -> NetResult<()> {
        self.data_quality_level = channel_derived_quality;
        let Some(expected) = Self::try_get_expected_payload_length(self.data_quality_level) else {
            BNL::log_error(format!("Invalid DataQualityLevel={}", self.data_quality_level));
            return Ok(());
        };
        if reader.available_bytes() < expected {
            BNL::log_error(format!("Unable to read avatar payload. Need {expected}, have {}.", reader.available_bytes()));
            return Ok(());
        }
        self.read_array(reader, expected)?;
        if !has_additional_data {
            self.additional_avatar_data_size = 0;
            self.additional_avatar_datas = None;
            return Ok(());
        }
        self.deserialize_additional_data(reader)
    }

    fn read_array(&mut self, reader: &mut NetDataReader, expected: usize) -> NetResult<()> {
        let array = match self.array.as_mut() {
            Some(a) if a.len() == expected => a,
            _ => self.array.insert(vec![0u8; expected]),
        };
        reader.get_bytes(array, expected)
    }

    fn deserialize_payload(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        let Some(expected) = Self::try_get_expected_payload_length(self.data_quality_level) else {
            BNL::log_error(format!("Invalid DataQualityLevel={}", self.data_quality_level));
            return Ok(());
        };
        if reader.available_bytes() < expected {
            BNL::log_error(format!("Unable to read avatar payload. Need {expected}, have {}.", reader.available_bytes()));
            return Ok(());
        }
        self.read_array(reader, expected)?;
        let Some(size) = reader.try_get_byte() else {
            BNL::log_error("Missing AdditionalAvatarDataSize!");
            return Ok(());
        };
        self.additional_avatar_data_size = size;
        if size == 0 {
            self.additional_avatar_datas = None;
            return Ok(());
        }
        self.deserialize_additional_entries(reader)
    }

    /// Reads the additional-data section [size:1][linkedIndex:1][entries...].
    pub fn deserialize_additional_data(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        let Some(size) = reader.try_get_byte() else {
            BNL::log_error("Missing AdditionalAvatarDataSize!");
            return Ok(());
        };
        self.additional_avatar_data_size = size;
        self.deserialize_additional_entries(reader)
    }

    fn deserialize_additional_entries(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        let Some(linked) = reader.try_get_byte() else {
            BNL::log_error("Missing LinkedAvatarIndex!");
            return Ok(());
        };
        self.linked_avatar_index = linked;
        let size = usize::from(self.additional_avatar_data_size);
        if !self.additional_avatar_datas.as_ref().is_some_and(|d| d.len() == size) {
            self.additional_avatar_datas = Some(vec![AdditionalAvatarData::default(); size]);
        }
        let datas = self.additional_avatar_datas.get_or_insert_with(Vec::new);
        for entry in datas.iter_mut() {
            // Deserialize in place: the slot's retained payload buffer is reused when the size matches.
            entry.deserialize(reader)?;
        }
        Ok(())
    }

    /// Serialize with DataQualityLevel in the payload (initial player creation, non-quality
    /// channels). `quality` of `None` is the C# cast of an invalid level.
    pub fn serialize(&mut self, writer: &mut NetDataWriter, quality: Option<BitQuality>) -> NetResult<()> {
        self.data_quality_level = quality.map(|q| q as u8).unwrap_or(self.data_quality_level);
        let Some(expected) = Self::try_get_expected_payload_length(self.data_quality_level) else {
            BNL::log_error(format!("Serialize invalid quality={:?} (DataQualityLevel={})", quality, self.data_quality_level));
            writer.put_byte(self.data_quality_level);
            writer.put_byte(0);
            return Ok(());
        };
        writer.put_byte(self.data_quality_level);
        let Some(array) = self.array.as_mut() else {
            BNL::log_error("array was null!!");
            writer.put_byte(0);
            return Ok(());
        };
        if array.len() != expected {
            *array = vec![0u8; expected];
        }
        writer.put_bytes_range(array, 0, expected)?;

        let count = self.additional_avatar_datas.as_ref().map(|d| d.len()).unwrap_or(0);
        if count == 0 || count > 255 {
            writer.put_byte(0);
            return Ok(());
        }
        self.additional_avatar_data_size = count as u8;
        writer.put_byte(self.additional_avatar_data_size);
        writer.put_byte(self.linked_avatar_index);
        if let Some(entries) = self.additional_avatar_datas.as_mut() {
            for entry in entries.iter_mut() {
                entry.serialize(writer)?;
            }
        }
        Ok(())
    }

    /// Serialize for the channel-based path (quality channels). Quality and additional-data
    /// presence are encoded in the channel — not written to the payload.
    pub fn serialize_for_channel(&mut self, writer: &mut NetDataWriter, quality: BitQuality) -> NetResult<()> {
        self.data_quality_level = quality as u8;
        let Some(expected) = Self::try_get_expected_payload_length(self.data_quality_level) else {
            BNL::log_error(format!("SerializeForChannel invalid quality={quality:?}"));
            return Ok(());
        };
        let Some(array) = self.array.as_mut() else {
            BNL::log_error("array was null!!");
            return Ok(());
        };
        if array.len() != expected {
            *array = vec![0u8; expected];
        }
        writer.put_bytes_range(array, 0, expected)?;

        // Additional data only written when present — the channel tells the receiver.
        let count = self.additional_avatar_datas.as_ref().map(|d| d.len()).unwrap_or(0);
        if count > 0 && count <= 255 {
            self.serialize_additional_only(writer)?;
        }
        Ok(())
    }

    /// Writes just the additional-data section [size:1][linkedIndex:1][entries...].
    pub fn serialize_additional_only(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        let count = self.additional_avatar_datas.as_ref().map(|d| d.len()).unwrap_or(0);
        if count == 0 || count > 255 {
            return Ok(());
        }
        self.additional_avatar_data_size = count as u8;
        writer.put_byte(self.additional_avatar_data_size);
        writer.put_byte(self.linked_avatar_index);
        if let Some(entries) = self.additional_avatar_datas.as_mut() {
            for entry in entries.iter_mut() {
                entry.serialize(writer)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteAvatarDataMessage {
    pub player_id_message: PlayerIdMessage,
    pub message_index: u8,
    pub payload: Option<Vec<u8>>,
    pub avatar_link_index: u8,
}

impl RemoteAvatarDataMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id_message.deserialize(reader)?;
        self.avatar_link_index = reader.get_byte().field("AvatarLinkIndex")?;
        self.message_index = reader.get_byte().field("messageIndex")?;
        if reader.available_bytes() != 0 {
            self.payload = Some(reader.get_remaining_bytes());
        } else {
            self.payload = None;
        }
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        self.player_id_message.serialize(writer)?;
        writer.put_byte(self.avatar_link_index);
        writer.put_byte(self.message_index);
        if let Some(payload) = &self.payload
            && !payload.is_empty()
        {
            writer.put_bytes(payload);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerAvatarChangeMessage {
    pub ushort_player_id: PlayerIdMessage,
    pub client_avatar_change_message: ClientAvatarChangeMessage,
}

impl ServerAvatarChangeMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.ushort_player_id.deserialize(reader)?;
        self.client_avatar_change_message.deserialize(reader)
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        self.ushort_player_id.serialize(writer)?;
        self.client_avatar_change_message.serialize(writer)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerAvatarDataMessage {
    pub player_id_message: PlayerIdMessage,
    pub avatar_data_message: RemoteAvatarDataMessage,
}

impl ServerAvatarDataMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id_message.deserialize(reader)?;
        self.avatar_data_message.deserialize(reader)
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        self.player_id_message.serialize(writer)?;
        self.avatar_data_message.serialize(writer)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerSideSyncPlayerMessage {
    pub player_id_message: PlayerIdMessage,
    pub interval: u8,
    pub sequence: u8,
    pub avatar_serialization: LocalAvatarSyncMessage,
}

impl ServerSideSyncPlayerMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id_message.deserialize(reader)?; // 2 bytes
        self.interval = reader.get_byte()?; // 1 byte
        self.sequence = reader.get_byte()?; // 1 byte
        self.avatar_serialization.deserialize(reader)
    }

    /// Deserialize when quality and additional-data presence are derived from the channel.
    pub fn deserialize_for_channel(&mut self, reader: &mut NetDataReader, channel_derived_quality: u8, has_additional_data: bool) -> NetResult<()> {
        self.player_id_message.deserialize(reader)?;
        self.interval = reader.get_byte()?;
        self.sequence = reader.get_byte()?;
        self.avatar_serialization.deserialize_for_channel(reader, channel_derived_quality, has_additional_data)
    }

    /// Deserialize with byte/ushort playerID based on channel variant.
    pub fn deserialize_for_channel_sized(&mut self, reader: &mut NetDataReader, channel_derived_quality: u8, has_additional_data: bool, large_id: bool) -> NetResult<()> {
        self.player_id_message.deserialize_sized(reader, large_id)?;
        self.interval = reader.get_byte()?;
        self.sequence = reader.get_byte()?;
        self.avatar_serialization.deserialize_for_channel(reader, channel_derived_quality, has_additional_data)
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        self.player_id_message.serialize(writer)?;
        writer.put_byte(self.interval);
        writer.put_byte(self.sequence);
        let quality = BitQuality::from_byte(self.avatar_serialization.data_quality_level);
        self.avatar_serialization.serialize(writer, quality)?;
        Ok(())
    }
}
