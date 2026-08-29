pub struct MathExtensions;

impl MathExtensions {
    pub fn clamp_f32(value: f32, min: f32, max: f32) -> f32 {
        if value < min {
            return min;
        }
        if value > max {
            return max;
        }
        value
    }

    pub fn clamp_i32(value: i32, min: i32, max: i32) -> i32 {
        if value < min {
            return min;
        }
        if value > max {
            return max;
        }
        value
    }

    pub fn clamp_f64(value: f64, min: f64, max: f64) -> f64 {
        if value < min {
            return min;
        }
        if value > max {
            return max;
        }
        value
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vector3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vector3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    /// Squared magnitude (squared length of a vector)
    pub fn squared_magnitude(&self) -> f32 {
        self.x * self.x + self.y * self.y + self.z * self.z
    }
}

impl std::ops::Sub for Vector3 {
    type Output = Vector3;
    fn sub(self, b: Vector3) -> Vector3 {
        Vector3::new(self.x - b.x, self.y - b.y, self.z - b.z)
    }
}

impl std::ops::Add for Vector3 {
    type Output = Vector3;
    fn add(self, b: Vector3) -> Vector3 {
        Vector3::new(self.x + b.x, self.y + b.y, self.z + b.z)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vector4 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Quaternion {
    pub value: Vector4,
}

impl Quaternion {
    pub const fn new(x: f32, y: f32, z: f32, w: f32) -> Self {
        Self { value: Vector4 { x, y, z, w } }
    }
}

#[allow(non_camel_case_types)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct float3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}
