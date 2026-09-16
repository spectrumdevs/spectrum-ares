pub const DEFAULT_BAND_COUNT: usize = 64;
#[allow(dead_code)]
pub const MIN_ANALYSIS_HZ: f32 = 40.0;
#[allow(dead_code)]
pub const MAX_ANALYSIS_HZ: f32 = 16_000.0;

pub fn clamp_normalized(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

pub fn normalize_frame(values: &mut [f32]) {
    for value in values {
        *value = clamp_normalized(*value);
    }
}
