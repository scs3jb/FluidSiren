//! Microphone capture via cpal (PipeWire/ALSA) with start/stop control.
//!
//! Produces mono f32 samples resampled to 16 kHz, which is what the ASR models
//! expect. Mics usually run at 44.1/48 kHz, so the resampler low-passes before
//! decimating: without that, everything between 8 kHz and the mic's Nyquist
//! (hiss, fan noise, sibilance) folds back into the speech band.

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

        // Room for a minute of audio up front so the callback rarely reallocates.
        let buffer: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::with_capacity(src_rate as usize * 60)));
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

    /// Native capture rate of the samples returned by [`Self::snapshot`].
    pub fn src_rate(&self) -> u32 {
        self.src_rate
    }

    /// Copy of the raw (native-rate, mono) samples captured so far, starting at
    /// `from`. Lets the engine transcribe finished phrases while still recording.
    pub fn snapshot(&self, from: usize) -> Vec<f32> {
        let buf = self.buffer.lock().unwrap();
        buf.get(from..).map(<[f32]>::to_vec).unwrap_or_default()
    }

    /// Stop capturing and return the raw (native-rate, mono) samples.
    pub fn stop_raw(self) -> Vec<f32> {
        drop(self.stream); // no more writers, so the buffer can be moved out
        std::mem::take(&mut *self.buffer.lock().unwrap())
    }

    /// Stop capturing and return mono f32 samples resampled to 16 kHz.
    pub fn stop(self) -> Vec<f32> {
        let rate = self.src_rate;
        resample(&self.stop_raw(), rate, WHISPER_SAMPLE_RATE)
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
    Ok(resample(&mono, spec.sample_rate, WHISPER_SAMPLE_RATE))
}

/// Zero crossings of the windowed-sinc kernel on each side. 16 gives a sharp
/// transition band with negligible ripple; cost is still trivial for speech.
const SINC_ZERO_CROSSINGS: f64 = 16.0;
/// Passband edge as a fraction of the output Nyquist (leaves room for the
/// transition band so nothing above Nyquist leaks through).
const SINC_CUTOFF: f64 = 0.92;
/// Polyphase tables are precomputed per phase; rate pairs needing more phases
/// than this (exotic rates) fall back to linear interpolation.
const MAX_PHASES: u64 = 4096;

/// Band-limited resampling (polyphase windowed sinc, exact rational ratio).
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == to {
        return input.to_vec();
    }
    let g = gcd(from as u64, to as u64);
    let (up, down) = (to as u64 / g, from as u64 / g);
    if up > MAX_PHASES {
        return resample_linear(input, from, to);
    }

    // Kernel in units of input samples: cutoff relative to the input Nyquist.
    let cutoff = SINC_CUTOFF * (to as f64 / from as f64).min(1.0);
    let half = (SINC_ZERO_CROSSINGS / cutoff).ceil() as i64;
    let taps = (2 * half) as usize;
    // table[phase][j] weights input sample `base - half + 1 + j` for an output
    // that sits `phase / up` input samples after `base`.
    let table: Vec<Vec<f32>> = (0..up)
        .map(|phase| {
            let frac = phase as f64 / up as f64;
            let mut row: Vec<f64> = (0..taps)
                .map(|j| {
                    let x = (j as i64 - half + 1) as f64 - frac;
                    sinc(cutoff * x) * kaiser(x / (half as f64), 8.0)
                })
                .collect();
            let sum: f64 = row.iter().sum();
            row.iter_mut().for_each(|w| *w /= sum); // unity DC gain per phase
            row.into_iter().map(|w| w as f32).collect()
        })
        .collect();

    let out_len = (input.len() as u64 * up / down) as usize;
    let last = input.len() as i64 - 1;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len as u64 {
        let pos = i * down;
        let (base, phase) = ((pos / up) as i64, (pos % up) as usize);
        let first = base - half + 1;
        let row = &table[phase];
        let acc: f32 = if first >= 0 && first + taps as i64 - 1 <= last {
            let window = &input[first as usize..first as usize + taps];
            window.iter().zip(row).map(|(s, w)| s * w).sum()
        } else {
            // Edges: clamp (repeat the boundary sample) instead of reading past it.
            row.iter()
                .enumerate()
                .map(|(j, w)| input[(first + j as i64).clamp(0, last) as usize] * w)
                .sum()
        };
        out.push(acc);
    }
    out
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Kaiser window at normalised position `x` in [-1, 1].
fn kaiser(x: f64, beta: f64) -> f64 {
    if x.abs() > 1.0 {
        return 0.0;
    }
    bessel_i0(beta * (1.0 - x * x).sqrt()) / bessel_i0(beta)
}

/// Zeroth-order modified Bessel function of the first kind (series expansion).
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > 1e-12 * sum {
        term *= (x / (2.0 * k)).powi(2);
        sum += term;
        k += 1.0;
    }
    sum
}

/// Linear interpolation fallback for rate pairs with an impractical number of
/// polyphase phases.
fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, rate: u32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        // Skip the edges, where the kernel sees clamped samples.
        let x = &x[x.len() / 10..x.len() * 9 / 10];
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn passes_speech_band() {
        for from in [48_000, 44_100, 32_000, 8_000] {
            let out = resample(&sine(1000.0, from, 1.0), from, WHISPER_SAMPLE_RATE);
            assert_eq!(out.len(), WHISPER_SAMPLE_RATE as usize, "{from}");
            assert!((rms(&out) - 0.7071).abs() < 0.01, "{from}: {}", rms(&out));
        }
    }

    #[test]
    fn rejects_content_above_nyquist() {
        // A 12 kHz tone would alias to 4 kHz with naive interpolation.
        let out = resample(&sine(12_000.0, 48_000, 1.0), 48_000, WHISPER_SAMPLE_RATE);
        assert!(rms(&out) < 0.001, "leaked {}", rms(&out));
        let naive = resample_linear(&sine(12_000.0, 48_000, 1.0), 48_000, WHISPER_SAMPLE_RATE);
        assert!(rms(&naive) > 0.1);
    }

    #[test]
    fn identity_and_empty() {
        assert!(resample(&[], 48_000, 16_000).is_empty());
        assert_eq!(resample(&[0.5, -0.5], 16_000, 16_000), vec![0.5, -0.5]);
    }
}
