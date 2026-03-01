//! JsRuntime creation and V8 startup snapshot management.

#![allow(unsafe_code)] // deno_core internals

use deno_core::{JsRuntime, RuntimeOptions};

use super::ops::rift_functions_ext;

/// The Web Standards shim JS, loaded into every isolate.
const SHIM_JS: &str = include_str!("shim.js");

/// Create a fresh JsRuntime for a single function invocation.
///
/// Each request gets a completely fresh isolate — no shared state between
/// invocations. The shim JS (Request, Response, Headers, fetch, etc.)
/// is executed at runtime creation.
pub fn create_function_runtime() -> Result<JsRuntime, deno_core::error::AnyError> {
    let mut runtime = JsRuntime::new(RuntimeOptions {
        extensions: vec![rift_functions_ext::init_ops()],
        ..Default::default()
    });

    // Load the Web Standards shim into the global scope
    runtime.execute_script("[rift:shim]", SHIM_JS)?;

    Ok(runtime)
}
