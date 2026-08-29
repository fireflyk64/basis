//! Port of `Networking/InitialData`: the XML-described resources and library items loaded at boot.
pub mod basis_default_library_configuration;
pub mod basis_default_library_loader;
pub mod basis_loadable_configuration;
pub mod basis_loadable_loader;

pub use basis_default_library_configuration::BasisDefaultLibraryConfiguration;
pub use basis_default_library_loader::BasisDefaultLibraryLoader;
pub use basis_loadable_configuration::BasisLoadableConfiguration;
pub use basis_loadable_loader::BasisLoadableLoader;

use std::collections::HashMap;

use basis_network_core::configuration::ConfigXmlError;

/// Reads a flat `<Root><Field>value</Field>...</Root>` document (what `XmlSerializer` writes for
/// these classes) into a name → text map. Unknown elements are kept; nested ones are flattened.
pub(crate) fn parse_flat_xml(xml: &str, root: &str) -> Result<HashMap<String, String>, ConfigXmlError> {
    use quick_xml::Reader;
    use quick_xml::events::Event;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    let mut fields = HashMap::new();
    let mut saw_root = false;
    let mut depth = 0usize;
    loop {
        match reader.read_event_into(&mut buf).map_err(|e| ConfigXmlError::Malformed(e.to_string()))? {
            Event::Start(e) => {
                depth += 1;
                let name = e.name().as_ref().to_owned();
                if depth == 1 {
                    if name != root {
                        return Err(ConfigXmlError::WrongRoot(name));
                    }
                    saw_root = true;
                } else {
                    let end = e.to_end().into_owned();
                    let text = reader.read_text(end.name()).map_err(|e| ConfigXmlError::Malformed(e.to_string()))?;
                    let text = quick_xml::escape::unescape(&text).map(|c| c.into_owned()).unwrap_or_else(|_| text.to_string());
                    depth -= 1;
                    fields.insert(name, text.trim().to_string());
                }
            }
            Event::Empty(e) => {
                if depth == 0 {
                    let name = e.name().as_ref().to_owned();
                    if name != root {
                        return Err(ConfigXmlError::WrongRoot(name));
                    }
                    saw_root = true;
                } else {
                    fields.insert(e.name().as_ref().to_owned(), String::new());
                }
            }
            Event::End(_) => depth = depth.saturating_sub(1),
            Event::Eof => {
                if depth > 0 {
                    return Err(ConfigXmlError::Malformed("unexpected end of document".to_string()));
                }
                break;
            }
            _ => {}
        }
        buf.clear();
    }
    if !saw_root {
        return Err(ConfigXmlError::NoRoot);
    }
    Ok(fields)
}

/// Writes a flat document the way `XmlSerializer` does.
pub(crate) fn write_flat_xml(root: &str, fields: &[(&str, String)]) -> String {
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    xml.push('<');
    xml.push_str(root);
    xml.push_str(" xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\">\n");
    for (name, value) in fields {
        xml.push_str("  <");
        xml.push_str(name);
        xml.push('>');
        xml.push_str(&quick_xml::escape::escape(value.as_str()));
        xml.push_str("</");
        xml.push_str(name);
        xml.push_str(">\n");
    }
    xml.push_str("</");
    xml.push_str(root);
    xml.push('>');
    xml
}

pub(crate) fn field<T: std::str::FromStr>(fields: &HashMap<String, String>, name: &str, default: T) -> T {
    fields.get(name).and_then(|v| v.trim().parse::<T>().ok()).unwrap_or(default)
}

pub(crate) fn field_bool(fields: &HashMap<String, String>, name: &str, default: bool) -> bool {
    fields.get(name).map(|v| v.trim().eq_ignore_ascii_case("true")).unwrap_or(default)
}

pub(crate) fn field_string(fields: &HashMap<String, String>, name: &str) -> String {
    fields.get(name).cloned().unwrap_or_default()
}

/// Every `*.xml` directly inside `folder`, sorted for a stable load order.
pub(crate) fn xml_files(folder: &std::path::Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut files: Vec<_> = std::fs::read_dir(folder)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("xml")))
        .collect();
    files.sort();
    Ok(files)
}
