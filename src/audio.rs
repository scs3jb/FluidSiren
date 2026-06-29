//! Microphone capture via cpal (PipeWire/ALSA) with start/stop control.
//!
//! Produces mono f32 samples resampled to 16 kHz, which is what whisper expects.

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat};
use std::sync::{Arc, Mutex};

pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

/// A live recording session. Drop or call `stop()` to finish capturing.
pub struct Recorder {
    stream: cpal::Stream,
    buffer: Arc<Mutex<Vec<f32>>>,
    src_rate: u32,
}

impl Recorder {
    /// Start capturing from the default input device immediately.
    pub fn start() -> Result<Self> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| anyhow!("no default input device (is PipeWire running?)"))?;
        let name = device.name().unwrap_or_else(|_| "<unknown>".into());
        let supported = device
            .default_input_config()
            .context("querying default input config")?;
        let src_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        tracing::info!(device = %name, rate = src_rate, channels, "capturing");

        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
        let config: cpal::StreamConfig = supported.config();
        let err_fn = |e| tracing::error!("audio stream error: {e}");

        // Downmix interleaved frames to mono and accumulate.
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build_stream::<f32>(&device, &config, channels, buffer.clone(), err_fn)?,
            SampleFormat::I16 => build_stream::<i16>(&device, &config, channels, buffer.clone(), err_fn)?,
            SampleFormat::U16 => build_stream::<u16>(&device, &config, channels, buffer.clone(), err_fn)?,
            other => return Err(anyhow!("unsupported sample format: {other:?}")),
        };
        stream.play().context("starting audio stream")?;
        Ok(Self { stream, buffer, src_rate })
    }

    /// Stop capturing and return mono f32 samples resampled to 16 kHz.
    pub fn stop(self) -> Vec<f32> {
        drop(self.stream);
        let mono = self.buffer.lock().unwrap().clone();
        resample_linear(&mono, self.src_rate, WHISPER_SAMPLE_RATE)
    }
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    buffer: Arc<Mutex<Vec<f32>>>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream>
where
    T: Sample + cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            let mut buf = buffer.lock().unwrap();
            for frame in data.chunks(channels) {
                let sum: f32 = frame.iter().map(|s| f32::from_sample(*s)).sum();
                buf.push(sum / channels as f32);
            }
        },
        err_fn,
        None,
    )?;
    Ok(stream)
}

/// Record from the default device for a fixed duration (non-interactive test mode).
pub fn record_for(seconds: f32) -> Result<Vec<f32>> {
    let rec = Recorder::start()?;
    std::thread::sleep(std::time::Duration::from_secs_f32(seconds));
    Ok(rec.stop())
}

/// Load a WAV file as mono f32 resampled to 16 kHz (non-interactive test mode).
pub fn load_wav(path: &std::path::Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).context("opening wav")?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(|s| s.ok()).collect(),
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .filter_map(|s| s.ok())
                .map(|s| s as f32 / max)
                .collect()
        }
    };
    let mono: Vec<f32> = if channels <= 1 {
        samples
    } else {
        samples
            .chunks(channels)
            .map(|f| f.iter().sum::<f32>() / channels as f32)
            .collect()
    };
    Ok(resample_linear(&mono, spec.sample_rate, WHISPER_SAMPLE_RATE))
}

/// Simple linear resampler. Whisper is robust to this; good enough for dictation.
fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == to {
        return input.to_vec();
    }
    let ratio = to as f64 / from as f64;
    let out_len = (input.len() as f64 * ratio).round() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 / ratio;
        let idx = src.floor() as usize;
        let frac = (src - idx as f64) as f32;
        let a = input[idx.min(input.len() - 1)];
        let b = input[(idx + 1).min(input.len() - 1)];
        out.push(a + (b - a) * frac);
    }
    out
}
