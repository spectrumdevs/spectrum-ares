use std::sync::{Mutex, OnceLock, RwLock};

use crate::backend::{self, BackendKind, BackendWorker};
use crate::dsp::bands::DEFAULT_BAND_COUNT;
use crate::error::{AresError, ARES_OK};

pub type BandFrame = [f32; DEFAULT_BAND_COUNT];

#[derive(Clone, Copy, Debug)]
pub struct AnalyzerSnapshot {
    pub bands: BandFrame,
    pub rms: f32,
    pub peak: f32,
}

impl AnalyzerSnapshot {
    pub fn silence() -> Self {
        Self {
            bands: [0.0; DEFAULT_BAND_COUNT],
            rms: 0.0,
            peak: 0.0,
        }
    }
}

#[derive(Debug)]
pub struct SharedAnalyzerState {
    snapshot: RwLock<AnalyzerSnapshot>,
}

impl SharedAnalyzerState {
    pub fn new() -> Self {
        Self {
            snapshot: RwLock::new(AnalyzerSnapshot::silence()),
        }
    }

    pub fn read_snapshot(&self) -> AnalyzerSnapshot {
        *self.snapshot.read().unwrap_or_else(|err| err.into_inner())
    }

    pub fn write_snapshot(&self, snapshot: AnalyzerSnapshot) {
        *self.snapshot.write().unwrap_or_else(|err| err.into_inner()) = snapshot;
    }

    pub fn reset(&self) {
        self.write_snapshot(AnalyzerSnapshot::silence());
    }
}

#[derive(Debug)]
struct RuntimeInner {
    worker: Option<BackendWorker>,
    backend_kind: Option<BackendKind>,
    last_error: String,
}

#[derive(Debug)]
pub struct AresRuntime {
    shared: std::sync::Arc<SharedAnalyzerState>,
    inner: Mutex<RuntimeInner>,
}

impl AresRuntime {
    fn new() -> Self {
        Self {
            shared: std::sync::Arc::new(SharedAnalyzerState::new()),
            inner: Mutex::new(RuntimeInner {
                worker: None,
                backend_kind: None,
                last_error: String::new(),
            }),
        }
    }

    pub fn start(&self) -> i32 {
        let mut inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        self.reap_finished_worker_locked(&mut inner);

        if inner.worker.as_ref().is_some_and(BackendWorker::is_running) {
            inner.last_error.clear();
            return ARES_OK;
        }

        match backend::start_default_backend(self.shared.clone()) {
            Ok(worker) => {
                inner.backend_kind = Some(worker.kind());
                inner.worker = Some(worker);
                inner.last_error.clear();
                ARES_OK
            }
            Err(err) => self.store_error_locked(&mut inner, err),
        }
    }

    pub fn stop(&self) -> i32 {
        let mut inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(worker) = inner.worker.take() {
            let _ = worker.stop();
        }
        inner.backend_kind = None;
        self.shared.reset();
        inner.last_error.clear();
        ARES_OK
    }

    pub fn is_running(&self) -> bool {
        let mut inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        self.reap_finished_worker_locked(&mut inner);
        inner.worker.as_ref().is_some_and(BackendWorker::is_running)
    }

    pub fn snapshot(&self) -> AnalyzerSnapshot {
        let mut inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        self.reap_finished_worker_locked(&mut inner);
        self.shared.read_snapshot()
    }

    #[allow(dead_code)]
    pub fn active_backend_kind(&self) -> Option<BackendKind> {
        let mut inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        self.reap_finished_worker_locked(&mut inner);
        inner.backend_kind
    }

    pub fn set_error(&self, error: AresError) -> i32 {
        let mut inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        self.store_error_locked(&mut inner, error)
    }

    pub fn last_error(&self) -> String {
        let mut inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        self.reap_finished_worker_locked(&mut inner);
        inner.last_error.clone()
    }

    #[allow(dead_code)]
    pub fn set_last_error_message(&self, message: impl Into<String>) {
        let mut inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        inner.last_error = message.into();
    }

    #[allow(dead_code)]
    pub fn clear_last_error_message(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        inner.last_error.clear();
    }

    fn store_error_locked(&self, inner: &mut RuntimeInner, error: AresError) -> i32 {
        inner.last_error.clear();
        inner.last_error.push_str(error.message());
        error.code()
    }

    fn reap_finished_worker_locked(&self, inner: &mut RuntimeInner) {
        let Some(result) = inner.worker.as_mut().and_then(BackendWorker::try_reap) else {
            return;
        };

        inner.worker = None;
        inner.backend_kind = None;
        self.shared.reset();

        if let Err(error) = result {
            inner.last_error.clear();
            inner.last_error.push_str(error.message());
        }
    }
}

pub fn runtime() -> &'static AresRuntime {
    static RUNTIME: OnceLock<AresRuntime> = OnceLock::new();
    RUNTIME.get_or_init(AresRuntime::new)
}
