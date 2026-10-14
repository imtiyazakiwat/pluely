//! Streaming turn pipeline: Silero VAD -> streaming Zipformer -> Smart Turn.
//!
//! Audio goes in at 16 kHz in any chunk size. Silero VAD (32 ms windows) opens
//! a turn and the recognizer decodes it while the person is still talking.
//! Each time the VAD sees a pause, Smart Turn looks at the turn's audio:
//!
//! - "finished": flush the recognizer (a few ms) and release the turn;
//! - "not finished": hold for up to `max_hold_s`. If speech resumes it joins
//!   the same turn, otherwise the turn is released when the hold expires.
//!
//! A turn keeps one recognizer stream from first word to release, pauses
//! included, so a pause never cuts the audio the recognizer hears.

use crate::engine::Engine;
use crate::SAMPLE_RATE;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sherpa_onnx::{OnlineStream, VoiceActivityDetector};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

/// Silero VAD window at 16 kHz (32 ms).
const WINDOW: usize = 512;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineParams {
    /// Silero speech probability threshold.
    pub vad_threshold: f32,
    /// Silence that closes a VAD segment and triggers Smart Turn. The main
    /// latency knob.
    pub min_silence_s: f32,
    /// Speech needed before a turn opens (filters clicks).
    pub min_speech_s: f32,
    /// Silero splits segments longer than this.
    pub max_speech_s: f32,
    /// Smart Turn probability at or above which a turn counts as finished.
    pub turn_threshold: f32,
    /// How long to wait for more speech after Smart Turn says "not finished".
    pub max_hold_s: f32,
    /// Safety cap on a single turn.
    pub max_turn_s: f32,
    pub use_turn_detector: bool,
}

impl Default for PipelineParams {
    fn default() -> Self {
        Self {
            vad_threshold: 0.5,
            min_silence_s: 0.2,
            min_speech_s: 0.25,
            max_speech_s: 20.0,
            turn_threshold: 0.5,
            max_hold_s: 2.0,
            max_turn_s: 30.0,
            use_turn_detector: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnEnd {
    /// Smart Turn judged the turn finished.
    Complete,
    /// No turn model: the first pause ends the turn.
    VadOnly,
    /// Smart Turn said "not finished" but no more speech came within `max_hold_s`.
    HoldTimeout,
    /// Hit `max_turn_s`.
    MaxLength,
    /// Input ended (capture stopped).
    EndOfInput,
}

#[derive(Clone, Debug, Serialize)]
pub struct TurnResult {
    pub text: String,
    pub end: TurnEnd,
    /// Smart Turn probability at the last pause, if it ran.
    pub probability: Option<f32>,
    /// Pauses (VAD segment ends) inside the turn.
    pub segments: u32,
    /// Audio clock (seconds since the pipeline started) where the turn begins.
    pub start_s: f64,
    /// Audio clock of the last pause the VAD reported.
    pub vad_end_s: f64,
    /// Audio clock when the turn was released (after `vad_end_s` if held).
    pub released_s: f64,
    /// Wall-clock time to finalise the recognizer.
    pub asr_flush_ms: f64,
    /// Wall-clock time of the last Smart Turn pass.
    pub turn_ms: Option<f64>,
    /// Wall-clock time from the trigger (pause or hold expiry) to release.
    pub decision_ms: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    SpeechStart { at_s: f64 },
    /// Live hypothesis for the turn so far.
    Partial { text: String },
    /// The VAD reported a pause. Diagnostic only; act on `Turn`.
    SegmentEnd {
        at_s: f64,
        probability: Option<f32>,
        turn_ms: Option<f64>,
        turn_error: Option<String>,
    },
    /// Smart Turn expects more speech; holding for up to `max_hold_s`.
    Held { text: String, probability: f32 },
    Turn(TurnResult),
    /// Speech was detected but nothing was recognised (noise, a cough).
    Discarded { at_s: f64 },
}

struct Turn {
    stream: OnlineStream,
    audio: Vec<f32>,
    start: u64,
    segments: u32,
    hold_until: Option<u64>,
    probability: Option<f32>,
    vad_end: u64,
    turn_ms: Option<f64>,
}

pub struct Pipeline {
    engine: Arc<Engine>,
    params: PipelineParams,
    vad: VoiceActivityDetector,
    pending: Vec<f32>,
    /// Samples processed so far (the audio clock).
    clock: u64,
    pre_roll: VecDeque<f32>,
    pre_roll_len: usize,
    in_speech: bool,
    turn: Option<Turn>,
    last_partial: String,
}

fn secs(samples: u64) -> f64 {
    samples as f64 / SAMPLE_RATE as f64
}

fn ms_since(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

impl Pipeline {
    pub fn new(engine: Arc<Engine>, params: PipelineParams) -> Result<Self> {
        let vad = engine.new_vad(
            params.vad_threshold,
            params.min_silence_s,
            params.min_speech_s,
            params.max_speech_s,
        )?;
        // Silero only fires after `min_speech_s` of speech and sherpa-onnx
        // back-dates segments by two windows; keep a margin beyond both so the
        // recognizer hears the first syllable.
        let pre_roll_len = ((params.min_speech_s + 0.3) * SAMPLE_RATE as f32) as usize + 2 * WINDOW;
        Ok(Self {
            engine,
            params,
            vad,
            pending: Vec::with_capacity(4 * WINDOW),
            clock: 0,
            pre_roll: VecDeque::with_capacity(pre_roll_len + WINDOW),
            pre_roll_len,
            in_speech: false,
            turn: None,
            last_partial: String::new(),
        })
    }

    pub fn params(&self) -> &PipelineParams {
        &self.params
    }

    /// Seconds of audio processed so far.
    pub fn clock_s(&self) -> f64 {
        secs(self.clock)
    }

    /// Feeds 16 kHz mono samples. Events are appended to `out`.
    pub fn accept(&mut self, samples: &[f32], out: &mut Vec<Event>) {
        self.pending.extend_from_slice(samples);
        let mut offset = 0;
        let mut window = [0f32; WINDOW];
        while self.pending.len() - offset >= WINDOW {
            window.copy_from_slice(&self.pending[offset..offset + WINDOW]);
            offset += WINDOW;
            self.process_window(&window, out);
        }
        self.pending.drain(..offset);
    }

    /// Ends the input: releases whatever turn is open.
    pub fn finish(&mut self, out: &mut Vec<Event>) {
        if !self.pending.is_empty() {
            let mut window = [0f32; WINDOW];
            window[..self.pending.len()].copy_from_slice(&self.pending);
            self.pending.clear();
            self.process_window(&window, out);
        }
        if self.turn.is_some() {
            self.release(TurnEnd::EndOfInput, None, out);
        }
        self.vad.reset();
        self.pre_roll.clear();
        self.in_speech = false;
    }

    fn process_window(&mut self, w: &[f32], out: &mut Vec<Event>) {
        // An open turn hears every window, pauses included.
        if let Some(turn) = self.turn.as_mut() {
            turn.audio.extend_from_slice(w);
            if self.engine.feed(&turn.stream, w) {
                let text = self.engine.current_text(&turn.stream);
                if !text.is_empty() && text != self.last_partial {
                    self.last_partial = text.clone();
                    out.push(Event::Partial { text });
                }
            }
        } else {
            self.pre_roll.extend(w.iter().copied());
            let excess = self.pre_roll.len().saturating_sub(self.pre_roll_len);
            self.pre_roll.drain(..excess);
        }

        self.vad.accept_waveform(w);
        self.clock += w.len() as u64;

        // The VAD queues a segment once it has seen `min_silence_s` of silence.
        // We keep our own copy of the audio; only the boundary matters.
        let paused = !self.vad.is_empty();
        while !self.vad.is_empty() {
            self.vad.pop();
        }
        if paused && self.in_speech {
            self.in_speech = false;
            self.on_pause(out);
        }

        if !self.in_speech && self.vad.detected() {
            self.in_speech = true;
            self.on_speech(out);
        }

        self.check_timers(out);
    }

    fn on_speech(&mut self, out: &mut Vec<Event>) {
        if let Some(turn) = self.turn.as_mut() {
            // Speech resumed inside a held turn: keep listening on the same stream.
            turn.hold_until = None;
            return;
        }
        let audio: Vec<f32> = self.pre_roll.drain(..).collect();
        let stream = self.engine.create_stream();
        self.engine.feed(&stream, &audio);
        let start = self.clock - audio.len() as u64;
        out.push(Event::SpeechStart { at_s: secs(start) });
        self.turn = Some(Turn {
            stream,
            audio,
            start,
            segments: 0,
            hold_until: None,
            probability: None,
            vad_end: self.clock,
            turn_ms: None,
        });
    }

    fn on_pause(&mut self, out: &mut Vec<Event>) {
        let started = Instant::now();
        let clock = self.clock;
        let use_turn = self.params.use_turn_detector && self.engine.has_turn_detector();
        let Some(turn) = self.turn.as_mut() else { return };
        turn.segments += 1;
        turn.vad_end = clock;

        if secs(clock - turn.start) >= self.params.max_turn_s as f64 {
            self.release(TurnEnd::MaxLength, Some(started), out);
            return;
        }
        if !use_turn {
            self.release(TurnEnd::VadOnly, Some(started), out);
            return;
        }

        let t = Instant::now();
        let verdict = match self.engine.turn_detector() {
            Some(detector) => detector
                .lock()
                .map_err(|_| anyhow::anyhow!("smart-turn lock poisoned"))
                .and_then(|mut model| model.predict(&turn.audio)),
            None => Err(anyhow::anyhow!("no turn detector")),
        };
        let turn_ms = ms_since(t);
        turn.turn_ms = Some(turn_ms);

        match verdict {
            Ok(p) => {
                turn.probability = Some(p);
                out.push(Event::SegmentEnd {
                    at_s: secs(clock),
                    probability: Some(p),
                    turn_ms: Some(turn_ms),
                    turn_error: None,
                });
                if p >= self.params.turn_threshold {
                    self.release(TurnEnd::Complete, Some(started), out);
                } else {
                    turn.hold_until =
                        Some(clock + (self.params.max_hold_s * SAMPLE_RATE as f32) as u64);
                    out.push(Event::Held {
                        text: crate::tidy_text(&self.last_partial),
                        probability: p,
                    });
                }
            }
            Err(e) => {
                // A broken turn model must not stall the conversation.
                out.push(Event::SegmentEnd {
                    at_s: secs(clock),
                    probability: None,
                    turn_ms: Some(turn_ms),
                    turn_error: Some(e.to_string()),
                });
                self.release(TurnEnd::VadOnly, Some(started), out);
            }
        }
    }

    fn check_timers(&mut self, out: &mut Vec<Event>) {
        let Some(turn) = self.turn.as_ref() else { return };
        if !self.in_speech && turn.hold_until.is_some_and(|deadline| self.clock >= deadline) {
            self.release(TurnEnd::HoldTimeout, None, out);
            return;
        }
        if secs(self.clock - turn.start) >= self.params.max_turn_s as f64 {
            // A monologue: release what we have and carry on in a new turn.
            self.release(TurnEnd::MaxLength, None, out);
            if self.in_speech {
                self.in_speech = false;
                self.vad_detected_restart(out);
            }
        }
    }

    fn vad_detected_restart(&mut self, out: &mut Vec<Event>) {
        if self.vad.detected() {
            self.in_speech = true;
            self.on_speech(out);
        }
    }

    /// Flushes the turn's recognizer and emits the result. `trigger` is when
    /// the decision started (the pause), for the latency breakdown.
    fn release(&mut self, end: TurnEnd, trigger: Option<Instant>, out: &mut Vec<Event>) {
        let Some(turn) = self.turn.take() else { return };
        let t = Instant::now();
        let text = self.engine.finish_stream(&turn.stream);
        let flush_ms = ms_since(t);
        let decision_ms = trigger.map(ms_since).unwrap_or(flush_ms);

        // Seed the next turn's look-back with the pause the VAD just measured.
        // Quiet onsets ("in and...", "Bri-") often sit inside that window; if
        // the speaker carries straight on they would otherwise be lost.
        self.pre_roll.clear();
        let pause = ((self.params.min_silence_s * SAMPLE_RATE as f32) as usize)
            .min(self.pre_roll_len)
            .min(turn.audio.len());
        self.pre_roll
            .extend(turn.audio[turn.audio.len() - pause..].iter().copied());
        self.last_partial.clear();

        if text.is_empty() {
            out.push(Event::Discarded { at_s: secs(self.clock) });
        } else {
            out.push(Event::Turn(TurnResult {
                text: crate::tidy_text(&text),
                end,
                probability: turn.probability,
                segments: turn.segments,
                start_s: secs(turn.start),
                vad_end_s: secs(turn.vad_end),
                released_s: secs(self.clock),
                asr_flush_ms: flush_ms,
                turn_ms: turn.turn_ms,
                decision_ms,
            }));
        }
    }
}
