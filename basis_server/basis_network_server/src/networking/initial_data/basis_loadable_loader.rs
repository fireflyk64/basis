//! Port of `InitialData/BasisLoadableLoader.cs`.

use std::path::Path;

use basis_error::BasisResult;
use basis_network_core::BNL;
use basis_network_core::SerializableBasis::LocalLoadResource;
use basis_network_core::configuration::Configuration;

use super::BasisLoadableConfiguration;
use crate::resources::BasisNetworkResourceManagement;

pub struct BasisLoadableLoader;

impl BasisLoadableLoader {
    pub const EXAMPLE_XML: &'static str = r#"<BasisLoadableConfiguration>
    <!-- Mode: 0 = Prop (spawned at the transform below), 1 = Scene (transform ignored), 2 = Avatar (transform ignored) -->
    <Mode>0</Mode>
    <!-- Network ID -->
    <LoadedNetID></LoadedNetID>
    <!-- Unlock password -->
    <UnlockPassword></UnlockPassword>
    <!-- Combined URl -->
    <CombinedURL></CombinedURL>
    <!-- Local load flag -->
    <IsLocalLoad>false</IsLocalLoad>

    <!-- Position values -->
    <PositionX>0</PositionX>
    <PositionY>0</PositionY>
    <PositionZ>0</PositionZ>

    <!-- Quaternion values -->
    <QuaternionX>0</QuaternionX>
    <QuaternionY>0</QuaternionY>
    <QuaternionZ>0</QuaternionZ>
    <QuaternionW>1</QuaternionW>

    <!-- Scale values -->
    <ScaleX>1</ScaleX>
    <ScaleY>1</ScaleY>
    <ScaleZ>1</ScaleZ>

    <!-- Persist flag -->
    <Persist>false</Persist>
</BasisLoadableConfiguration>"#;

    /// Loads every resource description under `folder_name` next to the executable (created with
    /// an example file when missing) and registers each with the resource manager. Failures are
    /// logged, as the C# caught and printed them.
    pub fn load_xml(folder_name: &str) {
        let folder = Configuration::base_directory().join(folder_name);
        if let Err(e) = Self::try_load_xml(&folder) {
            BNL::log_error(format!("Error: {e}"));
        }
    }

    fn try_load_xml(folder: &Path) -> BasisResult<()> {
        use basis_error::ResultExt;
        if !folder.is_dir() {
            std::fs::create_dir_all(folder).with_context(|| format!("creating '{}'", folder.display()))?;
            BNL::log(format!("Folder created successfully: {}", folder.display()));
            let example = folder.join("ExampleConfigdisabled.xml[remove]");
            std::fs::write(&example, Self::EXAMPLE_XML).with_context(|| format!("writing '{}'", example.display()))?;
            BNL::log(format!("Example XML file created at: {}", example.display()));
        }
        for config in BasisLoadableConfiguration::load_all_from_folder(folder)? {
            BNL::log(format!("CombinedURL: {}, LoadAssetPassword: {}", config.combined_url, config.unlock_password));
            let mut llr = Self::from_basis_loadable_configuration(&config);
            if llr.loaded_net_id.is_empty() {
                llr.loaded_net_id = Self::generate_unique_id();
                BNL::log(format!("No Network Id Assigned Generated to be {}", llr.loaded_net_id));
            }
            BasisNetworkResourceManagement::load_resource(llr);
        }
        Ok(())
    }

    /// A dashless GUID followed by the UTC date (`yyyyMMdd`).
    pub fn generate_unique_id() -> String {
        let guid = uuid::Uuid::new_v4().simple().to_string();
        format!("{guid}{}", crate::util::utc_today().replace('-', ""))
    }

    pub fn from_basis_loadable_configuration(config: &BasisLoadableConfiguration) -> LocalLoadResource {
        LocalLoadResource {
            mode: config.mode,
            loaded_net_id: config.loaded_net_id.clone(),
            unlock_password: config.unlock_password.clone(),
            combined_url: config.combined_url.clone(),
            position_x: config.position_x,
            position_y: config.position_y,
            position_z: config.position_z,
            quaternion_x: config.quaternion_x,
            quaternion_y: config.quaternion_y,
            quaternion_z: config.quaternion_z,
            quaternion_w: config.quaternion_w,
            scale_x: config.scale_x,
            scale_y: config.scale_y,
            scale_z: config.scale_z,
            persist: config.persist,
            modify_scale: config.modify_scale,
            is_admin_locked: true,
            ..Default::default()
        }
    }
}
