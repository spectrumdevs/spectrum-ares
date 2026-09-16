#![allow(dead_code)]

use std::sync::Arc;

use crate::backend::{AudioBackend, BackendWorker};
use crate::error::AresError;
use crate::state::SharedAnalyzerState;

#[derive(Debug, Default)]
pub struct PulseAudioBackend;

impl PulseAudioBackend {
    pub fn new() -> Self {
        Self
    }

    pub fn is_available() -> bool {
        false
    }
}

impl AudioBackend for PulseAudioBackend {
    fn start(&self, _shared: Arc<SharedAnalyzerState>) -> Result<BackendWorker, AresError> {
        Err(AresError::backend(
            "PulseAudio fallback is not implemented in stage 1",
        ))
    }
}
