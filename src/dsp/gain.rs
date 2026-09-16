const DEFAULT_TARGET_RMS: f32 = 0.08;
const DEFAULT_MIN_GAIN: f32 = 1.0;
const DEFAULT_MAX_GAIN: f32 = 64.0;
const DEFAULT_NOISE_FLOOR: f32 = 0.0005;
const DEFAULT_SIGNAL_GATE_FULL_SCALE_RMS: f32 = 0.004;
const DEFAULT_GAIN_RISE: f32 = 0.02;
const DEFAULT_GAIN_FALL: f32 = 0.15;
const DEFAULT_SILENCE_DECAY: f32 = 0.05;

#[derive(Debug, Clone, Copy)]
pub struct AdaptiveGainConfig {
    pub target_rms: f32,
    pub min_gain: f32,
    pub max_gain: f32,
    pub noise_floor: f32,
    pub signal_gate_full_scale_rms: f32,
    pub gain_rise: f32,
    pub gain_fall: f32,
    pub silence_decay: f32,
}

impl Default for AdaptiveGainConfig {
    fn default() -> Self {
        Self {
            target_rms: DEFAULT_TARGET_RMS,
            min_gain: DEFAULT_MIN_GAIN,
            max_gain: DEFAULT_MAX_GAIN,
            noise_floor: DEFAULT_NOISE_FLOOR,
            signal_gate_full_scale_rms: DEFAULT_SIGNAL_GATE_FULL_SCALE_RMS,
            gain_rise: DEFAULT_GAIN_RISE,
            gain_fall: DEFAULT_GAIN_FALL,
            silence_decay: DEFAULT_SILENCE_DECAY,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptiveGainState {
    pub gain: f32,
    pub signal_gate: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct AdaptiveGain {
    config: AdaptiveGainConfig,
    current_gain: f32,
}

impl AdaptiveGain {
    pub fn new(config: AdaptiveGainConfig) -> Self {
        assert!(config.target_rms > 0.0, "target RMS must be positive");
        assert!(config.min_gain > 0.0, "minimum gain must be positive");
        assert!(
            config.max_gain >= config.min_gain,
            "maximum gain must not be below minimum gain"
        );
        assert!(
            config.noise_floor >= 0.0,
            "noise floor must be non-negative"
        );
        assert!(
            config.signal_gate_full_scale_rms > config.noise_floor,
            "signal gate full-scale RMS must exceed the noise floor"
        );

        Self {
            current_gain: config.min_gain,
            config,
        }
    }

    #[allow(dead_code)]
    pub fn current_gain(&self) -> f32 {
        self.current_gain
    }

    pub fn update(&mut self, observed_rms: f32) -> AdaptiveGainState {
        let observed_rms = observed_rms.max(0.0);

        if observed_rms > self.config.noise_floor {
            let desired_gain = (self.config.target_rms / observed_rms)
                .clamp(self.config.min_gain, self.config.max_gain);
            let coefficient = if desired_gain > self.current_gain {
                self.config.gain_rise
            } else {
                self.config.gain_fall
            };
            self.current_gain = lerp(self.current_gain, desired_gain, coefficient);
        } else {
            self.current_gain = lerp(
                self.current_gain,
                self.config.min_gain,
                self.config.silence_decay,
            );
        }

        AdaptiveGainState {
            gain: self.current_gain,
            signal_gate: signal_gate(
                observed_rms,
                self.config.noise_floor,
                self.config.signal_gate_full_scale_rms,
            ),
        }
    }
}

fn signal_gate(observed_rms: f32, noise_floor: f32, full_scale_rms: f32) -> f32 {
    if observed_rms <= noise_floor {
        return 0.0;
    }

    let normalized =
        ((observed_rms - noise_floor) / (full_scale_rms - noise_floor)).clamp(0.0, 1.0);

    normalized * normalized * (3.0 - 2.0 * normalized)
}

fn lerp(current: f32, target: f32, coefficient: f32) -> f32 {
    current + (target - current) * coefficient.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gain_is_clamped_to_maximum() {
        let mut gain = AdaptiveGain::new(AdaptiveGainConfig {
            gain_rise: 1.0,
            gain_fall: 1.0,
            ..AdaptiveGainConfig::default()
        });

        let state = gain.update(0.0006);

        assert_eq!(state.gain, DEFAULT_MAX_GAIN);
    }

    #[test]
    fn gain_returns_toward_minimum_during_silence() {
        let mut gain = AdaptiveGain::new(AdaptiveGainConfig {
            gain_rise: 1.0,
            gain_fall: 1.0,
            silence_decay: 0.1,
            ..AdaptiveGainConfig::default()
        });

        gain.update(0.0006);
        for _ in 0..64 {
            gain.update(0.0);
        }

        assert!(gain.current_gain() < 2.0);
        assert!(gain.current_gain() >= DEFAULT_MIN_GAIN);
    }

    #[test]
    fn signal_gate_is_zero_below_noise_floor() {
        let mut gain = AdaptiveGain::new(AdaptiveGainConfig::default());

        let state = gain.update(DEFAULT_NOISE_FLOOR * 0.5);

        assert_eq!(state.signal_gate, 0.0);
    }
}
