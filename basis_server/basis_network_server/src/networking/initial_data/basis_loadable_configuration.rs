//! Port of `InitialData/BasisLoadableConfiguration.cs`.

use std::path::Path;

use basis_error::{BasisError, BasisResult, ErrorCode, ResultExt};

use super::{field, field_bool, field_string, parse_flat_xml, write_flat_xml, xml_files};

#[derive(Clone, Debug, PartialEq)]
pub struct BasisLoadableConfiguration {
    pub mode: u8,
    pub loaded_net_id: String,
    pub unlock_password: String,
    pub combined_url: String,
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
    pub modify_scale: bool,
}

impl Default for BasisLoadableConfiguration {
    fn default() -> Self {
        Self {
            mode: 0,
            loaded_net_id: String::new(),
            unlock_password: String::new(),
            combined_url: String::new(),
            position_x: 0.0,
            position_y: 0.0,
            position_z: 0.0,
            quaternion_x: 0.0,
            quaternion_y: 0.0,
            quaternion_z: 0.0,
            quaternion_w: 1.0,
            scale_x: 1.0,
            scale_y: 1.0,
            scale_z: 1.0,
            persist: false,
            modify_scale: false,
        }
    }
}

impl BasisLoadableConfiguration {
    pub const ROOT: &'static str = "BasisLoadableConfiguration";

    pub fn from_xml(xml: &str) -> BasisResult<Self> {
        let f = parse_flat_xml(xml, Self::ROOT).map_err(|e| BasisError::permanent(ErrorCode::Serialization, e.to_string()))?;
        let d = Self::default();
        Ok(Self {
            mode: field(&f, "Mode", d.mode),
            loaded_net_id: field_string(&f, "LoadedNetID"),
            unlock_password: field_string(&f, "UnlockPassword"),
            combined_url: field_string(&f, "CombinedURL"),
            position_x: field(&f, "PositionX", d.position_x),
            position_y: field(&f, "PositionY", d.position_y),
            position_z: field(&f, "PositionZ", d.position_z),
            quaternion_x: field(&f, "QuaternionX", d.quaternion_x),
            quaternion_y: field(&f, "QuaternionY", d.quaternion_y),
            quaternion_z: field(&f, "QuaternionZ", d.quaternion_z),
            quaternion_w: field(&f, "QuaternionW", d.quaternion_w),
            scale_x: field(&f, "ScaleX", d.scale_x),
            scale_y: field(&f, "ScaleY", d.scale_y),
            scale_z: field(&f, "ScaleZ", d.scale_z),
            persist: field_bool(&f, "Persist", d.persist),
            modify_scale: field_bool(&f, "ModifyScale", d.modify_scale),
        })
    }

    pub fn to_xml(&self) -> String {
        write_flat_xml(
            Self::ROOT,
            &[
                ("Mode", self.mode.to_string()),
                ("LoadedNetID", self.loaded_net_id.clone()),
                ("UnlockPassword", self.unlock_password.clone()),
                ("CombinedURL", self.combined_url.clone()),
                ("PositionX", self.position_x.to_string()),
                ("PositionY", self.position_y.to_string()),
                ("PositionZ", self.position_z.to_string()),
                ("QuaternionX", self.quaternion_x.to_string()),
                ("QuaternionY", self.quaternion_y.to_string()),
                ("QuaternionZ", self.quaternion_z.to_string()),
                ("QuaternionW", self.quaternion_w.to_string()),
                ("ScaleX", self.scale_x.to_string()),
                ("ScaleY", self.scale_y.to_string()),
                ("ScaleZ", self.scale_z.to_string()),
                ("Persist", self.persist.to_string()),
                ("ModifyScale", self.modify_scale.to_string()),
            ],
        )
    }

    pub fn load_all_from_folder(folder_path: &Path) -> BasisResult<Vec<Self>> {
        if !folder_path.is_dir() {
            return Err(BasisError::permanent(ErrorCode::NotFound, format!("The folder '{}' does not exist.", folder_path.display())));
        }
        let mut configurations = Vec::new();
        for file in xml_files(folder_path).with_context(|| format!("listing '{}'", folder_path.display()))? {
            let xml = std::fs::read_to_string(&file).with_context(|| format!("reading '{}'", file.display()))?;
            configurations.push(Self::from_xml(&xml).with_context(|| format!("parsing '{}'", file.display()))?);
        }
        Ok(configurations)
    }
}
