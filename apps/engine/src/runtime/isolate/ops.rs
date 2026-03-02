//! Rust ops exposed to function isolates via deno_core.
//!
//! These provide the runtime capabilities that user function handlers need:
//! outbound HTTP, logging, environment variables, crypto, timing.

#![allow(unsafe_code)] // deno_core's #[op2] macro generates unsafe code

use std::collections::HashMap;

use deno_core::op2;
use deno_core::OpState;

/// Console log — routes to tracing.
#[op2(fast)]
pub fn op_rift_console_log(#[string] msg: String) {
    tracing::info!(target: "rift_user_function", "{}", msg);
}

/// Console error — routes to tracing.
#[op2(fast)]
pub fn op_rift_console_error(#[string] msg: String) {
    tracing::error!(target: "rift_user_function", "{}", msg);
}

/// Outbound HTTP fetch. Async op that bridges to reqwest.
/// Takes body as a Vec<u8> via serde (JSON array of numbers from JS).
#[op2(async)]
#[serde]
pub async fn op_rift_fetch(
    #[string] url: String,
    #[string] method: String,
    #[serde] headers: Vec<(String, String)>,
    #[serde] body: Option<Vec<u8>>,
) -> Result<FetchResponse, deno_core::error::AnyError> {
    let client = reqwest::Client::new();
    let method: reqwest::Method = method
        .parse()
        .map_err(|e| deno_core::error::generic_error(format!("invalid HTTP method: {e}")))?;

    let mut req = client.request(method, &url);
    for (k, v) in &headers {
        req = req.header(k.as_str(), v.as_str());
    }
    if let Some(b) = body {
        req = req.body(b);
    }

    let resp = req
        .send()
        .await
        .map_err(|e| deno_core::error::generic_error(format!("fetch failed: {e}")))?;

    let status = resp.status().as_u16();
    let resp_headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let resp_body = resp.bytes().await.map_err(|e| {
        deno_core::error::generic_error(format!("failed to read response body: {e}"))
    })?;

    Ok(FetchResponse {
        status,
        headers: resp_headers,
        body: resp_body.to_vec(),
    })
}

#[derive(serde::Serialize)]
pub struct FetchResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// Read an environment variable from the per-request injected env vars.
/// Does NOT read from the actual process environment.
#[op2]
#[string]
pub fn op_rift_env_get(state: &mut OpState, #[string] key: String) -> Option<String> {
    state
        .try_borrow::<HashMap<String, String>>()
        .and_then(|env| env.get(&key).cloned())
}

/// crypto.randomUUID()
#[op2]
#[string]
pub fn op_rift_crypto_random_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Sleep for `ms` milliseconds. Used by setTimeout shim.
#[op2(async)]
pub async fn op_rift_sleep(#[number] ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

deno_core::extension!(
    rift_functions_ext,
    ops = [
        op_rift_console_log,
        op_rift_console_error,
        op_rift_fetch,
        op_rift_env_get,
        op_rift_crypto_random_uuid,
        op_rift_sleep,
    ],
);
