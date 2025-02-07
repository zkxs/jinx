// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! Jinxxy API calls and response objects

mod dto;

use super::HTTP1_CLIENT as HTTP_CLIENT;
use crate::error::JinxError;
pub use dto::{AuthUser, FullProduct, LicenseActivation, PartialProduct};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use reqwest::{header, Response};
use std::fmt::{Display, Formatter};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tracing::debug;

type Error = Box<dyn std::error::Error + Send + Sync>;

/// prefix used in activation descriptions
const DISCORD_PREFIX: &str = "discord_";
const JINXXY_BASE_URL: &str = "https://api.creators.jinxxy.com/v1/";

/// Get extra headers needed for Jinxxy API calls
fn get_headers(api_key: &str) -> header::HeaderMap {
    let mut api_key = header::HeaderValue::try_from(api_key)
        .expect("Failed to construct Jinxxy x-api-key header");
    api_key.set_sensitive(true);
    let mut header_map = header::HeaderMap::new();
    header_map.insert("x-api-key", api_key);
    header_map
}

/// Generic handler for any requests with non-successful status codes. Not suitable for requests
/// where some status codes are expected.
async fn handle_error(endpoint: &'static str, response: Response) -> Result<Response, JinxError> {
    if response.status().is_success() {
        Ok(response)
    } else {
        let status_code = response.status().as_u16();
        let headers = format!("{:?}", response.headers());
        let bytes_result = response.bytes().await;
        let body: String = bytes_result
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();

        let message = format!(
            "{} returned status code {}. Headers={}; Body={}",
            endpoint, status_code, headers, body,
        );
        Err(JinxError::new(message))
    }
}

/// Get the user the API key belongs to
pub async fn get_own_user(api_key: &str) -> Result<AuthUser, Error> {
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .get(format!("{}me", JINXXY_BASE_URL))
        .headers(get_headers(api_key))
        .send()
        .await?;
    debug!("GET /me took {}ms", start_time.elapsed().as_millis());
    let response = handle_error("GET /me", response).await?;
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
pub async fn get_license_id(
    api_key: &str,
    license: LicenseKey<'_>,
) -> Result<Option<String>, Error> {
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
            let start_time = Instant::now();
            let response = HTTP_CLIENT
                .get(format!("{}licenses", JINXXY_BASE_URL))
                .headers(get_headers(api_key))
                .query(&[(search_key, license_key)])
                .send()
                .await?;
            debug!("GET /licenses took {}ms", start_time.elapsed().as_millis());
            let response = handle_error("GET /licenses", response).await?;
            let response: dto::LicenseList = response.json().await?;
            if let Some(result) = response.results.first() {
                Ok(Some(result.id.to_string()))
            } else {
                debug!("could not look up user-provided license key");
                Ok(None)
            }
        }
    }
}

/// Get the license info corresponding to a license ID, or `None` if the license ID is invalid.
///
/// Note that this function **does** verify provided license ID.
pub async fn check_license_id(
    api_key: &str,
    license_id: &str,
    inject_product_version_name: bool,
) -> Result<Option<LicenseInfo>, Error> {
    check_license(
        api_key,
        LicenseKey::Id(license_id),
        inject_product_version_name,
    )
    .await
}

/// Get the license info corresponding to a license key, or `None` if the license key is invalid.
///
/// Note that this function **does** verify all provided licenses, whether it's an ID or a short/long key.
pub async fn check_license(
    api_key: &str,
    license: LicenseKey<'_>,
    inject_product_version_name: bool,
) -> Result<Option<LicenseInfo>, Error> {
    match license {
        LicenseKey::Id(license_id) => {
            // look up license directly by ID
            let start_time = Instant::now();
            let response = HTTP_CLIENT
                .get(format!("{}licenses/{}", JINXXY_BASE_URL, license_id))
                .headers(get_headers(api_key))
                .send()
                .await?;
            debug!(
                "GET /licenses/<id> took {}ms",
                start_time.elapsed().as_millis()
            );
            if response.status().is_success() {
                let response: dto::License = response.json().await?;
                let mut response: LicenseInfo = response.into();
                if inject_product_version_name {
                    add_product_version_name_to_license_info(api_key, &mut response).await?;
                }
                Ok(Some(response))
            } else {
                debug!("could not look up user-provided license id");
                // jinxxy API really doesn't expect you to pass invalid license IDs, so we have to do some convoluted bullshit here to figure out what exactly went wrong
                let status_code = response.status();
                let response: dto::JinxxyError = response.json().await?;
                if response.looks_like_403() || response.looks_like_404() {
                    Ok(None)
                } else {
                    Err(JinxError::boxed(format!(
                        "GET /licenses/<id> returned status code {}",
                        status_code.as_u16()
                    )))
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
            let start_time = Instant::now();
            let response = HTTP_CLIENT
                .get(format!("{}licenses", JINXXY_BASE_URL))
                .headers(get_headers(api_key))
                .query(&[(search_key, license_key)])
                .send()
                .await?;
            debug!("GET /licenses took {}ms", start_time.elapsed().as_millis());
            let response = handle_error("GET /licenses", response).await?;
            let response: dto::LicenseList = response.json().await?;
            if let Some(result) = response.results.first() {
                // now look up the license directly by ID
                let start_time = Instant::now();
                let response = HTTP_CLIENT
                    .get(format!("{}licenses/{}", JINXXY_BASE_URL, result.id))
                    .headers(get_headers(api_key))
                    .send()
                    .await?;
                debug!(
                    "GET /licenses/<id> took {}ms",
                    start_time.elapsed().as_millis()
                );
                let response = handle_error("GET /licenses/<id>", response).await?;
                let response: dto::License = response.json().await?;
                let mut response: LicenseInfo = response.into();
                if inject_product_version_name {
                    add_product_version_name_to_license_info(api_key, &mut response).await?;
                }
                Ok(Some(response))
            } else {
                debug!("could not look up user-provided license key");
                Ok(None)
            }
        }
    }
}

/// This performs an API call to get_product
async fn add_product_version_name_to_license_info(
    api_key: &str,
    license_info: &mut LicenseInfo,
) -> Result<(), Error> {
    if let Some(product_version_info) = &mut license_info.product_version_info {
        let product = get_product(api_key, &license_info.product_id).await?;
        if let Some(product_version_name) = product
            .versions
            .into_iter()
            .find(|found_product_version| {
                found_product_version.id == product_version_info.product_version_id
            })
            .map(|found_product_version| found_product_version.name)
        {
            product_version_info.product_version_name = product_version_name;
        }
    }
    Ok(())
}

/// Get list of all license activations
pub async fn get_license_activations(
    api_key: &str,
    license_id: &str,
) -> Result<Vec<LicenseActivation>, Error> {
    //TODO: build db cache into this using "Etag" header value into "If-None-Match" header value, and check for 304 Not Modified
    //TODO: ...actually... ugh this thing is a list. Is this thing cache-safe?
    //TODO: stop calling db from outside this function
    //TODO: `search_query` field "A search query to filter results"
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .get(format!(
            "{}licenses/{}/activations",
            JINXXY_BASE_URL, license_id
        ))
        .headers(get_headers(api_key))
        .send()
        .await?;
    debug!(
        "GET /licenses/<id>/activations took {}ms",
        start_time.elapsed().as_millis()
    );
    let response = handle_error("GET /licenses/<id>/activations", response).await?;

    let response: dto::LicenseActivationList = response.json().await?;
    Ok(response.results)
}

/// Create a new license activation
pub async fn create_license_activation(
    api_key: &str,
    license_id: &str,
    user_id: u64,
) -> Result<String, Error> {
    let body = dto::CreateLicenseActivation::from_user_id(user_id);
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .post(format!(
            "{}licenses/{}/activations",
            JINXXY_BASE_URL, license_id
        ))
        .headers(get_headers(api_key))
        .header(header::CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await?;
    debug!(
        "POST /licenses/<id>/activations took {}ms",
        start_time.elapsed().as_millis()
    );
    let response = handle_error("POST /licenses/<id>/activations", response).await?;
    let response: LicenseActivation = response.json().await?;
    Ok(response.id)
}

/// Delete a license activation. Returns `true` if the activation was deleted, or `false` if it was not found.
pub async fn delete_license_activation(
    api_key: &str,
    license_id: &str,
    activation_id: &str,
) -> Result<bool, Error> {
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .delete(format!(
            "{}licenses/{}/activations/{}",
            JINXXY_BASE_URL, license_id, activation_id
        ))
        .headers(get_headers(api_key))
        .send()
        .await?;
    debug!(
        "DELETE /licenses/<id>/activations took {}ms",
        start_time.elapsed().as_millis()
    );
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
            Err(JinxError::boxed(format!(
                "DELETE /licenses/<id>/activations/<id> returned status code {}",
                status_code.as_u16()
            )))
        }
    }
}

/// Look up a product. This includes product version information.
pub async fn get_product(api_key: &str, product_id: &str) -> Result<FullProduct, Error> {
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .get(format!("{}products/{}", JINXXY_BASE_URL, product_id))
        .headers(get_headers(api_key))
        .send()
        .await?;
    debug!(
        "GET /products/<id> took {}ms",
        start_time.elapsed().as_millis()
    );
    let response = handle_error("GET /products/<id>", response).await?;

    let response: FullProduct = response.json().await?;
    Ok(response)
}

/// Get all products on this account. This does NOT include product version information.
pub async fn get_products(api_key: &str) -> Result<Vec<PartialProduct>, Error> {
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .get(format!("{}products", JINXXY_BASE_URL))
        .headers(get_headers(api_key))
        .send()
        .await?;
    debug!("GET /products took {}ms", start_time.elapsed().as_millis());
    let response = handle_error("GET /products", response).await?;

    let response: dto::ProductList = response.json().await?;
    Ok(response.into())
}

/// Upgrade products from partial data to full data. This is expensive, as it has to call an API once per product.
/// This is done concurrently which speeds things up slightly, but it is still very costly.
/// Resulting vec is not guaranteed to be in the same order as the input vec.
pub async fn get_full_products(
    api_key: &str,
    partial_products: Vec<PartialProduct>,
) -> Result<Vec<FullProduct>, Error> {
    let mut products = Vec::with_capacity(partial_products.len());
    let mut join_set = JoinSet::new();
    for partial_product in partial_products {
        let api_key = api_key.to_string();
        let product_id = partial_product.id;
        join_set.spawn(async move { get_product(&api_key, &product_id).await });
    }
    while let Some(full_product) = join_set.join_next().await {
        products.push(full_product??);
    }
    Ok(products)
}

/// Not part of the Jinxxy API: this is an internal DTO that is only used for `/create_post`
pub struct DisplayUser {
    /// Custom display name, or username if no display name is set.
    pub display_name: String,
    profile_image_url: Option<String>,
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

impl GetProfileImageUrl for DisplayUser {
    fn profile_image_url(&self) -> Option<&str> {
        self.profile_image_url.as_deref()
    }
}

/// Not part of the Jinxxy API: this is an internal DTO
pub struct LicenseInfo {
    pub license_id: String,
    /// short key
    pub short_key: String,
    /// long key
    pub key: String,
    pub user_id: String,
    /// Account's username; used in profile URL
    pub username: Option<String>,
    pub product_id: String,
    pub product_name: String,
    pub product_version_info: Option<ProductVersionInfo>,
    pub activations: u32,
}

pub struct ProductVersionInfo {
    pub product_version_id: String,
    pub product_version_name: String,
}

impl LicenseInfo {
    /// create a new ProductVersionId by cloning fields
    pub fn new_product_version_id(&self) -> ProductVersionId {
        ProductVersionId {
            product_id: self.product_id.clone(),
            product_version_id: self
                .product_version_info
                .as_ref()
                .map(|info| info.product_version_id.clone()),
        }
    }
}

/// Not part of the Jinxxy API: this is an internal DTO
#[derive(Eq, PartialEq, Hash, Debug, Clone)]
pub struct ProductVersionId {
    pub product_id: String,
    pub product_version_id: Option<String>,
}

impl ProductVersionId {
    pub fn from_product_id(product_id: impl Into<String>) -> ProductVersionId {
        Self {
            product_id: product_id.into(),
            product_version_id: None,
        }
    }
}

impl Display for ProductVersionId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(product_version_id) = &self.product_version_id {
            write!(f, "{}.{}", self.product_id, product_version_id)
        } else {
            write!(f, "{}.null", self.product_id)
        }
    }
}

/// Internal struct for holding name info
#[derive(Clone)]
pub struct ProductNameInfo {
    pub id: String,
    pub product_name: String,
}

/// Internal struct for holding version name info
#[derive(Clone)]
pub struct ProductVersionNameInfo {
    pub id: ProductVersionId,
    pub product_version_name: String,
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
        self.username().map(|username| {
            format!(
                "https://jinxxy.com/{}",
                utf8_percent_encode(username, NON_ALPHANUMERIC)
            )
        })
    }
}

pub trait GetProfileImageUrl {
    fn profile_image_url(&self) -> Option<&str>;
}
