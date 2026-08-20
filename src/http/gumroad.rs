// This file is part of jinx. Copyright © 2026 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::http::HTTP2_CLIENT;
use bytes::Bytes;
use poise::serenity_prelude::StatusCode;
use reqwest::{Response, header};
use serde::Deserialize;
use tokio::time::Instant;
use tracing::debug;

const VERIFY_URL: &str = "https://api.gumroad.com/v2/licenses/verify";

/// Verify a Gumroad license given the product_id and license_key. Notably this API does not require any auth.
/// By default, the API wants to increment a use counter, but because Jinx is not designed to interact with this
/// system we opt out of it.
///
/// See: https://gumroad.com/api#licenses for more docs.
#[allow(dead_code)] //TODO: stop ignoring unused code
pub async fn verify_license(product_id: &str, license_key: &str) -> GumroadResult<bool> {
    let start_time = Instant::now();
    let response = HTTP2_CLIENT
        .post(format!(
            "{VERIFY_URL}?product_id={product_id}&license_key={license_key}&increment_uses_count=false"
        ))
        .header(header::ACCEPT, header::HeaderValue::from_static("application/json"))
        .send()
        .await
        .map_err(GumroadError::from_request)?;
    debug!("Gumroad /licenses/verify took {}ms", start_time.elapsed().as_millis());

    if response.status().is_success() {
        let bytes = response.bytes().await.map_err(GumroadError::from_read)?;
        let verification: Verification = serde_json::from_slice(&bytes).map_err(GumroadError::from_json)?;
        Ok(verification.success)
    } else if response.status().as_u16() == 404 {
        Ok(false)
    } else {
        Err(GumroadError::from_response(response).await)
    }
}

#[derive(Debug, Deserialize)]
struct Verification {
    success: bool,
}

pub type GumroadResult<T> = Result<T, GumroadError>;

#[derive(Debug)]
#[allow(dead_code)] // these are debug printed frequently
pub enum GumroadError {
    /// Any error for which we got an HTTP response from Gumroad. Happens when we detect non-200 status codes.
    /// If we're looking for a 404 we just build one of these errors directly. If we expect a 2xx these errors
    /// are built for any non-2xx response.
    HttpResponse(HttpResponse),
    /// Any error for which we did not get an HTTP response. Happens if we fail while during the initial request `.send()`.
    HttpRequest(ReqwestError),
    /// An error occurred reading response body. We did not expect an error, so headers were not captured.
    HttpRead(ReqwestError),
    /// We received a successful response from Gumroad which we could not deserialize
    JsonDeserialize(serde_json::Error),
}

impl GumroadError {
    /// Create a GumroadError from raw json bytes
    pub async fn from_response(response: Response) -> Self {
        let status_code = response.status();
        let headers = format!("{:?}", response.headers());
        let bytes = response.bytes().await;
        let body = match bytes {
            Ok(bytes) => HttpBody::UnknownErrorResponse(bytes),
            Err(read_error) => HttpBody::ReadError(read_error),
        };
        let http = HttpResponse {
            status_code,
            headers,
            body,
        };
        Self::HttpResponse(http)
    }

    /// Create a GumroadError from a reqwest error (use this after `.send()`)
    pub fn from_request(error: reqwest::Error) -> Self {
        let inner = ReqwestError { error };
        Self::HttpRequest(inner)
    }

    /// Create a GumroadError from a reqwest error attempting to read response body (use this after `.bytes()`)
    pub fn from_read(error: reqwest::Error) -> Self {
        let inner = ReqwestError { error };
        Self::HttpRead(inner)
    }

    /// Create a GumroadError from a serde_json Error
    pub fn from_json(json_error: serde_json::Error) -> Self {
        Self::JsonDeserialize(json_error)
    }
}

/// Generic wrapper for a reqwest error.
#[derive(Debug)]
#[allow(dead_code)] // these are debug printed frequently
pub struct ReqwestError {
    error: reqwest::Error,
}

#[derive(Debug)]
#[allow(dead_code)] // these are debug printed frequently
pub struct HttpResponse {
    status_code: StatusCode,
    headers: String,
    body: HttpBody,
}

#[derive(Debug)]
#[allow(dead_code)] // these are debug printed frequently
pub enum HttpBody {
    /// We received an error response from Gumroad which we could not deserialize
    UnknownErrorResponse(Bytes),
    /// An error occurred reading request body. We expected an error, so we captured headers already.
    ReadError(reqwest::Error),
}
