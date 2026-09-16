#[derive(Debug, Clone, Copy)]
pub struct ExponentialSmoother {
    attack: f32,
    release: f32,
}

impl ExponentialSmoother {
    pub fn new(attack: f32, release: f32) -> Self {
        Self {
            attack: attack.clamp(0.0, 1.0),
            release: release.clamp(0.0, 1.0),
        }
    }

    pub fn apply(&self, previous: f32, next: f32) -> f32 {
        let coefficient = if next > previous {
            self.attack
        } else {
            self.release
        };
        previous + (next - previous) * coefficient
    }
}
