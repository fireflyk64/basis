//! Port of `InitialData/BasisDefaultLibraryConfiguration.cs`.

use std::path::Path;

use basis_error::{BasisError, BasisResult, ErrorCode, ResultExt};

use super::{field, field_string, parse_flat_xml, write_flat_xml, xml_files};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BasisDefaultLibraryConfiguration {
    /// Mirrors BundledContentHolder.Mode on the client: 0=Avatar, 1=World, 2=Prop.
    pub mode: u8,
    pub url: String,
    pub password: String,
}

impl BasisDefaultLibraryConfiguration {
    pub const ROOT: &'static str = "BasisDefaultLibraryConfiguration";

    pub fn from_xml(xml: &str) -> BasisResult<Self> {
        let fields = parse_flat_xml(xml, Self::ROOT).map_err(|e| BasisError::permanent(ErrorCode::Serialization, e.to_string()))?;
        Ok(Self { mode: field(&fields, "Mode", 0), url: field_string(&fields, "Url"), password: field_string(&fields, "Password") })
    }

    pub fn to_xml(&self) -> String {
        write_flat_xml(Self::ROOT, &[("Mode", self.mode.to_string()), ("Url", self.url.clone()), ("Password", self.password.clone())])
    }

    /// Every entry in `folder_path`. A missing folder or an unreadable file is an error, as the
    /// C# `DirectoryNotFoundException` / serializer exception was.
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
