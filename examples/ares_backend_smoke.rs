use std::ffi::{c_char, CStr};
use std::thread;
use std::time::Duration;

#[path = "../src/api.rs"]
mod api;
#[path = "../src/backend/mod.rs"]
mod backend;
#[path = "../src/dsp/mod.rs"]
mod dsp;
#[path = "../src/error.rs"]
mod error;
#[path = "../src/state.rs"]
mod state;

use api::{
    ares_get_band_count, ares_get_bands, ares_get_last_error, ares_get_peak, ares_get_rms,
    ares_is_running, ares_start, ares_stop,
};

fn main() {
    println!("SPECTRUM_ARES_BACKEND={}", requested_backend_label());

    let start_status = ares_start();
    if start_status != 0 {
        eprintln!("ares_start() failed: {}", read_last_error());
        std::process::exit(1);
    }

    println!("ares_is_running()={}", ares_is_running());
    println!("active_backend={}", active_backend_label());

    let band_count = ares_get_band_count();
    if band_count <= 0 {
        eprintln!("invalid band count from ABI: {band_count}");
        let _ = ares_stop();
        std::process::exit(1);
    }

    let mut bands = vec![0.0_f32; band_count as usize];

    for tick in 0..40 {
        let band_status = ares_get_bands(bands.as_mut_ptr(), bands.len() as i32);
        let rms = ares_get_rms();
        let peak = ares_get_peak();
        let pipewire_status = pipewire_status_label();

        println!(
            "[{:02}] running={} bands_status={} rms={:.5} peak={:.5} status={} bands={}",
            tick,
            ares_is_running(),
            band_status,
            rms,
            peak,
            pipewire_status,
            compact_bands(&bands),
        );

        thread::sleep(Duration::from_millis(150));
    }

    println!("stopping backend");
    let stop_status = ares_stop();
    println!("ares_stop()={stop_status}");
    println!("ares_is_running()={}", ares_is_running());
}

fn requested_backend_label() -> String {
    std::env::var("SPECTRUM_ARES_BACKEND")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "auto (prefer pipewire)".to_string())
}

fn active_backend_label() -> String {
    state::runtime()
        .active_backend_kind()
        .map(|kind| kind.as_str().to_string())
        .unwrap_or_else(|| "none".to_string())
}

fn pipewire_status_label() -> String {
    if state::runtime().active_backend_kind() != Some(backend::BackendKind::PipeWire) {
        return "-".to_string();
    }

    let snapshot = backend::pipewire::current_runtime_status();
    format!("{} ({})", snapshot.status.as_str(), snapshot.detail)
}

fn compact_bands(bands: &[f32]) -> String {
    const RAMP: &[u8] = b" .:-=+*#%@";

    bands
        .iter()
        .copied()
        .map(|band| {
            let index = ((band.clamp(0.0, 1.0) * (RAMP.len() - 1) as f32).round() as usize)
                .min(RAMP.len() - 1);
            RAMP[index] as char
        })
        .collect()
}

fn read_last_error() -> String {
    let mut buffer = [0 as c_char; 256];
    let _ = ares_get_last_error(buffer.as_mut_ptr(), buffer.len());
    unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned()
}
