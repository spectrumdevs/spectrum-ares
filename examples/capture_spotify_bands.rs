#[path = "../src/dsp/mod.rs"]
mod dsp;
#[path = "../src/backend/pipewire/capture.rs"]
mod pipewire_capture;
#[path = "../src/backend/pipewire/discovery.rs"]
mod pipewire_discovery;

use std::cell::RefCell;
use std::rc::Rc;

use dsp::analyzer::{AnalysisSnapshot, RealtimeAnalyzer};
use pipewire_capture::{CaptureEvent, CaptureTarget};
use pipewire_discovery::DiscoveredSource;

fn main() {
    let best = match pipewire_discovery::find_best_spotify_source() {
        Ok(Some(source)) => source,
        Ok(None) => {
            eprintln!("No Spotify playback source was selected.");
            eprintln!(
                "Run `cargo run --example find_spotify_pipewire` while Spotify is actively playing."
            );
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!("PipeWire discovery failed: {err}");
            std::process::exit(1);
        }
    };

    if !best.is_capture_eligible() {
        eprintln!("Best Spotify candidate is not capture-eligible.");
        print_source(&best);
        std::process::exit(1);
    }

    let target = CaptureTarget {
        node_id: best.id,
        object_serial: best.object_serial,
        display_name: best.display_name().to_string(),
        node_name: best.node_name.clone(),
    };

    println!("Selected Spotify capture candidate");
    println!("---------------------------------");
    print_source(&best);
    println!();
    println!("capture target: {}", target.preferred_target_id());
    println!(
        "analysis settings: window=Hann frame_size=4096 hop_size=1024 bands=64 range=40Hz..16kHz"
    );
    println!("Press Ctrl+C to stop.");
    println!();

    let analyzer = Rc::new(RefCell::new(None::<RealtimeAnalyzer>));
    let latest_snapshot = Rc::new(RefCell::new(AnalysisSnapshot::silence()));
    let analyzer_for_samples = analyzer.clone();
    let snapshot_for_samples = latest_snapshot.clone();
    let snapshot_for_prints = latest_snapshot.clone();

    match pipewire_capture::run_capture_loop_with_mono_samples(
        &target,
        move |event| match event {
            CaptureEvent::Info { message } => println!("[info] {message}"),
            CaptureEvent::StateChanged {
                old,
                new,
                capture_node_id,
            } => match capture_node_id {
                Some(capture_node_id) => {
                    println!("[state] {old} -> {new} (capture stream node id {capture_node_id})")
                }
                None => println!("[state] {old} -> {new}"),
            },
            CaptureEvent::FormatNegotiated { format } => {
                println!("[format] {format}");
            }
            CaptureEvent::Levels {
                buffers_processed, ..
            } => {
                let snapshot = *snapshot_for_prints.borrow();
                println!(
                    "[analysis] buffers={} rms={:.5} peak={:.5} bands={}",
                    buffers_processed,
                    snapshot.rms,
                    snapshot.peak,
                    compact_bands(&snapshot),
                );
            }
            CaptureEvent::Warning { message } => eprintln!("[warn] {message}"),
        },
        move |format, mono_samples| {
            let mut analyzer = analyzer_for_samples.borrow_mut();
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
                    *snapshot_for_samples.borrow_mut() = snapshot;
                }
            }
        },
    ) {
        Ok(summary) => {
            let snapshot = *latest_snapshot.borrow();
            println!();
            println!("Capture stopped.");
            println!("  target: {}", summary.target_id);
            if let Some(capture_node_id) = summary.capture_node_id {
                println!("  capture stream node id: {capture_node_id}");
            }
            if let Some(format) = summary.observed_format {
                println!("  observed format: {format}");
            }
            if let Some(levels) = summary.last_levels {
                println!(
                    "  last raw levels: rms={:.5} peak={:.5} frames={}",
                    levels.rms, levels.peak, levels.frames
                );
            }
            println!(
                "  last analyzer snapshot: rms={:.5} peak={:.5}",
                snapshot.rms, snapshot.peak
            );
            println!("  last bands: {}", compact_bands(&snapshot));
            println!("  processed buffers: {}", summary.buffers_processed);
        }
        Err(err) => {
            eprintln!("PipeWire capture failed: {err}");
            std::process::exit(1);
        }
    }
}

fn compact_bands(snapshot: &AnalysisSnapshot) -> String {
    const RAMP: &[u8] = b" .:-=+*#%@";

    snapshot
        .bands
        .iter()
        .copied()
        .map(|band| {
            let index = ((band.clamp(0.0, 1.0) * (RAMP.len() - 1) as f32).round() as usize)
                .min(RAMP.len() - 1);
            RAMP[index] as char
        })
        .collect()
}

fn print_source(source: &DiscoveredSource) {
    println!(
        "#{:<4} serial={:<6} type={:<14} class={:<26} score={:<4} eligible={} {}",
        source.id,
        format_optional_u64(source.object_serial),
        source.object_type,
        source.classification(),
        source.spotify_candidate_score(),
        yes_no(source.is_capture_eligible()),
        source.display_name()
    );
    print_optional("node.name", source.node_name.as_deref());
    print_optional("node.description", source.node_description.as_deref());
    print_optional("application.name", source.application_name.as_deref());
    print_optional(
        "application.process.binary",
        source.application_process_binary.as_deref(),
    );
    print_optional("client.name", source.client_name.as_deref());
    print_optional("media.name", source.media_name.as_deref());
    print_optional("media.class", source.media_class.as_deref());
    print_optional("media.category", source.media_category.as_deref());
    print_optional("media.role", source.media_role.as_deref());
    print_optional("media.type", source.media_type.as_deref());
    print_optional_u32("client.id", source.client_id);
    print_optional_u32("linked_client_id", source.linked_client_id);
    print_optional("linked_client_name", source.linked_client_name.as_deref());
    println!("  reason: {}", source.capture_eligibility_reason());
    println!("  selection: {}", source.selection_reason());
}

fn print_optional(label: &str, value: Option<&str>) {
    if let Some(value) = value {
        println!("  {label}: {value}");
    }
}

fn print_optional_u32(label: &str, value: Option<u32>) {
    if let Some(value) = value {
        println!("  {label}: {value}");
    }
}

fn format_optional_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}
