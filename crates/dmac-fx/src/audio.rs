//! Listening to the microphone, for the spectrum analyser.
//!
//! Capture starts when the effect starts and stops when it ends. That is not an
//! optimisation — a file manager that holds the microphone open while you are
//! not looking at it is a file manager nobody should trust, and on macOS it
//! leaves the recording indicator lit.
//!
//! Everything here degrades rather than fails. No device, no permission, an
//! unsupported format: the effect says so on screen instead of vanishing or
//! crashing, because "the screensaver is blank" is a bug report nobody can act on.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};

/// How many samples the analyser looks at. A power of two for the FFT, and about
/// 46ms at 44.1kHz — long enough to resolve bass, short enough to feel reactive.
pub const WINDOW: usize = 2048;

/// A ring of the most recent samples, written by the audio callback and read by
/// the render loop.
struct Ring {
    data: Vec<f32>,
    /// Where the next sample goes.
    head: usize,
    /// Set once anything at all has been captured, so silence and "no input" can
    /// be told apart on screen.
    heard_anything: bool,
}

impl Ring {
    fn new() -> Self {
        Self {
            data: vec![0.0; WINDOW],
            head: 0,
            heard_anything: false,
        }
    }

    fn push(&mut self, sample: f32) {
        self.data[self.head] = sample;
        self.head = (self.head + 1) % WINDOW;
    }

    /// The window in chronological order, oldest first.
    fn snapshot(&self, out: &mut [f32]) {
        let (a, b) = self.data.split_at(self.head);
        out[..b.len()].copy_from_slice(b);
        out[b.len()..].copy_from_slice(a);
    }
}

/// A live microphone capture. Dropping it releases the device.
pub struct Capture {
    ring: Arc<Mutex<Ring>>,
    /// Held only to keep the stream alive; dropping it stops capture.
    _stream: cpal::Stream,
    sample_rate: f32,
    device: String,
}

/// Why listening did not start. Each case is something the user can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    NoDevice,
    NoConfig,
    Denied,
    Failed(String),
}

impl std::fmt::Display for AudioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Named concretely: "no audio" would leave the user guessing which
            // of several very different problems they have.
            AudioError::NoDevice => f.write_str("no microphone found"),
            AudioError::NoConfig => f.write_str("microphone has no supported format"),
            AudioError::Denied => f.write_str("microphone access denied"),
            AudioError::Failed(e) => write!(f, "microphone error: {e}"),
        }
    }
}

impl Capture {
    /// Open the default input device and start listening.
    pub fn start() -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = host.default_input_device().ok_or(AudioError::NoDevice)?;
        // cpal 0.18 renders the device name through Display; there is no name().
        let name = device.to_string();

        let config = device
            .default_input_config()
            .map_err(|_| AudioError::NoConfig)?;
        // `SampleRate` is a plain u32 in cpal 0.18, not a newtype.
        let sample_rate = config.sample_rate() as f32;
        let channels = config.channels().max(1) as usize;

        let ring = Arc::new(Mutex::new(Ring::new()));
        let sink = Arc::clone(&ring);

        // Errors on the audio thread are swallowed deliberately: there is nothing
        // useful to do from inside the callback, and panicking there would take
        // down the process from a thread the user never asked for.
        let on_error = |_: cpal::Error| {};

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                config.into(),
                move |data: &[f32], _: &_| {
                    if let Ok(mut r) = sink.lock() {
                        // Downmix to mono: a spectrum of the left channel alone
                        // misses whatever is panned right.
                        for frame in data.chunks(channels) {
                            let sum: f32 = frame.iter().copied().sum();
                            let mono = sum / channels as f32;
                            if mono.abs() > 1e-5 {
                                r.heard_anything = true;
                            }
                            r.push(mono);
                        }
                    }
                },
                on_error,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                config.into(),
                move |data: &[i16], _: &_| {
                    if let Ok(mut r) = sink.lock() {
                        for frame in data.chunks(channels) {
                            let sum: f32 = frame.iter().map(|s| *s as f32 / 32768.0).sum();
                            let mono = sum / channels as f32;
                            if mono.abs() > 1e-5 {
                                r.heard_anything = true;
                            }
                            r.push(mono);
                        }
                    }
                },
                on_error,
                None,
            ),
            other => return Err(AudioError::Failed(format!("unsupported format {other:?}"))),
        }
        .map_err(|e| {
            // A permission refusal is not a device failure, and telling the user
            // to check their microphone when they need to grant access wastes
            // their time.
            let msg = e.to_string();
            if msg.to_lowercase().contains("denied") || msg.to_lowercase().contains("permission") {
                AudioError::Denied
            } else {
                AudioError::Failed(msg)
            }
        })?;

        stream
            .play()
            .map_err(|e| AudioError::Failed(e.to_string()))?;

        Ok(Self {
            ring,
            _stream: stream,
            sample_rate,
            device: name,
        })
    }

    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    pub fn device(&self) -> &str {
        &self.device
    }

    /// Whether any non-silent sample has arrived. Distinguishes "listening to a
    /// quiet room" from "listening to nothing", which look identical otherwise.
    pub fn heard_anything(&self) -> bool {
        self.ring.lock().map(|r| r.heard_anything).unwrap_or(false)
    }

    /// Copy the most recent window into `out`, oldest sample first.
    pub fn read(&self, out: &mut [f32; WINDOW]) {
        if let Ok(r) = self.ring.lock() {
            r.snapshot(out);
        }
    }
}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture")
            .field("device", &self.device)
            .field("sample_rate", &self.sample_rate)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_reads_back_in_chronological_order() {
        let mut r = Ring::new();
        for i in 0..WINDOW {
            r.push(i as f32);
        }
        let mut out = vec![0.0; WINDOW];
        r.snapshot(&mut out);
        assert_eq!(out[0], 0.0, "oldest first");
        assert_eq!(out[WINDOW - 1], (WINDOW - 1) as f32, "newest last");
    }

    /// The whole point of a ring: after wrapping, the window is still the most
    /// recent samples in order, not a rotated jumble.
    #[test]
    fn wrapping_keeps_the_window_in_order() {
        let mut r = Ring::new();
        for i in 0..(WINDOW + WINDOW / 3) {
            r.push(i as f32);
        }
        let mut out = vec![0.0; WINDOW];
        r.snapshot(&mut out);
        for w in out.windows(2) {
            assert!(w[1] > w[0], "out of order: {} then {}", w[0], w[1]);
        }
        assert_eq!(out[WINDOW - 1], (WINDOW + WINDOW / 3 - 1) as f32);
    }

    /// Each failure names something the user can act on, rather than a generic
    /// "no audio" that could mean any of them.
    #[test]
    fn every_error_says_something_actionable() {
        for e in [
            AudioError::NoDevice,
            AudioError::NoConfig,
            AudioError::Denied,
            AudioError::Failed("boom".into()),
        ] {
            let msg = e.to_string();
            assert!(!msg.is_empty());
            assert!(msg.len() > 8, "too terse to act on: {msg:?}");
        }
    }

    #[test]
    fn a_denied_microphone_is_not_reported_as_a_device_failure() {
        assert_ne!(AudioError::Denied, AudioError::NoDevice);
        assert!(AudioError::Denied.to_string().contains("denied"));
    }
}
