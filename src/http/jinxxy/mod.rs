// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! Jinxxy API calls and response objects

mod dto;

use super::HTTP1_CLIENT as HTTP_CLIENT;
use crate::error::JinxError;
pub use dto::{AuthUser, FullProduct, LicenseActivation, PartialProduct};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::header;
use tracing::debug;

type Error = Box<dyn std::error::Error + Send + Sync>;

const JINXXY_BASE_URL: &str = "https://api.creators.jinxxy.com/v1/";


/// Get extra headers needed for Jinxxy API calls
fn get_headers(api_key: &str) -> header::HeaderMap {
    let mut api_key = header::HeaderValue::try_from(api_key).unwrap();
    api_key.set_sensitive(true);
    let mut header_map = header::HeaderMap::new();
    header_map.insert("x-api-key", api_key);
    header_map
}

/// Get the user the API key belongs to
pub async fn get_own_user(api_key: &str) -> Result<AuthUser, Error> {
    let response = HTTP_CLIENT.get(format!("{}me", JINXXY_BASE_URL))
        .headers(get_headers(api_key))
        .send()
        .await?;
    if !response.status().is_success() {
        JinxError::fail(format!("/me returned status code {}", response.status().as_u16()))?;
        unreachable!()
    }
    let response: AuthUser = response.json().await?;
    Ok(response)
}

/// Represents all allowed license formats
pub enum LicenseKey<'a> {
    Id(&'a str),
    Short(&'a str),
    Long(&'a str),
}

/// Get the license id corresponding to a license key, or `None` if the license key is invalid.
///
/// Note that this function does **not** verify if a provided license ID is valid: it only converts
/// keys into IDs.
pub async fn get_license_id(api_key: &str, license: LicenseKey<'_>) -> Result<Option<String>, Error> {
    match license {
        LicenseKey::Id(license_id) => {
            // maybe one day I'll need to verify these, but not today
            Ok(Some(license_id.to_string()))
        }
        LicenseKey::Short(license_key) | LicenseKey::Long(license_key) => {
            // first, search for the license key
            let search_key = if matches!(license, LicenseKey::Short(_)) {
                "short_key"
            } else {
                "key"
            };
            let response = HTTP_CLIENT.get(format!("{}licenses", JINXXY_BASE_URL))
                .headers(get_headers(api_key))
                .query(&[(search_key, license_key)])
                .send()
                .await?;
            if !response.status().is_success() {
                JinxError::fail(format!("/licenses returned status code {}", response.status().as_u16()))?;
                unreachable!()
            }
            let response: dto::LicenseList = response.json().await?;
            if let Some(result) = response.results.first() {
                Ok(Some(result.id.to_string()))
            } else {
                debug!("could not look up user-provided license key \"{license_key}\"");
                Ok(None)
            }
        }
    }
}


/// Get the license info corresponding to a license ID, or `None` if the license ID is invalid.
///
/// Note that this function **does** verify provided license ID.
pub async fn check_license_id(api_key: &str, license_id: &str) -> Result<Option<LicenseInfo>, Error> {
    check_license(api_key, LicenseKey::Id(license_id)).await
}

/// Get the license info corresponding to a license key, or `None` if the license key is invalid.
///
/// Note that this function **does** verify all provided licenses, whether it's an ID or a short/long key.
pub async fn check_license(api_key: &str, license: LicenseKey<'_>) -> Result<Option<LicenseInfo>, Error> {
    match license {
        LicenseKey::Id(license_id) => {
            // look up license directly by ID
            let response = HTTP_CLIENT.get(format!("{}licenses/{}", JINXXY_BASE_URL, license_id))
                .headers(get_headers(api_key))
                .send()
                .await?;
            if response.status().is_success() {
                let response: dto::License = response.json().await?;
                Ok(Some(response.into()))
            } else {
                debug!("could not look up user-provided license id \"{license_id}\"");
                // jinxxy API really doesn't expect you to pass invalid license IDs, so we have to do some convoluted bullshit here to figure out what exactly went wrong
                let status_code = response.status();
                let response: dto::JinxxyError = response.json().await?;
                if response.looks_like_403() || response.looks_like_404() {
                    Ok(None)
                } else {
                    Err(JinxError::boxed(format!("/licenses/<id> returned status code {}", status_code.as_u16())))
                }
            }
        }
        LicenseKey::Short(license_key) | LicenseKey::Long(license_key) => {
            // first, search for the license key
            let search_key = if matches!(license, LicenseKey::Short(_)) {
                "short_key"
            } else {
                "key"
            };
            let response = HTTP_CLIENT.get(format!("{}licenses", JINXXY_BASE_URL))
                .headers(get_headers(api_key))
                .query(&[(search_key, license_key)])
                .send()
                .await?;
            if !response.status().is_success() {
                JinxError::fail(format!("/licenses returned status code {}", response.status().as_u16()))?;
                unreachable!()
            }
            let response: dto::LicenseList = response.json().await?;
            if let Some(result) = response.results.first() {
                // now look up the license directly by ID
                let response = HTTP_CLIENT.get(format!("{}licenses/{}", JINXXY_BASE_URL, result.id))
                    .headers(get_headers(api_key))
                    .send()
                    .await?;
                if !response.status().is_success() {
                    JinxError::fail(format!("/licenses/<id> returned status code {}", response.status().as_u16()))?;
                    unreachable!()
                }
                let response: dto::License = response.json().await?;
                Ok(Some(response.into()))
            } else {
                debug!("could not look up user-provided license key \"{license_key}\"");
                Ok(None)
            }
        }
    }
}

/// Get list of all license activations
pub async fn get_license_activations(api_key: &str, license_id: &str) -> Result<Vec<LicenseActivation>, Error> {
    //TODO: build db cache into this using "Etag" header value into "If-None-Match" header value, and check for 304 Not Modified
    //TODO: ...actually... ugh this thing is a list. Is this thing cache-safe?
    //TODO: stop calling db from outside this function
    //TODO: `search_query` field "A search query to filter results"
    let response = HTTP_CLIENT.get(format!("{}licenses/{}/activations", JINXXY_BASE_URL, license_id))
        .headers(get_headers(api_key))
        .send()
        .await?;
    if !response.status().is_success() {
        JinxError::fail(format!("/licenses/<id>/activations returned status code {}", response.status().as_u16()))?;
        unreachable!()
    }

    let response: dto::LicenseActivationList = response.json().await?;
    Ok(response.results)
}

/// Create a new license activation
pub async fn create_license_activation(api_key: &str, license_id: &str, user_id: u64) -> Result<String, Error> {
    let body = dto::CreateLicenseActivation::from_user_id(user_id);
    let response = HTTP_CLIENT.post(format!("{}licenses/{}/activations", JINXXY_BASE_URL, license_id))
        .headers(get_headers(api_key))
        .header(header::CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await?;
    if !response.status().is_success() {
        JinxError::fail(format!("POST /licenses/<id>/activations returned status code {}", response.status().as_u16()))?;
        unreachable!()
    }
    let response: LicenseActivation = response.json().await?;
    Ok(response.id)
}

/// Delete a license activation. Returns `true` if the activation was deleted, or `false` if it was not found.
pub async fn delete_license_activation(api_key: &str, license_id: &str, activation_id: &str) -> Result<bool, Error> {
    let response = HTTP_CLIENT.delete(format!("{}licenses/{}/activations/{}", JINXXY_BASE_URL, license_id, activation_id))
        .headers(get_headers(api_key))
        .send()
        .await?;
    if response.status().is_success() {
        Ok(true)
    } else {
        debug!("could not delete license id \"{license_id}\" activation id \"{activation_id}\"");
        // jinxxy API has a bug where it doesn't delete license activations from the List or Retrieve APIs.
        let status_code = response.status();
        let response: dto::JinxxyError = response.json().await?;
        if response.looks_like_404() {
            // license was not found
            Ok(false)
        } else {
            Err(JinxError::boxed(format!("DELETE /licenses/<id>/activations/<id> returned status code {}", status_code.as_u16())))
        }
    }
}

/// Look up a product
pub async fn get_product(api_key: &str, product_id: &str) -> Result<FullProduct, Error> {
    //TODO: add disk cache for this
    let response = HTTP_CLIENT.get(format!("{}products/{}", JINXXY_BASE_URL, product_id))
        .headers(get_headers(api_key))
        .send()
        .await?;
    if !response.status().is_success() {
        JinxError::fail(format!("/products/<id> returned status code {}", response.status().as_u16()))?;
        unreachable!()
    }

    let response: FullProduct = response.json().await?;
    Ok(response)
}

/// Get all products on this account
pub async fn get_products(api_key: &str) -> Result<Vec<PartialProduct>, Error> {
    //TODO: add disk cache for this (see above issue with list caching)
    let response = HTTP_CLIENT.get(format!("{}products", JINXXY_BASE_URL))
        .headers(get_headers(api_key))
        .send()
        .await?;
    if !response.status().is_success() {
        JinxError::fail(format!("/products returned status code {}", response.status().as_u16()))?;
        unreachable!()
    }

    let response: dto::ProductList = response.json().await?;
    Ok(response.into())
}

/// Not part of the Jinxxy API: this is an internal DTO that is only used for `/create_post`
pub struct DisplayUser {
    /// Custom display name, or username if no display name is set.
    pub display_name: String,
    pub profile_image_url: Option<String>,
}

impl DisplayUser {
    /// Get possessive form of this user's display name
    pub fn name_possessive(&self) -> String {
        if self.display_name.ends_with('s') {
            format!("{}'", self.display_name)
        } else {
            format!("{}'s", self.display_name)
        }
    }
}

/// Not part of the Jinxxy API: this is an internal DTO
pub struct LicenseInfo {
    pub license_id: String,
    pub short_key: String,
    pub user_id: String,
    /// Account's username; used in profile URL
    pub username: Option<String>,
    pub product_id: String,
    pub product_name: String,
    pub product_version_id: Option<String>,
    pub activations: u32,
}

trait GetUsername {
    fn username(&self) -> Option<&str>;
}

impl GetUsername for LicenseInfo {
    fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }
}

pub trait GetProfileUrl {
    fn profile_url(&self) -> Option<String>;
}

impl<T: GetUsername> GetProfileUrl for T {
    fn profile_url(&self) -> Option<String> {
        self.username().map(|username| format!("https://jinxxy.com/{}", utf8_percent_encode(username, NON_ALPHANUMERIC)))
    }
}
