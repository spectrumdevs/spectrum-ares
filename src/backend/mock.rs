use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::{Duration, Instant};

use crate::backend::{AudioBackend, BackendKind, BackendWorker};
use crate::dsp::bands::{clamp_normalized, normalize_frame, DEFAULT_BAND_COUNT};
use crate::dsp::smoothing::ExponentialSmoother;
use crate::error::AresError;
use crate::state::{AnalyzerSnapshot, BandFrame, SharedAnalyzerState};

#[derive(Debug, Default)]
pub struct MockBackend;

impl MockBackend {
    pub fn new() -> Self {
        Self
    }
}

impl AudioBackend for MockBackend {
    fn start(&self, shared: Arc<SharedAnalyzerState>) -> Result<BackendWorker, AresError> {
        BackendWorker::spawn(
            BackendKind::Mock,
            "spectrum-ares-mock",
            move |stop_requested| {
                run_mock_analyzer(shared, stop_requested);
                Ok(())
            },
        )
    }
}

fn run_mock_analyzer(shared: Arc<SharedAnalyzerState>, stop_requested: Arc<AtomicBool>) {
    let started_at = Instant::now();
    let smoother = ExponentialSmoother::new(0.38, 0.12);
    let mut previous = [0.0_f32; DEFAULT_BAND_COUNT];

    while !stop_requested.load(Ordering::Acquire) {
        let elapsed = started_at.elapsed().as_secs_f32();
        let raw = generate_mock_bands(elapsed);
        let mut bands = [0.0_f32; DEFAULT_BAND_COUNT];

        for index in 0..DEFAULT_BAND_COUNT {
            bands[index] = smoother.apply(previous[index], raw[index]);
        }

        normalize_frame(&mut bands);
        previous = bands;

        let rms = calculate_rms(&bands);
        let peak = bands.iter().copied().fold(0.0_f32, f32::max);

        shared.write_snapshot(AnalyzerSnapshot { bands, rms, peak });
        thread::sleep(Duration::from_millis(16));
    }
}

fn generate_mock_bands(elapsed: f32) -> BandFrame {
    let mut bands = [0.0_f32; DEFAULT_BAND_COUNT];
    let count = (DEFAULT_BAND_COUNT - 1) as f32;
    let sweep_center = ((elapsed * 0.85).sin() * 0.5 + 0.5) * count;
    let bass_pulse = (elapsed * 3.7).sin() * 0.5 + 0.5;
    let beat_gate = if (elapsed * 2.0).fract() < 0.18 {
        1.0
    } else {
        0.0
    };

    for (index, band) in bands.iter_mut().enumerate() {
        let position = index as f32 / count;
        let distance = (index as f32 - sweep_center).abs();
        let sweep = (1.0 - distance / 16.0).max(0.0).powf(1.7) * 0.65;
        let bass = (1.0 - position).powf(3.5) * (0.18 + bass_pulse * 0.55);
        let shimmer = ((elapsed * 7.0 + index as f32 * 0.41).sin() * 0.5 + 0.5) * 0.12;
        let kick = beat_gate * (1.0 - position).powf(5.0) * 0.22;

        *band = clamp_normalized(sweep + bass + shimmer + kick);
    }

    bands
}

fn calculate_rms(bands: &[f32; DEFAULT_BAND_COUNT]) -> f32 {
    let sum = bands.iter().map(|value| value * value).sum::<f32>();
    clamp_normalized((sum / bands.len() as f32).sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_bands_are_normalized() {
        let bands = generate_mock_bands(0.5);
        assert!(bands.iter().all(|value| (0.0..=1.0).contains(value)));
    }
}
