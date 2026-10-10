// Copyright (C) 2026 The Argus Capture community
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use std::collections::BTreeSet;
use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

/// Whisper expects 16 kHz mono f32 samples.
const TARGET_SAMPLE_RATE: u32 = 16_000;
/// Voice commands are short; cap the recording so memory stays bounded.
const MAX_RECORDING_SECONDS: usize = 15;
/// Recordings shorter than this are treated as accidental key presses.
const MIN_RECORDING_SECONDS: f32 = 0.3;
/// whisper.cpp needs at least one second of audio, so short clips are padded
/// with silence up to this length before recognition.
const PAD_TO_SECONDS: f32 = 1.2;
/// RMS below this (about -46 dBFS) is treated as silence. Speech models can
/// hallucinate text on silence, so we skip them instead.
const SILENCE_RMS_THRESHOLD: f32 = 0.005;
/// Segments Whisper itself considers "probably not speech" are dropped.
const NO_SPEECH_THRESHOLD: f32 = 0.6;

/// English-only "base" model (ggml format) from the whisper.cpp project.
const MODEL_FILE: &str = "ggml-base.en.bin";
const MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";
/// Used for the progress bar until the server tells us the real size.
const APPROX_DOWNLOAD_BYTES: u64 = 148_000_000;

/// Optional: path to your own ggml Whisper model file. Skips the download.
const MODEL_ENV: &str = "ARGUS_CAPTURE_VOICE_MODEL";
/// Optional: folder for the downloaded model (default `~/.argus-capture/models`).
const CACHE_ENV: &str = "ARGUS_CAPTURE_VOICE_CACHE";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum VoiceCommand {
    Capture,
    Focus,
    Disconnect,
}

impl VoiceCommand {
    /// Name of the GIO action (without the `app.` prefix).
    pub(crate) fn action_name(self) -> &'static str {
        match self {
            Self::Capture => "camera-capture",
            Self::Focus => "camera-focus",
            Self::Disconnect => "camera-disconnect",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Capture => "capture",
            Self::Focus => "focus",
            Self::Disconnect => "disconnect",
        }
    }
}

/// Maps a transcript onto at most one command.
///
/// Matching is by whole word. There is deliberately no "connect" command:
/// voice control is only enabled while the camera is connected, so it could
/// never do anything. If the transcript names several different commands, or contains a
/// negation, nothing is returned: a missed command is cheaper than a
/// wrong shutter release.
pub(crate) fn parse_command(transcript: &str) -> Option<VoiceCommand> {
    let words: Vec<String> = transcript
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let has = |candidates: &[&str]| words.iter().any(|word| candidates.contains(&word.as_str()));

    if has(&["not", "dont", "don", "no", "cancel", "stop", "wait"]) {
        return None;
    }

    let mut matches = BTreeSet::new();
    if has(&["capture", "shoot", "photo", "picture", "snap", "shutter"]) {
        matches.insert(VoiceCommand::Capture);
    }
    if has(&["focus", "autofocus"]) {
        matches.insert(VoiceCommand::Focus);
    }
    if has(&["disconnect"]) {
        matches.insert(VoiceCommand::Disconnect);
    }

    if matches.len() == 1 {
        matches.into_iter().next()
    } else {
        None
    }
}

/// `$ARGUS_CAPTURE_VOICE_CACHE`, or `~/.argus-capture/models`.
pub(crate) fn model_cache_dir() -> Option<PathBuf> {
    if let Some(path) = env::var_os(CACHE_ENV) {
        return Some(PathBuf::from(path));
    }

    let home = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".argus-capture").join("models"))
}

/// The model file to load: `$ARGUS_CAPTURE_VOICE_MODEL` if set, otherwise
/// `<cache>/ggml-base.en.bin`.
fn model_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os(MODEL_ENV) {
        return Some(PathBuf::from(path));
    }

    model_cache_dir().map(|cache| cache.join(MODEL_FILE))
}

/// True once the model file exists. The downloader writes to a `.part` file
/// and renames it only when the download is complete, so an existing model
/// file is always a finished one.
pub(crate) fn is_model_cached() -> bool {
    model_path().map(|path| path.is_file()).unwrap_or(false)
}

/// True when voice commands can run right now: the model file is on disk, or
/// it was already loaded into memory earlier in this session (deleting the
/// file afterwards does not affect an already loaded model).
pub(crate) fn is_voice_ready() -> bool {
    RECOGNIZER.get().is_some() || is_model_cached()
}

/// Where the model is (or will be) stored, for showing to the user.
pub(crate) fn model_location() -> Option<PathBuf> {
    model_path()
}

/// Rough download size for prompts, for example `148 MB`.
pub(crate) fn approx_download_size() -> String {
    format_bytes(APPROX_DOWNLOAD_BYTES)
}

// Progress of the current download, shared between the worker thread (writes)
// and the GTK thread (reads).
static DOWNLOADED_BYTES: AtomicU64 = AtomicU64::new(0);
static TOTAL_BYTES: AtomicU64 = AtomicU64::new(0);

/// How far the first-use model download has got.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DownloadProgress {
    pub(crate) downloaded_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) complete: bool,
}

impl DownloadProgress {
    /// 0.0 to 1.0 for a progress bar.
    pub(crate) fn fraction(&self) -> f64 {
        if self.complete {
            return 1.0;
        }
        if self.total_bytes == 0 {
            return 0.0;
        }
        (self.downloaded_bytes as f64 / self.total_bytes as f64).clamp(0.0, 1.0)
    }

    /// Text for the progress bar, for example `88 MB of 148 MB (59%)`.
    pub(crate) fn label(&self) -> String {
        format!(
            "{} of {} ({:.0}%)",
            format_bytes(self.downloaded_bytes),
            format_bytes(self.total_bytes),
            self.fraction() * 100.0
        )
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    } else {
        format!("{} MB", bytes / 1_000_000)
    }
}

/// Reads the shared download counters. Cheap enough to call twice a second.
pub(crate) fn download_progress() -> DownloadProgress {
    DownloadProgress {
        downloaded_bytes: DOWNLOADED_BYTES.load(Ordering::Relaxed),
        total_bytes: TOTAL_BYTES.load(Ordering::Relaxed),
        complete: is_model_cached(),
    }
}

static DOWNLOADING: AtomicBool = AtomicBool::new(false);

pub(crate) fn is_model_downloading() -> bool {
    DOWNLOADING.load(Ordering::Acquire)
}

struct DownloadGuard;

impl DownloadGuard {
    fn acquire() -> Option<Self> {
        DOWNLOADING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self)
    }
}

impl Drop for DownloadGuard {
    fn drop(&mut self) {
        DOWNLOADING.store(false, Ordering::Release);
    }
}

/// Downloads the model on a background thread. This is the only place the
/// model is ever downloaded, so it always happens because the user asked for
/// it. Poll the receiver from the main thread with `glib::timeout_add_local`
/// and read `download_progress()` meanwhile.
pub(crate) fn spawn_model_download() -> mpsc::Receiver<Result<(), String>> {
    let (sender, receiver) = mpsc::channel();

    let Some(guard) = DownloadGuard::acquire() else {
        let _ = sender.send(Err("The voice model is already downloading.".to_owned()));
        return receiver;
    };

    // Reset here, not in the worker, so the first progress poll cannot show
    // numbers left over from an earlier download.
    DOWNLOADED_BYTES.store(0, Ordering::Relaxed);
    TOTAL_BYTES.store(APPROX_DOWNLOAD_BYTES, Ordering::Relaxed);

    thread::spawn(move || {
        let _guard = guard;
        let result = match model_path() {
            Some(path) => ensure_model(&path),
            None => Err("HOME is not set; cannot locate the voice model folder".to_owned()),
        };
        let _ = sender.send(result);
    });
    receiver
}

fn ensure_model(path: &Path) -> Result<(), String> {
    if path.is_file() {
        return Ok(());
    }

    // A custom model path is the user's own file; never overwrite it with a
    // download.
    if env::var_os(MODEL_ENV).is_some() {
        return Err(format!(
            "{MODEL_ENV} points to {}, which does not exist.",
            path.display()
        ));
    }

    download_model(path)
}

/// Downloads the model to `path`, updating the progress counters.
///
/// Data goes to `<file>.part` first and is renamed on success, so a crash or
/// lost connection never leaves a half-written model that looks complete.
fn download_model(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }

    DOWNLOADED_BYTES.store(0, Ordering::Relaxed);
    TOTAL_BYTES.store(APPROX_DOWNLOAD_BYTES, Ordering::Relaxed);

    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .build();
    let response = agent
        .get(MODEL_URL)
        .call()
        .map_err(|error| format!("Could not download the voice model: {error}"))?;

    let expected_bytes = response
        .header("Content-Length")
        .and_then(|value| value.trim().parse::<u64>().ok());
    if let Some(total) = expected_bytes {
        TOTAL_BYTES.store(total, Ordering::Relaxed);
    }

    let partial_path = path.with_file_name(format!("{MODEL_FILE}.part"));
    let result = copy_with_progress(response.into_reader(), &partial_path, expected_bytes);
    if let Err(error) = result {
        let _ = fs::remove_file(&partial_path);
        return Err(error);
    }

    fs::rename(&partial_path, path)
        .map_err(|error| format!("Could not save the voice model: {error}"))
}

fn copy_with_progress(
    mut reader: impl Read,
    partial_path: &Path,
    expected_bytes: Option<u64>,
) -> Result<(), String> {
    let mut file = File::create(partial_path)
        .map_err(|error| format!("Could not write {}: {error}", partial_path.display()))?;
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut downloaded = 0_u64;

    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("Voice model download interrupted: {error}"))?;
        if read == 0 {
            break;
        }

        file.write_all(&buffer[..read])
            .map_err(|error| format!("Could not write the voice model: {error}"))?;
        downloaded += read as u64;
        DOWNLOADED_BYTES.store(downloaded, Ordering::Relaxed);
    }

    file.flush()
        .map_err(|error| format!("Could not write the voice model: {error}"))?;

    if let Some(expected) = expected_bytes
        && downloaded != expected
    {
        return Err(format!(
            "Voice model download incomplete ({downloaded} of {expected} bytes)."
        ));
    }

    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct Transcript {
    pub(crate) text: String,
    pub(crate) language: String,
    pub(crate) audio_seconds: f32,
    pub(crate) elapsed: Duration,
}

impl Transcript {
    /// Processing time divided by audio length; below 1.0 is faster than
    /// real time. Handy for benchmarking on different machines.
    pub(crate) fn real_time_factor(&self) -> f32 {
        if self.audio_seconds <= f32::EPSILON {
            return 0.0;
        }
        self.elapsed.as_secs_f32() / self.audio_seconds
    }
}

struct Recognizer {
    // Kept alive for the lifetime of the state.
    _context: WhisperContext,
    state: Mutex<WhisperState>,
}

impl Recognizer {
    fn load() -> Result<Self, String> {
        let path = model_path()
            .ok_or_else(|| "HOME is not set; cannot locate the voice model folder".to_owned())?;

        if !path.is_file() {
            return Err(format!(
                "Voice model not found at {}. Use Edit > Configuration > Voice.",
                path.display()
            ));
        }

        // CPU by default. GPU backends are enabled at build time (see the
        // whisper-rs features), and `use_gpu` then defaults to true.
        let context = WhisperContext::new_with_params(&path, WhisperContextParameters::default())
            .map_err(|error| format!("Failed to load the voice model: {error}"))?;
        let state = context
            .create_state()
            .map_err(|error| format!("Failed to prepare the voice model: {error}"))?;

        Ok(Self {
            _context: context,
            state: Mutex::new(state),
        })
    }

    fn transcribe(&self, samples: &[f32]) -> Result<String, String> {
        let samples = pad_to_minimum_length(samples);
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Speech recognizer is unavailable.".to_owned())?;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_translate(false);
        params.set_n_threads(recognition_threads());
        params.set_single_segment(true);
        params.set_no_context(true);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);

        state
            .full(params, &samples)
            .map_err(|error| format!("Speech recognition failed: {error}"))?;

        let mut text = String::new();
        for segment in state.as_iter() {
            if segment.no_speech_probability() > NO_SPEECH_THRESHOLD {
                continue;
            }
            let piece = segment
                .to_str_lossy()
                .map_err(|error| format!("Could not read the recognized text: {error}"))?;
            text.push_str(&piece);
        }

        Ok(text.trim().to_owned())
    }
}

fn recognition_threads() -> std::ffi::c_int {
    thread::available_parallelism()
        .map(|count| count.get().min(8))
        .unwrap_or(4) as std::ffi::c_int
}

/// whisper.cpp rejects clips shorter than one second, so pad short ones with
/// silence. Longer clips are returned unchanged.
fn pad_to_minimum_length(samples: &[f32]) -> Vec<f32> {
    let minimum = (TARGET_SAMPLE_RATE as f32 * PAD_TO_SECONDS) as usize;
    let mut padded = samples.to_vec();
    if padded.len() < minimum {
        padded.resize(minimum, 0.0);
    }
    padded
}

// Loading is slow, so it happens lazily on the worker thread. Only a
// successful load is kept, so a failed download (for example, no internet)
// can be retried without restarting the app.
static RECOGNIZER: OnceLock<Recognizer> = OnceLock::new();

fn transcribe_blocking(samples: &[f32]) -> Result<Transcript, String> {
    let recognizer = match RECOGNIZER.get() {
        Some(recognizer) => recognizer,
        None => {
            let loaded = Recognizer::load()?;
            RECOGNIZER.get_or_init(|| loaded)
        }
    };

    let started = Instant::now();
    let text = recognizer.transcribe(samples)?;

    Ok(Transcript {
        text,
        language: "en".to_owned(),
        audio_seconds: samples.len() as f32 / TARGET_SAMPLE_RATE as f32,
        elapsed: started.elapsed(),
    })
}

/// Runs recognition on a background thread so the GTK main loop never blocks.
/// Poll the receiver from the main thread with `glib::timeout_add_local`.
pub(crate) fn spawn_transcription(samples: Vec<f32>) -> mpsc::Receiver<Result<Transcript, String>> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let _ = sender.send(transcribe_blocking(&samples));
    });
    receiver
}

/// True for recordings that are too short or too quiet to contain speech.
pub(crate) fn is_probably_silence(samples: &[f32]) -> bool {
    let minimum_len = (TARGET_SAMPLE_RATE as f32 * MIN_RECORDING_SECONDS) as usize;
    if samples.len() < minimum_len {
        return true;
    }

    let mean_square =
        samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32;
    mean_square.sqrt() < SILENCE_RMS_THRESHOLD
}

/// Records from the default microphone until `finish` is called.
///
/// Not `Send` (CoreAudio streams must stay on their thread), so keep it on
/// the GTK main thread.
pub(crate) struct Recorder {
    stream: Stream,
    buffer: Arc<Mutex<Vec<f32>>>,
    sample_rate: u32,
}

impl Recorder {
    pub(crate) fn start() -> Result<Self, String> {
        let device = cpal::default_host()
            .default_input_device()
            .ok_or_else(|| "No microphone found.".to_owned())?;
        let supported = device
            .default_input_config()
            .map_err(|error| format!("Microphone configuration error: {error}"))?;

        let sample_rate = supported.sample_rate().0;
        let channels = usize::from(supported.channels());
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let max_samples = sample_rate as usize * MAX_RECORDING_SECONDS;

        let stream = match format {
            SampleFormat::F32 => {
                build_stream::<f32>(&device, &config, channels, max_samples, &buffer)
            }
            SampleFormat::I16 => {
                build_stream::<i16>(&device, &config, channels, max_samples, &buffer)
            }
            SampleFormat::U16 => {
                build_stream::<u16>(&device, &config, channels, max_samples, &buffer)
            }
            other => Err(format!("Unsupported microphone sample format: {other:?}")),
        }?;
        stream
            .play()
            .map_err(|error| format!("Could not start microphone: {error}"))?;

        Ok(Self {
            stream,
            buffer,
            sample_rate,
        })
    }

    /// Stops recording and returns 16 kHz mono samples.
    pub(crate) fn finish(self) -> Vec<f32> {
        drop(self.stream);
        let raw = self
            .buffer
            .lock()
            .map(|samples| samples.clone())
            .unwrap_or_default();
        resample_to_16k(&raw, self.sample_rate)
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    max_samples: usize,
    buffer: &Arc<Mutex<Vec<f32>>>,
) -> Result<Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let buffer = Arc::clone(buffer);
    device
        .build_input_stream(
            config,
            move |data: &[T], _| {
                let Ok(mut samples) = buffer.lock() else {
                    return;
                };
                for frame in data.chunks(channels.max(1)) {
                    if samples.len() >= max_samples {
                        break;
                    }
                    // Downmix to mono.
                    let sum: f32 = frame.iter().map(|sample| sample.to_sample::<f32>()).sum();
                    samples.push(sum / frame.len() as f32);
                }
            },
            |error| eprintln!("[argus-capture voice] audio stream error: {error}"),
            None,
        )
        .map_err(|error| format!("Could not open microphone: {error}"))
}

/// Box-filter decimation to 16 kHz. Averaging each source window acts as a
/// crude low-pass filter, which is adequate for speech. For higher fidelity,
/// swap in the `rubato` crate.
fn resample_to_16k(input: &[f32], from_rate: u32) -> Vec<f32> {
    if from_rate == TARGET_SAMPLE_RATE || input.is_empty() {
        return input.to_vec();
    }

    let ratio = f64::from(from_rate) / f64::from(TARGET_SAMPLE_RATE);
    let output_len = (input.len() as f64 / ratio) as usize;
    let mut output = Vec::with_capacity(output_len);

    for index in 0..output_len {
        let start = (index as f64 * ratio) as usize;
        let end = (((index + 1) as f64 * ratio) as usize)
            .min(input.len())
            .max(start + 1);
        let window = &input[start..end];
        output.push(window.iter().sum::<f32>() / window.len() as f32);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_phrases_to_commands() {
        assert_eq!(parse_command("Capture."), Some(VoiceCommand::Capture));
        assert_eq!(
            parse_command("Take a picture!"),
            Some(VoiceCommand::Capture)
        );
        assert_eq!(parse_command("  SHOOT  "), Some(VoiceCommand::Capture));
        assert_eq!(parse_command("focus"), Some(VoiceCommand::Focus));
        // Voice only works while connected, so "connect" is not a command.
        assert_eq!(parse_command("Connect the camera"), None);
    }

    #[test]
    fn recognizes_the_disconnect_command() {
        assert_eq!(parse_command("disconnect"), Some(VoiceCommand::Disconnect));
        assert_eq!(
            parse_command("Disconnect the camera."),
            Some(VoiceCommand::Disconnect)
        );
    }

    #[test]
    fn ignores_ambiguous_negated_and_unrelated_speech() {
        assert_eq!(parse_command("focus and capture"), None);
        assert_eq!(parse_command("don't take a picture"), None);
        assert_eq!(parse_command("do not capture"), None);
        assert_eq!(parse_command("stop"), None);
        assert_eq!(parse_command("the weather is nice today"), None);
        assert_eq!(parse_command(""), None);
    }

    #[test]
    fn command_actions_match_shortcut_action_names() {
        assert_eq!(VoiceCommand::Capture.action_name(), "camera-capture");
        assert_eq!(VoiceCommand::Focus.action_name(), "camera-focus");
        assert_eq!(VoiceCommand::Disconnect.action_name(), "camera-disconnect");
    }

    #[test]
    fn detects_silence_and_short_recordings() {
        assert!(is_probably_silence(&[]));
        assert!(is_probably_silence(&vec![0.5; 100]));
        assert!(is_probably_silence(&vec![0.0; 16_000]));
        assert!(!is_probably_silence(&vec![0.1; 16_000]));
    }

    #[test]
    fn pads_short_clips_but_not_long_ones() {
        let short = pad_to_minimum_length(&vec![0.1; 8_000]);
        assert_eq!(short.len(), 19_200);
        assert!(short[..8_000].iter().all(|sample| *sample == 0.1));
        assert!(short[8_000..].iter().all(|sample| *sample == 0.0));

        let long = vec![0.1; 40_000];
        assert_eq!(pad_to_minimum_length(&long), long);
    }

    #[test]
    fn resamples_48k_to_16k() {
        let input = vec![0.25_f32; 4_800];
        let output = resample_to_16k(&input, 48_000);

        assert_eq!(output.len(), 1_600);
        assert!(output.iter().all(|sample| (*sample - 0.25).abs() < 1e-6));
    }

    #[test]
    fn leaves_16k_audio_untouched() {
        let input = vec![0.1, 0.2, 0.3];
        assert_eq!(resample_to_16k(&input, 16_000), input);
    }

    #[test]
    fn progress_fraction_uses_exact_totals() {
        let half = DownloadProgress {
            downloaded_bytes: 74_000_000,
            total_bytes: 148_000_000,
            complete: false,
        };
        assert!((half.fraction() - 0.5).abs() < 1e-9);

        let unknown_total = DownloadProgress {
            downloaded_bytes: 10,
            total_bytes: 0,
            complete: false,
        };
        assert_eq!(unknown_total.fraction(), 0.0);

        let done = DownloadProgress {
            downloaded_bytes: 0,
            total_bytes: 148_000_000,
            complete: true,
        };
        assert_eq!(done.fraction(), 1.0);
    }

    #[test]
    fn progress_label_shows_sizes_and_percent() {
        let progress = DownloadProgress {
            downloaded_bytes: 88_000_000,
            total_bytes: 148_000_000,
            complete: false,
        };
        assert_eq!(progress.label(), "88 MB of 148 MB (59%)");
    }

    #[test]
    fn copy_reports_progress_and_detects_truncation() {
        let directory = env::temp_dir().join(format!("argus-voice-test-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let target = directory.join("model.part");

        let data = vec![7_u8; 200_000];
        assert!(copy_with_progress(&data[..], &target, Some(200_000)).is_ok());
        assert_eq!(fs::metadata(&target).unwrap().len(), 200_000);
        assert_eq!(DOWNLOADED_BYTES.load(Ordering::Relaxed), 200_000);

        let error = copy_with_progress(&data[..], &target, Some(300_000)).unwrap_err();
        assert!(error.contains("incomplete"));

        let _ = fs::remove_dir_all(&directory);
    }

    #[test]
    fn only_one_download_can_run_at_a_time() {
        let first = DownloadGuard::acquire();
        assert!(first.is_some());
        assert!(is_model_downloading());
        assert!(DownloadGuard::acquire().is_none());

        drop(first);
        assert!(!is_model_downloading());
        assert!(DownloadGuard::acquire().is_some());
    }

    #[test]
    fn real_time_factor_is_elapsed_over_audio_length() {
        let transcript = Transcript {
            text: String::new(),
            language: String::new(),
            audio_seconds: 4.0,
            elapsed: Duration::from_secs(2),
        };
        assert!((transcript.real_time_factor() - 0.5).abs() < 1e-6);
    }
}
