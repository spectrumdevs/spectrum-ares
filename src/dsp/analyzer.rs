use std::collections::VecDeque;
use std::sync::Arc;

use rustfft::num_complex::Complex32;
use rustfft::{Fft, FftPlanner};

use crate::dsp::bands::{
    clamp_normalized, normalize_frame, DEFAULT_BAND_COUNT, MAX_ANALYSIS_HZ, MIN_ANALYSIS_HZ,
};
use crate::dsp::fft::{
    hann_window_coefficients, FftPlan, DEFAULT_FFT_FRAME_SIZE, DEFAULT_FFT_HOP_SIZE,
};
use crate::dsp::gain::{AdaptiveGain, AdaptiveGainConfig};
use crate::dsp::smoothing::ExponentialSmoother;

const DEFAULT_DB_FLOOR: f32 = -90.0;
const DEFAULT_DB_CEILING: f32 = 0.0;
const MAGNITUDE_EPSILON: f32 = 1.0e-12;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalysisSnapshot {
    pub bands: [f32; DEFAULT_BAND_COUNT],
    pub rms: f32,
    pub peak: f32,
}

impl AnalysisSnapshot {
    #[allow(dead_code)]
    pub fn silence() -> Self {
        Self {
            bands: [0.0; DEFAULT_BAND_COUNT],
            rms: 0.0,
            peak: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct AnalyzerConfig {
    pub sample_rate: u32,
    pub frame_size: usize,
    pub hop_size: usize,
    pub min_frequency_hz: f32,
    pub max_frequency_hz: f32,
    pub smoothing_attack: f32,
    pub smoothing_release: f32,
    pub db_floor: f32,
    pub db_ceiling: f32,
    pub adaptive_gain: AdaptiveGainConfig,
}

impl AnalyzerConfig {
    pub fn default_for_sample_rate(sample_rate: u32) -> Self {
        Self {
            sample_rate,
            frame_size: DEFAULT_FFT_FRAME_SIZE,
            hop_size: DEFAULT_FFT_HOP_SIZE,
            min_frequency_hz: MIN_ANALYSIS_HZ,
            max_frequency_hz: MAX_ANALYSIS_HZ,
            smoothing_attack: 0.38,
            smoothing_release: 0.12,
            db_floor: DEFAULT_DB_FLOOR,
            db_ceiling: DEFAULT_DB_CEILING,
            adaptive_gain: AdaptiveGainConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct BandBinRange {
    start_bin: usize,
    end_bin: usize,
}

pub struct RealtimeAnalyzer {
    config: AnalyzerConfig,
    fft_plan: FftPlan,
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    window_normalization: f32,
    input_buffer: VecDeque<f32>,
    frame_scratch: Vec<f32>,
    fft_scratch: Vec<Complex32>,
    band_ranges: Vec<BandBinRange>,
    adaptive_gain: AdaptiveGain,
    smoother: ExponentialSmoother,
    previous_bands: [f32; DEFAULT_BAND_COUNT],
}

impl RealtimeAnalyzer {
    pub fn new(config: AnalyzerConfig) -> Self {
        assert!(config.frame_size >= 2, "frame size must be at least 2");
        assert!(config.hop_size >= 1, "hop size must be at least 1");
        assert!(
            config.hop_size <= config.frame_size,
            "hop size must not exceed frame size"
        );
        assert!(
            config.max_frequency_hz > config.min_frequency_hz,
            "max analysis frequency must be above the minimum"
        );
        assert!(
            config.db_ceiling > config.db_floor,
            "db ceiling must be above the db floor"
        );

        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(config.frame_size);
        let window = hann_window_coefficients(config.frame_size);
        let window_normalization = (window.iter().copied().sum::<f32>() * 0.5).max(1.0e-6);

        Self {
            config,
            fft_plan: FftPlan::new(config.sample_rate, config.frame_size),
            fft,
            window,
            window_normalization,
            input_buffer: VecDeque::with_capacity(config.frame_size * 2),
            frame_scratch: vec![0.0; config.frame_size],
            fft_scratch: vec![Complex32::new(0.0, 0.0); config.frame_size],
            band_ranges: build_log_band_ranges(config),
            adaptive_gain: AdaptiveGain::new(config.adaptive_gain),
            smoother: ExponentialSmoother::new(config.smoothing_attack, config.smoothing_release),
            previous_bands: [0.0; DEFAULT_BAND_COUNT],
        }
    }

    pub fn default_for_sample_rate(sample_rate: u32) -> Self {
        Self::new(AnalyzerConfig::default_for_sample_rate(sample_rate))
    }

    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }

    pub fn push_samples(&mut self, mono_samples: &[f32]) -> Option<AnalysisSnapshot> {
        for sample in mono_samples {
            self.input_buffer.push_back(sample.clamp(-1.0, 1.0));
        }

        let mut latest = None;
        while self.input_buffer.len() >= self.config.frame_size {
            latest = Some(self.analyze_current_frame());
            self.discard_hop();
        }

        latest
    }

    fn analyze_current_frame(&mut self) -> AnalysisSnapshot {
        {
            let samples = self.input_buffer.make_contiguous();
            self.frame_scratch
                .copy_from_slice(&samples[..self.config.frame_size]);
        }

        let mut sum_squares = 0.0_f32;
        let mut peak = 0.0_f32;

        for (index, sample) in self.frame_scratch.iter().copied().enumerate() {
            sum_squares += sample * sample;
            peak = peak.max(sample.abs());
            self.fft_scratch[index] = Complex32::new(sample * self.window[index], 0.0);
        }

        self.fft.process(&mut self.fft_scratch);

        let raw_rms = (sum_squares / self.fft_plan.frame_size as f32).sqrt();
        let gain_state = self.adaptive_gain.update(raw_rms);
        let mut bands = [0.0_f32; DEFAULT_BAND_COUNT];
        let db_span = self.config.db_ceiling - self.config.db_floor;

        for (index, range) in self.band_ranges.iter().copied().enumerate() {
            let mut band_amplitude = 0.0_f32;

            for bin in range.start_bin..range.end_bin {
                let amplitude = self.fft_scratch[bin].norm() / self.window_normalization;
                band_amplitude = band_amplitude.max(amplitude);
            }

            let adjusted_amplitude = band_amplitude * gain_state.gain;
            let db = 20.0 * adjusted_amplitude.max(MAGNITUDE_EPSILON).log10();
            let normalized = (db - self.config.db_floor) / db_span;
            bands[index] = clamp_normalized(normalized) * gain_state.signal_gate;
        }

        for (index, band) in bands.iter_mut().enumerate() {
            *band = self.smoother.apply(self.previous_bands[index], *band);
        }

        normalize_frame(&mut bands);
        self.previous_bands = bands;

        AnalysisSnapshot {
            bands,
            rms: clamp_normalized(raw_rms * gain_state.gain * gain_state.signal_gate),
            peak: clamp_normalized(peak * gain_state.gain * gain_state.signal_gate),
        }
    }

    fn discard_hop(&mut self) {
        let samples_to_discard = self.config.hop_size.min(self.input_buffer.len());
        for _ in 0..samples_to_discard {
            let _ = self.input_buffer.pop_front();
        }
    }
}

fn build_log_band_ranges(config: AnalyzerConfig) -> Vec<BandBinRange> {
    let nyquist_hz = config.sample_rate as f32 * 0.5;
    let max_frequency_hz = config
        .max_frequency_hz
        .min(nyquist_hz.max(config.min_frequency_hz + 1.0));
    let bin_width_hz = config.sample_rate as f32 / config.frame_size as f32;
    let max_bin = config.frame_size / 2;
    let frequency_span_ratio = max_frequency_hz / config.min_frequency_hz;

    (0..DEFAULT_BAND_COUNT)
        .map(|index| {
            let start_ratio = index as f32 / DEFAULT_BAND_COUNT as f32;
            let end_ratio = (index + 1) as f32 / DEFAULT_BAND_COUNT as f32;
            let start_hz = config.min_frequency_hz * frequency_span_ratio.powf(start_ratio);
            let end_hz = config.min_frequency_hz * frequency_span_ratio.powf(end_ratio);

            let start_bin = frequency_to_bin(start_hz, bin_width_hz, max_bin).max(1);
            let end_bin = frequency_to_bin_ceil(end_hz, bin_width_hz, max_bin + 1)
                .max(start_bin + 1)
                .min(max_bin + 1);

            BandBinRange { start_bin, end_bin }
        })
        .collect()
}

fn frequency_to_bin(frequency_hz: f32, bin_width_hz: f32, max_bin: usize) -> usize {
    ((frequency_hz / bin_width_hz).floor() as usize).min(max_bin)
}

fn frequency_to_bin_ceil(frequency_hz: f32, bin_width_hz: f32, max_bin: usize) -> usize {
    ((frequency_hz / bin_width_hz).ceil() as usize).min(max_bin)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_RATE: u32 = 48_000;
    const FRAME_SIZE: usize = 4096;

    #[test]
    fn low_frequency_sine_emphasizes_low_bands() {
        let mut analyzer = test_analyzer();
        let snapshot = analyzer
            .push_samples(&generate_sine_wave(90.0, 0.8, FRAME_SIZE))
            .expect("expected a completed analysis frame");

        let low_energy = average(&snapshot.bands[..16]);
        let high_energy = average(&snapshot.bands[48..]);

        assert!(low_energy > high_energy * 2.0);
    }

    #[test]
    fn high_frequency_sine_emphasizes_high_bands() {
        let mut analyzer = test_analyzer();
        let snapshot = analyzer
            .push_samples(&generate_sine_wave(8_000.0, 0.8, FRAME_SIZE))
            .expect("expected a completed analysis frame");

        let low_energy = average(&snapshot.bands[..16]);
        let high_energy = average(&snapshot.bands[48..]);

        assert!(high_energy > low_energy * 2.0);
    }

    #[test]
    fn silence_produces_near_zero_levels() {
        let mut analyzer = test_analyzer();
        let snapshot = analyzer
            .push_samples(&vec![0.0_f32; FRAME_SIZE])
            .expect("expected a completed analysis frame");

        assert!(snapshot.rms <= 0.0001);
        assert!(snapshot.peak <= 0.0001);
        assert!(snapshot.bands.iter().all(|band| *band <= 0.0001));
    }

    #[test]
    fn analyzer_emits_sixty_four_bands() {
        let mut analyzer = test_analyzer();
        let snapshot = analyzer
            .push_samples(&generate_sine_wave(440.0, 0.8, FRAME_SIZE))
            .expect("expected a completed analysis frame");

        assert_eq!(snapshot.bands.len(), DEFAULT_BAND_COUNT);
    }

    #[test]
    fn adaptive_gain_brings_quiet_and_loud_sines_closer_together() {
        let loud = settled_snapshot(440.0, 0.7, 48);
        let quiet = settled_snapshot(440.0, 0.02, 192);

        let loud_peak_band = dominant_band(&loud);
        let quiet_peak_band = dominant_band(&quiet);

        assert!(quiet_peak_band > loud_peak_band * 0.7);
        assert!(quiet_peak_band <= 1.0);
    }

    #[test]
    fn near_noise_floor_signal_does_not_explode_to_full_scale() {
        let snapshot = settled_snapshot(440.0, 0.0008, 192);

        assert!(snapshot.rms < 0.01);
        assert!(snapshot.peak < 0.02);
        assert!(dominant_band(&snapshot) < 0.05);
    }

    fn test_analyzer() -> RealtimeAnalyzer {
        RealtimeAnalyzer::new(AnalyzerConfig {
            sample_rate: SAMPLE_RATE,
            frame_size: FRAME_SIZE,
            hop_size: FRAME_SIZE,
            min_frequency_hz: MIN_ANALYSIS_HZ,
            max_frequency_hz: MAX_ANALYSIS_HZ,
            smoothing_attack: 1.0,
            smoothing_release: 1.0,
            db_floor: DEFAULT_DB_FLOOR,
            db_ceiling: DEFAULT_DB_CEILING,
            adaptive_gain: AdaptiveGainConfig::default(),
        })
    }

    fn generate_sine_wave(frequency_hz: f32, amplitude: f32, sample_count: usize) -> Vec<f32> {
        (0..sample_count)
            .map(|index| {
                let phase =
                    std::f32::consts::TAU * frequency_hz * index as f32 / SAMPLE_RATE as f32;
                phase.sin() * amplitude
            })
            .collect()
    }

    fn settled_snapshot(frequency_hz: f32, amplitude: f32, frames: usize) -> AnalysisSnapshot {
        let mut analyzer = test_analyzer();
        analyzer
            .push_samples(&generate_sine_wave(
                frequency_hz,
                amplitude,
                FRAME_SIZE * frames,
            ))
            .expect("expected a completed analysis frame")
    }

    fn dominant_band(snapshot: &AnalysisSnapshot) -> f32 {
        snapshot.bands.iter().copied().fold(0.0, f32::max)
    }

    fn average(values: &[f32]) -> f32 {
        values.iter().copied().sum::<f32>() / values.len() as f32
    }
}
