use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
#[repr(u8)]
pub enum BasisUserRestrictionMode {
    #[default]
    Normal = 0,
    BanList = 1,
    AllowList = 2,
    RejoinOnly = 3,
}

impl BasisUserRestrictionMode {
    pub fn from_byte(value: u8) -> Self {
        match value {
            1 => Self::BanList,
            2 => Self::AllowList,
            3 => Self::RejoinOnly,
            _ => Self::Normal,
        }
    }

    pub fn as_byte(self) -> u8 {
        self as u8
    }

    /// The names the C# `XmlSerializer` writes for this enum.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::BanList => "BanList",
            Self::AllowList => "AllowList",
            Self::RejoinOnly => "RejoinOnly",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "Normal" => Some(Self::Normal),
            "BanList" => Some(Self::BanList),
            "AllowList" => Some(Self::AllowList),
            "RejoinOnly" => Some(Self::RejoinOnly),
            other => other.parse::<u8>().ok().map(Self::from_byte),
        }
    }
}

impl std::fmt::Display for BasisUserRestrictionMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
