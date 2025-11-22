// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::db::DoubleFuckedError;
use crate::http::jinxxy::JinxxyError;
use poise::serenity_prelude as serenity;
use serenity::Error as SerenityError;
use std::fmt::{Display, Formatter};
use tokio_rusqlite::Error as SqliteError;

#[derive(Debug)]
#[allow(unused)] // these are debug printed frequently
pub enum JinxError {
    Message(String),
    Jinxxy(JinxxyError),
    Sqlite(SqliteError),
    Serenity(SerenityError),
}

impl Display for JinxError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            JinxError::Message(message) => f.write_str(message.as_str()),
            JinxError::Jinxxy(e) => write!(f, "{}", e.safe_display()),
            JinxError::Sqlite(_) => write!(f, "DB error"),
            JinxError::Serenity(_) => write!(f, "Discord API error"),
        }
    }
}

/// mark the normal Display impl as being safe
impl<'a> SafeDisplay<'a, &'a Self> for JinxError {
    fn safe_display(&'a self) -> &'a Self {
        self
    }
}

impl From<JinxxyError> for JinxError {
    fn from(e: JinxxyError) -> Self {
        Self::Jinxxy(e)
    }
}

impl From<SqliteError> for JinxError {
    fn from(e: SqliteError) -> Self {
        Self::Sqlite(e)
    }
}

impl From<SerenityError> for JinxError {
    fn from(e: SerenityError) -> Self {
        Self::Serenity(e)
    }
}

impl From<DoubleFuckedError> for JinxError {
    fn from(e: DoubleFuckedError) -> Self {
        match e {
            tokio_rusqlite::Error::Error(e) => JinxError::Sqlite(e),
            _ => JinxError::new("error flattening doubly-fucked Sqlite error"),
        }
    }
}

impl std::error::Error for JinxError {}

impl JinxError {
    /// `message` is a message that is safe to display to a user
    pub fn new<T: Into<String>>(message: T) -> Self {
        Self::Message(message.into())
    }

    /// `message` is a message that is safe to display to a user
    pub fn boxed<T: Into<String>>(message: T) -> Box<Self> {
        Box::new(Self::new(message))
    }
}

/// A type with an alternate Display implementation that is safe to display to untrusted users
pub trait SafeDisplay<'a, T>
where
    T: Display,
{
    fn safe_display(&'a self) -> T;
}
