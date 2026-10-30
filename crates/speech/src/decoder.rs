// SPDX-License-Identifier: Apache-2.0
//! CPU-only Whisper decoding. All operations run inside the engine's private
//! Rayon pool; the audio frontend does not create additional worker threads.
use candle_core::{Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::whisper::{Config, DTYPE, model::Whisper};
use rustfft::{FftPlanner, num_complex::Complex32};
use std::sync::atomic::{AtomicBool, Ordering};
use tokenizers::Tokenizer;

#[derive(Debug, thiserror::Error)]
pub(crate) enum DecodeError {
    #[error("speech session cancelled")]
    Cancelled,
    #[error("local speech model computation failed")]
    Tensor(#[from] candle_core::Error),
    #[error("local speech model configuration is invalid")]
    Configuration,
    #[error("local speech tokenizer failed")]
    Tokenizer,
}

pub(crate) trait Decoder: Send {
    fn decode(&mut self, audio: &[i16], cancel: &AtomicBool) -> Result<String, DecodeError>;
}

pub(crate) struct LocalDecoder {
    model: Whisper,
    tokenizer: Tokenizer,
    mel: Mel,
}

impl LocalDecoder {
    pub(crate) fn load(
        weights: Vec<u8>,
        config: &[u8],
        tokenizer: &[u8],
    ) -> Result<Self, DecodeError> {
        let cfg: Config = serde_json::from_slice(config).map_err(|_| DecodeError::Configuration)?;
        if cfg.num_mel_bins != 80 || cfg.vocab_size != 51_864 {
            return Err(DecodeError::Configuration);
        }
        let vb = VarBuilder::from_buffered_safetensors(weights, DTYPE, &Device::Cpu)?;
        let model = Whisper::load(&vb, cfg)?;
        let tokenizer = Tokenizer::from_bytes(tokenizer).map_err(|_| DecodeError::Tokenizer)?;
        Ok(Self {
            model,
            tokenizer,
            mel: Mel::new(),
        })
    }

    fn recognize(&mut self, audio: &[i16], cancel: &AtomicBool) -> Result<String, DecodeError> {
        check_cancel(cancel)?;
        let (mel, frames) = self.mel.compute(audio, cancel)?;
        let mel = Tensor::from_vec(mel, (1, 80, frames), &Device::Cpu)?;
        let encoded = self.model.encoder.forward(&mel, true)?;
        // The reviewed models are English-only. They use SOT + no-timestamps;
        // segment times come from the actual VAD capture span, never word counts.
        let mut tokens = vec![50_257u32, 50_362];
        let max_tokens = (audio.len() * 8 / 16_000 + 16).min(128);
        let mut log_probability = 0.0f64;
        let mut no_speech = 0.0f32;
        for step in 0..max_tokens {
            check_cancel(cancel)?;
            let input = Tensor::new(tokens.as_slice(), &Device::Cpu)?.unsqueeze(0)?;
            let decoded = self.model.decoder.forward(&input, &encoded, step == 0)?;
            if step == 0 {
                let logits = self
                    .model
                    .decoder
                    .final_linear(&decoded.i((.., 0..1))?)?
                    .i((0, 0))?;
                no_speech = candle_nn::ops::softmax(&logits, 0)?
                    .i(50_361)?
                    .to_scalar()?;
            }
            let logits = self
                .model
                .decoder
                .final_linear(&decoded.i((.., tokens.len() - 1..))?)?
                .i((0, 0))?;
            let mut values = logits.to_vec1::<f32>()?;
            for (id, value) in values.iter_mut().enumerate() {
                if id > 50_256
                    || self.model.config.suppress_tokens.contains(&(id as u32))
                    || (step == 0 && (id == 50_256 || id == 220))
                {
                    *value = f32::NEG_INFINITY;
                }
            }
            let (next, &best) = values
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .ok_or(DecodeError::Configuration)?;
            if !best.is_finite() {
                return Err(DecodeError::Configuration);
            }
            let sum: f64 = values.iter().map(|v| f64::from(*v - best).exp()).sum();
            log_probability -= sum.ln();
            if next == 50_256 {
                break;
            }
            tokens.push(next as u32);
            // Streaming prefixes sometimes loop before EOT. Stop repeated token
            // phrases instead of spending the full budget hallucinating them.
            let words = &tokens[2..];
            let repeated = (2..=12).find(|&n| {
                words.len() >= 3 * n
                    && words[words.len() - n..] == words[words.len() - 2 * n..words.len() - n]
                    && words[words.len() - n..] == words[words.len() - 3 * n..words.len() - 2 * n]
            });
            if let Some(n) = repeated {
                tokens.truncate(tokens.len() - 2 * n);
                break;
            }
        }
        check_cancel(cancel)?;
        if no_speech > 0.6 && log_probability / ((tokens.len() - 1) as f64) < -1.0 {
            return Ok(String::new());
        }
        self.tokenizer
            .decode(&tokens[2..], true)
            .map(|s| s.trim().to_owned())
            .map_err(|_| DecodeError::Tokenizer)
    }
}

impl Decoder for LocalDecoder {
    fn decode(&mut self, audio: &[i16], cancel: &AtomicBool) -> Result<String, DecodeError> {
        self.model.reset_kv_cache();
        let result = self.recognize(audio, cancel);
        // No transcript, audio features or session cache survives a decode.
        self.model.reset_kv_cache();
        result
    }
}

pub(crate) fn check_cancel(cancel: &AtomicBool) -> Result<(), DecodeError> {
    if cancel.load(Ordering::Acquire) {
        Err(DecodeError::Cancelled)
    } else {
        Ok(())
    }
}

struct Mel {
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    hann: Vec<f32>,
    filters: Vec<Vec<f32>>,
}

impl Mel {
    fn new() -> Self {
        let hz_to_mel = |hz: f64| {
            if hz < 1000.0 {
                hz / (200.0 / 3.0)
            } else {
                15.0 + (hz / 1000.0).ln() / (6.4f64.ln() / 27.0)
            }
        };
        let mel_to_hz = |mel: f64| {
            if mel < 15.0 {
                mel * (200.0 / 3.0)
            } else {
                1000.0 * ((mel - 15.0) * (6.4f64.ln() / 27.0)).exp()
            }
        };
        let edges: Vec<f64> = (0..82)
            .map(|i| mel_to_hz(hz_to_mel(8000.0) * f64::from(i) / 81.0))
            .collect();
        let filters = (0..80)
            .map(|i| {
                (0..201)
                    .map(|k| {
                        let hz = f64::from(k) * 40.0;
                        let weight = ((hz - edges[i]) / (edges[i + 1] - edges[i]))
                            .min((edges[i + 2] - hz) / (edges[i + 2] - edges[i + 1]))
                            .max(0.0);
                        (weight * 2.0 / (edges[i + 2] - edges[i])) as f32
                    })
                    .collect()
            })
            .collect();
        let hann = (0..400)
            .map(|i| 0.5 * (1.0 - (std::f32::consts::TAU * i as f32 / 400.0).cos()))
            .collect();
        Self {
            fft: FftPlanner::new().plan_fft_forward(400),
            hann,
            filters,
        }
    }

    fn compute(
        &self,
        audio: &[i16],
        cancel: &AtomicBool,
    ) -> Result<(Vec<f32>, usize), DecodeError> {
        // 500 ms zero padding; an even frame count is required by stride-2 conv.
        let frames = (audio.len() + 8000).div_ceil(320).max(50) * 2;
        let mut output = vec![0.0; 80 * frames];
        let mut buffer = vec![Complex32::default(); 400];
        let mut scratch = vec![Complex32::default(); self.fft.get_inplace_scratch_len()];
        let mut power = [0.0; 201];
        let mut maximum = -10.0f32;
        for frame in 0..frames {
            check_cancel(cancel)?;
            for (i, value) in buffer.iter_mut().enumerate() {
                let at = (frame * 160 + i) as isize - 200;
                let at = at.unsigned_abs(); // reflect left edge, zero-pad right
                *value = Complex32::new(
                    audio
                        .get(at)
                        .copied()
                        .map_or(0.0, |s| f32::from(s) / 32768.0)
                        * self.hann[i],
                    0.0,
                );
            }
            self.fft.process_with_scratch(&mut buffer, &mut scratch);
            for (p, value) in power.iter_mut().zip(&buffer) {
                *p = value.norm_sqr();
            }
            for (bin, filter) in self.filters.iter().enumerate() {
                let value: f32 = filter.iter().zip(power).map(|(w, p)| w * p).sum();
                let value = value.max(1e-10).log10();
                maximum = maximum.max(value);
                output[bin * frames + frame] = value;
            }
        }
        for value in &mut output {
            *value = (value.max(maximum - 8.0) + 4.0) / 4.0;
        }
        Ok((output, frames))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_is_finite_and_cancellation_is_checked() {
        let mel = Mel::new();
        let (values, frames) = mel.compute(&[0; 16_000], &AtomicBool::new(false)).unwrap();
        assert_eq!(frames, 150);
        assert!(values.iter().all(|v| *v == -1.5));
        assert!(matches!(
            mel.compute(&[0; 320], &AtomicBool::new(true)),
            Err(DecodeError::Cancelled)
        ));
    }
}
