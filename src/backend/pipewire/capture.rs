use std::cell::RefCell;
use std::error::Error;
use std::fmt;
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use pipewire as pw;
use pw::properties::properties;
use pw::spa;
use spa::param::audio::{AudioFormat, AudioInfoRaw};
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;

const LEVEL_EMIT_INTERVAL: Duration = Duration::from_millis(100);
const WAITING_NOTICE_DELAY: Duration = Duration::from_secs(2);
const BUFFER_STALL_NOTICE_DELAY: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureTargetId {
    ObjectSerial(u64),
    GlobalNodeId(u32),
}

impl fmt::Display for CaptureTargetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ObjectSerial(serial) => write!(formatter, "object.serial={serial}"),
            Self::GlobalNodeId(id) => write!(formatter, "global node id={id}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureTarget {
    pub node_id: u32,
    pub object_serial: Option<u64>,
    pub display_name: String,
    pub node_name: Option<String>,
}

impl CaptureTarget {
    pub fn preferred_target_id(&self) -> CaptureTargetId {
        self.object_serial
            .map(CaptureTargetId::ObjectSerial)
            .unwrap_or(CaptureTargetId::GlobalNodeId(self.node_id))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObservedAudioFormat {
    pub sample_format: AudioFormat,
    pub sample_rate: u32,
    pub channels: u32,
}

impl ObservedAudioFormat {
    pub fn is_planar(&self) -> bool {
        self.sample_format.is_planar()
    }

    pub fn layout_name(&self) -> &'static str {
        if self.is_planar() {
            "planar"
        } else {
            "interleaved"
        }
    }
}

impl fmt::Display for ObservedAudioFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?}, {} Hz, {} channel(s), {}",
            self.sample_format,
            self.sample_rate,
            self.channels,
            self.layout_name()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioLevelSnapshot {
    pub rms: f32,
    pub peak: f32,
    pub frames: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptureRunSummary {
    pub target_id: CaptureTargetId,
    pub capture_node_id: Option<u32>,
    pub observed_format: Option<ObservedAudioFormat>,
    pub last_levels: Option<AudioLevelSnapshot>,
    pub buffers_processed: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CaptureEvent {
    Info {
        message: String,
    },
    StateChanged {
        old: String,
        new: String,
        capture_node_id: Option<u32>,
    },
    FormatNegotiated {
        format: ObservedAudioFormat,
    },
    Levels {
        format: ObservedAudioFormat,
        levels: AudioLevelSnapshot,
        buffers_processed: u64,
    },
    Warning {
        message: String,
    },
}

#[derive(Debug)]
pub enum PipeWireCaptureError {
    InvalidTarget(&'static str),
    PipeWire(String),
    Stream(String),
}

impl fmt::Display for PipeWireCaptureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTarget(message) => formatter.write_str(message),
            Self::PipeWire(message) => formatter.write_str(message),
            Self::Stream(message) => formatter.write_str(message),
        }
    }
}

impl Error for PipeWireCaptureError {}

#[allow(dead_code)]
pub fn run_capture_loop<F>(
    target: &CaptureTarget,
    on_event: F,
) -> Result<CaptureRunSummary, PipeWireCaptureError>
where
    F: FnMut(CaptureEvent) + 'static,
{
    run_capture_loop_with_mono_samples(target, on_event, |_, _| {})
}

pub fn run_capture_loop_with_mono_samples<F, G>(
    target: &CaptureTarget,
    on_event: F,
    on_mono_samples: G,
) -> Result<CaptureRunSummary, PipeWireCaptureError>
where
    F: FnMut(CaptureEvent) + 'static,
    G: FnMut(ObservedAudioFormat, &[f32]) + 'static,
{
    let stop_requested = Arc::new(AtomicBool::new(false));

    ctrlc::set_handler({
        let stop_requested = stop_requested.clone();
        move || {
            stop_requested.store(true, Ordering::SeqCst);
        }
    })
    .map_err(|err| {
        PipeWireCaptureError::PipeWire(format!("install Ctrl+C handler for capture example: {err}"))
    })?;

    run_capture_loop_internal(
        target,
        stop_requested,
        Some("received Ctrl+C, stopping capture"),
        on_event,
        on_mono_samples,
    )
}

#[allow(dead_code)]
pub fn run_capture_loop_with_stop_signal<F, G>(
    target: &CaptureTarget,
    stop_requested: Arc<AtomicBool>,
    on_event: F,
    on_mono_samples: G,
) -> Result<CaptureRunSummary, PipeWireCaptureError>
where
    F: FnMut(CaptureEvent) + 'static,
    G: FnMut(ObservedAudioFormat, &[f32]) + 'static,
{
    run_capture_loop_internal(target, stop_requested, None, on_event, on_mono_samples)
}

fn run_capture_loop_internal<F, G>(
    target: &CaptureTarget,
    stop_requested: Arc<AtomicBool>,
    stop_notice: Option<&'static str>,
    on_event: F,
    on_mono_samples: G,
) -> Result<CaptureRunSummary, PipeWireCaptureError>
where
    F: FnMut(CaptureEvent) + 'static,
    G: FnMut(ObservedAudioFormat, &[f32]) + 'static,
{
    if target.node_id == 0 {
        return Err(PipeWireCaptureError::InvalidTarget(
            "target node id must be non-zero",
        ));
    }

    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|err| PipeWireCaptureError::PipeWire(format!("create main loop: {err}")))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|err| PipeWireCaptureError::PipeWire(format!("create context: {err}")))?;
    let core = context
        .connect_rc(None)
        .map_err(|err| PipeWireCaptureError::PipeWire(format!("connect to core: {err}")))?;

    let target_id = target.preferred_target_id();
    let progress = Rc::new(RefCell::new(CaptureProgress::new(target_id)));
    let observer = Rc::new(RefCell::new(on_event));
    let mono_consumer = Rc::new(RefCell::new(on_mono_samples));

    emit_event(
        &observer,
        CaptureEvent::Info {
            message: match target_id {
                CaptureTargetId::ObjectSerial(serial) => format!(
                    "targeting Spotify node {} via target.object=object.serial {}",
                    target.display_name, serial
                ),
                CaptureTargetId::GlobalNodeId(id) => format!(
                    "targeting Spotify node {} via global node id {} fallback",
                    target.display_name, id
                ),
            },
        },
    );

    let mut props = properties! {
        *pw::keys::APP_NAME => "spectrum-ares-capture",
        *pw::keys::NODE_NAME => "spectrum-ares-capture",
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
        "node.passive" => "true",
        "node.dont-reconnect" => "true",
    };

    if let CaptureTargetId::ObjectSerial(serial) = target_id {
        props.insert("target.object", serial.to_string());
    }

    let stream = pw::stream::StreamBox::new(&core, "spectrum-ares-capture", props)
        .map_err(|err| PipeWireCaptureError::PipeWire(format!("create stream: {err}")))?;

    let timer_progress = progress.clone();
    let timer_observer = observer.clone();
    let timer_stop_requested = stop_requested.clone();
    let timer_loop = mainloop.downgrade();
    let _timer = mainloop.loop_().add_timer(move |_| {
        if timer_stop_requested.load(Ordering::SeqCst) {
            let should_emit = {
                let mut progress = timer_progress.borrow_mut();
                if progress.stop_notice_emitted {
                    false
                } else {
                    progress.stop_notice_emitted = true;
                    true
                }
            };

            if should_emit {
                if let Some(message) = stop_notice {
                    emit_event(
                        &timer_observer,
                        CaptureEvent::Info {
                            message: message.to_string(),
                        },
                    );
                }
            }

            quit_loop(&timer_loop);
            return;
        }

        let event = {
            let mut progress = timer_progress.borrow_mut();
            progress.pending_timer_event()
        };

        if let Some(event) = event {
            emit_event(&timer_observer, event);
        }
    });
    let _ = _timer.update_timer(Some(LEVEL_EMIT_INTERVAL), Some(LEVEL_EMIT_INTERVAL));

    let listener_progress = progress.clone();
    let listener_observer = observer.clone();
    let listener_loop = mainloop.downgrade();
    let _listener = stream
        .add_local_listener_with_user_data(CaptureUserData {
            progress: listener_progress,
            observer: listener_observer,
            mono_consumer,
            mainloop: listener_loop,
        })
        .state_changed(|stream, user_data, old, new| {
            let capture_node_id = sanitize_node_id(stream.node_id());
            user_data.progress.borrow_mut().capture_node_id = capture_node_id;

            emit_event(
                &user_data.observer,
                CaptureEvent::StateChanged {
                    old: stream_state_name(&old),
                    new: stream_state_name(&new),
                    capture_node_id,
                },
            );

            if let pw::stream::StreamState::Error(message) = new {
                user_data.progress.borrow_mut().last_error = Some(message);
                quit_loop(&user_data.mainloop);
            }
        })
        .param_changed(|_, user_data, id, param| {
            let Some(param) = param else {
                return;
            };

            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }

            let (media_type, media_subtype) = match format_utils::parse_format(param) {
                Ok(value) => value,
                Err(err) => {
                    emit_event(
                        &user_data.observer,
                        CaptureEvent::Warning {
                            message: format!("failed to parse stream format: {err:?}"),
                        },
                    );
                    return;
                }
            };

            if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                emit_event(
                    &user_data.observer,
                    CaptureEvent::Warning {
                        message: format!(
                            "unsupported PipeWire media format: {:?} / {:?}",
                            media_type, media_subtype
                        ),
                    },
                );
                return;
            }

            let mut audio_info = AudioInfoRaw::new();
            if let Err(err) = audio_info.parse(param) {
                emit_event(
                    &user_data.observer,
                    CaptureEvent::Warning {
                        message: format!("failed to parse audio format details: {err:?}"),
                    },
                );
                return;
            }

            let observed = ObservedAudioFormat {
                sample_format: audio_info.format(),
                sample_rate: audio_info.rate(),
                channels: audio_info.channels(),
            };

            user_data.progress.borrow_mut().observed_format = Some(observed);
            emit_event(
                &user_data.observer,
                CaptureEvent::FormatNegotiated { format: observed },
            );
        })
        .process(|stream, user_data| match stream.dequeue_buffer() {
            None => {}
            Some(mut buffer) => {
                let observed_format = {
                    let progress = user_data.progress.borrow();
                    progress.observed_format
                };

                let Some(observed_format) = observed_format else {
                    let warn = {
                        let mut progress = user_data.progress.borrow_mut();
                        if progress.warned_missing_format {
                            None
                        } else {
                            progress.warned_missing_format = true;
                            Some(
                                "received audio buffers before format negotiation completed"
                                    .to_string(),
                            )
                        }
                    };
                    if let Some(message) = warn {
                        emit_event(&user_data.observer, CaptureEvent::Warning { message });
                    }
                    return;
                };

                let decoded = match decode_audio_buffer(buffer.datas_mut(), observed_format) {
                    Ok(Some(decoded)) => decoded,
                    Ok(None) => return,
                    Err(message) => {
                        let warn = {
                            let mut progress = user_data.progress.borrow_mut();
                            if progress.last_conversion_warning.as_deref() == Some(message.as_str())
                            {
                                None
                            } else {
                                progress.last_conversion_warning = Some(message.clone());
                                Some(message)
                            }
                        };
                        if let Some(message) = warn {
                            emit_event(&user_data.observer, CaptureEvent::Warning { message });
                        }
                        return;
                    }
                };

                (user_data.mono_consumer.borrow_mut())(observed_format, &decoded.mono_samples);

                let mut progress = user_data.progress.borrow_mut();
                progress.buffers_processed += 1;
                progress.last_levels = Some(decoded.levels);
                progress.last_buffer_at = Some(Instant::now());
                progress.stall_notice_emitted = false;
            }
        })
        .register()
        .map_err(|err| {
            PipeWireCaptureError::PipeWire(format!("register stream listener: {err}"))
        })?;

    let mut params = build_audio_params()?;
    stream
        .connect(
            spa::utils::Direction::Input,
            connect_target_id(target_id),
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|err| PipeWireCaptureError::PipeWire(format!("connect capture stream: {err}")))?;

    mainloop.run();

    let progress = progress.borrow();
    if let Some(message) = progress.last_error.clone() {
        Err(PipeWireCaptureError::Stream(message))
    } else {
        Ok(progress.summary())
    }
}

struct CaptureUserData<F, G>
where
    F: FnMut(CaptureEvent) + 'static,
    G: FnMut(ObservedAudioFormat, &[f32]) + 'static,
{
    progress: Rc<RefCell<CaptureProgress>>,
    observer: Rc<RefCell<F>>,
    mono_consumer: Rc<RefCell<G>>,
    mainloop: pw::main_loop::MainLoopWeak,
}

struct CaptureProgress {
    target_id: CaptureTargetId,
    capture_node_id: Option<u32>,
    observed_format: Option<ObservedAudioFormat>,
    last_levels: Option<AudioLevelSnapshot>,
    buffers_processed: u64,
    last_emitted_buffer_count: u64,
    started_at: Instant,
    last_buffer_at: Option<Instant>,
    wait_notice_emitted: bool,
    warned_missing_format: bool,
    last_conversion_warning: Option<String>,
    last_error: Option<String>,
    stop_notice_emitted: bool,
    stall_notice_emitted: bool,
}

impl CaptureProgress {
    fn new(target_id: CaptureTargetId) -> Self {
        Self {
            target_id,
            capture_node_id: None,
            observed_format: None,
            last_levels: None,
            buffers_processed: 0,
            last_emitted_buffer_count: 0,
            started_at: Instant::now(),
            last_buffer_at: None,
            wait_notice_emitted: false,
            warned_missing_format: false,
            last_conversion_warning: None,
            last_error: None,
            stop_notice_emitted: false,
            stall_notice_emitted: false,
        }
    }

    fn pending_timer_event(&mut self) -> Option<CaptureEvent> {
        if self.buffers_processed > self.last_emitted_buffer_count {
            self.last_emitted_buffer_count = self.buffers_processed;

            if let (Some(format), Some(levels)) = (self.observed_format, self.last_levels) {
                return Some(CaptureEvent::Levels {
                    format,
                    levels,
                    buffers_processed: self.buffers_processed,
                });
            }
        }

        if !self.wait_notice_emitted
            && self.buffers_processed == 0
            && self.started_at.elapsed() >= WAITING_NOTICE_DELAY
        {
            self.wait_notice_emitted = true;
            return Some(CaptureEvent::Warning {
                message: if self.observed_format.is_some() {
                    "connected to PipeWire but no audio buffers have arrived yet".to_string()
                } else {
                    "waiting for PipeWire format negotiation and first audio buffers".to_string()
                },
            });
        }

        if !self.stall_notice_emitted
            && self.buffers_processed > 0
            && self
                .last_buffer_at
                .is_some_and(|at| at.elapsed() >= BUFFER_STALL_NOTICE_DELAY)
        {
            self.stall_notice_emitted = true;
            return Some(CaptureEvent::Warning {
                message: format!(
                    "no audio buffers received for {} ms; Spotify may be paused or corked",
                    BUFFER_STALL_NOTICE_DELAY.as_millis()
                ),
            });
        }

        None
    }

    fn summary(&self) -> CaptureRunSummary {
        CaptureRunSummary {
            target_id: self.target_id,
            capture_node_id: self.capture_node_id,
            observed_format: self.observed_format,
            last_levels: self.last_levels,
            buffers_processed: self.buffers_processed,
        }
    }
}

fn build_audio_params() -> Result<[&'static Pod; 1], PipeWireCaptureError> {
    let mut audio_info = AudioInfoRaw::new();
    audio_info.set_format(AudioFormat::F32LE);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|err| PipeWireCaptureError::PipeWire(format!("serialize audio format pod: {err:?}")))?
    .0
    .into_inner();

    let pod = Pod::from_bytes(values.leak()).ok_or_else(|| {
        PipeWireCaptureError::PipeWire("construct audio format pod from bytes".to_string())
    })?;
    Ok([pod])
}

fn connect_target_id(target_id: CaptureTargetId) -> Option<u32> {
    match target_id {
        CaptureTargetId::ObjectSerial(_) => None,
        CaptureTargetId::GlobalNodeId(id) => Some(id),
    }
}

struct DecodedAudioBuffer {
    mono_samples: Vec<f32>,
    levels: AudioLevelSnapshot,
}

fn decode_audio_buffer(
    datas: &mut [spa::buffer::Data],
    observed_format: ObservedAudioFormat,
) -> Result<Option<DecodedAudioBuffer>, String> {
    let channels = usize::try_from(observed_format.channels)
        .map_err(|_| "channel count does not fit in usize".to_string())?;
    if channels == 0 {
        return Err("PipeWire negotiated zero audio channels".to_string());
    }

    if observed_format.is_planar() {
        decode_planar_audio(datas, observed_format.sample_format, channels)
    } else {
        decode_interleaved_audio(datas, observed_format.sample_format, channels)
    }
}

fn decode_interleaved_audio(
    datas: &mut [spa::buffer::Data],
    sample_format: AudioFormat,
    channels: usize,
) -> Result<Option<DecodedAudioBuffer>, String> {
    let sample_width = sample_width_bytes(sample_format)?;
    let Some(data) = datas.get_mut(0) else {
        return Ok(None);
    };
    let frame_stride = interleaved_frame_stride(data, sample_width, channels);
    let Some(bytes) = sliced_data_bytes(data) else {
        return Ok(None);
    };

    if frame_stride == 0 || bytes.len() < sample_width {
        return Ok(None);
    }

    let mut mono_samples = Vec::with_capacity(bytes.len() / frame_stride);
    let mut sum_squares = 0.0_f32;
    let mut peak = 0.0_f32;
    let mut frames = 0_usize;

    let mut frame_start = 0_usize;
    while frame_start.saturating_add(sample_width * channels) <= bytes.len() {
        let mut mono = 0.0_f32;

        for channel_index in 0..channels {
            let sample_start = frame_start + channel_index * sample_width;
            let sample_end = sample_start + sample_width;
            let sample = decode_sample(sample_format, &bytes[sample_start..sample_end])?;
            mono += sample;
        }

        mono /= channels as f32;
        mono_samples.push(mono);
        sum_squares += mono * mono;
        peak = peak.max(mono.abs());
        frames += 1;
        frame_start += frame_stride;
    }

    Ok(
        finalize_levels(sum_squares, peak, frames).map(|levels| DecodedAudioBuffer {
            mono_samples,
            levels,
        }),
    )
}

fn decode_planar_audio(
    datas: &mut [spa::buffer::Data],
    sample_format: AudioFormat,
    channels: usize,
) -> Result<Option<DecodedAudioBuffer>, String> {
    let sample_width = sample_width_bytes(sample_format)?;
    let mut planes = Vec::new();
    for data in datas.iter_mut().take(channels) {
        let stride = planar_stride(data, sample_width);
        if let Some(bytes) = sliced_data_bytes(data) {
            planes.push((bytes, stride));
        }
    }

    if planes.is_empty() {
        return Ok(None);
    }

    let frame_count = planes
        .iter()
        .map(|(plane, stride)| max_planar_frames(plane.len(), *stride, sample_width))
        .min()
        .unwrap_or(0);
    if frame_count == 0 {
        return Ok(None);
    }

    let mut mono_samples = Vec::with_capacity(frame_count);
    let mut sum_squares = 0.0_f32;
    let mut peak = 0.0_f32;

    for frame_index in 0..frame_count {
        let mut mono = 0.0_f32;

        for (plane, stride) in &planes {
            let sample_start = frame_index * *stride;
            let sample_end = sample_start + sample_width;
            let sample = decode_sample(sample_format, &plane[sample_start..sample_end])?;
            mono += sample;
        }

        mono /= planes.len() as f32;
        mono_samples.push(mono);
        sum_squares += mono * mono;
        peak = peak.max(mono.abs());
    }

    Ok(
        finalize_levels(sum_squares, peak, frame_count).map(|levels| DecodedAudioBuffer {
            mono_samples,
            levels,
        }),
    )
}

fn finalize_levels(sum_squares: f32, peak: f32, frames: usize) -> Option<AudioLevelSnapshot> {
    (frames > 0).then(|| AudioLevelSnapshot {
        rms: (sum_squares / frames as f32).sqrt(),
        peak,
        frames,
    })
}

fn interleaved_frame_stride(
    data: &spa::buffer::Data,
    sample_width: usize,
    channels: usize,
) -> usize {
    match usize::try_from(data.chunk().stride()).ok() {
        Some(stride) if stride >= sample_width * channels => stride,
        _ => sample_width * channels,
    }
}

fn planar_stride(data: &spa::buffer::Data, sample_width: usize) -> usize {
    match usize::try_from(data.chunk().stride()).ok() {
        Some(stride) if stride >= sample_width => stride,
        _ => sample_width,
    }
}

fn max_planar_frames(len: usize, stride: usize, sample_width: usize) -> usize {
    if stride == 0 || len < sample_width {
        0
    } else {
        1 + (len - sample_width) / stride
    }
}

fn sliced_data_bytes(data: &mut spa::buffer::Data) -> Option<&[u8]> {
    let chunk = data.chunk();
    let offset = usize::try_from(chunk.offset()).ok()?;
    let size = usize::try_from(chunk.size()).ok()?;
    let bytes = data.data()?;

    if offset >= bytes.len() {
        return None;
    }

    let end = offset.saturating_add(size).min(bytes.len());
    (end > offset).then_some(&bytes[offset..end])
}

fn sample_width_bytes(sample_format: AudioFormat) -> Result<usize, String> {
    match sample_format {
        AudioFormat::F32LE
        | AudioFormat::F32BE
        | AudioFormat::F32P
        | AudioFormat::S32LE
        | AudioFormat::S32BE
        | AudioFormat::S32P => Ok(4),
        AudioFormat::F64LE | AudioFormat::F64BE | AudioFormat::F64P => Ok(8),
        AudioFormat::S16LE | AudioFormat::S16BE | AudioFormat::S16P => Ok(2),
        AudioFormat::U8 | AudioFormat::U8P | AudioFormat::S8 | AudioFormat::S8P => Ok(1),
        other => Err(format!(
            "unsupported negotiated sample format for prototype capture: {other:?}"
        )),
    }
}

fn decode_sample(sample_format: AudioFormat, bytes: &[u8]) -> Result<f32, String> {
    match sample_format {
        AudioFormat::F32LE | AudioFormat::F32P => Ok(f32::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| "invalid f32 sample width".to_string())?,
        )),
        AudioFormat::F32BE => Ok(f32::from_be_bytes(
            bytes
                .try_into()
                .map_err(|_| "invalid f32 sample width".to_string())?,
        )),
        AudioFormat::F64LE | AudioFormat::F64P => Ok(f64::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| "invalid f64 sample width".to_string())?,
        ) as f32),
        AudioFormat::F64BE => Ok(f64::from_be_bytes(
            bytes
                .try_into()
                .map_err(|_| "invalid f64 sample width".to_string())?,
        ) as f32),
        AudioFormat::S16LE | AudioFormat::S16P => Ok(i16::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| "invalid s16 sample width".to_string())?,
        ) as f32
            / i16::MAX as f32),
        AudioFormat::S16BE => Ok(i16::from_be_bytes(
            bytes
                .try_into()
                .map_err(|_| "invalid s16 sample width".to_string())?,
        ) as f32
            / i16::MAX as f32),
        AudioFormat::S32LE | AudioFormat::S32P => Ok(i32::from_le_bytes(
            bytes
                .try_into()
                .map_err(|_| "invalid s32 sample width".to_string())?,
        ) as f32
            / i32::MAX as f32),
        AudioFormat::S32BE => Ok(i32::from_be_bytes(
            bytes
                .try_into()
                .map_err(|_| "invalid s32 sample width".to_string())?,
        ) as f32
            / i32::MAX as f32),
        AudioFormat::U8 | AudioFormat::U8P => Ok((bytes[0] as f32 - 128.0) / 128.0),
        AudioFormat::S8 | AudioFormat::S8P => Ok((bytes[0] as i8) as f32 / i8::MAX as f32),
        other => Err(format!(
            "unsupported negotiated sample format for prototype capture: {other:?}"
        )),
    }
}

fn emit_event<F>(observer: &Rc<RefCell<F>>, event: CaptureEvent)
where
    F: FnMut(CaptureEvent) + 'static,
{
    (observer.borrow_mut())(event);
}

fn quit_loop(mainloop: &pw::main_loop::MainLoopWeak) {
    if let Some(mainloop) = mainloop.upgrade() {
        mainloop.quit();
    }
}

fn sanitize_node_id(node_id: u32) -> Option<u32> {
    (node_id != u32::MAX && node_id != 0).then_some(node_id)
}

fn stream_state_name(state: &pw::stream::StreamState) -> String {
    match state {
        pw::stream::StreamState::Error(message) => format!("Error({message})"),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_serial_is_preferred_over_global_id() {
        let target = CaptureTarget {
            node_id: 138,
            object_serial: Some(312),
            display_name: "audio-src".to_string(),
            node_name: Some("audio-src".to_string()),
        };

        assert_eq!(
            target.preferred_target_id(),
            CaptureTargetId::ObjectSerial(312)
        );
    }

    #[test]
    fn interleaved_f32_stereo_downmix_produces_expected_levels() {
        let samples = [
            1.0_f32.to_le_bytes(),
            0.0_f32.to_le_bytes(),
            (-1.0_f32).to_le_bytes(),
            0.0_f32.to_le_bytes(),
        ]
        .concat();

        let levels = compute_interleaved_mono_levels_for_test(&samples, AudioFormat::F32LE, 2)
            .expect("interleaved levels should parse")
            .expect("levels should be present");

        assert!((levels.rms - 0.5).abs() < 0.0001);
        assert!((levels.peak - 0.5).abs() < 0.0001);
        assert_eq!(levels.frames, 2);
    }

    #[test]
    fn planar_s16_stereo_downmix_produces_expected_levels() {
        let left = [32767_i16.to_le_bytes(), 0_i16.to_le_bytes()].concat();
        let right = [0_i16.to_le_bytes(), 32767_i16.to_le_bytes()].concat();

        let levels = compute_planar_mono_levels_for_test(
            &[left.as_slice(), right.as_slice()],
            AudioFormat::S16P,
        )
        .expect("planar levels should parse")
        .expect("levels should be present");

        assert!((levels.rms - 0.5).abs() < 0.01);
        assert!((levels.peak - 0.5).abs() < 0.01);
        assert_eq!(levels.frames, 2);
    }

    #[test]
    fn unsupported_formats_are_rejected() {
        let result = compute_interleaved_mono_levels_for_test(&[0_u8; 8], AudioFormat::S24_32LE, 2);

        assert!(result.is_err());
    }

    fn compute_interleaved_mono_levels_for_test(
        bytes: &[u8],
        sample_format: AudioFormat,
        channels: usize,
    ) -> Result<Option<AudioLevelSnapshot>, String> {
        let sample_width = sample_width_bytes(sample_format)?;
        let frame_stride = sample_width * channels;
        if frame_stride == 0 || bytes.len() < sample_width {
            return Ok(None);
        }

        let mut sum_squares = 0.0_f32;
        let mut peak = 0.0_f32;
        let mut frames = 0_usize;

        for frame in bytes.chunks_exact(frame_stride) {
            let mut mono = 0.0_f32;
            for channel in 0..channels {
                let start = channel * sample_width;
                let end = start + sample_width;
                mono += decode_sample(sample_format, &frame[start..end])?;
            }
            mono /= channels as f32;
            sum_squares += mono * mono;
            peak = peak.max(mono.abs());
            frames += 1;
        }

        Ok(finalize_levels(sum_squares, peak, frames))
    }

    fn compute_planar_mono_levels_for_test(
        planes: &[&[u8]],
        sample_format: AudioFormat,
    ) -> Result<Option<AudioLevelSnapshot>, String> {
        let sample_width = sample_width_bytes(sample_format)?;
        let frame_count = planes
            .iter()
            .map(|plane| plane.len() / sample_width)
            .min()
            .unwrap_or(0);
        if frame_count == 0 {
            return Ok(None);
        }

        let mut sum_squares = 0.0_f32;
        let mut peak = 0.0_f32;

        for frame_index in 0..frame_count {
            let mut mono = 0.0_f32;
            for plane in planes {
                let start = frame_index * sample_width;
                let end = start + sample_width;
                mono += decode_sample(sample_format, &plane[start..end])?;
            }
            mono /= planes.len() as f32;
            sum_squares += mono * mono;
            peak = peak.max(mono.abs());
        }

        Ok(finalize_levels(sum_squares, peak, frame_count))
    }
}
