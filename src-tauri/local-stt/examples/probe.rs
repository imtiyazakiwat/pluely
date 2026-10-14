//! Smoke test: links, loads every model, and runs each stage once.
//!
//! cargo run -p clocks-local-stt --release --example probe -- <models-dir>

use clocks_local_stt::{models, Engine, SmartTurn};
use std::path::PathBuf;
use std::time::Instant;

fn main() -> anyhow::Result<()> {
    let root = PathBuf::from(std::env::args().nth(1).expect("usage: probe <models-dir>"));
    println!("onnxruntime {}", clocks_local_stt::onnxruntime_version()?);

    let cfg = models::default_engine_config(&root);
    let t = Instant::now();
    let engine = Engine::load(&cfg)?;
    println!("engine loaded in {:.0} ms", t.elapsed().as_secs_f64() * 1000.0);

    let silence = vec![0.0f32; 16_000];
    let out = engine.transcribe(&silence);
    println!("transcribe(1 s silence) = {:?} in {:.1} ms", out.text, out.compute_ms);

    let dir = cfg.turn_model.as_ref().unwrap().parent().unwrap().to_path_buf();
    for (file, threads) in [
        ("smart-turn-v3.2-cpu.onnx", 1),
        ("smart-turn-v3.2-cpu.onnx", 2),
        ("smart-turn-v3.2-cpu.onnx", 4),
        ("smart-turn-v3.2-gpu.onnx", 1),
        ("smart-turn-v3.2-gpu.onnx", 2),
        ("smart-turn-v3.2-gpu.onnx", 4),
    ] {
        let path = dir.join(file);
        if !path.exists() {
            continue;
        }
        print!("{file} ");
        let mut turn = SmartTurn::load(&path, threads)?;
        let _ = turn.predict(&silence)?; // warm-up
        let mut best = f64::MAX;
        let mut last = None;
        for _ in 0..10 {
            let s = turn.predict_timed(&silence)?;
            best = best.min(s.features_ms + s.inference_ms);
            last = Some(s);
        }
        let s = last.unwrap();
        println!(
            "smart-turn threads={threads}: p={:.4} features={:.1} ms inference={:.1} ms (best total {:.1} ms)",
            s.probability, s.features_ms, s.inference_ms, best
        );
    }
    Ok(())
}
