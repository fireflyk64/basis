//! Port of `Security/BasisBanList.cs`: a line-delimited, file-backed set of banned UUIDs.

use std::path::{Path, PathBuf};

use basis_network_core::BNL;
use dashmap::DashMap;

pub struct BasisBanList {
    banned_players: DashMap<String, ()>,
    file_path: Option<PathBuf>,
}

impl BasisBanList {
    pub fn with_file(path: impl Into<PathBuf>) -> Self {
        let list = Self { banned_players: DashMap::new(), file_path: Some(path.into()) };
        if let Err(e) = list.load_ban_list() {
            BNL::log_warning(format!("Could not load the ban list: {e}"));
        }
        list
    }

    pub fn in_memory() -> Self {
        Self { banned_players: DashMap::new(), file_path: None }
    }

    pub fn file_path(&self) -> Option<&Path> {
        self.file_path.as_deref()
    }

    fn load_ban_list(&self) -> std::io::Result<usize> {
        self.banned_players.clear();
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
                self.banned_players.insert(trimmed.to_string(), ());
                count += 1;
            }
        }
        Ok(count)
    }

    pub fn is_banned(&self, player_id: &str) -> bool {
        self.banned_players.contains_key(player_id)
    }

    pub fn len(&self) -> usize {
        self.banned_players.len()
    }

    pub fn is_empty(&self) -> bool {
        self.banned_players.is_empty()
    }

    pub fn reload_ban_list(&self) -> std::io::Result<()> {
        self.load_ban_list()?;
        BNL::log("Ban list reloaded.");
        Ok(())
    }

    pub fn add_to_ban_list(&self, player_id: &str) -> std::io::Result<bool> {
        if player_id.is_empty() || self.banned_players.contains_key(player_id) {
            return Ok(false);
        }
        self.banned_players.insert(player_id.to_string(), ());
        if let Some(path) = &self.file_path {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
            writeln!(file, "{player_id}")?;
        }
        BNL::log(format!("{player_id} added to ban list."));
        Ok(true)
    }

    pub fn remove_from_ban_list(&self, player_id: &str) -> std::io::Result<bool> {
        if self.banned_players.remove(player_id).is_none() {
            return Ok(false);
        }
        self.save_ban_list()?;
        BNL::log(format!("{player_id} removed from ban list."));
        Ok(true)
    }

    fn save_ban_list(&self) -> std::io::Result<()> {
        let Some(path) = &self.file_path else {
            return Ok(());
        };
        let mut text = String::new();
        for entry in self.banned_players.iter() {
            text.push_str(entry.key());
            text.push('\n');
        }
        std::fs::write(path, text)
    }

    pub fn entries(&self) -> Vec<String> {
        self.banned_players.iter().map(|e| e.key().clone()).collect()
    }
}
