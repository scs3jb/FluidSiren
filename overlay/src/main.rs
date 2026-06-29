//! FluidSiren recording overlay.
//!
//! A small, frameless, always-on-top rounded pill anchored bottom-center via the
//! Wayland `wlr-layer-shell` protocol (KWin supports it). It shows a live volume
//! waveform that moves as you talk. Spawned by the main app only while recording,
//! so it "disappears when not recording" by exiting.
//!
//! stdin protocol (one word per line):
//!   * `recording`    → red waveform
//!   * `transcribing` → blue waveform
//!   * `quit` / EOF   → exit

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat};
use gtk4::prelude::*;
use gtk4::{glib, Application, ApplicationWindow, CssProvider, DrawingArea};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use std::cell::{Cell, RefCell};
use std::io::BufRead;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

const APP_ID: &str = "dev.altic.FluidSiren.Overlay";
const WIDTH: i32 = 180;
const HEIGHT: i32 = 46;
const N_BARS: usize = 16;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Recording,
    Transcribing,
}

fn main() {
    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run_with_args::<&str>(&[]);
}

fn build_ui(app: &Application) {
    // Make the GTK window itself transparent so only our Cairo-drawn rounded pill
    // shows (otherwise the theme paints an opaque square behind the corners).
    let provider = CssProvider::new();
    provider.load_from_data("window, window.background { background: transparent; }");
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let window = ApplicationWindow::new(app);
    window.init_layer_shell();
    window.set_layer(Layer::Overlay); // topmost layer-shell layer
    window.set_anchor(Edge::Bottom, true); // bottom edge, horizontally centered
    window.set_margin(Edge::Bottom, 120);
    window.set_keyboard_mode(KeyboardMode::None);

    // Shared state.
    let level = Arc::new(AtomicU32::new(0)); // live mic level (f32 bits)
    start_audio_level(level.clone());
    let bars = Rc::new(RefCell::new(vec![0f32; N_BARS]));
    let mode = Rc::new(Cell::new(Mode::Recording));

    let area = DrawingArea::new();
    area.set_content_width(WIDTH);
    area.set_content_height(HEIGHT);
    {
        let bars = bars.clone();
        let mode = mode.clone();
        area.set_draw_func(move |_, cr, w, h| draw(cr, w, h, &bars.borrow(), mode.get()));
    }
    window.set_child(Some(&area));
    window.present();

    // Initial status from argv.
    if std::env::args().nth(1).as_deref() == Some("transcribing") {
        mode.set(Mode::Transcribing);
    }

    // ~30 fps animation: ease each bar toward a center-weighted target from the level.
    {
        let bars = bars.clone();
        let level = level.clone();
        let area = area.downgrade();
        let frame = Cell::new(0u64);
        glib::timeout_add_local(Duration::from_millis(33), move || {
            let lvl = f32::from_bits(level.load(Ordering::Relaxed));
            let scaled = (lvl * 9.0).min(1.0);
            update_bars(&mut bars.borrow_mut(), scaled, frame.get());
            frame.set(frame.get().wrapping_add(1));
            if let Some(a) = area.upgrade() {
                a.queue_draw();
            }
            glib::ControlFlow::Continue
        });
    }

    // Status updates / quit on stdin.
    let (tx, rx) = async_channel::bounded::<String>(8);
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            match line {
                Ok(l) => {
                    if tx.send_blocking(l).is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx.send_blocking("quit".into());
    });
    let app2 = app.clone();
    glib::spawn_future_local(async move {
        while let Ok(msg) = rx.recv().await {
            match msg.trim() {
                "quit" | "idle" => {
                    app2.quit();
                    break;
                }
                "transcribing" => mode.set(Mode::Transcribing),
                "recording" => mode.set(Mode::Recording),
                _ => {}
            }
        }
    });
}

/// Per-frame bar update: center bars are tallest; a deterministic wobble keeps it
/// lively; each bar eases toward its target so motion is smooth.
fn update_bars(bars: &mut [f32], level: f32, frame: u64) {
    let n = bars.len();
    let center = (n as f32 - 1.0) / 2.0;
    for (i, b) in bars.iter_mut().enumerate() {
        let dist = (i as f32 - center).abs() / center.max(1.0);
        let profile = 1.0 - dist; // tall in the middle
        let wobble = 0.5 + 0.5 * (frame as f32 * 0.30 + i as f32 * 1.7).sin();
        let target = (0.10 + level * profile * (0.55 + 0.45 * wobble)).min(1.0);
        *b += (target - *b) * 0.35;
    }
}

fn draw(cr: &gtk4::cairo::Context, w: i32, h: i32, bars: &[f32], mode: Mode) {
    let (w, h) = (w as f64, h as f64);
    let (r, g, b) = match mode {
        Mode::Recording => (0.953, 0.545, 0.659),    // red
        Mode::Transcribing => (0.537, 0.706, 0.980), // blue
    };

    // Rounded pill background + colored border.
    rounded_rect(cr, 1.0, 1.0, w - 2.0, h - 2.0, 16.0);
    cr.set_source_rgba(0.067, 0.067, 0.106, 0.94);
    let _ = cr.fill_preserve();
    cr.set_source_rgb(r, g, b);
    cr.set_line_width(2.0);
    let _ = cr.stroke();

    // Waveform bars.
    let pad_x = 16.0;
    let pad_y = 9.0;
    let area_w = w - 2.0 * pad_x;
    let area_h = h - 2.0 * pad_y;
    let bw = 4.0;
    let n = bars.len();
    let gap = if n > 1 {
        (area_w - n as f64 * bw) / (n as f64 - 1.0)
    } else {
        0.0
    };
    let cy = h / 2.0;
    cr.set_source_rgb(r, g, b);
    for (i, &bar) in bars.iter().enumerate() {
        let x = pad_x + i as f64 * (bw + gap);
        let bh = (bar as f64 * area_h).max(3.0);
        rounded_rect(cr, x, cy - bh / 2.0, bw, bh, bw / 2.0);
        let _ = cr.fill();
    }
}

fn rounded_rect(cr: &gtk4::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    let r = r.min(w / 2.0).min(h / 2.0);
    let deg = std::f64::consts::PI / 180.0;
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -90.0 * deg, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, 90.0 * deg);
    cr.arc(x + r, y + h - r, r, 90.0 * deg, 180.0 * deg);
    cr.arc(x + r, y + r, r, 180.0 * deg, 270.0 * deg);
    cr.close_path();
}

/// Capture mic audio and publish a smoothed level (mean-abs amplitude) as f32 bits.
fn start_audio_level(level: Arc<AtomicU32>) {
    std::thread::spawn(move || {
        let host = cpal::default_host();
        let Some(device) = host.default_input_device() else {
            return;
        };
        let Ok(supported) = device.default_input_config() else {
            return;
        };
        let config: cpal::StreamConfig = supported.config();
        let err = |e| eprintln!("overlay audio error: {e}");
        let smooth = Arc::new(AtomicU32::new(0));
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build_stream::<f32>(&device, &config, level, smooth, err),
            SampleFormat::I16 => build_stream::<i16>(&device, &config, level, smooth, err),
            SampleFormat::U16 => build_stream::<u16>(&device, &config, level, smooth, err),
            _ => return,
        };
        if let Ok(stream) = stream {
            if stream.play().is_ok() {
                std::thread::park(); // keep the stream alive for the process lifetime
            }
        }
    });
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    level: Arc<AtomicU32>,
    smooth: Arc<AtomicU32>,
    err: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: Sample + cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    device.build_input_stream(
        config,
        move |data: &[T], _: &cpal::InputCallbackInfo| {
            if data.is_empty() {
                return;
            }
            let sum: f32 = data.iter().map(|s| f32::from_sample(*s).abs()).sum();
            let raw = sum / data.len() as f32;
            // Exponential smoothing for a less jittery wave.
            let prev = f32::from_bits(smooth.load(Ordering::Relaxed));
            let next = prev * 0.6 + raw * 0.4;
            smooth.store(next.to_bits(), Ordering::Relaxed);
            level.store(next.to_bits(), Ordering::Relaxed);
        },
        err,
        None,
    )
}
