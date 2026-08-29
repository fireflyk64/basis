//! Port of `NameGenerator.cs`.

use rand::RngExt;

pub struct NameGenerator;

impl NameGenerator {
    pub const ADJECTIVES: [&'static str; 15] = ["Swift", "Brave", "Clever", "Fierce", "Nimble", "Silent", "Bold", "Lucky", "Strong", "Mighty", "Sneaky", "Fearless", "Wise", "Vicious", "Daring"];
    pub const NOUNS: [&'static str; 15] = ["Warrior", "Hunter", "Mage", "Rogue", "Paladin", "Shaman", "Knight", "Archer", "Monk", "Druid", "Assassin", "Sorcerer", "Ranger", "Guardian", "Berserker"];
    pub const TITLES: [&'static str; 10] = ["the Swift", "the Bold", "the Silent", "the Brave", "the Fierce", "the Wise", "the Protector", "the Shadow", "the Flame", "the Phantom"];
    pub const ANIMALS: [&'static str; 12] = ["Wolf", "Tiger", "Eagle", "Dragon", "Lion", "Bear", "Hawk", "Panther", "Raven", "Serpent", "Fox", "Falcon"];
    /// Colors with their names and hex codes for Unity's Rich Text
    pub const COLORS: [(&'static str, &'static str); 12] = [
        ("Red", "#FF0000"),
        ("Blue", "#0000FF"),
        ("Green", "#008000"),
        ("Yellow", "#FFFF00"),
        ("Black", "#000000"),
        ("White", "#FFFFFF"),
        ("Silver", "#C0C0C0"),
        ("Golden", "#FFD700"),
        ("Crimson", "#DC143C"),
        ("Azure", "#007FFF"),
        ("Emerald", "#50C878"),
        ("Amber", "#FFBF00"),
    ];

    pub fn generate_random_player_name() -> String {
        let mut rng = rand::rng();
        let adjective = Self::ADJECTIVES[rng.random_range(0..Self::ADJECTIVES.len())];
        let noun = Self::NOUNS[rng.random_range(0..Self::NOUNS.len())];
        let title = Self::TITLES[rng.random_range(0..Self::TITLES.len())];
        let (color_name, color_hex) = Self::COLORS[rng.random_range(0..Self::COLORS.len())];
        let animal = Self::ANIMALS[rng.random_range(0..Self::ANIMALS.len())];
        format!("{adjective}{noun} {title} of the <color={color_hex}>{color_name}</color> {animal}")
    }
}
