// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::http::jinxxy::JinxxyError;
use poise::serenity_prelude as serenity;
use serenity::Error as SerenityError;
use sqlx::error::Error as SqlxError;
use std::fmt::{Display, Formatter};

/// A type with an alternate Display implementation that is safe to display to untrusted users
pub trait SafeDisplay<'a, T>
where
    T: Display,
{
    fn safe_display(&'a self) -> T;
}

#[derive(Debug)]
#[allow(unused)] // these are debug printed frequently
pub enum JinxError {
    Message(String),
    Jinxxy(JinxxyError),
    Sqlite(SqlxError),
    Serenity(SerenityError),
}

impl std::error::Error for JinxError {}

impl Display for JinxError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            JinxError::Message(message) => f.write_str(message.as_str()),
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

impl From<JinxxyError> for JinxError {
    fn from(e: JinxxyError) -> Self {
        Self::Jinxxy(e)
    }
}

impl From<SqlxError> for JinxError {
    fn from(e: SqlxError) -> Self {
        Self::Sqlite(e)
    }
}

impl From<SerenityError> for JinxError {
    fn from(e: SerenityError) -> Self {
        Self::Serenity(e)
    }
}

impl JinxError {
    /// `message` is a message that is safe to display to a user
    pub fn new<T: Into<String>>(message: T) -> Self {
        Self::Message(message.into())
    }
}
