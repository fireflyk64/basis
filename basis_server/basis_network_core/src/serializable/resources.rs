use crate::io::{NetDataReader, NetDataWriter, NetResult};

use super::identity::PlayerIdMessage;

/// Content types that can be shared via a content sphere.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum ContentShareType {
    #[default]
    Avatar = 0,
    Prop = 1,
    World = 2,
    /// A saved-server entry. ContentURL carries the connection string.
    Server = 3,
}

impl ContentShareType {
    pub fn from_byte(b: u8) -> Self {
        match b {
            1 => Self::Prop,
            2 => Self::World,
            3 => Self::Server,
            _ => Self::Avatar,
        }
    }
}

/// Sent by a client to drop a content share sphere into the world.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ContentShareMessage {
    /// Unique ID for this sphere instance (GUID string).
    pub sphere_net_id: String,
    pub content_url: String,
    pub unlock_password: String,
    pub content_type: ContentShareType,
    pub position_x: f32,
    pub position_y: f32,
    pub position_z: f32,
}

impl ContentShareMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.sphere_net_id = reader.get_string()?;
        self.content_url = reader.get_string()?;
        self.unlock_password = reader.get_string()?;
        self.content_type = ContentShareType::from_byte(reader.get_byte()?);
        self.position_x = reader.get_float()?;
        self.position_y = reader.get_float()?;
        self.position_z = reader.get_float()?;
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        writer.put_string(&self.sphere_net_id);
        writer.put_string(&self.content_url);
        writer.put_string(&self.unlock_password);
        writer.put_byte(self.content_type as u8);
        writer.put_float(self.position_x);
        writer.put_float(self.position_y);
        writer.put_float(self.position_z);
    }
}

/// Server wraps the client's ContentShareMessage with the sender's player ID and authoritative identity.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ServerContentShareMessage {
    pub player_id_message: PlayerIdMessage,
    pub sharer_uuid: String,
    pub sharer_display_name: String,
    pub content_share_message: ContentShareMessage,
}

impl ServerContentShareMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id_message.deserialize(reader)?;
        self.sharer_uuid = reader.get_string()?;
        self.sharer_display_name = reader.get_string()?;
        self.content_share_message.deserialize(reader)
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        self.player_id_message.serialize(writer);
        writer.put_string(&self.sharer_uuid);
        writer.put_string(&self.sharer_display_name);
        self.content_share_message.serialize(writer);
    }
}

/// Sent to remove a content share sphere from the world.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContentShareCleanupMessage {
    pub sphere_net_id: String,
}

impl ContentShareCleanupMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.sphere_net_id = reader.get_string()?;
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        writer.put_string(&self.sphere_net_id);
    }
}

/// Server wraps cleanup with sender player ID.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerContentShareCleanupMessage {
    pub player_id_message: PlayerIdMessage,
    pub content_share_cleanup_message: ContentShareCleanupMessage,
}

impl ServerContentShareCleanupMessage {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id_message.deserialize(reader)?;
        self.content_share_cleanup_message.deserialize(reader)
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        self.player_id_message.serialize(writer);
        self.content_share_cleanup_message.serialize(writer);
    }
}

/// Client→server request to change a flag on an already-spawned resource, and the
/// server→client broadcast that applies the change on every client.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ModifyResource {
    pub loaded_net_id: String,
    /// 0 = GameObject, 1 = Scene.
    pub mode: u8,
    pub r#static: bool,
    pub static_admin_locked: bool,
}

impl ModifyResource {
    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        writer.put_string(&self.loaded_net_id);
        writer.put_byte(self.mode);
        writer.put_bool(self.r#static);
        writer.put_bool(self.static_admin_locked);
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.loaded_net_id = reader.get_string()?;
        self.mode = reader.get_byte()?;
        self.r#static = reader.get_bool()?;
        self.static_admin_locked = reader.get_bool()?;
        Ok(())
    }
}

/// Client → server: load a resource.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalLoadResource {
    /// 0 = Game object under prop content limits, 1 = Scene, 2 = Game object under avatar content limits.
    pub mode: u8,
    /// A unique string that this object is linked with over the network.
    pub loaded_net_id: String,
    pub unlock_password: String,
    pub combined_url: String,
    pub uuid_of_creator: String,
    /// Normal users can't remove these items. Handled by the server.
    pub is_admin_locked: bool,
    pub position_x: f32,
    pub position_y: f32,
    pub position_z: f32,
    pub quaternion_x: f32,
    pub quaternion_y: f32,
    pub quaternion_z: f32,
    pub quaternion_w: f32,
    pub scale_x: f32,
    pub scale_y: f32,
    pub scale_z: f32,
    pub persist: bool,
    pub r#static: bool,
    pub static_admin_locked: bool,
    pub modify_scale: bool,
    /// 0 = Immediate, 2 = Synchronized, 3 = Predownload.
    pub load_strategy: u8,
}

impl LocalLoadResource {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.mode = reader.get_byte()?;
        self.loaded_net_id = reader.get_string()?;
        self.unlock_password = reader.get_string()?;
        self.combined_url = reader.get_string()?;
        self.uuid_of_creator = reader.get_string()?;
        self.is_admin_locked = reader.get_bool()?;
        self.persist = reader.get_bool()?;
        self.r#static = reader.get_bool()?;
        self.static_admin_locked = reader.get_bool()?;
        self.modify_scale = reader.get_bool()?;
        self.load_strategy = reader.get_byte()?;
        if self.mode == 0 {
            self.position_x = reader.get_float()?;
            self.position_y = reader.get_float()?;
            self.position_z = reader.get_float()?;
            self.quaternion_x = reader.get_float()?;
            self.quaternion_y = reader.get_float()?;
            self.quaternion_z = reader.get_float()?;
            self.quaternion_w = reader.get_float()?;
            self.scale_x = reader.get_float()?;
            self.scale_y = reader.get_float()?;
            self.scale_z = reader.get_float()?;
        }
        Ok(())
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        writer.put_byte(self.mode);
        writer.put_string(&self.loaded_net_id);
        writer.put_string(&self.unlock_password);
        writer.put_string(&self.combined_url);
        writer.put_string(&self.uuid_of_creator);
        writer.put_bool(self.is_admin_locked);
        writer.put_bool(self.persist);
        writer.put_bool(self.r#static);
        writer.put_bool(self.static_admin_locked);
        writer.put_bool(self.modify_scale);
        writer.put_byte(self.load_strategy);
        if self.mode == 0 {
            writer.put_float(self.position_x);
            writer.put_float(self.position_y);
            writer.put_float(self.position_z);
            writer.put_float(self.quaternion_x);
            writer.put_float(self.quaternion_y);
            writer.put_float(self.quaternion_z);
            writer.put_float(self.quaternion_w);
            writer.put_float(self.scale_x);
            writer.put_float(self.scale_y);
            writer.put_float(self.scale_z);
        }
    }
}

/// Sent from client to server to report preload readiness for a synchronized load.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PreloadReadyMessage {
    pub loaded_net_id: String,
    pub is_ready: bool,
}

impl PreloadReadyMessage {
    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        writer.put_string(&self.loaded_net_id);
        writer.put_bool(self.is_ready);
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.loaded_net_id = reader.get_string()?;
        self.is_ready = reader.get_bool()?;
        Ok(())
    }
}

/// Sent from server to all clients to signal that a preloaded resource should now be spawned.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpawnPreloadedMessage {
    pub loaded_net_id: String,
}

impl SpawnPreloadedMessage {
    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        writer.put_string(&self.loaded_net_id);
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.loaded_net_id = reader.get_string()?;
        Ok(())
    }
}

/// Single library entry the server pushes to a client on connect. Mode: 0=Avatar, 1=World, 2=Prop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerLibraryItem {
    pub mode: u8,
    pub url: String,
    pub password: String,
}

impl ServerLibraryItem {
    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        writer.put_byte(self.mode);
        writer.put_string(&self.url);
        writer.put_string(&self.password);
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.mode = reader.get_byte()?;
        self.url = reader.get_string()?;
        self.password = reader.get_string()?;
        Ok(())
    }
}

/// Wraps the full default-library list. Empty array is valid (means: no defaults).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ServerLibraryMessage {
    pub items: Vec<ServerLibraryItem>,
}

impl ServerLibraryMessage {
    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        writer.put_ushort(self.items.len() as u16);
        for item in self.items.iter_mut() {
            item.serialize(writer);
        }
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        let count = reader.get_ushort()?;
        self.items = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let mut item = ServerLibraryItem::default();
            item.deserialize(reader)?;
            self.items.push(item);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UnLoadResource {
    /// 0 = Game object, 1 = Scene.
    pub mode: u8,
    pub loaded_net_id: String,
}

impl UnLoadResource {
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> bool {
        let Some(mode) = reader.try_get_byte() else { return false };
        self.mode = mode;
        let Some(id) = reader.try_get_string() else { return false };
        self.loaded_net_id = id;
        true
    }

    pub fn serialize(&mut self, writer: &mut NetDataWriter) {
        writer.put_byte(self.mode);
        writer.put_string(&self.loaded_net_id);
    }
}
