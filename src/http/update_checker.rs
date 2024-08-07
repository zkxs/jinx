// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! GitHub Releases-based update checking

use super::HTTP2_CLIENT as HTTP_CLIENT;
use crate::error::JinxError;
use reqwest::header;
use semver::Version;
use serde::Deserialize;
use std::cmp::Ordering;
use std::fmt::{Display, Formatter};
use tracing::{debug, warn};

type Error = Box<dyn std::error::Error + Send + Sync>;

const UPDATE_CHECK_URI: &str = "https://api.github.com/repos/zkxs/jinx/releases/latest";

thread_local! {
    static LOCAL_VERSION: Result<Version, JinxError> = Version::parse(env!("CARGO_PKG_VERSION"))
        .map_err(|e| {
            let message = format!("Local version \"{}\" didn't follow semver! {}", env!("CARGO_PKG_VERSION"), e);
            warn!(message); // obnoxiously, this gets logged once per thread. Oh well.
            JinxError::new(message)
        });
}

/// Compare the local version to the latest GitHub release. If there's a newer version available, return its URL.
pub async fn check_for_update() -> VersionCheck {
    match get_latest_release().await {
        Ok(response) => {
            debug!("Update Url: {:?}", response.url);
            debug!("Update Version: {:?}", response.version);
            match Version::parse(&response.version) {
                Ok(remote_version) => {
                    LOCAL_VERSION.with(|local_version| {
                        match local_version {
                            Ok(local_version) => {
                                match remote_version.cmp(local_version) {
                                    Ordering::Greater => {
                                        // we are behind
                                        warn!("Local version is outdated.");
                                        VersionCheck::Outdated(response)
                                    }
                                    Ordering::Less => {
                                        // we are NEWER than remote
                                        warn!("Local version is NEWER than remote version! If you're not beta testing a pre-release then something is wrong.");
                                        VersionCheck::Future(response)
                                    }
                                    Ordering::Equal => {
                                        // we are up-to-date
                                        VersionCheck::Current // even though the response is in-scope we don't need it
                                    }
                                }
                            }
                            Err(_) => {
                                // could not get local version (this gets logged in the thread_local)
                                VersionCheck::BadLocal(response)
                            }
                        }
                    })
                }
                Err(e) => {
                    warn!("Error parsing remote version: {e:?}");
                    VersionCheck::BadRemote(response)
                }
            }
        }
        Err(e) => {
            warn!("Failed to get latest version info: {e:?}");
            VersionCheck::UnknownRemote
        }
    }
}

/// Get latest release from GitHub
async fn get_latest_release() -> Result<RemoteVersion, Error> {
    let request = HTTP_CLIENT.get(UPDATE_CHECK_URI)
        .header(header::ACCEPT, "application/json")
        .build()
        .map_err(|e| format!("Failed to build update check HTTP request: {e}"))?;

    let response = HTTP_CLIENT.execute(request).await
        .map_err(|e| format!("Update check failed: {e}"))?;

    //TODO: implement etag-based caching

    let status = response.status();
    let result = response.json::<RemoteVersion>().await
        .map_err(|e| JinxError::new(format!("error parsing github release {} response: {}", status.as_str(), e)))?;
    Ok(result)
}


/// Status of our code version
pub enum VersionCheck {
    Outdated(RemoteVersion),
    Current,
    Future(RemoteVersion),
    BadLocal(RemoteVersion),
    BadRemote(RemoteVersion),
    UnknownRemote,
}

impl VersionCheck {
    /// should this version check result be warned about?
    pub fn is_warn(&self) -> bool {
        matches!(self, VersionCheck::Outdated(_))
    }

    /// should this version check result be errored about?
    pub fn is_error(&self) -> bool {
        matches!(self, VersionCheck::Future(_) | VersionCheck::UnknownRemote | VersionCheck::BadLocal(_) | VersionCheck::BadRemote(_))
    }
}

impl Display for VersionCheck {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            VersionCheck::Outdated(remote_version) => write!(f, "{} is outdated! The bot owner should upgrade to {}.", env!("CARGO_PKG_NAME"), remote_version),
            VersionCheck::Current => write!(f, "{} is up-to-date!", env!("CARGO_PKG_NAME")),
            VersionCheck::Future(remote_version) => write!(f, "{} is somehow ahead of the version on GitHub ({}). If you're not beta testing a pre-release then something is wrong.", env!("CARGO_PKG_NAME"), remote_version),
            VersionCheck::BadLocal(remote_version) => write!(f, "{} local version is not semver-compatible, so could not be compared to the version on GitHub ({}).", env!("CARGO_PKG_NAME"), remote_version),
            VersionCheck::BadRemote(remote_version) => write!(f, "{} version on GitHub ({}) is not semver-compatible, so could not be compared to the currently running version.", env!("CARGO_PKG_NAME"), remote_version),
            VersionCheck::UnknownRemote => write!(f, "Unable to retrieve version from GitHub. This means we cannot determine if {} is outdated.", env!("CARGO_PKG_NAME")),
        }
    }
}

/// Version information for a GitHub release.
///
/// This also happens to be the GitHub API response object.
#[derive(Deserialize)]
pub struct RemoteVersion {
    #[serde(rename = "html_url")]
    pub url: String,
    #[serde(rename = "tag_name")]
    pub version: String,
}

impl Display for RemoteVersion {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}](<{}>)", self.version, self.url)
    }
}
