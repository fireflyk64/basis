use crate::io::{NetDataReader, NetDataWriter, NetResult};
use crate::BNL;

/// Compact encoding for player identifiers. Deployments use did:key, Steam64, Meta/Oculus
/// numeric ids, plain GUIDs, or anything else an operator plugs in, so this recognises the common
/// shapes, packs each to its natural binary form, and falls back to a raw string. Every compact
/// form is VERIFIED by re-encoding before it is chosen.
pub struct BasisCompactId;

impl BasisCompactId {
    const TAG_RAW: u8 = 0; // [len:ushort][utf8]
    const TAG_UUID: u8 = 1; // [fmt:1][16 bytes]
    const TAG_UINT64: u8 = 2; // [8 bytes]
    const TAG_HEX: u8 = 3; // [flags:1][len:1][bytes]
    const TAG_DID_KEY: u8 = 4; // [len:1][utf8 after "did:key:"]

    const DID_KEY_PREFIX: &'static str = "did:key:";

    const UUID_FORMAT_DASHED_LOWER: u8 = 0;
    const UUID_FORMAT_DASHED_UPPER: u8 = 1;
    const UUID_FORMAT_PLAIN_LOWER: u8 = 2;
    const UUID_FORMAT_PLAIN_UPPER: u8 = 3;

    const HEX_FLAG_UPPER: u8 = 1;

    pub fn write(writer: &mut NetDataWriter, value: &str) {
        if Self::try_write_uuid(writer, value) {
            return;
        }
        if Self::try_write_uint64(writer, value) {
            return;
        }
        if Self::try_write_did_key(writer, value) {
            return;
        }
        if Self::try_write_hex(writer, value) {
            return;
        }
        writer.put_byte(Self::TAG_RAW);
        writer.put_string(value);
    }

    pub fn read(reader: &mut NetDataReader) -> NetResult<String> {
        let tag = reader.get_byte()?;
        match tag {
            Self::TAG_UUID => Self::read_uuid(reader),
            Self::TAG_UINT64 => Ok(reader.get_ulong()?.to_string()),
            Self::TAG_HEX => Self::read_hex(reader),
            Self::TAG_DID_KEY => Ok(format!("{}{}", Self::DID_KEY_PREFIX, Self::read_short_string(reader)?)),
            _ => reader.get_string(),
        }
    }

    fn try_write_uuid(writer: &mut NetDataWriter, value: &str) -> bool {
        if value.len() != 32 && value.len() != 36 {
            return false;
        }
        let Some(guid) = Self::parse_guid(value) else {
            return false;
        };
        let format = if value.len() == 36 {
            if Self::has_upper_hex(value) { Self::UUID_FORMAT_DASHED_UPPER } else { Self::UUID_FORMAT_DASHED_LOWER }
        } else if Self::has_upper_hex(value) {
            Self::UUID_FORMAT_PLAIN_UPPER
        } else {
            Self::UUID_FORMAT_PLAIN_LOWER
        };
        // Mixed case, or any rendering that will not reproduce exactly, must not take this path.
        if Self::render_uuid(&guid, format) != value {
            return false;
        }
        writer.put_byte(Self::TAG_UUID);
        writer.put_byte(format);
        writer.put_bytes(&guid);
        true
    }

    fn read_uuid(reader: &mut NetDataReader) -> NetResult<String> {
        let format = reader.get_byte()?;
        let raw = reader.get_guid()?;
        Ok(Self::render_uuid(&raw, format))
    }

    /// Parses a GUID in the N (32 hex) or D (8-4-4-4-12) rendering into .NET `Guid.ToByteArray`
    /// order: the first three groups little-endian, the rest in string order.
    pub fn parse_guid(value: &str) -> Option<[u8; 16]> {
        let bytes = value.as_bytes();
        let mut hex = [0u8; 32];
        match bytes.len() {
            32 => hex.copy_from_slice(bytes),
            36 => {
                if bytes[8] != b'-' || bytes[13] != b'-' || bytes[18] != b'-' || bytes[23] != b'-' {
                    return None;
                }
                let mut n = 0;
                for (i, b) in bytes.iter().enumerate() {
                    if i == 8 || i == 13 || i == 18 || i == 23 {
                        continue;
                    }
                    hex[n] = *b;
                    n += 1;
                }
            }
            _ => return None,
        }
        let mut big = [0u8; 16];
        for i in 0..16 {
            let hi = Self::hex_val(hex[i * 2] as char)?;
            let lo = Self::hex_val(hex[i * 2 + 1] as char)?;
            big[i] = (hi << 4) | lo;
        }
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&[big[3], big[2], big[1], big[0]]);
        out[4..6].copy_from_slice(&[big[5], big[4]]);
        out[6..8].copy_from_slice(&[big[7], big[6]]);
        out[8..16].copy_from_slice(&big[8..16]);
        Some(out)
    }

    /// Renders .NET-ordered GUID bytes in the chosen format.
    pub fn render_uuid(guid: &[u8; 16], format: u8) -> String {
        let mut big = [0u8; 16];
        big[0..4].copy_from_slice(&[guid[3], guid[2], guid[1], guid[0]]);
        big[4..6].copy_from_slice(&[guid[5], guid[4]]);
        big[6..8].copy_from_slice(&[guid[7], guid[6]]);
        big[8..16].copy_from_slice(&guid[8..16]);
        let upper = format == Self::UUID_FORMAT_DASHED_UPPER || format == Self::UUID_FORMAT_PLAIN_UPPER;
        let alphabet: &[u8; 16] = if upper { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
        let dashed = format == Self::UUID_FORMAT_DASHED_LOWER || format == Self::UUID_FORMAT_DASHED_UPPER;
        let mut s = String::with_capacity(36);
        for (i, b) in big.iter().enumerate() {
            if dashed && (i == 4 || i == 6 || i == 8 || i == 10) {
                s.push('-');
            }
            s.push(alphabet[(b >> 4) as usize] as char);
            s.push(alphabet[(b & 0xF) as usize] as char);
        }
        s
    }

    fn try_write_uint64(writer: &mut NetDataWriter, value: &str) -> bool {
        if value.is_empty() || value.len() > 20 || !value.bytes().all(|c| c.is_ascii_digit()) {
            return false;
        }
        let Ok(parsed) = value.parse::<u64>() else {
            return false;
        };
        // Rejects leading zeros ("007" would come back as "7").
        if parsed.to_string() != value {
            return false;
        }
        writer.put_byte(Self::TAG_UINT64);
        writer.put_ulong(parsed);
        true
    }

    fn try_write_did_key(writer: &mut NetDataWriter, value: &str) -> bool {
        let Some(body) = value.strip_prefix(Self::DID_KEY_PREFIX) else {
            return false;
        };
        let chars = body.chars().count();
        if chars == 0 || chars > 255 {
            return false;
        }
        writer.put_byte(Self::TAG_DID_KEY);
        Self::write_short_string(writer, body);
        true
    }

    fn try_write_hex(writer: &mut NetDataWriter, value: &str) -> bool {
        if value.is_empty() || (value.len() & 1) != 0 || value.len() > 510 {
            return false;
        }
        let (mut upper, mut lower) = (false, false);
        for c in value.chars() {
            match c {
                '0'..='9' => {}
                'a'..='f' => lower = true,
                'A'..='F' => upper = true,
                _ => return false,
            }
        }
        if upper && lower {
            return false; // mixed case would not round-trip
        }
        let count = value.len() / 2;
        let bytes: Vec<u8> = value
            .as_bytes()
            .chunks_exact(2)
            .map(|p| (Self::hex_val(p[0] as char).unwrap() << 4) | Self::hex_val(p[1] as char).unwrap())
            .collect();
        writer.put_byte(Self::TAG_HEX);
        writer.put_byte(if upper { Self::HEX_FLAG_UPPER } else { 0 });
        writer.put_byte(count as u8);
        writer.put_bytes(&bytes);
        true
    }

    fn read_hex(reader: &mut NetDataReader) -> NetResult<String> {
        let flags = reader.get_byte()?;
        let count = usize::from(reader.get_byte()?);
        let bytes = reader.get_bytes_vec(count)?;
        let alphabet: &[u8; 16] = if (flags & Self::HEX_FLAG_UPPER) != 0 { b"0123456789ABCDEF" } else { b"0123456789abcdef" };
        let mut s = String::with_capacity(count * 2);
        for b in bytes {
            s.push(alphabet[(b >> 4) as usize] as char);
            s.push(alphabet[(b & 0xF) as usize] as char);
        }
        Ok(s)
    }

    fn hex_val(c: char) -> Option<u8> {
        c.to_digit(16).map(|d| d as u8)
    }

    fn has_upper_hex(value: &str) -> bool {
        value.chars().any(|c| c.is_ascii_uppercase() && c.is_ascii_hexdigit())
    }

    fn write_short_string(writer: &mut NetDataWriter, value: &str) {
        let bytes = value.as_bytes();
        // Callers gate on length <= 255 in chars; multi-byte UTF-8 could still overflow, so re-check.
        if bytes.len() > 255 {
            writer.put_byte(0);
            return;
        }
        writer.put_byte(bytes.len() as u8);
        writer.put_bytes(bytes);
    }

    fn read_short_string(reader: &mut NetDataReader) -> NetResult<String> {
        let length = usize::from(reader.get_byte()?);
        if length == 0 {
            return Ok(String::new());
        }
        let bytes = reader.get_bytes_vec(length)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Compact encoding for the player platform. APPEND-ONLY: the index IS the wire id.
pub struct BasisPlatformCodec;

impl BasisPlatformCodec {
    const TAG_RAW: u8 = 0;

    pub const KNOWN: [&'static str; 36] = [
        "WindowsPlayer", "WindowsEditor", "WindowsServer",
        "OSXPlayer", "OSXEditor", "OSXServer",
        "LinuxPlayer", "LinuxEditor", "LinuxServer",
        "Android", "IPhonePlayer", "VisionOS",
        "WebGLPlayer",
        "PS4", "PS5", "XboxOne", "GameCoreXboxOne", "GameCoreXboxSeries", "Switch", "tvOS",
        "WSAPlayerX86", "WSAPlayerX64", "WSAPlayerARM",
        "EmbeddedLinuxArm64", "EmbeddedLinuxArm32", "EmbeddedLinuxX64", "EmbeddedLinuxX86",
        "QNXArm32", "QNXArm64", "QNXX64", "QNXX86",
        "Stadia", "CloudRendering", "LinuxHeadlessSimulation", "Lumin",
        // Not a Unity platform: what the load-test console reports.
        "Headless",
    ];

    pub fn write(writer: &mut NetDataWriter, platform: &str) {
        for (i, known) in Self::KNOWN.iter().enumerate() {
            if *known == platform {
                writer.put_byte((i + 1) as u8);
                return;
            }
        }
        writer.put_byte(Self::TAG_RAW);
        writer.put_string(platform);
    }

    pub fn read(reader: &mut NetDataReader) -> NetResult<String> {
        let id = reader.get_byte()?;
        if id == Self::TAG_RAW {
            return reader.get_string();
        }
        let index = usize::from(id) - 1;
        // An id from a newer server than this client knows: do not guess, report it as unknown.
        Ok(Self::KNOWN.get(index).map(|s| s.to_string()).unwrap_or_default())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientMetaDataMessage {
    pub player_uuid: String,
    pub player_display_name: String,
    pub player_platform: String,
}

impl ClientMetaDataMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_uuid = BasisCompactId::read(reader)?;
        self.player_display_name = reader.get_string()?;
        self.player_platform = BasisPlatformCodec::read(reader)?;
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        BasisCompactId::write(writer, if self.player_uuid.is_empty() { "Failure" } else { &self.player_uuid });
        writer.put_string(if self.player_display_name.is_empty() { "Failure" } else { &self.player_display_name });
        BasisPlatformCodec::write(writer, if self.player_platform.is_empty() { "Failure" } else { &self.player_platform });
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NetIDMessage {
    pub player_id: String,
}

impl NetIDMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        let bytes = reader.available_bytes();
        if bytes != 0 {
            self.player_id = reader.get_string_max(256)?;
        } else {
            BNL::log_error(format!("Unable to read remaining bytes: {bytes}"));
        }
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        if !self.player_id.is_empty() {
            writer.put_string(&self.player_id);
        } else {
            BNL::log_error("Unable to serialize. Field was null or empty.");
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnershipTransferMessage {
    pub player_id_message: PlayerIdMessage,
    pub ownership_id: String,
}

impl OwnershipTransferMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id_message.deserialize(reader)?;
        self.ownership_id = reader.get_string_max(256)?;
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        self.player_id_message.serialize(writer);
        writer.put_string(&self.ownership_id);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PlayerIdMessage {
    pub player_id: u16,
}

impl PlayerIdMessage {
    pub const fn new(player_id: u16) -> Self {
        Self { player_id }
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id = reader.get_ushort()?;
        Ok(())
    }

    /// `large_id`: false = read byte, true = read ushort.
    pub fn deserialize_sized(&mut self, reader: &mut NetDataReader, large_id: bool) -> NetResult<()> {
        self.player_id = if large_id { reader.get_ushort()? } else { u16::from(reader.get_byte()?) };
        Ok(())
    }

    pub fn serialize(&self, writer: &mut NetDataWriter) {
        writer.put_ushort(self.player_id);
    }

    /// `large_id`: false = write byte, true = write ushort.
    pub fn serialize_sized(&self, writer: &mut NetDataWriter, large_id: bool) {
        if large_id {
            writer.put_ushort(self.player_id);
        } else {
            writer.put_byte(self.player_id as u8);
        }
    }
}

/// Contains all necessary data to go along with the player's return message locally.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerMetaDataMessage {
    pub client_meta_data_message: ClientMetaDataMessage,
    pub sync_interval: i32,
    pub base_multiplier: i32,
    pub increase_rate: f32,
    pub slowest_send_rate: f32,
    pub peer_limit: i32,
    /// v42: server accepts client→server avatar deltas on DeltaAvatarChannel.
    pub uplink_delta_enabled: bool,
    /// Server egress one sharing client may spend replicating an image, in megabits per second.
    pub image_share_egress_megabits_per_second: i32,
    /// Maximum distance a sharing client replicates an image pickup over, in metres.
    pub image_pickup_range_meters: f32,
    /// Fast, fixed — known nodes as bits.
    pub permissions_bitset: Vec<u8>,
    /// Dynamic fallback — compressed on the wire.
    pub extra_permissions: Vec<String>,
}

impl ServerMetaDataMessage {
    /// Populate bitset + extras from a collection of allowed permission node strings.
    pub fn set_permissions(&mut self, allowed_nodes: &[String], denied_nodes: Option<&[String]>) {
        let (bitset, extras) = PermissionBitsetMap::encode(allowed_nodes, denied_nodes);
        self.permissions_bitset = bitset;
        self.extra_permissions = extras;
    }

    /// Decode bitset + extras back into the full set of permission node strings.
    pub fn get_permissions(&self) -> std::collections::HashSet<String> {
        PermissionBitsetMap::decode(&self.permissions_bitset, &self.extra_permissions)
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.client_meta_data_message.deserialize(reader)?;
        self.sync_interval = reader.get_int()?;
        self.base_multiplier = reader.get_int()?;
        self.increase_rate = reader.get_float()?;
        self.slowest_send_rate = reader.get_float()?;
        self.peer_limit = reader.get_int()?;

        // Permissions (backward compatible — skip if no more data)
        if reader.available_bytes() > 0 {
            self.permissions_bitset = reader.get_bytes_with_length()?;
            let extra_count = reader.get_ushort()?;
            if extra_count > 0 {
                let compressed = reader.get_bytes_with_length()?;
                self.extra_permissions = PermissionCompression::decompress_extras(&compressed, usize::from(extra_count));
            } else {
                self.extra_permissions = Vec::new();
            }
        } else {
            self.permissions_bitset = Vec::new();
            self.extra_permissions = Vec::new();
        }

        self.uplink_delta_enabled = reader.available_bytes() > 0 && reader.get_byte()? != 0;
        self.image_share_egress_megabits_per_second = if reader.available_bytes() >= 4 { reader.get_int()? } else { 0 };
        self.image_pickup_range_meters = if reader.available_bytes() >= 4 { reader.get_float()? } else { 0.0 };
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        self.client_meta_data_message.serialize(writer);

        if self.sync_interval == 0 {
            self.sync_interval = 50;
            BNL::log_error("SyncInterval was not set! ");
        }
        if self.base_multiplier == 0 {
            self.base_multiplier = 1;
            BNL::log_error("Base Multiplier was not set! ");
        }
        if self.increase_rate == 0.0 {
            self.increase_rate = 0.005;
            BNL::log_error("IncreaseRate was not set! ");
        }
        if self.slowest_send_rate == 0.0 {
            self.slowest_send_rate = 2.55;
            BNL::log_error("Slowest Send Rate was not set!");
        }

        writer.put_int(self.sync_interval);
        writer.put_int(self.base_multiplier);
        writer.put_float(self.increase_rate);
        writer.put_float(self.slowest_send_rate);
        writer.put_int(self.peer_limit);

        writer.put_bytes_with_length(&self.permissions_bitset);

        let extra_count = self.extra_permissions.len() as u16;
        writer.put_ushort(extra_count);
        if extra_count > 0 {
            let compressed = PermissionCompression::compress_extras(&self.extra_permissions);
            writer.put_bytes_with_length(&compressed);
        }

        writer.put_byte(u8::from(self.uplink_delta_enabled));
        writer.put_int(self.image_share_egress_megabits_per_second);
        writer.put_float(self.image_pickup_range_meters);
    }
}

use super::permissions::{PermissionBitsetMap, PermissionCompression};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerNetIDMessage {
    pub net_id_message: NetIDMessage,
    pub ushort_unique_id_message: UshortUniqueIDMessage,
}

impl ServerNetIDMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.net_id_message.deserialize(reader)?;
        self.ushort_unique_id_message.deserialize(reader)
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        self.net_id_message.serialize(writer);
        self.ushort_unique_id_message.serialize(writer);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerUniqueIDMessages {
    pub message_count: u16,
    pub messages: Option<Vec<ServerNetIDMessage>>,
}

impl ServerUniqueIDMessages {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        let bytes = reader.available_bytes();
        if bytes >= 2 {
            self.message_count = reader.get_ushort()?;
            let mut messages = Vec::with_capacity(usize::from(self.message_count));
            for _ in 0..self.message_count {
                let mut m = ServerNetIDMessage::default();
                m.deserialize(reader)?;
                messages.push(m);
            }
            self.messages = Some(messages);
        } else {
            self.messages = None;
            BNL::log_error(format!("Unable to read remaining bytes for MessageCount. Available: {bytes}"));
        }
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        match self.messages.as_mut() {
            Some(messages) => {
                self.message_count = messages.len() as u16;
                writer.put_ushort(self.message_count);
                for message in messages.iter_mut() {
                    message.serialize(writer);
                }
            }
            None => BNL::log_error("Unable to serialize. Messages array was null."),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UshortUniqueIDMessage {
    pub unique_id_ushort: u16,
}

impl UshortUniqueIDMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        let bytes = reader.available_bytes();
        if bytes != 0 {
            self.unique_id_ushort = reader.get_ushort()?;
        } else {
            BNL::log_error(format!("Unable to read remaining bytes: {bytes}"));
        }
        Ok(())
    }

    pub fn serialize(&self, writer: &mut NetDataWriter) {
        writer.put_ushort(self.unique_id_ushort);
    }
}
