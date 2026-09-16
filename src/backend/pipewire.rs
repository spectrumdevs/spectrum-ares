#![allow(dead_code)]

use std::cell::RefCell;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock, RwLock,
};
use std::thread;
use std::time::Duration;

use crate::backend::{AudioBackend, BackendKind, BackendWorker};
use crate::dsp::analyzer::{AnalysisSnapshot, RealtimeAnalyzer};
use crate::error::AresError;
use crate::state::{runtime, AnalyzerSnapshot, SharedAnalyzerState};

#[path = "pipewire/capture.rs"]
pub mod capture;
#[path = "pipewire/discovery.rs"]
pub mod discovery;

const REDISCOVERY_INTERVAL: Duration = Duration::from_secs(2);
const STOP_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipeWireRuntimeStatus {
    Stopped,
    Starting,
    Discovering,
    WaitingForSpotify,
    Capturing,
    PausedOrSilent,
    StreamLost,
    Reconnecting,
    Error,
}

impl PipeWireRuntimeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "Stopped",
            Self::Starting => "Starting",
            Self::Discovering => "Discovering",
            Self::WaitingForSpotify => "WaitingForSpotify",
            Self::Capturing => "Capturing",
            Self::PausedOrSilent => "PausedOrSilent",
            Self::StreamLost => "StreamLost",
            Self::Reconnecting => "Reconnecting",
            Self::Error => "Error",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeWireRuntimeSnapshot {
    pub status: PipeWireRuntimeStatus,
    pub detail: String,
}

impl PipeWireRuntimeSnapshot {
    fn new(status: PipeWireRuntimeStatus, detail: impl Into<String>) -> Self {
        Self {
            status,
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Default)]
pub struct PipeWireBackend;

impl PipeWireBackend {
    pub fn new() -> Self {
        Self
    }

    pub fn is_available() -> bool {
        Self::probe_pipewire().is_ok()
    }

    fn probe_pipewire() -> Result<(), AresError> {
        discovery::discover_sources()
            .map(|_| ())
            .map_err(|error| AresError::backend(format!("PipeWire discovery failed: {error}")))
    }
}

enum DiscoveryOutcome {
    Ready {
        source: discovery::DiscoveredSource,
        target: capture::CaptureTarget,
    },
    Waiting {
        reason: String,
    },
}

pub fn capture_target_for_source(
    source: &discovery::DiscoveredSource,
) -> Option<capture::CaptureTarget> {
    source
        .is_capture_eligible()
        .then(|| capture::CaptureTarget {
            node_id: source.id,
            object_serial: source.object_serial,
            display_name: source.display_name().to_string(),
            node_name: source.node_name.clone(),
        })
}

pub fn current_runtime_status() -> PipeWireRuntimeSnapshot {
    runtime_status_lock()
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

pub fn run_capture_into_shared_state<F>(
    target: &capture::CaptureTarget,
    shared: Arc<SharedAnalyzerState>,
    stop_requested: Arc<AtomicBool>,
    on_event: F,
) -> Result<capture::CaptureRunSummary, capture::PipeWireCaptureError>
where
    F: FnMut(capture::CaptureEvent) + 'static,
{
    let analyzer = RefCell::new(None::<RealtimeAnalyzer>);

    capture::run_capture_loop_with_stop_signal(
        target,
        stop_requested,
        on_event,
        move |format, mono_samples| {
            let mut analyzer = analyzer.borrow_mut();
            let needs_reset = analyzer.as_ref().map_or(true, |analyzer| {
                analyzer.sample_rate() != format.sample_rate
            });

            if needs_reset {
                *analyzer = Some(RealtimeAnalyzer::default_for_sample_rate(
                    format.sample_rate,
                ));
            }

            if let Some(analyzer) = analyzer.as_mut() {
                if let Some(snapshot) = analyzer.push_samples(mono_samples) {
                    shared.write_snapshot(shared_snapshot(snapshot));
                }
            }
        },
    )
}

fn shared_snapshot(snapshot: AnalysisSnapshot) -> AnalyzerSnapshot {
    AnalyzerSnapshot {
        bands: snapshot.bands,
        rms: snapshot.rms,
        peak: snapshot.peak,
    }
}

impl AudioBackend for PipeWireBackend {
    fn start(&self, shared: Arc<SharedAnalyzerState>) -> Result<BackendWorker, AresError> {
        Self::probe_pipewire()?;
        set_runtime_status(
            PipeWireRuntimeStatus::Starting,
            "starting PipeWire Spotify backend",
        );

        BackendWorker::spawn(
            BackendKind::PipeWire,
            "spectrum-ares-pipewire",
            move |stop_requested| run_pipewire_backend_worker(shared, stop_requested),
        )
    }
}

fn run_pipewire_backend_worker(
    shared: Arc<SharedAnalyzerState>,
    stop_requested: Arc<AtomicBool>,
) -> Result<(), AresError> {
    shared.reset();
    set_runtime_status(
        PipeWireRuntimeStatus::Starting,
        "starting PipeWire Spotify backend",
    );
    runtime().clear_last_error_message();

    while !stop_requested.load(Ordering::Acquire) {
        shared.reset();
        set_runtime_status(
            PipeWireRuntimeStatus::Discovering,
            "discovering Spotify PipeWire playback source",
        );

        match discover_capture_target() {
            Ok(DiscoveryOutcome::Ready { source, target }) => {
                runtime().clear_last_error_message();
                let stopped_normally =
                    run_capture_session(source, target, shared.clone(), stop_requested.clone());
                if stopped_normally || stop_requested.load(Ordering::Acquire) {
                    break;
                }

                set_runtime_status(
                    PipeWireRuntimeStatus::Reconnecting,
                    format!(
                        "rediscovering Spotify playback source in {} ms",
                        REDISCOVERY_INTERVAL.as_millis()
                    ),
                );
                if !sleep_with_stop(&stop_requested, REDISCOVERY_INTERVAL) {
                    break;
                }
            }
            Ok(DiscoveryOutcome::Waiting { reason }) => {
                shared.reset();
                runtime().set_last_error_message(reason.clone());
                set_runtime_status(PipeWireRuntimeStatus::WaitingForSpotify, reason);
                if !sleep_with_stop(&stop_requested, REDISCOVERY_INTERVAL) {
                    break;
                }
            }
            Err(error) => {
                let message = error.message().to_string();
                shared.reset();
                runtime().set_last_error_message(message.clone());
                set_runtime_status(PipeWireRuntimeStatus::Error, message);
                if !sleep_with_stop(&stop_requested, REDISCOVERY_INTERVAL) {
                    break;
                }
            }
        }
    }

    shared.reset();
    set_runtime_status(PipeWireRuntimeStatus::Stopped, "PipeWire backend stopped");
    Ok(())
}

fn run_capture_session(
    source: discovery::DiscoveredSource,
    target: capture::CaptureTarget,
    shared: Arc<SharedAnalyzerState>,
    stop_requested: Arc<AtomicBool>,
) -> bool {
    set_runtime_status(
        PipeWireRuntimeStatus::Starting,
        format!(
            "connecting to Spotify source `{}` via {}",
            source.display_name(),
            target.preferred_target_id()
        ),
    );

    let shared_for_events = shared.clone();
    let source_name = source.display_name().to_string();
    let capture_result =
        run_capture_into_shared_state(&target, shared, stop_requested.clone(), move |event| {
            handle_capture_event(&shared_for_events, event)
        });

    if stop_requested.load(Ordering::Acquire) {
        return true;
    }

    match capture_result {
        Ok(summary) => {
            let message = format!(
                "Spotify stream `{source_name}` stopped after {} buffers; rediscovering",
                summary.buffers_processed
            );
            runtime().set_last_error_message(message.clone());
            set_runtime_status(PipeWireRuntimeStatus::StreamLost, message);
        }
        Err(error) => {
            let message = format!("Spotify stream `{source_name}` was lost: {error}");
            runtime().set_last_error_message(message.clone());
            set_runtime_status(PipeWireRuntimeStatus::StreamLost, message);
        }
    }

    false
}

fn handle_capture_event(shared: &Arc<SharedAnalyzerState>, event: capture::CaptureEvent) {
    match event {
        capture::CaptureEvent::Info { message } => {
            debug_log(&format!("[info] {message}"));
        }
        capture::CaptureEvent::StateChanged {
            old,
            new,
            capture_node_id,
        } => {
            debug_log(&format!(
                "[state] {} -> {} (capture node id {:?})",
                old, new, capture_node_id
            ));

            match new.as_str() {
                "Connecting" | "Unconnected" => set_runtime_status(
                    PipeWireRuntimeStatus::Starting,
                    format!("PipeWire stream state: {old} -> {new}"),
                ),
                "Paused" => {
                    shared.reset();
                    let message = "Spotify stream is paused or waiting for audio".to_string();
                    runtime().set_last_error_message(message.clone());
                    set_runtime_status(PipeWireRuntimeStatus::PausedOrSilent, message);
                }
                "Streaming" => {
                    runtime().clear_last_error_message();
                    set_runtime_status(
                        PipeWireRuntimeStatus::Capturing,
                        "capturing Spotify audio".to_string(),
                    );
                }
                _ => {}
            }
        }
        capture::CaptureEvent::FormatNegotiated { format } => {
            debug_log(&format!("[format] {format}"));
            set_runtime_status(
                PipeWireRuntimeStatus::Starting,
                format!("negotiated format: {format}"),
            );
        }
        capture::CaptureEvent::Levels {
            buffers_processed,
            levels,
            ..
        } => {
            debug_log(&format!(
                "[levels] buffers={} rms={:.5} peak={:.5}",
                buffers_processed, levels.rms, levels.peak
            ));
            runtime().clear_last_error_message();
            set_runtime_status(
                PipeWireRuntimeStatus::Capturing,
                format!(
                    "capturing Spotify audio (buffers={}, rms={:.5}, peak={:.5})",
                    buffers_processed, levels.rms, levels.peak
                ),
            );
        }
        capture::CaptureEvent::Warning { message } => {
            debug_log(&format!("[warn] {message}"));
            if is_pause_related_warning(&message) {
                shared.reset();
                runtime().set_last_error_message(message.clone());
                set_runtime_status(PipeWireRuntimeStatus::PausedOrSilent, message);
            } else {
                runtime().set_last_error_message(message.clone());
            }
        }
    }
}

fn discover_capture_target() -> Result<DiscoveryOutcome, AresError> {
    let sources = discovery::discover_sources()
        .map_err(|error| AresError::backend(format!("PipeWire discovery failed: {error}")))?;

    Ok(classify_discovered_sources(&sources))
}

fn classify_discovered_sources(sources: &[discovery::DiscoveredSource]) -> DiscoveryOutcome {
    match discovery::select_best_spotify_source(sources) {
        Some(source) if source.is_capture_eligible() => {
            if let Some(target) = capture_target_for_source(&source) {
                DiscoveryOutcome::Ready { source, target }
            } else {
                DiscoveryOutcome::Waiting {
                    reason: format!(
                        "waiting for a usable Spotify capture target: {}",
                        source.capture_eligibility_reason()
                    ),
                }
            }
        }
        Some(source) => DiscoveryOutcome::Waiting {
            reason: format!(
                "waiting for capture-eligible Spotify playback: {}",
                source.capture_eligibility_reason()
            ),
        },
        None => DiscoveryOutcome::Waiting {
            reason: "waiting for a capture-eligible Spotify PipeWire playback source".to_string(),
        },
    }
}

fn is_pause_related_warning(message: &str) -> bool {
    message.contains("no audio buffers received for")
        || message.contains("paused or corked")
        || message.contains("waiting for PipeWire format negotiation and first audio buffers")
        || message.contains("connected to PipeWire but no audio buffers have arrived yet")
}

fn sleep_with_stop(stop_requested: &Arc<AtomicBool>, duration: Duration) -> bool {
    let started_at = std::time::Instant::now();
    while started_at.elapsed() < duration {
        if stop_requested.load(Ordering::Acquire) {
            return false;
        }
        thread::sleep(STOP_POLL_INTERVAL.min(duration - started_at.elapsed()));
    }

    !stop_requested.load(Ordering::Acquire)
}

fn runtime_status_lock() -> &'static RwLock<PipeWireRuntimeSnapshot> {
    static STATUS: OnceLock<RwLock<PipeWireRuntimeSnapshot>> = OnceLock::new();
    STATUS.get_or_init(|| {
        RwLock::new(PipeWireRuntimeSnapshot::new(
            PipeWireRuntimeStatus::Stopped,
            "PipeWire backend has not started",
        ))
    })
}

fn set_runtime_status(status: PipeWireRuntimeStatus, detail: impl Into<String>) {
    *runtime_status_lock()
        .write()
        .unwrap_or_else(|error| error.into_inner()) = PipeWireRuntimeSnapshot::new(status, detail);
}

fn debug_log(message: &str) {
    if std::env::var_os("SPECTRUM_ARES_PIPEWIRE_DEBUG").is_some() {
        eprintln!("[spectrum-ares pipewire-backend] {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(object_type: &str, id: u32) -> discovery::DiscoveredSource {
        discovery::DiscoveredSource {
            id,
            object_serial: Some(u64::from(id + 100)),
            object_type: object_type.to_string(),
            client_id: None,
            linked_client_id: None,
            linked_client_name: None,
            node_name: None,
            node_description: None,
            application_name: None,
            application_process_binary: None,
            client_name: None,
            media_name: None,
            media_class: None,
            media_role: None,
            media_category: None,
            media_type: None,
            target_object: None,
            node_target: None,
        }
    }

    #[test]
    fn classify_sources_waits_when_only_client_is_found() {
        let mut spotify_client = source("Client", 135);
        spotify_client.application_name = Some("spotify".to_string());

        match classify_discovered_sources(&[spotify_client]) {
            DiscoveryOutcome::Waiting { reason } => {
                assert!(reason.contains("no linked playback stream/node was discovered"));
            }
            DiscoveryOutcome::Ready { .. } => panic!("expected waiting outcome"),
        }
    }

    #[test]
    fn classify_sources_returns_ready_for_linked_playback_node() {
        let mut spotify_node = source("Node", 138);
        spotify_node.client_id = Some(135);
        spotify_node.linked_client_id = Some(135);
        spotify_node.linked_client_name = Some("spotify".to_string());
        spotify_node.node_name = Some("audio-src".to_string());
        spotify_node.media_class = Some("Stream/Output/Audio".to_string());
        spotify_node.media_category = Some("Playback".to_string());
        spotify_node.media_role = Some("Music".to_string());

        match classify_discovered_sources(&[spotify_node]) {
            DiscoveryOutcome::Ready { target, .. } => {
                assert_eq!(target.node_id, 138);
                assert_eq!(
                    target.preferred_target_id(),
                    capture::CaptureTargetId::ObjectSerial(238)
                );
            }
            DiscoveryOutcome::Waiting { .. } => panic!("expected ready outcome"),
        }
    }

    #[test]
    fn pause_related_warning_detection_matches_expected_messages() {
        assert!(is_pause_related_warning(
            "no audio buffers received for 750 ms; Spotify may be paused or corked"
        ));
        assert!(is_pause_related_warning(
            "waiting for PipeWire format negotiation and first audio buffers"
        ));
        assert!(!is_pause_related_warning(
            "failed to parse audio format details"
        ));
    }

    #[test]
    fn sleep_with_stop_exits_immediately_when_stop_is_already_set() {
        let stop_requested = Arc::new(AtomicBool::new(true));

        assert!(!sleep_with_stop(&stop_requested, Duration::from_secs(2)));
    }
}
