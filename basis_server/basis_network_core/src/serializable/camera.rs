use crate::io::{NetDataReader, NetDataWriter, NetResult};

/// Sent reliably when a player's PIP camera is created or destroyed. Server stores this and
/// replays to late joiners.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CameraPIPStateMessage {
    pub player_id: u16,
    pub is_active: bool,
    pub position_x: f32,
    pub position_y: f32,
    pub position_z: f32,
    pub rotation_x: f32,
    pub rotation_y: f32,
    pub rotation_z: f32,
    pub rotation_w: f32,
}

impl CameraPIPStateMessage {
    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_ushort(self.player_id);
        writer.put_bool(self.is_active);
        if self.is_active {
            writer.put_float(self.position_x);
            writer.put_float(self.position_y);
            writer.put_float(self.position_z);
            writer.put_float(self.rotation_x);
            writer.put_float(self.rotation_y);
            writer.put_float(self.rotation_z);
            writer.put_float(self.rotation_w);
        }
        Ok(())
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id = reader.get_ushort()?;
        self.is_active = reader.get_bool()?;
        if self.is_active {
            self.position_x = reader.get_float()?;
            self.position_y = reader.get_float()?;
            self.position_z = reader.get_float()?;
            self.rotation_x = reader.get_float()?;
            self.rotation_y = reader.get_float()?;
            self.rotation_z = reader.get_float()?;
            self.rotation_w = reader.get_float()?;
        }
        Ok(())
    }
}

/// Position and rotation update for PIP camera.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CameraPIPPositionMessage {
    pub player_id: u16,
    pub position_x: f32,
    pub position_y: f32,
    pub position_z: f32,
    pub rotation_x: f32,
    pub rotation_y: f32,
    pub rotation_z: f32,
    pub rotation_w: f32,
}

impl CameraPIPPositionMessage {
    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_ushort(self.player_id);
        writer.put_float(self.position_x);
        writer.put_float(self.position_y);
        writer.put_float(self.position_z);
        writer.put_float(self.rotation_x);
        writer.put_float(self.rotation_y);
        writer.put_float(self.rotation_z);
        writer.put_float(self.rotation_w);
        Ok(())
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id = reader.get_ushort()?;
        self.position_x = reader.get_float()?;
        self.position_y = reader.get_float()?;
        self.position_z = reader.get_float()?;
        self.rotation_x = reader.get_float()?;
        self.rotation_y = reader.get_float()?;
        self.rotation_z = reader.get_float()?;
        self.rotation_w = reader.get_float()?;
        Ok(())
    }
}

/// Client -> server: camera state change (no PlayerID, server fills it from peer).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClientCameraPIPStateMessage {
    pub is_active: bool,
    pub position_x: f32,
    pub position_y: f32,
    pub position_z: f32,
    pub rotation_x: f32,
    pub rotation_y: f32,
    pub rotation_z: f32,
    pub rotation_w: f32,
}

impl ClientCameraPIPStateMessage {
    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_bool(self.is_active);
        if self.is_active {
            writer.put_float(self.position_x);
            writer.put_float(self.position_y);
            writer.put_float(self.position_z);
            writer.put_float(self.rotation_x);
            writer.put_float(self.rotation_y);
            writer.put_float(self.rotation_z);
            writer.put_float(self.rotation_w);
        }
        Ok(())
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.is_active = reader.get_bool()?;
        if self.is_active {
            self.position_x = reader.get_float()?;
            self.position_y = reader.get_float()?;
            self.position_z = reader.get_float()?;
            self.rotation_x = reader.get_float()?;
            self.rotation_y = reader.get_float()?;
            self.rotation_z = reader.get_float()?;
            self.rotation_w = reader.get_float()?;
        }
        Ok(())
    }
}

/// Client -> server: position and rotation update (no PlayerID, server fills it from peer).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ClientCameraPIPPositionMessage {
    pub position_x: f32,
    pub position_y: f32,
    pub position_z: f32,
    pub rotation_x: f32,
    pub rotation_y: f32,
    pub rotation_z: f32,
    pub rotation_w: f32,
}

impl ClientCameraPIPPositionMessage {
    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_float(self.position_x);
        writer.put_float(self.position_y);
        writer.put_float(self.position_z);
        writer.put_float(self.rotation_x);
        writer.put_float(self.rotation_y);
        writer.put_float(self.rotation_z);
        writer.put_float(self.rotation_w);
        Ok(())
    }

    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.position_x = reader.get_float()?;
        self.position_y = reader.get_float()?;
        self.position_z = reader.get_float()?;
        self.rotation_x = reader.get_float()?;
        self.rotation_y = reader.get_float()?;
        self.rotation_z = reader.get_float()?;
        self.rotation_w = reader.get_float()?;
        Ok(())
    }
}

/// Server -> clients: a player took a photo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CameraShutterSoundMessage {
    pub player_id: u16,
}

impl CameraShutterSoundMessage {
    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_ushort(self.player_id);
        Ok(())
    }
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id = reader.get_ushort()?;
        Ok(())
    }
}

/// Server -> clients: a player started a countdown timer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CameraCountdownMessage {
    pub player_id: u16,
    pub seconds: u8,
}

impl CameraCountdownMessage {
    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_ushort(self.player_id);
        writer.put_byte(self.seconds);
        Ok(())
    }
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.player_id = reader.get_ushort()?;
        self.seconds = reader.get_byte()?;
        Ok(())
    }
}

/// Client -> server: local player started a countdown timer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClientCameraCountdownMessage {
    pub seconds: u8,
}

impl ClientCameraCountdownMessage {
    pub fn serialize(&mut self, writer: &mut NetDataWriter) -> NetResult<()> {
        writer.put_byte(self.seconds);
        Ok(())
    }
    pub fn deserialize(&mut self, reader: &mut NetDataReader) -> NetResult<()> {
        self.seconds = reader.get_byte()?;
        Ok(())
    }
}
