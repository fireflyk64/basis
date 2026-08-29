//! Port of `Randomizer.cs`.

use basis_network_core::mathematics::Vector3;
use rand::RngExt;

pub struct Randomizer;

impl Randomizer {
    /// Per-step jitter for the random walk. Deliberately small — this is the "moving about"
    /// signal, not the spread.
    pub fn get_random_offset() -> Vector3 {
        let mut rng = rand::rng();
        let mut axis = || ((rng.random::<f64>() * 2.0 - 1.0) / 4.0) as f32;
        Vector3::new(axis(), axis(), axis())
    }

    /// A spawn point distributed uniformly over a horizontal disc of the given radius. The sqrt
    /// keeps the distribution uniform by AREA; Y stays near standing height, since players occupy
    /// a floor rather than a sphere.
    pub fn get_spawn_position(radius_meters: f32) -> Vector3 {
        if radius_meters <= 0.0 {
            return Self::get_random_offset();
        }
        let mut rng = rand::rng();
        let angle = rng.random::<f64>() * std::f64::consts::PI * 2.0;
        let radius = radius_meters as f64 * rng.random::<f64>().sqrt();
        Vector3::new((angle.cos() * radius) as f32, 1.0 + (rng.random::<f64>() * 0.2 - 0.1) as f32, (angle.sin() * radius) as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spawn_stays_inside_the_disc() {
        for _ in 0..1000 {
            let p = Randomizer::get_spawn_position(40.0);
            assert!((p.x * p.x + p.z * p.z).sqrt() <= 40.0 + 1e-3);
            assert!((0.9..=1.1).contains(&p.y));
        }
        let o = Randomizer::get_random_offset();
        assert!(o.x.abs() <= 0.25 && o.y.abs() <= 0.25 && o.z.abs() <= 0.25);
    }
}
