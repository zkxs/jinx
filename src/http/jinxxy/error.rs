// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::error::SafeDisplay;
use bytes::Bytes;
use reqwest::{Response, StatusCode};
use serde::Deserialize;
use std::fmt::Formatter;

pub type JinxxyResult<T> = Result<T, JinxxyError>;

#[derive(Debug)]
#[allow(unused)] // these are debug printed frequently
pub enum JinxxyError {
    /// Any error for which we got an HTTP response from Jinxxy. Happens when we detect non-200 status codes.
    /// If we're looking for a 404 we just build one of these errors directly. If we expect a 2xx these errors
    /// are built for any non-2xx response.
    HttpResponse(HttpResponse),
    /// Any error for which we did not get an HTTP response. Happens if we fail while during the initial request `.send()`.
    HttpRequest(ReqwestError),
    /// An error occurred reading response body. We did not expect an error, so headers were not captured.
    HttpRead(ReqwestError),
    /// We received a successful response from Jinxxy which we could not deserialize
    JsonDeserialize(serde_json::Error),
    /// Some parallel task join failed.
    Join(tokio::task::JoinError),
    /// Impossible 304 response: we got a 304 without Etag set
    Impossible304,
}

impl std::fmt::Display for JinxxyError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HttpResponse(_) => write!(f, "Jinxxy API error"),
            Self::HttpRequest(_) => write!(f, "HTTP general failure"),
            Self::HttpRead(_) => write!(f, "HTTP body read failed"),
            Self::JsonDeserialize(_) => write!(f, "JSON deserialization failed"),
            Self::Join(e) => write!(f, "parallel task join failed: {e}"),
            Self::Impossible304 => write!(
                f,
                "got 304 response from Jinxxy API without having set If-None-Match header"
            ),
        }
    }
}

/// mark the normal Display impl as being safe
impl<'a> SafeDisplay<'a, &'a Self> for JinxxyError {
    fn safe_display(&'a self) -> &'a Self {
        self
    }
}

impl std::error::Error for JinxxyError {}

impl JinxxyError {
    /// Create a JinxxyError from raw json bytes
    pub async fn from_response(endpoint: &'static str, response: Response) -> Self {
        let status_code = response.status();
        let headers = format!("{:?}", response.headers());
        let bytes = response.bytes().await;
        let body = match bytes {
            Ok(bytes) => match serde_json::from_slice::<JinxxyErrorResponse>(&bytes) {
                Ok(json) => HttpBody::JsonErrorResponse(json),
                Err(_json_error) => HttpBody::UnknownErrorResponse(bytes),
            },
            Err(read_error) => HttpBody::ReadError(read_error),
        };
        let http = HttpResponse {
            endpoint,
            status_code,
            headers,
            body,
        };
        Self::HttpResponse(http)
    }

    /// Create a JinxxyError from a reqwest error (use this after `.send()`)
    pub fn from_request(endpoint: &'static str, error: reqwest::Error) -> Self {
        let inner = ReqwestError { endpoint, error };
        Self::HttpRequest(inner)
    }

    /// Create a JinxxyError from a reqwest error attempting to read response body (use this after `.bytes()`)
    pub fn from_read(endpoint: &'static str, error: reqwest::Error) -> Self {
        let inner = ReqwestError { endpoint, error };
        Self::HttpRead(inner)
    }

    /// Create a JinxxyError from a serde_json Error
    pub fn from_json(json_error: serde_json::Error) -> Self {
        Self::JsonDeserialize(json_error)
    }

    /// Create a JinxxyError from a tokio JoinError
    pub fn from_join(join_error: tokio::task::JoinError) -> Self {
        Self::Join(join_error)
    }

    /// Check if an error looks like a 403.
    ///
    /// For some reason Jinxxy does not return a reasonable status code, leaving it up to me to parse their 500 response JSON.
    pub fn looks_like_403(&self) -> bool {
        match self {
            Self::HttpResponse(response) => match &response.body {
                HttpBody::JsonErrorResponse(response) => response.status_code == 403 || response.looks_like_403(),
                _ => false,
            },
            _ => false,
        }
    }

    /// Check if an error looks like a 404.
    ///
    /// For some reason Jinxxy does not return a reasonable status code, leaving it up to me to parse their 500 response JSON.
    pub fn looks_like_404(&self) -> bool {
        match self {
            Self::HttpResponse(response) => match &response.body {
                HttpBody::JsonErrorResponse(response) => response.status_code == 404 || response.looks_like_404(),
                _ => false,
            },
            _ => false,
        }
    }
}

/// Generic wrapper for a reqwest error.
#[derive(Debug)]
#[allow(unused)] // these are debug printed frequently
pub struct ReqwestError {
    endpoint: &'static str,
    error: reqwest::Error,
}

#[derive(Debug)]
#[allow(unused)] // these are debug printed frequently
pub struct HttpResponse {
    endpoint: &'static str,
    status_code: StatusCode,
    headers: String,
    body: HttpBody,
}

#[derive(Debug)]
#[allow(unused)] // these are debug printed frequently
pub enum HttpBody {
    /// We received an error response from Jinxxy which was successfully deserialized
    JsonErrorResponse(JinxxyErrorResponse),
    /// We received an error response from Jinxxy which we could not deserialize
    UnknownErrorResponse(Bytes),
    /// An error occurred reading request body. We expected an error, so we captured headers already.
    ReadError(reqwest::Error),
}

/// Undocumented part of the Jinxxy API. JSON looks like this:
/// ```json
/// {
///   "status_code": 500,
///   "error": "Bad Request",
///   "message": "You are not authorized.",
///   "code": "GRAPHQL_ERROR"
/// }
/// ```
///
/// However, in some cases it also looks like this:
/// ```json
/// {
///   "status_code": 400,
///   "error": "Bad Request",
///   "message": [
///     {
///       "message": "an unknown value was passed to the validate function",
///       "code": "validation_error"
///     }
///   ],
///   "code": "internal_server_error"
/// }
/// ```
#[derive(Debug, Deserialize)]
pub struct JinxxyErrorResponse {
    status_code: u16,
    error: String,
    message: JinxxyErrorMessage,
    /// This field appears completely useless for my own use, but might be helpful for the Jinxxy devs if I need to
    /// forward an error report along.
    #[allow(unused)]
    code: String,
}

impl JinxxyErrorResponse {
    /// Check if an error looks like a 403.
    ///
    /// For some reason Jinxxy does not return a reasonable status code, leaving it up to me to parse their 500 response JSON.
    pub fn looks_like_403(&self) -> bool {
        self.status_code == 403 || (self.error == "Bad Request" && self.message.matches("You are not authorized."))
    }

    /// Check if an error looks like a 404.
    ///
    /// For some reason Jinxxy does not return a reasonable status code, leaving it up to me to parse their 500 response JSON.
    pub fn looks_like_404(&self) -> bool {
        self.status_code == 404 || (self.error == "Bad Request" && self.message.matches("Resource not found."))
    }
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum JinxxyErrorMessage {
    SingleMessage(String),
    MultiMessage(Vec<JinxxyErrorMultiMessagePart>),
}

impl JinxxyErrorMessage {
    fn matches(&self, string: &str) -> bool {
        match self {
            // For single messages, do an exact string match. I've seen this case be useful in the wild.
            Self::SingleMessage(message) => message == string,
            // For multi-messages, match each message. I've never seen this be useful in the wild, though.
            Self::MultiMessage(messages) => messages
                .iter()
                .map(|item| &item.message)
                .any(|message| message == string),
        }
    }
}

#[derive(Debug, Deserialize)]
#[allow(unused)] // these are debug printed frequently
pub struct JinxxyErrorMultiMessagePart {
    message: String,
    code: String,
}
