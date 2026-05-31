use crate::managers::transcription::TranscriptionManager;
use crate::settings::{get_settings, write_settings, ModelUnloadTimeout};
use chrono::Utc;
use rubato::{FftFixedIn, Resampler};
use serde::Serialize;
use specta::Type;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use tauri::{AppHandle, State};

use crate::managers::history::HistoryManager;

const TARGET_SAMPLE_RATE: u32 = 16_000;
const SUPPORTED_AUDIO_EXTENSIONS: [&str; 6] = ["mp3", "mp4", "m4a", "wav", "flac", "ogg"];

#[derive(Serialize, Type)]
pub struct ModelLoadStatus {
    is_loaded: bool,
    current_model: Option<String>,
}

#[tauri::command]
#[specta::specta]
pub fn set_model_unload_timeout(app: AppHandle, timeout: ModelUnloadTimeout) {
    let mut settings = get_settings(&app);
    settings.model_unload_timeout = timeout;
    write_settings(&app, settings);
}

#[tauri::command]
#[specta::specta]
pub fn get_model_load_status(
    transcription_manager: State<TranscriptionManager>,
) -> Result<ModelLoadStatus, String> {
    Ok(ModelLoadStatus {
        is_loaded: transcription_manager.is_model_loaded(),
        current_model: transcription_manager.get_current_model(),
    })
}

#[tauri::command]
#[specta::specta]
pub fn unload_model_manually(
    transcription_manager: State<TranscriptionManager>,
) -> Result<(), String> {
    transcription_manager
        .unload_model()
        .map_err(|e| format!("Failed to unload model: {}", e))
}

#[tauri::command]
#[specta::specta]
pub async fn transcribe_file(
    file_path: String,
    transcription_manager: State<'_, Arc<TranscriptionManager>>,
    history_manager: State<'_, Arc<HistoryManager>>,
) -> Result<String, String> {
    let input_path = PathBuf::from(&file_path);

    if !input_path.is_file() {
        return Err("Selected file could not be found.".to_string());
    }

    if !is_supported_audio_file(&input_path) {
        return Err(
            "Unsupported audio format. Allowed formats: MP3, MP4, M4A, WAV, FLAC, OGG.".to_string(),
        );
    }

    transcription_manager.initiate_model_load();

    let decode_path = input_path.clone();
    let decoded_samples =
        tauri::async_runtime::spawn_blocking(move || decode_audio_file_to_mono(&decode_path))
            .await
            .map_err(|e| format!("Audio decoding failed: {}", e))??;

    if decoded_samples.is_empty() {
        return Err("Selected file does not contain audio samples.".to_string());
    }

    let samples_for_history = decoded_samples.clone();
    let tm = Arc::clone(&transcription_manager);
    let transcription =
        tauri::async_runtime::spawn_blocking(move || tm.transcribe(decoded_samples))
            .await
            .map_err(|e| format!("Transcription task failed: {}", e))?
            .map_err(|e| format!("Transcription failed: {}", e))?;

    let original_stem = input_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("audio");
    let safe_stem = sanitize_file_stem(original_stem);
    let file_name = format!(
        "file-upload-{}-{}.wav",
        safe_stem,
        Utc::now().timestamp_millis()
    );
    let wav_path = history_manager.recordings_dir().join(&file_name);
    let wav_path_for_save = wav_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::audio_toolkit::save_wav_file(&wav_path_for_save, &samples_for_history)
    })
    .await
    .map_err(|e| format!("Saving decoded WAV failed: {}", e))?
    .map_err(|e| format!("Saving decoded WAV failed: {}", e))?;

    history_manager
        .save_entry(file_name, transcription.clone(), false, None, None)
        .map_err(|e| format!("Saving history entry failed: {}", e))?;

    Ok(transcription)
}

fn is_supported_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .is_some_and(|ext| SUPPORTED_AUDIO_EXTENSIONS.contains(&ext.as_str()))
}

fn decode_audio_file_to_mono(path: &Path) -> Result<Vec<f32>, String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("Failed to open audio file: {}", e))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|ext| ext.to_str()) {
        hint.with_extension(extension);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("Failed to detect audio format: {}", e))?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "No valid audio track found.".to_string())?;

    let track_id = track.id;
    let track_sample_rate = track.codec_params.sample_rate;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Failed to initialize audio decoder: {}", e))?;

    let mut mono_samples = Vec::new();
    let mut decoded_sample_rate = track_sample_rate;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(_)) => break,
            Err(SymphoniaError::ResetRequired) => {
                return Err("Audio stream requires decoder reset.".to_string());
            }
            Err(e) => return Err(format!("Failed to read audio stream: {}", e)),
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("Failed to decode audio: {}", e)),
        };

        let spec = *decoded.spec();
        decoded_sample_rate = Some(spec.rate);
        let channel_count = spec.channels.count();
        if channel_count == 0 {
            continue;
        }

        let mut sample_buffer = SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        sample_buffer.copy_interleaved_ref(decoded);

        for frame in sample_buffer.samples().chunks(channel_count) {
            let sum: f32 = frame.iter().copied().sum();
            mono_samples.push(sum / channel_count as f32);
        }
    }

    if mono_samples.is_empty() {
        return Ok(Vec::new());
    }

    let source_sample_rate =
        decoded_sample_rate.ok_or_else(|| "Unable to determine file sample rate.".to_string())?;

    resample_to_target_rate(&mono_samples, source_sample_rate, TARGET_SAMPLE_RATE)
}

fn resample_to_target_rate(
    samples: &[f32],
    source_sample_rate: u32,
    target_sample_rate: u32,
) -> Result<Vec<f32>, String> {
    if source_sample_rate == target_sample_rate {
        return Ok(samples.to_vec());
    }

    // Chosen as a balance between throughput and memory usage for long uploads.
    const RESAMPLE_CHUNK_SIZE: usize = 2048;

    let mut resampler = FftFixedIn::<f32>::new(
        source_sample_rate as usize,
        target_sample_rate as usize,
        RESAMPLE_CHUNK_SIZE,
        1,
        1,
    )
    .map_err(|e| format!("Failed to initialize resampler: {}", e))?;

    let mut output = Vec::new();
    let mut offset = 0;

    while offset < samples.len() {
        let end = (offset + RESAMPLE_CHUNK_SIZE).min(samples.len());
        let mut chunk = samples[offset..end].to_vec();
        if chunk.len() < RESAMPLE_CHUNK_SIZE {
            // Final chunk is zero-padded because FftFixedIn operates on a fixed input length.
            chunk.resize(RESAMPLE_CHUNK_SIZE, 0.0);
        }

        let resampled_chunk = resampler
            .process(&[&chunk], None)
            .map_err(|e| format!("Resampling failed: {}", e))?;

        output.extend_from_slice(&resampled_chunk[0]);
        offset = end;
    }

    if output.is_empty() {
        return Err("Resampling returned no audio samples.".to_string());
    }

    let expected_len = ((samples.len() as f64 * target_sample_rate as f64)
        / source_sample_rate as f64)
        .round() as usize;
    output.truncate(expected_len.max(1));

    Ok(output)
}

fn sanitize_file_stem(file_stem: &str) -> String {
    let sanitized: String = file_stem
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();

    let compact = sanitized.trim_matches('-');
    if compact.is_empty() {
        "audio".to_string()
    } else {
        compact.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_extensions_are_detected_case_insensitive() {
        assert!(is_supported_audio_file(Path::new("/tmp/input.MP3")));
        assert!(is_supported_audio_file(Path::new("/tmp/input.m4a")));
        assert!(is_supported_audio_file(Path::new("/tmp/input.flac")));
    }

    #[test]
    fn unsupported_extension_is_rejected() {
        assert!(!is_supported_audio_file(Path::new("/tmp/input.txt")));
    }

    #[test]
    fn resampling_skips_when_rate_matches() {
        let samples = vec![0.1, -0.2, 0.3];
        let result = resample_to_target_rate(&samples, TARGET_SAMPLE_RATE, TARGET_SAMPLE_RATE)
            .expect("resampling should succeed");
        assert_eq!(samples, result);
    }

    #[test]
    fn sanitize_file_stem_replaces_unsafe_characters() {
        assert_eq!(
            sanitize_file_stem("hello world?.mp3"),
            "hello-world--mp3".to_string()
        );
        assert_eq!(sanitize_file_stem(""), "audio".to_string());
    }
}
