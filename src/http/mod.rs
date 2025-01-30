// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! HTTP API calls

use crate::constants;
use std::sync::LazyLock;
use std::time::Duration;

pub mod jinxxy;
pub mod update_checker;

static HTTP1_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(constants::USER_AGENT)
        .gzip(true)
        .https_only(true)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(10))
        // .connection_verbose(true) // useful for debugging
        .build()
        .expect("Failed to build HTTP1 client")
});

static HTTP2_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .user_agent(constants::USER_AGENT)
        .http2_prior_knowledge()
        // the following are disabled because we don't do enough HTTP/2 requests to justify it
        // .http2_keep_alive_interval(Duration::from_secs(5)) // sets an interval for HTTP2 Ping frames should be sent to keep a connection alive
        // .http2_keep_alive_timeout(Duration::from_secs(10)) // if the ping is not acknowledged within the timeout, the connection will be closed
        .gzip(true)
        .https_only(true)
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(10))
        // .connection_verbose(true) // useful for debugging
        .build()
        .expect("Failed to build HTTP2 client")
});
