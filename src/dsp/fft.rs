#![allow(dead_code)]

pub const DEFAULT_FFT_FRAME_SIZE: usize = 4096;
pub const DEFAULT_FFT_HOP_SIZE: usize = 1024;

#[derive(Debug, Clone, Copy)]
pub struct FftPlan {
    pub sample_rate: u32,
    pub frame_size: usize,
}

impl FftPlan {
    pub fn new(sample_rate: u32, frame_size: usize) -> Self {
        Self {
            sample_rate,
            frame_size,
        }
    }
}

pub fn hann_window_coefficients(len: usize) -> Vec<f32> {
    if len <= 1 {
        return vec![1.0; len];
    }

    let denominator = (len - 1) as f32;
    (0..len)
        .map(|index| {
            let phase = std::f32::consts::TAU * index as f32 / denominator;
            0.5 * (1.0 - phase.cos())
        })
        .collect()
}

pub fn apply_hann_window(samples: &mut [f32]) {
    let coefficients = hann_window_coefficients(samples.len());
    for (sample, coefficient) in samples.iter_mut().zip(coefficients.into_iter()) {
        *sample *= coefficient;
    }
}
