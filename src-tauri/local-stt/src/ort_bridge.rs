//! Points the `ort` crate at the ONNX Runtime that sherpa-onnx links statically.
//!
//! sherpa-onnx ships one static `libonnxruntime.a`. Linking a second copy for
//! `ort` would duplicate thousands of symbols, so `ort` is built with
//! `alternative-backend` (no linking) and handed the `OrtApi` table from the
//! copy that is already in the binary.

use anyhow::{anyhow, Result};
use ort::environment::Environment;
use std::ffi::CStr;
use std::sync::OnceLock;

struct Bridge {
    version: String,
    env: Environment,
}

static INIT: OnceLock<Result<Bridge, String>> = OnceLock::new();

fn bridge() -> Result<&'static Bridge> {
    INIT.get_or_init(|| {
        let version = unsafe {
            // Resolved at link time against sherpa-onnx's static ONNX Runtime.
            let base = ort::sys::OrtGetApiBase();
            if base.is_null() {
                return Err("OrtGetApiBase returned null".to_string());
            }
            let version = CStr::from_ptr(((*base).GetVersionString)())
                .to_string_lossy()
                .into_owned();
            let api = ((*base).GetApi)(ort::sys::ORT_API_VERSION);
            if api.is_null() {
                return Err(format!(
                    "ONNX Runtime {version} does not provide C API v{}",
                    ort::sys::ORT_API_VERSION
                ));
            }
            // `OrtApi` is compiled with only the v17 fields (feature `api-17`),
            // so this copies exactly the table prefix ONNX Runtime >= 1.17 has.
            ort::set_api((*api).clone());
            version
        };
        // ONNX Runtime keeps one environment per process; sherpa-onnx's
        // sessions and ours share it.
        let env = ort::init()
            .with_name("clocks-local-stt")
            .with_telemetry(false)
            .build()
            .map_err(|e| format!("ONNX Runtime environment: {e}"))?;
        Ok(Bridge { version, env })
    })
    .as_ref()
    .map_err(|e| anyhow!(e.clone()))
}

/// Initialise `ort` once. Returns the ONNX Runtime version string.
pub fn init() -> Result<String> {
    bridge().map(|b| b.version.clone())
}

pub fn environment() -> Result<&'static Environment> {
    bridge().map(|b| &b.env)
}
