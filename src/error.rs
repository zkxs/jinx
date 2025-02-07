// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use std::fmt::{Display, Formatter};

#[derive(Debug)]
pub struct JinxError {
    message: String,
}

impl Display for JinxError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message.as_str())
    }
}

impl std::error::Error for JinxError {}

impl JinxError {
    /// `message` is a message that is safe to display to a user
    pub fn new<T: Into<String>>(message: T) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// `message` is a message that is safe to display to a user
    pub fn boxed<T: Into<String>>(message: T) -> Box<Self> {
        Box::new(Self::new(message))
    }
}
