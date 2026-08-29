/// The parameters the distance sweep turns into a per-pair send interval and quality tier.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BasisDistanceSolveParameters {
    pub high_distance_sq: f32,
    pub medium_distance_sq: f32,
    pub low_distance_sq: f32,
    pub base_multiplier: f32,
    pub increase_rate: f32,
    pub base_interval_ms: i32,
}

/// One slice of the sweep: receivers `[slice_start, slice_end)` of a roster of `player_count`,
/// measured against every player in that roster. Positions are dense and in roster order.
#[derive(Clone, Debug, Default)]
pub struct BasisDistanceSolveRequest {
    pub pos_x: Vec<f32>,
    pub pos_y: Vec<f32>,
    pub pos_z: Vec<f32>,
    pub player_count: usize,
    pub slice_start: usize,
    pub slice_end: usize,
    pub parameters: BasisDistanceSolveParameters,
}

impl BasisDistanceSolveRequest {
    pub fn slice_length(&self) -> usize {
        self.slice_end - self.slice_start
    }

    pub fn result_length(&self) -> usize {
        self.slice_length() * self.player_count
    }
}

/// A backend that can produce the distance cache. Results are two bytes per pair at
/// `(slice_index * player_count) + j`.
pub trait IBasisDistanceSolver: Send + Sync {
    fn backend(&self) -> &str;
    fn device_name(&self) -> &str;
    fn solve(&self, request: &BasisDistanceSolveRequest, interval_byte: &mut [u8], quality: &mut [u8]);
}
