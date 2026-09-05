//! A spectrum analyser that listens to the room.
//!
//! Three things separate this from a row of jumping bars:
//!
//! * **Logarithmic frequency bands.** Linear FFT bins put nearly everything
//!   musical in the leftmost tenth of the screen. Music is logarithmic — an
//!   octave is a doubling — so the bands are too, and the display finally
//!   corresponds to what you hear.
//! * **Fast attack, slow decay.** A transient has to arrive instantly or the
//!   display feels laggy; it has to fall slowly or it flickers. The two are
//!   different constants, which is the whole trick.
//! * **Sub-cell resolution.** Eight block glyphs per row give eight times the
//!   vertical detail a character grid otherwise allows, so quiet passages still
//!   move instead of sitting flat on zero.
//!
//! The microphone is opened when this starts and released when it stops.

use crate::audio::{AudioError, Capture, WINDOW};
use crate::{Canvas, Cell, Effect, Rgb};
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::sync::Arc;
use std::time::Duration;

/// Eighths of a cell, for sub-character resolution.
const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The band edges, in Hz. Below 30 is inaudible rumble; above 16k is mostly
/// nothing on a laptop microphone.
const F_MIN: f32 = 30.0;
const F_MAX: f32 = 16_000.0;

/// Rise almost immediately, fall over about a third of a second.
const ATTACK: f32 = 0.55;
const DECAY: f32 = 3.2;
/// Peak markers hang, then fall away faster and faster.
const PEAK_HANG: f32 = 0.55;
const PEAK_FALL: f32 = 0.9;

const TEXT: Rgb = Rgb(190, 200, 220);
const DIM: Rgb = Rgb(90, 100, 125);

pub struct Spectrum {
    capture: Option<Capture>,
    error: Option<AudioError>,
    fft: Arc<dyn Fft<f32>>,
    /// Hann window, precomputed: it never changes and it is 2048 cosines.
    window: Vec<f32>,
    samples: [f32; WINDOW],
    scratch: Vec<Complex<f32>>,
    /// Per-column magnitude, 0..1, after smoothing.
    bands: Vec<f32>,
    peaks: Vec<f32>,
    peak_age: Vec<f32>,
    /// Colour phase, so the palette drifts slowly rather than being static.
    hue: f32,
}

impl Spectrum {
    pub fn new() -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(WINDOW);
        let window = (0..WINDOW)
            .map(|i| {
                // Hann: without a window, every frame's discontinuity smears
                // energy across the whole spectrum and the display turns to mush.
                let t = i as f32 / (WINDOW - 1) as f32;
                0.5 - 0.5 * (std::f32::consts::TAU * t).cos()
            })
            .collect();

        Self {
            capture: None,
            error: None,
            fft,
            window,
            samples: [0.0; WINDOW],
            scratch: vec![Complex { re: 0.0, im: 0.0 }; WINDOW],
            bands: Vec::new(),
            peaks: Vec::new(),
            peak_age: Vec::new(),
            hue: 0.0,
        }
    }

    fn ensure_listening(&mut self) {
        if self.capture.is_some() || self.error.is_some() {
            return;
        }
        match Capture::start() {
            Ok(c) => self.capture = Some(c),
            Err(e) => self.error = Some(e),
        }
    }

    /// Analyse the current window into `self.bands`.
    fn analyse(&mut self, dt: f32) {
        let Some(capture) = &self.capture else {
            return;
        };
        let n = self.bands.len();
        if n == 0 {
            return;
        }

        capture.read(&mut self.samples);
        for (i, c) in self.scratch.iter_mut().enumerate() {
            *c = Complex {
                re: self.samples[i] * self.window[i],
                im: 0.0,
            };
        }
        self.fft.process(&mut self.scratch);

        let rate = capture.sample_rate().max(1.0);
        let bin_hz = rate / WINDOW as f32;
        let nyquist_bin = WINDOW / 2;

        // One band per column, spaced logarithmically.
        let ratio = (F_MAX / F_MIN).powf(1.0 / n as f32);
        let mut lo_hz = F_MIN;

        for i in 0..n {
            let hi_hz = lo_hz * ratio;
            let lo = ((lo_hz / bin_hz) as usize).max(1);
            let hi = ((hi_hz / bin_hz) as usize).clamp(lo + 1, nyquist_bin);

            // Peak within the band, not the mean: a mean over a wide high band
            // buries a single strong tone in the noise around it.
            let mut peak = 0.0f32;
            for c in &self.scratch[lo..hi] {
                peak = peak.max(c.norm());
            }

            // dB, because hearing is logarithmic in amplitude too. The floor is
            // chosen so a quiet room sits just above zero rather than jittering.
            let db = 20.0 * (peak / WINDOW as f32).max(1e-9).log10();
            let mut target = ((db + 78.0) / 68.0).clamp(0.0, 1.0);

            // Gentle high-frequency lift: real music has far less energy up
            // there, and without this the right half of the display never moves.
            let tilt = 1.0 + 0.85 * (i as f32 / n as f32);
            target = (target * tilt).clamp(0.0, 1.0);

            self.bands[i] = approach(self.bands[i], target, dt);

            if self.bands[i] >= self.peaks[i] {
                self.peaks[i] = self.bands[i];
                self.peak_age[i] = 0.0;
            } else {
                self.peak_age[i] += dt;
                if self.peak_age[i] > PEAK_HANG {
                    // Accelerating fall, so a peak does not linger once the
                    // sound that made it is clearly gone.
                    let fallen = self.peak_age[i] - PEAK_HANG;
                    self.peaks[i] = (self.peaks[i] - PEAK_FALL * dt * (1.0 + fallen)).max(0.0);
                }
            }

            lo_hz = hi_hz;
        }
    }

    /// Colour for a band: hue sweeps across the spectrum, brightness follows
    /// level, so a loud band is vivid and a quiet one recedes.
    fn colour(&self, i: usize, n: usize, level: f32) -> Rgb {
        let t = i as f32 / n.max(1) as f32;
        let h = (t * 0.75 + self.hue).fract();
        let (r, g, b) = hsv(h, 0.85, 0.35 + 0.65 * level.clamp(0.0, 1.0));
        Rgb(r, g, b)
    }

    fn write(canvas: &mut Canvas, x: i32, y: i32, text: &str, fg: Rgb) {
        for (i, ch) in text.chars().enumerate() {
            canvas.set(
                x + i as i32,
                y,
                Cell {
                    ch,
                    fg,
                    bg: Rgb::BLACK,
                },
            );
        }
    }

    fn centre(canvas: &mut Canvas, y: i32, text: &str, fg: Rgb) {
        let x = ((canvas.width() as i32 - text.chars().count() as i32) / 2).max(0);
        Self::write(canvas, x, y, text, fg);
    }
}

impl Default for Spectrum {
    fn default() -> Self {
        Self::new()
    }
}

impl Effect for Spectrum {
    fn name(&self) -> &'static str {
        "spectrum"
    }

    fn resize(&mut self, width: u16, height: u16) {
        let n = width as usize;
        self.bands = vec![0.0; n];
        self.peaks = vec![0.0; n];
        self.peak_age = vec![0.0; n];
        let _ = height;
    }

    fn tick(&mut self, dt: Duration, canvas: &mut Canvas) {
        let dt = dt.as_secs_f32().min(0.1);
        self.ensure_listening();
        canvas.clear();

        let (w, h) = (canvas.width(), canvas.height());
        if w < 8 || h < 4 {
            Self::write(canvas, 0, 0, "too small", TEXT);
            return;
        }

        if let Some(err) = &self.error {
            // Say what went wrong and what to do about it. A blank screen would
            // be indistinguishable from a broken screensaver.
            Self::centre(canvas, h as i32 / 2 - 1, &err.to_string(), TEXT);
            let hint = match err {
                AudioError::Denied => "grant microphone access in system settings",
                AudioError::NoDevice => "connect a microphone and restart",
                _ => "any key to leave",
            };
            Self::centre(canvas, h as i32 / 2 + 1, hint, DIM);
            return;
        }

        self.hue = (self.hue + dt * 0.03).fract();
        self.analyse(dt);

        // The bottom row is the readout, so the bars get everything above it.
        let bar_rows = (h as i32 - 1).max(1);
        let n = self.bands.len().min(w as usize);

        for i in 0..n {
            let level = self.bands[i].clamp(0.0, 1.0);
            let eighths = (level * (bar_rows * 8) as f32).round() as i32;
            let colour = self.colour(i, n, level);

            for row in 0..bar_rows {
                // Row 0 is the bottom of the bar.
                let filled = (eighths - row * 8).clamp(0, 8);
                if filled == 0 {
                    continue;
                }
                let y = bar_rows - 1 - row;
                // Dim towards the top of each bar, which reads as a glow.
                let shade = colour.dim(0.55 + 0.45 * (1.0 - row as f32 / bar_rows as f32));
                canvas.set(
                    i as i32,
                    y,
                    Cell {
                        ch: BLOCKS[filled as usize],
                        fg: shade,
                        bg: Rgb::BLACK,
                    },
                );
            }

            // Peak marker, floating above the bar.
            let peak_eighths = (self.peaks[i].clamp(0.0, 1.0) * (bar_rows * 8) as f32) as i32;
            let peak_row = (peak_eighths / 8).min(bar_rows - 1);
            if peak_eighths > eighths + 2 {
                canvas.set(
                    i as i32,
                    bar_rows - 1 - peak_row,
                    Cell {
                        ch: '▔',
                        fg: Rgb(255, 255, 255),
                        bg: Rgb::BLACK,
                    },
                );
            }
        }

        let status = match &self.capture {
            Some(c) if c.heard_anything() => format!(
                " {} · {:.0} kHz · {} bands ",
                c.device(),
                c.sample_rate() / 1000.0,
                n
            ),
            // Silence and "not listening" look identical on a spectrum; say which.
            Some(c) => format!(" {} · listening… (silence) ", c.device()),
            None => " starting… ".to_string(),
        };
        Self::write(canvas, 0, h as i32 - 1, &status, DIM);
    }
}

/// Move `current` towards `target` by one frame's worth.
///
/// Two different constants, and that asymmetry is the entire feel of the
/// display: a transient has to arrive instantly or it looks laggy, and it has
/// to leave slowly or it flickers. Exponential rather than linear, so the step
/// is frame-rate independent.
fn approach(current: f32, target: f32, dt: f32) -> f32 {
    let rate = if target > current { ATTACK } else { DECAY };
    let k = (1.0 - (-dt / rate.max(0.001)).exp()).clamp(0.0, 1.0);
    current + (target - current) * k
}

/// HSV to RGB. Cheap, and good enough for a spectrum where the point is that
/// adjacent bands differ rather than that any one is a specific colour.
fn hsv(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let h = h.rem_euclid(1.0) * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    ((r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything except opening the microphone, which CI has none of.
    fn offline() -> Spectrum {
        let mut s = Spectrum::new();
        s.error = Some(AudioError::NoDevice);
        s.resize(80, 20);
        s
    }

    #[test]
    fn a_missing_microphone_is_explained_rather_than_left_blank() {
        let mut s = offline();
        let mut c = Canvas::new(80, 20);
        s.tick(Duration::from_millis(33), &mut c);
        let text: String = c.rows().flatten().map(|cell| cell.ch).collect();
        assert!(text.contains("no microphone"), "got: {text:?}");
        assert!(text.contains("connect"), "and what to do about it");
    }

    /// Denied access is a different problem from a missing device, and the
    /// advice differs too.
    #[test]
    fn a_denied_microphone_gives_different_advice() {
        let mut s = Spectrum::new();
        s.error = Some(AudioError::Denied);
        s.resize(80, 20);
        let mut c = Canvas::new(80, 20);
        s.tick(Duration::from_millis(33), &mut c);
        let text: String = c.rows().flatten().map(|cell| cell.ch).collect();
        assert!(text.contains("denied"));
        assert!(text.contains("system settings"));
    }

    #[test]
    fn it_survives_every_size_including_none() {
        let mut s = offline();
        for (w, h) in [(0u16, 0u16), (1, 1), (8, 4), (200, 60)] {
            s.resize(w, h);
            let mut c = Canvas::new(w, h);
            s.tick(Duration::from_millis(33), &mut c);
        }
    }

    /// One band per column, or the display either clips or leaves a gap.
    #[test]
    fn there_is_exactly_one_band_per_column() {
        let mut s = offline();
        for w in [10u16, 80, 240] {
            s.resize(w, 20);
            assert_eq!(s.bands.len(), w as usize);
            assert_eq!(s.peaks.len(), w as usize);
        }
    }

    /// Fast attack and slow decay is the whole feel of the thing. Measured in
    /// frames, not asserted about the constants — the constants are an
    /// implementation detail, the behaviour is not.
    #[test]
    fn levels_rise_faster_than_they_fall() {
        let dt = 1.0 / 60.0;
        let frames_to = |from: f32, to: f32| {
            let mut v = from;
            let mut n = 0;
            while (v - to).abs() > 0.05 && n < 10_000 {
                v = approach(v, to, dt);
                n += 1;
            }
            n
        };
        let rising = frames_to(0.0, 1.0);
        let falling = frames_to(1.0, 0.0);
        assert!(
            rising * 3 < falling,
            "rise took {rising} frames, fall took {falling}: the asymmetry is the point"
        );
    }

    /// And the step must not depend on how often we happen to be drawing.
    #[test]
    fn smoothing_is_frame_rate_independent() {
        let one_big = approach(0.0, 1.0, 0.1);
        let mut many_small = 0.0;
        for _ in 0..10 {
            many_small = approach(many_small, 1.0, 0.01);
        }
        assert!(
            (one_big - many_small).abs() < 0.01,
            "one 100ms step gave {one_big}, ten 10ms steps gave {many_small}"
        );
    }

    #[test]
    fn a_level_that_is_already_there_does_not_drift() {
        assert!((approach(0.5, 0.5, 1.0 / 60.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn the_block_ramp_covers_every_eighth() {
        assert_eq!(BLOCKS.len(), 9, "empty through full");
        assert_eq!(BLOCKS[0], ' ');
        assert_eq!(BLOCKS[8], '█');
    }

    #[test]
    fn the_hue_sweep_stays_in_range() {
        let s = offline();
        for i in 0..64 {
            let Rgb(r, g, b) = s.colour(i, 64, 1.0);
            assert!(r > 0 || g > 0 || b > 0, "band {i} is invisible");
        }
    }

    #[test]
    fn hsv_endpoints_are_sane() {
        assert_eq!(hsv(0.0, 0.0, 0.0), (0, 0, 0));
        let (r, g, b) = hsv(0.0, 0.0, 1.0);
        assert!(
            r > 250 && g > 250 && b > 250,
            "white expected, got {r},{g},{b}"
        );
    }
}
