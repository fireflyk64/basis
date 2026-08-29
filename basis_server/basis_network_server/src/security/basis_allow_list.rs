//! Port of `Security/BasisAllowList.cs`: a line-delimited, file-backed set of allowed UUIDs.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use basis_network_core::BNL;
use dashmap::DashMap;

pub struct BasisAllowList {
    allowlisted_players: DashMap<String, ()>,
    /// `None` = in-memory only (the host disabled file support).
    file_path: Option<PathBuf>,
}

static EMPTY: LazyLock<DashMap<String, ()>> = LazyLock::new(DashMap::new);

impl BasisAllowList {
    /// Loads `path` (missing is fine: empty list). A read failure is logged; the list starts
    /// empty and can be reloaded.
    pub fn with_file(path: impl Into<PathBuf>) -> Self {
        let list = Self { allowlisted_players: DashMap::new(), file_path: Some(path.into()) };
        if let Err(e) = list.load() {
            BNL::log_warning(format!("Could not load the allowlist: {e}"));
        }
        list
    }

    pub fn in_memory() -> Self {
        Self { allowlisted_players: DashMap::new(), file_path: None }
    }

    pub fn file_path(&self) -> Option<&Path> {
        self.file_path.as_deref()
    }

    fn load(&self) -> std::io::Result<usize> {
        self.allowlisted_players.clear();
        let Some(path) = &self.file_path else {
            return Ok(0);
        };
        if !path.exists() {
            return Ok(0);
        }
        let text = std::fs::read_to_string(path)?;
        let mut count = 0;
        for line in text.lines() {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                self.allowlisted_players.insert(trimmed.to_string(), ());
                count += 1;
            }
        }
        Ok(count)
    }

    pub fn is_allowed(&self, player_id: &str) -> bool {
        self.allowlisted_players.contains_key(player_id)
    }

    pub fn len(&self) -> usize {
        self.allowlisted_players.len()
    }

    pub fn is_empty(&self) -> bool {
        self.allowlisted_players.is_empty()
    }

    pub fn reload_allowlist(&self) -> std::io::Result<()> {
        self.load()?;
        BNL::log("Allowlist reloaded.");
        Ok(())
    }

    /// Adds `player_id`; `Ok(false)` when it was already present.
    pub fn add_to_allowlist(&self, player_id: &str) -> std::io::Result<bool> {
        // The store is line-delimited, so an embedded newline would turn one requested entry
        // into several while the admin UI still reports one.
        let player_id: String = player_id.chars().filter(|c| *c != '\r' && *c != '\n').collect::<String>().trim().to_string();
        if player_id.is_empty() {
            return Ok(false);
        }
        if self.allowlisted_players.contains_key(&player_id) {
            return Ok(false);
        }
        self.allowlisted_players.insert(player_id.clone(), ());
        if let Some(path) = &self.file_path {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
            writeln!(file, "{player_id}")?;
        }
        BNL::log(format!("{player_id} added to allowlist."));
        Ok(true)
    }

    /// Removes `player_id`; `Ok(false)` when it was not present.
    pub fn remove_from_allowlist(&self, player_id: &str) -> std::io::Result<bool> {
        if self.allowlisted_players.remove(player_id).is_none() {
            return Ok(false);
        }
        self.save_allowlist()?;
        BNL::log(format!("{player_id} removed from allowlist."));
        Ok(true)
    }

    fn save_allowlist(&self) -> std::io::Result<()> {
        let Some(path) = &self.file_path else {
            return Ok(());
        };
        let mut text = String::new();
        for entry in self.allowlisted_players.iter() {
            text.push_str(entry.key());
            text.push('\n');
        }
        std::fs::write(path, text)
    }

    /// Every entry, for admin listings.
    pub fn entries(&self) -> Vec<String> {
        self.allowlisted_players.iter().map(|e| e.key().clone()).collect()
    }

    #[allow(dead_code)]
    fn empty() -> &'static DashMap<String, ()> {
        &EMPTY
    }
}
