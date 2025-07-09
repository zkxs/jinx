// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! Jinxxy API calls and response objects

mod dto;
mod error;

use super::HTTP1_CLIENT as HTTP_CLIENT;
use crate::bot::util;
pub use dto::{AuthUser, FullProduct, LicenseActivation, PartialProduct};
pub use error::{Error, Result};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{Response, header};
use serde::de::DeserializeOwned;
use std::fmt::{Display, Formatter};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tracing::{debug, warn};

/// prefix used in activation descriptions
const DISCORD_PREFIX: &str = "discord_";
const JINXXY_BASE_URL: &str = "https://api.creators.jinxxy.com/v1/";
const PAGINATION_LIMIT: usize = 100;
const PRODUCT_PAGINATION_LIMIT: usize = PAGINATION_LIMIT;
const ACTIVATION_PAGINATION_LIMIT: usize = PAGINATION_LIMIT;

/// Get extra headers needed for Jinxxy API calls
fn get_headers(api_key: &str) -> header::HeaderMap {
    let mut api_key = header::HeaderValue::try_from(api_key).expect("Failed to construct Jinxxy x-api-key header");
    api_key.set_sensitive(true);
    let mut header_map = header::HeaderMap::new();
    header_map.insert("x-api-key", api_key);
    header_map
}

/// Deserialize json after ensuring a 2xx status code was received. Not suitable for requests
/// where some status codes are expected.
async fn read_2xx_json<T>(endpoint: &'static str, response: Response) -> Result<T>
where
    T: DeserializeOwned,
{
    let result = handle_unexpected_status(endpoint, response).await?;
    read_any_json(endpoint, result).await
}

/// Deserialize json without checking status code.
async fn read_any_json<T>(endpoint: &'static str, response: Response) -> Result<T>
where
    T: DeserializeOwned,
{
    let bytes = response.bytes().await.map_err(|e| Error::from_read(endpoint, e))?;
    serde_json::from_slice::<T>(&bytes).map_err(Error::from_json)
}

/// Generic handler for any requests with non-successful status codes. Not suitable for requests
/// where some status codes are expected.
async fn handle_unexpected_status(endpoint: &'static str, response: Response) -> Result<Response> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(Error::from_response(endpoint, response).await)
    }
}

/// Get the user the API key belongs to
pub async fn get_own_user(api_key: &str) -> Result<AuthUser> {
    static ENDPOINT: &str = "GET /me";
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .get(format!("{JINXXY_BASE_URL}me"))
        .headers(get_headers(api_key))
        .send()
        .await
        .map_err(|e| Error::from_request(ENDPOINT, e))?;
    debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
    let response: AuthUser = read_2xx_json(ENDPOINT, response).await?;
    Ok(response)
}

/// Represents all allowed license formats
#[derive(Clone)]
pub enum LicenseKey<'a> {
    Id(&'a str),
    Short(&'a str),
    Long(&'a str),
}

/// Get the license id corresponding to a license key, or `None` if the license key is invalid.
///
/// Note that this function does **not** verify if a provided license ID is valid: it only converts
/// keys into IDs.
pub async fn get_license_id(api_key: &str, license: LicenseKey<'_>) -> Result<Option<String>> {
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
            static ENDPOINT: &str = "GET /licenses";
            let start_time = Instant::now();
            let response = HTTP_CLIENT
                .get(format!("{JINXXY_BASE_URL}licenses")) // this does NOT work with `limit` set.
                .headers(get_headers(api_key))
                .query(&[(search_key, license_key)])
                .send()
                .await
                .map_err(|e| Error::from_request(ENDPOINT, e))?;
            debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
            let response: dto::LicenseList = read_2xx_json(ENDPOINT, response).await?;
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
) -> Result<Option<LicenseInfo>> {
    check_license(api_key, LicenseKey::Id(license_id), inject_product_version_name).await
}

/// Get the license info corresponding to a license key, or `None` if the license key is invalid.
///
/// Note that this function **does** verify all provided licenses, whether it's an ID or a short/long key.
pub async fn check_license(
    api_key: &str,
    license: LicenseKey<'_>,
    inject_product_version_name: bool,
) -> Result<Option<LicenseInfo>> {
    match license {
        LicenseKey::Id(license_id) => {
            // look up license directly by ID
            static ENDPOINT: &str = "GET /licenses/<id>";
            let start_time = Instant::now();
            let response = HTTP_CLIENT
                .get(format!("{JINXXY_BASE_URL}licenses/{license_id}"))
                .headers(get_headers(api_key))
                .send()
                .await
                .map_err(|e| Error::from_request(ENDPOINT, e))?;
            debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
            if response.status().is_success() {
                let response: dto::License = read_any_json(ENDPOINT, response).await?;
                let mut response: LicenseInfo = response.into();
                if inject_product_version_name {
                    add_product_version_name_to_license_info(api_key, &mut response).await?;
                }
                Ok(Some(response))
            } else {
                debug!("could not look up user-provided license id");
                // jinxxy API really doesn't expect you to pass invalid license IDs, so we have to do some convoluted bullshit here to figure out what exactly went wrong
                let error = Error::from_response(ENDPOINT, response).await;
                if error.looks_like_403() || error.looks_like_404() {
                    Ok(None)
                } else {
                    Err(error)
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
            static ENDPOINT: &str = "GET /licenses";
            let start_time = Instant::now();
            let response = HTTP_CLIENT
                .get(format!("{JINXXY_BASE_URL}licenses"))
                .headers(get_headers(api_key))
                .query(&[(search_key, license_key)])
                .send()
                .await
                .map_err(|e| Error::from_request(ENDPOINT, e))?;
            debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
            let response: dto::LicenseList = read_2xx_json(ENDPOINT, response).await?;
            if let Some(result) = response.results.first() {
                // now look up the license directly by ID
                static ENDPOINT: &str = "GET /licenses/<id>";
                let start_time = Instant::now();
                let response = HTTP_CLIENT
                    .get(format!("{}licenses/{}", JINXXY_BASE_URL, result.id))
                    .headers(get_headers(api_key))
                    .send()
                    .await
                    .map_err(|e| Error::from_request(ENDPOINT, e))?;
                debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
                let response: dto::License = read_2xx_json(ENDPOINT, response).await?;
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
async fn add_product_version_name_to_license_info(api_key: &str, license_info: &mut LicenseInfo) -> Result<()> {
    if let Some(product_version_info) = &mut license_info.product_version_info {
        let product = get_product(api_key, &license_info.product_id).await?;
        if let Some(product_version_name) = product
            .versions
            .into_iter()
            .find(|found_product_version| found_product_version.id == product_version_info.product_version_id)
            .map(|found_product_version| found_product_version.name)
        {
            product_version_info.product_version_name = product_version_name;
        }
    }
    Ok(())
}

/// Get list of all license activations
pub async fn get_license_activations(api_key: &str, license_id: &str) -> Result<Vec<LicenseActivation>> {
    //TODO: build db cache into this using "Etag" header value into "If-None-Match" header value, and check for 304 Not Modified
    //TODO: ...actually... ugh this thing is a list. Is this thing cache-safe?
    //TODO: stop calling db from outside this function
    //TODO: `search_query` field "A search query to filter results"
    static ENDPOINT: &str = "GET /licenses/<id>/activations";
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .get(format!(
            "{JINXXY_BASE_URL}licenses/{license_id}/activations?limit={ACTIVATION_PAGINATION_LIMIT}"
        ))
        .headers(get_headers(api_key))
        .send()
        .await
        .map_err(|e| Error::from_request(ENDPOINT, e))?;
    debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
    let response: dto::LicenseActivationList = read_2xx_json(ENDPOINT, response).await?;
    if response.len() == ACTIVATION_PAGINATION_LIMIT {
        warn!("{ENDPOINT} returned exactly {ACTIVATION_PAGINATION_LIMIT} items, which is the pagination limit");
    }
    Ok(response.results)
}

/// Create a new license activation
pub async fn create_license_activation(api_key: &str, license_id: &str, user_id: u64) -> Result<String> {
    let body = dto::CreateLicenseActivation::from_user_id(user_id);
    static ENDPOINT: &str = "POST /licenses/<id>/activations";
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .post(format!("{JINXXY_BASE_URL}licenses/{license_id}/activations"))
        .headers(get_headers(api_key))
        .header(header::CONTENT_TYPE, "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| Error::from_request(ENDPOINT, e))?;
    debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
    let response: LicenseActivation = read_2xx_json(ENDPOINT, response).await?;
    Ok(response.id)
}

/// Delete a license activation. Returns `true` if the activation was deleted, or `false` if it was not found.
pub async fn delete_license_activation(api_key: &str, license_id: &str, activation_id: &str) -> Result<bool> {
    static ENDPOINT: &str = "DELETE /licenses/<id>/activations";
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .delete(format!(
            "{JINXXY_BASE_URL}licenses/{license_id}/activations/{activation_id}"
        ))
        .headers(get_headers(api_key))
        .send()
        .await
        .map_err(|e| Error::from_request(ENDPOINT, e))?;
    debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
    if response.status().is_success() {
        Ok(true)
    } else {
        debug!("could not delete license id \"{license_id}\" activation id \"{activation_id}\"");
        // jinxxy API has a bug where it doesn't delete license activations from the List or Retrieve APIs.
        let error = Error::from_response(ENDPOINT, response).await;
        if error.looks_like_404() {
            // license was not found
            Ok(false)
        } else {
            Err(error)
        }
    }
}

/// Look up a product. This includes product version information.
pub async fn get_product(api_key: &str, product_id: &str) -> Result<FullProduct> {
    static ENDPOINT: &str = "GET /products/<id>";
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .get(format!("{JINXXY_BASE_URL}products/{product_id}"))
        .headers(get_headers(api_key))
        .send()
        .await
        .map_err(|e| Error::from_request(ENDPOINT, e))?;
    debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
    let response: FullProduct = read_2xx_json(ENDPOINT, response).await?;
    Ok(response)
}

/// Get a single page of products.
async fn get_products_page(api_key: &str, page_number: u32) -> Result<dto::ProductList> {
    static ENDPOINT: &str = "GET /products";
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .get(format!(
            "{JINXXY_BASE_URL}products?limit={PRODUCT_PAGINATION_LIMIT}&page={page_number}"
        ))
        .headers(get_headers(api_key))
        .send()
        .await
        .map_err(|e| Error::from_request(ENDPOINT, e))?;
    debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
    let response: dto::ProductList = read_2xx_json(ENDPOINT, response).await?;
    Ok(response)
}

/// Get all products on this account by performing a paginated request. This does NOT include product version information.
///
/// You should not wrap this in retry logic, as the retry logic is already built in to each paged request, as should be
/// done for dependent serial results (e.g. if page 5 fails don't retry the entire paginated request: just retry page 5.)
pub async fn get_products(api_key: &str) -> Result<Vec<PartialProduct>> {
    let mut products = Vec::new();
    let mut page_number: u32 = 1;
    let mut should_be_empty = false;
    loop {
        let response = util::retry_thrice(|| get_products_page(api_key, page_number)).await?;
        let response = response.products();

        if response.is_empty() {
            // we are on the last page and should stop iterating
            if !should_be_empty {
                warn!(
                    "GET /products returned <{PRODUCT_PAGINATION_LIMIT} items on the previous page and >0 items on this page. This implies the data mutated during iteration."
                );
            }
            // `products` vec already contains everything we need to return
            break;
        } else {
            if response.len() < PRODUCT_PAGINATION_LIMIT {
                // if we got less than the limit, we expect the next page to be empty
                should_be_empty = true;
            }

            // add this page of products to the `products` vec we will eventually return
            if products.is_empty() {
                // don't do an allocation + copy for the first page: just steal the vec
                products = response;
            } else {
                products.extend(response);
            }
        }
        page_number += 1;
    }

    Ok(products)
}

/// Upgrade products from partial data to full data. This is expensive, as it has to call an API once per product.
/// This is done concurrently which speeds things up slightly, but it is still very costly.
/// Resulting vec is not guaranteed to be in the same order as the input vec.
pub async fn get_full_products<const PARALLEL: bool>(
    api_key: &str,
    partial_products: Vec<PartialProduct>,
) -> Result<Vec<FullProduct>> {
    let mut products = Vec::with_capacity(partial_products.len());
    if PARALLEL {
        let mut join_set = JoinSet::new();
        for partial_product in partial_products {
            let api_key = api_key.to_string();
            let product_id = partial_product.id;
            join_set.spawn(async move { util::retry_thrice(|| get_product(&api_key, &product_id)).await });
        }
        while let Some(full_product) = join_set.join_next().await {
            let full_product = full_product.map_err(Error::from_join)??;
            products.push(full_product);
        }
    } else {
        for partial_product in partial_products {
            let full_product = get_product(api_key, &partial_product.id).await?;
            products.push(full_product);
        }
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

    /// get a reference to the product version ID, if it exists
    pub fn version_id(&self) -> Option<&str> {
        self.product_version_info
            .as_ref()
            .map(|info| info.product_version_id.as_str())
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
        self.username()
            .map(|username| format!("https://jinxxy.com/{}", utf8_percent_encode(username, NON_ALPHANUMERIC)))
    }
}

pub trait GetProfileImageUrl {
    fn profile_image_url(&self) -> Option<&str>;
}
