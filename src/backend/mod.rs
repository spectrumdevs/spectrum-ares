use std::env;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};

use crate::error::AresError;
use crate::state::SharedAnalyzerState;

pub mod mock;
pub mod pipewire;
pub mod pulseaudio;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    PipeWire,
    Mock,
}

impl BackendKind {
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PipeWire => "pipewire",
            Self::Mock => "mock",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestedBackend {
    Auto,
    PipeWire,
    Mock,
}

pub trait AudioBackend {
    fn start(&self, shared: Arc<SharedAnalyzerState>) -> Result<BackendWorker, AresError>;
}

#[derive(Debug)]
pub struct BackendWorker {
    kind: BackendKind,
    stop_requested: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), AresError>>>,
}

impl BackendWorker {
    pub fn spawn<F>(kind: BackendKind, name: &'static str, run: F) -> Result<Self, AresError>
    where
        F: FnOnce(Arc<AtomicBool>) -> Result<(), AresError> + Send + 'static,
    {
        let stop_requested = Arc::new(AtomicBool::new(false));
        let thread_stop = stop_requested.clone();
        let thread = thread::Builder::new()
            .name(name.to_string())
            .spawn(move || run(thread_stop))
            .map_err(|_| AresError::backend("failed to start backend worker thread"))?;

        Ok(Self {
            kind,
            stop_requested,
            thread: Some(thread),
        })
    }

    pub fn kind(&self) -> BackendKind {
        self.kind
    }

    pub fn is_running(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }

    pub fn try_reap(&mut self) -> Option<Result<(), AresError>> {
        self.thread
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            .then(|| self.join_thread())
    }

    pub fn stop(mut self) -> Result<(), AresError> {
        self.stop_requested.store(true, Ordering::Release);
        self.join_thread()
    }

    fn join_thread(&mut self) -> Result<(), AresError> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };

        match thread.join() {
            Ok(result) => result,
            Err(_) => Err(AresError::backend("backend worker thread panicked")),
        }
    }
}

pub fn start_default_backend(shared: Arc<SharedAnalyzerState>) -> Result<BackendWorker, AresError> {
    match requested_backend_from_env()? {
        RequestedBackend::Mock => mock::MockBackend::new().start(shared),
        RequestedBackend::PipeWire => pipewire::PipeWireBackend::new().start(shared),
        RequestedBackend::Auto => pipewire::PipeWireBackend::new()
            .start(shared)
            .map_err(|error| {
                AresError::backend(format!(
                    "PipeWire backend unavailable: {}. Set SPECTRUM_ARES_BACKEND=mock to force mock output for development.",
                    error.message()
                ))
            }),
    }
}

fn requested_backend_from_env() -> Result<RequestedBackend, AresError> {
    parse_requested_backend(env::var("SPECTRUM_ARES_BACKEND").ok().as_deref())
}

fn parse_requested_backend(value: Option<&str>) -> Result<RequestedBackend, AresError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(RequestedBackend::Auto);
    };

    if value.eq_ignore_ascii_case("mock") {
        Ok(RequestedBackend::Mock)
    } else if value.eq_ignore_ascii_case("pipewire") {
        Ok(RequestedBackend::PipeWire)
    } else {
        Err(AresError::backend(format!(
            "unknown SPECTRUM_ARES_BACKEND value `{value}`; expected `mock` or `pipewire`"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_backend_env_defaults_to_auto() {
        assert_eq!(
            parse_requested_backend(None).unwrap(),
            RequestedBackend::Auto
        );
        assert_eq!(
            parse_requested_backend(Some("   ")).unwrap(),
            RequestedBackend::Auto
        );
    }

    #[test]
    fn backend_env_parses_known_values_case_insensitively() {
        assert_eq!(
            parse_requested_backend(Some("mock")).unwrap(),
            RequestedBackend::Mock
        );
        assert_eq!(
            parse_requested_backend(Some("PipeWire")).unwrap(),
            RequestedBackend::PipeWire
        );
    }

    #[test]
    fn backend_env_rejects_unknown_values() {
        let error = parse_requested_backend(Some("alsa")).unwrap_err();

        assert_eq!(error.code(), crate::error::ARES_ERROR_BACKEND);
        assert!(error
            .message()
            .contains("unknown SPECTRUM_ARES_BACKEND value `alsa`"));
    }
}
