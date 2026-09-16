# Spectrum ARES
Audio Reactivity Engine for Spotify (whole lotta words for an audio capturing library for Spectrum Client's spotify audio visualizer.)

## TODO
* Windows & Mac support

Spectrum ARES is a Linux-first native audio analysis library for Spectrum Client's Spotify visualizer. It is intended to capture Spotify audio locally, process it inside the native library, and expose normalized frequency/loudness values through a stable C ABI that can later be consumed from Java through JNA or JNI.

The current implementation is a Rust `cdylib` with a stable C ABI, a mock default backend, PipeWire discovery, and a prototype real PipeWire capture path that can already produce live RMS, peak, and 64-band analysis in examples.

## Status

- Target platform: Linux x86_64 first.
- Current ABI backend: PipeWire Spotify capture by default, mock when forced for tests/development.
- Prototype inspection/debug paths: dedicated PipeWire discovery and capture examples.
- Later fallback: PulseAudio.
- Out of scope for this stage: Minecraft/Fabric integration, Spotify Web API, metadata, playback control, microphone capture, raw PCM API, Windows, macOS, and ARM64.

Loading the library is intentionally passive. Consumers must call `ares_start()` before analysis begins, and should call `ares_stop()` to stop the internal worker thread.

## Build

```sh
cargo build --release
```

Linux release builds produce:

```text
target/release/libspectrum_ares.so
```

Run tests with:

```sh
cargo test
```

## Backend Selection

`ares_start()` now selects its internal backend from `SPECTRUM_ARES_BACKEND`.

Supported values:

- unset or empty: try the real PipeWire Spotify backend
- `pipewire`: force the real PipeWire Spotify backend
- `mock`: force the mock backend

Default behavior is intentionally production-leaning: when the variable is unset, Spectrum ARES starts the real PipeWire backend if PipeWire itself is reachable. If Spotify is not currently exposing a capture-eligible playback source, the worker stays alive, outputs silence, and periodically rediscovers Spotify instead of silently falling back to mock output.

Useful commands:

```sh
SPECTRUM_ARES_BACKEND=mock cargo run --example ares_backend_smoke
SPECTRUM_ARES_BACKEND=pipewire cargo run --example ares_backend_smoke
```

## Public C ABI

The ABI version is currently `1`. Rust types and owned strings are not exposed across the boundary.

```c
int ares_get_abi_version(void);
int ares_start(void);
int ares_stop(void);
int ares_is_running(void);
int ares_get_band_count(void);
int ares_get_bands(float *out, int len);
float ares_get_rms(void);
float ares_get_peak(void);
int ares_get_last_error(char *buffer, size_t len);
```

Return codes use this convention:

- `0`: success
- negative values: errors
- positive values: partial or special status

Current return codes:

- `ARES_OK = 0`
- `ARES_STATUS_TRUNCATED = 1`
- `ARES_ERROR_NULL_POINTER = -1`
- `ARES_ERROR_INVALID_LENGTH = -2`
- `ARES_ERROR_BACKEND = -3`

`ares_get_bands` writes up to `ares_get_band_count()` normalized `float` values into the caller-provided buffer. The default band count is `64`; values are normalized around `0.0` to `1.0`.

`ares_get_last_error` writes a null-terminated message into a caller-provided buffer. The library never returns owned strings or Rust pointers over the ABI.

A small manual C header is provided at `include/spectrum_ares.h`.

## DSP Plan

The real backend is expected to:

1. identify Spotify through PipeWire node/application metadata,
2. capture Spotify audio locally,
3. convert backend-native samples to `f32`,
4. downmix to mono,
5. window samples,
6. run an FFT,
7. map FFT bins into logarithmic bands from roughly 40 Hz to 16 kHz,
8. apply adaptive loudness normalization so low Spotify application volume still produces useful visualizer output,
9. smooth the output with basic attack/release behavior,
10. store the latest visualizer-ready bands, RMS, and peak for consumers.

Getter calls should only copy the latest processed values. Consumers should not process raw PCM.

The current analyzer uses a slow-rising, faster-falling adaptive gain stage driven by frame RMS. Quiet Spotify playback is boosted toward a target loudness, while silence and near-noise-floor signals are gated so fake bars do not appear when the stream is effectively silent.

## PipeWire Discovery

Stage 2 adds PipeWire registry discovery using the Rust `pipewire` bindings. It enumerates relevant globals such as nodes, clients, client nodes, and endpoint streams, copies selected properties into internal Rust structs, and scores Spotify candidates from both direct metadata matches and `client.id` links.

Important behavior: Spotify may appear as a PipeWire `Client`, while the active playback stream appears as a separate generic `Node` such as `audio-src`. In that case the stream has to be treated as Spotify by resolving `node client.id -> Spotify client id`, not by relying on the node name alone.

Useful commands while Spotify is actively playing:

```sh
wpctl status
pw-cli ls Node
pactl list sink-inputs
cargo run --example list_pipewire_nodes
cargo run --example find_spotify_pipewire
cargo run --example capture_spotify_rms
cargo run --example capture_spotify_bands
SPECTRUM_ARES_BACKEND=mock cargo run --example ares_backend_smoke
SPECTRUM_ARES_BACKEND=pipewire cargo run --example ares_backend_smoke
```

`list_pipewire_nodes` prints all retained PipeWire objects with classification, score, link metadata, and capture-eligibility reasoning.

`find_spotify_pipewire` focuses on:

- Spotify-like clients
- playback/audio nodes
- linked Spotify playback candidates
- the best current Spotify source candidate

`capture_spotify_rms` is the first real PipeWire capture prototype. It:

- discovers the best Spotify playback source,
- targets that node by `target.object=object.serial` when available,
- falls back to the global node id only if no `object.serial` was discovered,
- negotiates a raw audio capture stream,
- converts incoming PCM into `f32`,
- downmixes to mono for RMS and peak,
- prints changing RMS/peak values while Spotify is playing.

`capture_spotify_bands` extends that prototype into the real analyzer path. It:

- discovers and selects the best Spotify playback node,
- captures mono `f32` PCM from PipeWire,
- feeds that PCM into a Hann-windowed 4096-sample FFT,
- maps the spectrum into 64 logarithmic bands from roughly 40 Hz to 16 kHz,
- applies adaptive loudness normalization so low Spotify app volume still produces usable motion,
- applies audio-domain attack/release smoothing,
- prints changing RMS, peak, and compact 64-band output while Spotify is playing.

`ares_backend_smoke` exercises the real ABI path instead of the direct prototype modules. It:

- calls `ares_start()`,
- reads bands, RMS, and peak through the exported ABI functions for several seconds,
- prints compact live output,
- calls `ares_stop()`.

The direct PipeWire examples are still prototype-oriented, but the same capture/analyzer path now also powers `ares_start()` when PipeWire is selected. It is not production-ready yet, and it does not silently fall back to default desktop-wide output capture.

The current library backend path now uses PipeWire through `ares_start()` when `SPECTRUM_ARES_BACKEND` is unset or set to `pipewire`. The mock backend remains available for tests and development by forcing `SPECTRUM_ARES_BACKEND=mock`.

If only a Spotify client is found, the examples report that clearly. If no capture-eligible Spotify playback stream is found, the output explains that Spotify probably needs to be actively playing or that PipeWire may expose the stream differently through `pipewire-pulse`.

Current PipeWire worker lifecycle:

- if Spotify is paused or corked, output decays to silence and the worker stays alive
- if no buffers arrive for a while, the worker stays alive and keeps waiting instead of crashing
- if the stream is lost or Spotify closes, output resets to silence and the worker rediscovers Spotify every 2 seconds
- if a capture-eligible Spotify playback node reappears, the worker reconnects to it automatically
- the backend still never falls back to full-desktop monitor capture

Set `SPECTRUM_ARES_PIPEWIRE_DEBUG=1` to print internal discovery logs while enumerating.

TODO for the next PipeWire stage:

- improve reconnect robustness for harder PipeWire edge cases such as changing node identities during active playback,
- keep the public C ABI unchanged while replacing only the backend worker internals.

## Privacy

Spectrum ARES is for local audio analysis only. It does not upload audio, does not use Spotify authentication, does not call the Spotify Web API, does not capture microphone input, and does not extract DRM-protected streams.

The current PipeWire path intentionally targets the selected Spotify playback node. It does not intentionally fall back to default full-desktop output capture.

## License

Apache-2.0. See [LICENSE](LICENSE).
