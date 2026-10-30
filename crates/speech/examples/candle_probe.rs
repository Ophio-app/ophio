// SPDX-License-Identifier: Apache-2.0
//! CPU candidate benchmark. Arguments: model directory, mel filters, PCM16 WAV.
use candle_core::{Device, IndexOp, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::whisper::{self, Config};
use std::{error::Error, path::PathBuf, time::Instant};
use tokenizers::Tokenizer;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err("expected model directory, mel filters, PCM16 WAV".into());
    }
    let dir = PathBuf::from(&args[0]);
    let cfg: Config = serde_json::from_slice(&std::fs::read(dir.join("config.json"))?)?;
    let tokenizer = Tokenizer::from_file(dir.join("tokenizer.json"))?;
    let filters: Vec<_> = std::fs::read(&args[1])?
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    let mut wav = hound::WavReader::open(&args[2])?;
    assert_eq!(wav.spec().sample_rate, 16_000);
    assert_eq!(wav.spec().channels, 1);
    let samples: Vec<_> = wav
        .samples::<i16>()
        .map(|s| s.map(|s| f32::from(s) / 32768.))
        .collect::<Result<_, _>>()?;
    let vb = VarBuilder::from_buffered_safetensors(
        std::fs::read(dir.join("model.safetensors"))?,
        whisper::DTYPE,
        &Device::Cpu,
    )?;
    let mut model = whisper::model::Whisper::load(&vb, cfg.clone())?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    pool.install(|| -> Result<(), Box<dyn Error + Send + Sync>> {
        for seconds in [1, 5, samples.len() / 16_000] {
            let audio = &samples[..(seconds * 16_000).min(samples.len())];
            let now = Instant::now();
            let mel = whisper::audio::pcm_to_mel(&cfg, audio, &filters);
            let frames = mel.len() / cfg.num_mel_bins;
            // Short context plus 500 ms padding avoids encoding 30 seconds per push.
            let mel = Tensor::from_vec(mel, (1, cfg.num_mel_bins, frames), &Device::Cpu)?.narrow(
                2,
                0,
                (seconds * 100 + 50).min(frames),
            )?;
            let encoded = model.encoder.forward(&mel, true)?;
            let mut tokens = vec![50257u32, 50362];
            for step in 0..96 {
                let input = Tensor::new(tokens.as_slice(), &Device::Cpu)?.unsqueeze(0)?;
                let out = model.decoder.forward(&input, &encoded, step == 0)?;
                let logits = model
                    .decoder
                    .final_linear(&out.i((.., tokens.len() - 1..))?)?
                    .i((0, 0))?;
                let mut values = logits.to_vec1::<f32>()?;
                for (id, v) in values.iter_mut().enumerate() {
                    if (id > 50256)
                        || cfg.suppress_tokens.contains(&(id as u32))
                        || (step == 0 && (id == 50256 || id == 220))
                    {
                        *v = f32::NEG_INFINITY;
                    }
                }
                let next = values
                    .iter()
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .ok_or("empty logits")?
                    .0 as u32;
                if next == 50256 {
                    break;
                }
                tokens.push(next);
            }
            let elapsed = now.elapsed().as_secs_f64();
            println!(
                "seconds={seconds} elapsed={elapsed:.3} rtf={:.3} text={}",
                elapsed / seconds as f64,
                tokenizer.decode(&tokens, true)?
            );
            model.reset_kv_cache();
        }
        Ok(())
    })
}
