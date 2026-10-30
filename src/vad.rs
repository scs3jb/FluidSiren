//! Lightweight energy-based voice activity detection.
//!
//! Not a neural VAD — it only has to answer three cheap questions about a
//! dictation clip: is there any speech at all, where does it start/end (so the
//! ASR never sees seconds of hotkey-press silence, which costs time and invites
//! Whisper hallucinations), and where is a pause long enough to cut the clip
//! for incremental transcription. Thresholds adapt to each clip's own noise
//! floor, and when the clip has too little dynamic range to tell speech from
//! noise it errs on the side of keeping everything.

/// Analysis frame length.
const FRAME_SECS: f32 = 0.02;
/// Audio kept before the first / after the last speech frame, so soft onsets and
/// trailing consonants are never clipped.
const PAD_BEFORE_SECS: f32 = 0.3;
const PAD_AFTER_SECS: f32 = 0.45;
/// Frames quieter than this peak are treated as a silent clip.
const SILENCE_PEAK_DB: f32 = -60.0;
/// Below this loud-vs-quiet spread we can't separate speech from noise, so we
/// assume everything might be speech.
const MIN_DYNAMIC_RANGE_DB: f32 = 10.0;
/// Minimum total speech for a clip to count as containing speech.
const MIN_SPEECH_SECS: f32 = 0.16;

/// Per-frame speech/non-speech decisions for a clip.
pub struct Activity {
    frame_len: usize,
    speech: Vec<bool>,
    len: usize,
}

impl Activity {
    pub fn analyze(samples: &[f32], rate: u32) -> Self {
        let frame_len = ((rate as f32 * FRAME_SECS) as usize).max(1);
        let db: Vec<f32> = samples
            .chunks(frame_len)
            .map(|f| {
                let mean_sq = f.iter().map(|s| s * s).sum::<f32>() / f.len() as f32;
                10.0 * (mean_sq + 1e-12).log10()
            })
            .collect();

        let speech = if db.is_empty() {
            Vec::new()
        } else {
            let mut sorted = db.clone();
            sorted.sort_by(|a, b| a.total_cmp(b));
            let pct = |p: f32| sorted[((sorted.len() - 1) as f32 * p) as usize];
            let (floor, peak) = (pct(0.15), pct(0.97));
            if peak < SILENCE_PEAK_DB {
                vec![false; db.len()]
            } else if peak - floor < MIN_DYNAMIC_RANGE_DB {
                vec![true; db.len()]
            } else {
                let threshold = floor + ((peak - floor) * 0.3).max(6.0);
                db.iter().map(|&d| d > threshold).collect()
            }
        };
        Self { frame_len, speech, len: samples.len() }
    }

    fn secs_to_frames(&self, secs: f32, rate: u32) -> usize {
        (secs * rate as f32 / self.frame_len as f32).ceil() as usize
    }

    /// Whether the clip contains enough speech to be worth transcribing.
    pub fn has_speech(&self, rate: u32) -> bool {
        self.speech.iter().filter(|&&s| s).count() >= self.secs_to_frames(MIN_SPEECH_SECS, rate)
    }

    /// Sample range covering the speech plus padding, or `None` if there is none.
    pub fn speech_bounds(&self, rate: u32) -> Option<std::ops::Range<usize>> {
        if !self.has_speech(rate) {
            return None;
        }
        let first = self.speech.iter().position(|&s| s)?;
        let last = self.speech.iter().rposition(|&s| s)?;
        let start = (first * self.frame_len).saturating_sub((PAD_BEFORE_SECS * rate as f32) as usize);
        let end = ((last + 1) * self.frame_len + (PAD_AFTER_SECS * rate as f32) as usize).min(self.len);
        Some(start..end)
    }

    /// A sample index in the middle of the last pause of at least `min_pause_secs`
    /// that follows some speech — a safe place to cut the clip without splitting a
    /// word. `None` if there is no such pause.
    pub fn last_pause_cut(&self, rate: u32, min_pause_secs: f32) -> Option<usize> {
        let min_run = self.secs_to_frames(min_pause_secs, rate);
        let (n, mut i) = (self.speech.len(), 0);
        let (mut seen_speech, mut cut) = (false, None);
        while i < n {
            if self.speech[i] {
                seen_speech = true;
                i += 1;
                continue;
            }
            let run_start = i;
            while i < n && !self.speech[i] {
                i += 1;
            }
            // A trailing run counts too: the speaker has clearly paused there.
            if seen_speech && i - run_start >= min_run {
                cut = Some(((run_start + i) / 2 * self.frame_len).min(self.len));
            }
        }
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 16_000;

    fn tone(secs: f32, amp: f32) -> Vec<f32> {
        (0..(secs * RATE as f32) as usize)
            .map(|i| amp * (i as f32 * 2.0 * std::f32::consts::PI * 220.0 / RATE as f32).sin())
            .collect()
    }

    fn noise(secs: f32, amp: f32) -> Vec<f32> {
        // Deterministic pseudo-noise.
        let mut x: u32 = 12345;
        (0..(secs * RATE as f32) as usize)
            .map(|_| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                amp * ((x >> 16) as f32 / 32768.0 - 1.0)
            })
            .collect()
    }

    #[test]
    fn digital_silence_has_no_speech() {
        let a = Activity::analyze(&vec![0.0; RATE as usize * 2], RATE);
        assert!(!a.has_speech(RATE));
        assert!(a.speech_bounds(RATE).is_none());
    }

    #[test]
    fn trims_leading_and_trailing_silence() {
        let mut clip = noise(1.0, 0.001);
        clip.extend(tone(1.0, 0.3));
        clip.extend(noise(2.0, 0.001));
        let a = Activity::analyze(&clip, RATE);
        let b = a.speech_bounds(RATE).unwrap();
        let (s, e) = (b.start as f32 / RATE as f32, b.end as f32 / RATE as f32);
        assert!((0.6..=1.0).contains(&s), "start {s}");
        assert!((2.0..=2.6).contains(&e), "end {e}");
    }

    #[test]
    fn flat_clip_is_kept_whole() {
        // No quiet part to calibrate against: keep everything.
        let clip = tone(2.0, 0.3);
        let b = Activity::analyze(&clip, RATE).speech_bounds(RATE).unwrap();
        assert_eq!(b, 0..clip.len());
    }

    #[test]
    fn finds_pause_between_phrases() {
        let mut clip = tone(2.0, 0.3);
        clip.extend(noise(1.0, 0.001));
        clip.extend(tone(1.0, 0.3));
        let a = Activity::analyze(&clip, RATE);
        let cut = a.last_pause_cut(RATE, 0.6).unwrap() as f32 / RATE as f32;
        assert!((2.3..=2.7).contains(&cut), "cut {cut}");
        assert!(a.last_pause_cut(RATE, 1.5).is_none());
    }
}
