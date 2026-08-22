// This file is part of jinx. Copyright © 2025-2026 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::bot::util::SafeDisplay;
use crate::http::jinxxy::JinxxyError;
use poise::serenity_prelude as serenity;
use serenity::Error as SerenityError;
use sqlx::error::Error as SqlxError;
use std::fmt::{Display, Formatter};

pub type JinxResult<T> = Result<T, Box<JinxError>>;

/// High level error type that can contain anything that goes wrong with Jinx. This type has grown quite large, so it
/// is always boxed
#[derive(Debug)]
#[allow(dead_code)] // these are debug printed frequently
pub enum JinxError {
    Message(String),
    Sensitive { public: String, private: String },
    Jinxxy(Box<JinxxyError>),
    Sqlite(SqlxError),
    Serenity(SerenityError),
}

impl std::error::Error for JinxError {}

impl Display for JinxError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            JinxError::Message(message) => f.write_str(message.as_str()),
            JinxError::Sensitive { public, private } => write!(f, "{public}: {private}"),
            JinxError::Jinxxy(e) => write!(f, "{}", e),
            JinxError::Sqlite(e) => write!(f, "DB error: {e}"),
            JinxError::Serenity(e) => write!(f, "Discord API error: {e}"),
        }
    }
}

/// A JinxError wrapper with a redacted Display implementation
pub struct RedactedJinxError<'a>(&'a JinxError);

impl<'a> Display for RedactedJinxError<'a> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            JinxError::Message(message) => f.write_str(message.as_str()),
            JinxError::Sensitive { public, private: _ } => f.write_str(public.as_str()),
            JinxError::Jinxxy(e) => write!(f, "{}", e.safe_display()),
            JinxError::Sqlite(_) => write!(f, "DB error"),
            JinxError::Serenity(_) => write!(f, "Discord API error"),
        }
    }
}

/// mark the normal Display impl as being safe
impl<'a> SafeDisplay<'a, RedactedJinxError<'a>> for JinxError {
    fn safe_display(&'a self) -> RedactedJinxError<'a> {
        RedactedJinxError(self)
    }
}

impl From<Box<JinxxyError>> for Box<JinxError> {
    fn from(e: Box<JinxxyError>) -> Self {
        Box::new(JinxError::Jinxxy(e))
    }
}

impl From<SqlxError> for Box<JinxError> {
    fn from(e: SqlxError) -> Self {
        Box::new(JinxError::Sqlite(e))
    }
}

impl From<SerenityError> for Box<JinxError> {
    fn from(e: SerenityError) -> Self {
        Box::new(JinxError::Serenity(e))
    }
}

impl JinxError {
    /// `message` is a message that is safe to display to a user
    pub fn new(message: impl Into<String>) -> Box<Self> {
        Box::new(Self::Message(message.into()))
    }

    /// `public` is a message that is safe to display to a user.
    /// `private` is a message that may contain sensitive information.
    pub fn sensitive(public: impl Into<String>, private: impl Into<String>) -> Self {
        Self::Sensitive {
            public: public.into(),
            private: private.into(),
        }
    }

    /// Check if this error was caused by an invalid Jinxxy API key
    pub fn is_api_key_invalid(&self) -> bool {
        matches!(self, JinxError::Jinxxy(jinx_error) if jinx_error.is_api_key_invalid())
    }
}
