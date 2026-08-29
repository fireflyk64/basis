//! Port of `InitialData/BasisDefaultLibraryLoader.cs`.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use basis_network_core::BNL;
use basis_network_core::configuration::Configuration;
use parking_lot::RwLock;

use super::BasisDefaultLibraryConfiguration;

static LOADED_ITEMS: LazyLock<RwLock<Vec<BasisDefaultLibraryConfiguration>>> = LazyLock::new(|| RwLock::new(Vec::new()));

pub struct BasisDefaultLibraryLoader;

impl BasisDefaultLibraryLoader {
    pub const EXAMPLE_XML: &'static str = r#"<BasisDefaultLibraryConfiguration>
    <!-- 0 = Avatar, 1 = World, 2 = Prop -->
    <Mode>0</Mode>
    <!-- Bee file URL -->
    <Url></Url>
    <!-- Unlock password -->
    <Password></Password>
</BasisDefaultLibraryConfiguration>"#;

    pub fn loaded_items() -> Vec<BasisDefaultLibraryConfiguration> {
        LOADED_ITEMS.read().clone()
    }

    fn folder(folder_name: &str) -> PathBuf {
        Configuration::base_directory().join(folder_name)
    }

    /// Loads every library entry under `folder_name` (created with an example file when missing).
    /// Failures are logged, as the C# caught and printed them.
    pub fn load_xml(folder_name: &str) {
        if let Err(e) = Self::try_load_xml(&Self::folder(folder_name)) {
            BNL::log_error(format!("Error loading default library: {e}"));
        }
    }

    fn try_load_xml(folder: &Path) -> std::io::Result<()> {
        if !folder.is_dir() {
            std::fs::create_dir_all(folder)?;
            BNL::log(format!("Folder created successfully: {}", folder.display()));
            let example = folder.join("ExampleAvatardisabled.xml[remove]");
            std::fs::write(&example, Self::EXAMPLE_XML)?;
            BNL::log(format!("Example XML file created at: {}", example.display()));
        }

        LOADED_ITEMS.write().clear();

        let configurations =
            BasisDefaultLibraryConfiguration::load_all_from_folder(folder).map_err(|e| std::io::Error::other(e.to_string()))?;
        let mut loaded = LOADED_ITEMS.write();
        for config in configurations {
            if config.url.is_empty() {
                BNL::log_error("Skipping default library entry with empty Url");
                continue;
            }
            BNL::log(format!("Default library entry loaded: Mode={}, Url={}", config.mode, config.url));
            loaded.push(config);
        }
        BNL::log(format!("Default library loaded with {} item(s).", loaded.len()));
        Ok(())
    }

    /// Persists a single entry as a new XML file under the configured folder and appends it to
    /// the in-memory list. Returns the absolute path written, or an empty string on failure.
    pub fn save_item(folder_name: &str, config: &BasisDefaultLibraryConfiguration) -> String {
        if config.url.trim().is_empty() {
            BNL::log_error("Refusing to save default library entry with empty Url.");
            return String::new();
        }
        let folder = Self::folder(folder_name);
        let written = std::fs::create_dir_all(&folder).and_then(|_| {
            let full_path = folder.join(Self::build_unique_file_name(&folder, config));
            std::fs::write(&full_path, config.to_xml()).map(|_| full_path)
        });
        match written {
            Ok(full_path) => {
                LOADED_ITEMS.write().push(config.clone());
                BNL::log(format!("Default library entry saved: {}", full_path.display()));
                full_path.to_string_lossy().into_owned()
            }
            Err(e) => {
                BNL::log_error(format!("Failed to save default library entry: {e}"));
                String::new()
            }
        }
    }

    /// Removes every persisted default-library XML whose Url matches (case-insensitive) and drops
    /// matching entries from the in-memory list. Returns the number of files deleted.
    pub fn remove_item(folder_name: &str, url: &str) -> i32 {
        if url.trim().is_empty() {
            BNL::log_error("Refusing to remove default library entry with empty Url.");
            return 0;
        }
        let mut removed = 0;
        let folder = Self::folder(folder_name);
        if folder.is_dir() {
            match super::xml_files(&folder) {
                Ok(files) => {
                    for file in files {
                        let config = std::fs::read_to_string(&file)
                            .map_err(|e| e.to_string())
                            .and_then(|xml| BasisDefaultLibraryConfiguration::from_xml(&xml).map_err(|e| e.to_string()));
                        let config = match config {
                            Ok(config) => config,
                            Err(e) => {
                                BNL::log_error(format!("Skipping unreadable default library file {}: {e}", file.display()));
                                continue;
                            }
                        };
                        if config.url.eq_ignore_ascii_case(url) {
                            match std::fs::remove_file(&file) {
                                Ok(()) => {
                                    removed += 1;
                                    BNL::log(format!("Default library entry removed: {}", file.display()));
                                }
                                Err(e) => BNL::log_error(format!("Failed to delete default library file {}: {e}", file.display())),
                            }
                        }
                    }
                }
                Err(e) => BNL::log_error(format!("Failed to remove default library entry: {e}")),
            }
        }
        LOADED_ITEMS.write().retain(|c| !c.url.eq_ignore_ascii_case(url));
        removed
    }

    fn build_unique_file_name(folder: &Path, config: &BasisDefaultLibraryConfiguration) -> String {
        let mode_name = match config.mode {
            0 => "avatar",
            1 => "world",
            2 => "prop",
            _ => "item",
        };
        let stamp = Self::utc_stamp();
        let mut base_name = format!("{mode_name}_{stamp}.xml");
        // Defensive: collisions are virtually impossible with millisecond precision but two
        // clicks within the same millisecond would clobber. Append a counter.
        let mut counter = 1;
        while folder.join(&base_name).exists() {
            base_name = format!("{mode_name}_{stamp}_{counter}.xml");
            counter += 1;
        }
        base_name
    }

    /// `yyyyMMddHHmmssfff` in UTC.
    fn utc_stamp() -> String {
        crate::util::utc_now_compact_stamp()
    }
}
