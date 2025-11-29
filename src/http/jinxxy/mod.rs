// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! Jinxxy API calls and response objects

mod dto;
mod error;

use super::HTTP1_CLIENT as HTTP_CLIENT;
use crate::bot::util;
use crate::db::JinxDb;
use crate::error::JinxError;
pub use dto::{AuthUser, FullProduct, LicenseActivation, PartialProduct};
pub use error::{JinxxyError, JinxxyResult};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use poise::serenity_prelude as serenity;
use reqwest::{Response, header};
use serde::de::DeserializeOwned;
use serenity::GuildId;
use std::collections::HashMap;
use std::fmt::{Display, Formatter};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tracing::{debug, error, warn};

/// prefix used in activation descriptions
const DISCORD_PREFIX: &str = "discord_";
const JINXXY_BASE_URL: &str = "https://api.creators.jinxxy.com/v1/";
/// Jinxxy API has a hard limit of 100 for page size beyond which it denies requests
const PAGINATION_LIMIT: usize = 100;
const PRODUCT_PAGINATION_LIMIT: usize = PAGINATION_LIMIT;
const ACTIVATION_PAGINATION_LIMIT: usize = PAGINATION_LIMIT;

/// Get extra headers needed for Jinxxy API calls
fn get_headers(api_key: &str) -> header::HeaderMap {
    let mut header_map = header::HeaderMap::with_capacity(3);
    let mut api_key = header::HeaderValue::try_from(api_key).expect("Failed to construct Jinxxy x-api-key header");
    api_key.set_sensitive(true);

    // required for Jinxxy API to work
    header_map.insert("x-api-key", api_key);

    // as far as I can tell completely ignored by the API, but still good practice to set
    header_map.insert(header::ACCEPT, header::HeaderValue::from_static("application/json"));

    // "no-cache" is SUPPOSED to instruct the server to validate the cache, which does not actually exclude it from
    // caching, and certainly should not break ETag/If-None-Match behavior. But it does for the Jinxxy server impl.
    // max-age=0 is essentially the same meaning, but works with the Jinxxy API even with ETag/If-None-Match.
    header_map.insert(header::CACHE_CONTROL, header::HeaderValue::from_static("max-age=0"));

    header_map
}

/// Deserialize json after ensuring a 2xx status code was received. Not suitable for requests
/// where some status codes are expected.
async fn read_2xx_json<T>(endpoint: &'static str, response: Response) -> JinxxyResult<T>
where
    T: DeserializeOwned,
{
    let result = handle_unexpected_status(endpoint, response).await?;
    read_any_json(endpoint, result).await
}

/// Deserialize json without checking status code.
async fn read_any_json<T>(endpoint: &'static str, response: Response) -> JinxxyResult<T>
where
    T: DeserializeOwned,
{
    let bytes = response
        .bytes()
        .await
        .map_err(|e| JinxxyError::from_read(endpoint, e))?;
    serde_json::from_slice::<T>(&bytes).map_err(JinxxyError::from_json)
}

/// Generic handler for any requests with non-successful status codes. Not suitable for requests
/// where some status codes are expected.
async fn handle_unexpected_status(endpoint: &'static str, response: Response) -> JinxxyResult<Response> {
    if response.status().is_success() {
        Ok(response)
    } else {
        Err(JinxxyError::from_response(endpoint, response).await)
    }
}

/// Get the user the API key belongs to
pub async fn get_own_user(api_key: &str) -> JinxxyResult<AuthUser> {
    static ENDPOINT: &str = "GET /me";
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .get(format!("{JINXXY_BASE_URL}me"))
        .headers(get_headers(api_key))
        .send()
        .await
        .map_err(|e| JinxxyError::from_request(ENDPOINT, e))?;
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
pub async fn get_license_id(api_key: &str, license: LicenseKey<'_>) -> JinxxyResult<Option<String>> {
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
                .map_err(|e| JinxxyError::from_request(ENDPOINT, e))?;
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
) -> JinxxyResult<Option<LicenseInfo>> {
    check_license(api_key, LicenseKey::Id(license_id), inject_product_version_name).await
}

/// Get the license info corresponding to a license key, or `None` if the license key is invalid.
///
/// Note that this function **does** verify all provided licenses, whether it's an ID or a short/long key.
pub async fn check_license(
    api_key: &str,
    license: LicenseKey<'_>,
    inject_product_version_name: bool,
) -> JinxxyResult<Option<LicenseInfo>> {
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
                .map_err(|e| JinxxyError::from_request(ENDPOINT, e))?;
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
                let error = JinxxyError::from_response(ENDPOINT, response).await;
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
                .map_err(|e| JinxxyError::from_request(ENDPOINT, e))?;
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
                    .map_err(|e| JinxxyError::from_request(ENDPOINT, e))?;
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
async fn add_product_version_name_to_license_info(api_key: &str, license_info: &mut LicenseInfo) -> JinxxyResult<()> {
    if let Some(product_version_info) = &mut license_info.product_version_info {
        let product = get_product_uncached(api_key, &license_info.product_id).await?;
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
pub async fn get_license_activations(api_key: &str, license_id: &str) -> JinxxyResult<Vec<LicenseActivation>> {
    static ENDPOINT: &str = "GET /licenses/<id>/activations";
    let start_time = Instant::now();
    let url = format!("{JINXXY_BASE_URL}licenses/{license_id}/activations?limit={ACTIVATION_PAGINATION_LIMIT}");
    let response = HTTP_CLIENT
        .get(&url)
        .headers(get_headers(api_key))
        .send()
        .await
        .map_err(|e| JinxxyError::from_request(ENDPOINT, e))?;
    debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
    let response: dto::LicenseActivationList = read_2xx_json(ENDPOINT, response).await?;
    if response.len() == ACTIVATION_PAGINATION_LIMIT {
        // if we hit the activation pagination limit we cannot safely continue, as it's possible all Jinx activations
        // occur after page 1, and would therefore be missed.
        let nonce: u64 = util::generate_nonce();
        warn!(
            "NONCE[{nonce}] {url} returned exactly {ACTIVATION_PAGINATION_LIMIT} items, which is the pagination limit"
        );
        Err(JinxxyError::UnsupportedPagination(nonce))
    } else {
        Ok(response.results)
    }
}

/// Create a new license activation
pub async fn create_license_activation(api_key: &str, license_id: &str, user_id: u64) -> JinxxyResult<String> {
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
        .map_err(|e| JinxxyError::from_request(ENDPOINT, e))?;
    debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
    let response: LicenseActivation = read_2xx_json(ENDPOINT, response).await?;
    Ok(response.id)
}

/// Delete a license activation. Returns `true` if the activation was deleted, or `false` if it was not found.
pub async fn delete_license_activation(api_key: &str, license_id: &str, activation_id: &str) -> JinxxyResult<bool> {
    static ENDPOINT: &str = "DELETE /licenses/<id>/activations";
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .delete(format!(
            "{JINXXY_BASE_URL}licenses/{license_id}/activations/{activation_id}"
        ))
        .headers(get_headers(api_key))
        .send()
        .await
        .map_err(|e| JinxxyError::from_request(ENDPOINT, e))?;
    debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
    if response.status().is_success() {
        Ok(true)
    } else {
        debug!("could not delete license id \"{license_id}\" activation id \"{activation_id}\"");
        // jinxxy API has a bug where it doesn't delete license activations from the List or Retrieve APIs.
        let error = JinxxyError::from_response(ENDPOINT, response).await;
        if error.looks_like_404() {
            // license was not found
            Ok(false)
        } else {
            Err(error)
        }
    }
}
/// Look up a product. This includes product version information.
///
/// This completely ignores the cache and is useful if we need guaranteed correct information NOW.
pub async fn get_product_uncached(api_key: &str, product_id: &str) -> JinxxyResult<FullProduct> {
    get_product_cached(api_key, product_id, None)
        .await
        .map(|option| option.expect("cannot get a 304 response because etag wasn't used"))
}

/// Look up a product. This includes product version information.
///
/// Optionally, you can specify an etag. If the etag matches the resource in the server and we get a
/// 304 Not Modified response, `Ok(None)` will be returned. This is the ONLY case where `Ok(None)` has to be handled.
///
/// Note that this function is only _helpful_ for getting products in a cached way: it does not actually handle the
/// fallback read to cached values!
pub async fn get_product_cached(
    api_key: &str,
    product_id: &str,
    expected_etag: Option<&[u8]>,
) -> JinxxyResult<Option<FullProduct>> {
    static ENDPOINT: &str = "GET /products/<id>";
    let request = HTTP_CLIENT
        .get(format!("{JINXXY_BASE_URL}products/{product_id}"))
        .headers(get_headers(api_key));
    let request = if let Some(expected_etag) = expected_etag {
        request.header(header::IF_NONE_MATCH, expected_etag)
    } else {
        request
    };

    let start_time = Instant::now();
    let response = request
        .send()
        .await
        .map_err(|e| JinxxyError::from_request(ENDPOINT, e))?;
    let elapsed = start_time.elapsed();

    let actual_etag = response
        .headers()
        .get(header::ETAG)
        .map(|actual_etag| actual_etag.as_bytes().to_owned());
    let status_code = response.status();
    if expected_etag.is_some() && status_code.as_u16() == 304 {
        // 304 not modified!
        debug!("{} took {}ms (cached)", ENDPOINT, elapsed.as_millis());
        Ok(None)
    } else {
        // cached read failed
        debug!("{} took {}ms", ENDPOINT, elapsed.as_millis());
        let mut response: FullProduct = read_2xx_json(ENDPOINT, response).await?;
        response.etag = actual_etag;
        Ok(Some(response))
    }
}

/// Get a single page of products.
async fn get_products_page(api_key: &str, page_number: u32) -> JinxxyResult<dto::ProductList> {
    static ENDPOINT: &str = "GET /products";
    let start_time = Instant::now();
    let response = HTTP_CLIENT
        .get(format!(
            "{JINXXY_BASE_URL}products?limit={PRODUCT_PAGINATION_LIMIT}&page={page_number}"
        ))
        .headers(get_headers(api_key))
        .send()
        .await
        .map_err(|e| JinxxyError::from_request(ENDPOINT, e))?;
    debug!("{} took {}ms", ENDPOINT, start_time.elapsed().as_millis());
    let response: dto::ProductList = read_2xx_json(ENDPOINT, response).await?;
    Ok(response)
}

/// Get all products on this account by performing a paginated request. This does NOT include product version information.
///
/// You should not wrap this in retry logic, as the retry logic is already built in to each internal request.
pub async fn get_products(api_key: &str) -> JinxxyResult<Vec<PartialProduct>> {
    const HARD_PAGE_LIMIT: u32 = 100;
    let mut products = Vec::new();
    let mut page_number: u32 = 1;
    loop {
        let response = util::retry_thrice(|| get_products_page(api_key, page_number)).await?;
        let response = response.products();

        if response.is_empty() {
            // we are past the last page and should stop iterating
            // `products` vec already contains everything we need to return
            break;
        } else {
            let response_len = response.len();

            // add this page of products to the `products` vec we will eventually return
            if products.is_empty() {
                // don't do an allocation + copy for the first page: just steal the vec
                products = response;
            } else {
                products.extend(response);
            }

            if response_len < PRODUCT_PAGINATION_LIMIT {
                // we got less than the limit, which implies this is the last page
                break;
            }

            if page_number >= HARD_PAGE_LIMIT {
                // if we pass some hard page limit just stop because something is probably wrong
                error!("passed hard products page limit! Aborting pagination.");
                break;
            }
        }
        page_number += 1;
    }

    Ok(products)
}

enum ParallelFullProductResult {
    FullProduct(FullProduct),
    NotModified { product_id: String },
}

/// Either an API full product or cached data from DB we've determined is non-stale
pub enum LoadedProduct {
    Api(FullProduct),
    Cached {
        /// this is ONLY in an `Option` so you can steal the value later. It is guaranteed to be set!
        product_info: Option<ProductNameInfo>,
        versions: Vec<ProductVersionNameInfo>,
    },
}

/// Upgrade products from partial data to full data. This is expensive, as it has to call an API once per product.
/// This is done concurrently which speeds things up slightly, but it is still very costly.
/// Resulting vec is not guaranteed to be in the same order as the input vec.
///
/// You should not wrap this in retry logic, as the retry logic is already built in to each internal subrequest.
pub async fn get_full_products<const PARALLEL: bool>(
    db: &JinxDb,
    api_key: &str,
    guild_id: GuildId,
    partial_products: Vec<PartialProduct>,
) -> Result<Vec<LoadedProduct>, JinxError> {
    let mut products = Vec::with_capacity(partial_products.len());

    // get cached data (including etags) from db
    let cached_products: HashMap<String, ProductNameInfoValue, ahash::RandomState> = db
        .product_names_in_guild(guild_id)
        .await?
        .into_iter()
        .map(|info| (info.id, info.value))
        .collect();

    if PARALLEL {
        // parallel load
        let mut join_set = JoinSet::new();
        for partial_product in partial_products {
            let api_key = api_key.to_string();
            let product_id = partial_product.id;
            // we have to clone the etag because tokio does not support scoped tasks
            let etag = cached_products.get(&product_id).and_then(|info| info.etag.clone());
            join_set.spawn(async move {
                util::retry_thrice(|| get_product_cached(&api_key, &product_id, etag.as_deref()))
                    .await
                    .map(|option| match option {
                        Some(full_product) => ParallelFullProductResult::FullProduct(full_product),
                        None => ParallelFullProductResult::NotModified { product_id },
                    })
            });
        }
        while let Some(result) = join_set.join_next().await {
            let result = result.map_err(JinxxyError::from_join)??;
            let full_product = if let ParallelFullProductResult::FullProduct(full_product) = result {
                LoadedProduct::Api(full_product)
            } else if let ParallelFullProductResult::NotModified { product_id } = result
                && let Some(cached_product) = cached_products.get(&product_id)
            {
                let versions = db.product_versions(guild_id, product_id.clone()).await?;
                LoadedProduct::Cached {
                    product_info: Some(ProductNameInfo {
                        id: product_id,
                        value: ProductNameInfoValue {
                            product_name: cached_product.product_name.clone(),
                            etag: cached_product.etag.to_owned(),
                        },
                    }),
                    versions,
                }
            } else {
                // uh oh, we got a 304 response but somehow did not have the necessary data in the cache?
                // This ought not to be possible, as we should need etag from cache to even see a 304
                Err(JinxxyError::Impossible304)?
            };
            products.push(full_product);
        }
    } else {
        // sequential load
        for partial_product in partial_products {
            let product_id = partial_product.id;
            let full_product = if let Some(cached_product) = cached_products.get(&product_id) {
                let etag = cached_product.etag.as_deref();
                let full_product = util::retry_thrice(|| get_product_cached(api_key, &product_id, etag)).await?;
                if let Some(full_product) = full_product {
                    LoadedProduct::Api(full_product)
                } else {
                    let versions = db.product_versions(guild_id, product_id.clone()).await?;
                    LoadedProduct::Cached {
                        product_info: Some(ProductNameInfo {
                            id: product_id,
                            value: ProductNameInfoValue {
                                product_name: cached_product.product_name.clone(),
                                etag: etag.map(|slice| slice.to_vec()),
                            },
                        }),
                        versions,
                    }
                }
            } else {
                LoadedProduct::Api(get_product_uncached(api_key, &product_id).await?)
            };
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
    pub value: ProductNameInfoValue,
}

#[derive(Clone)]
pub struct ProductNameInfoValue {
    pub product_name: String,
    pub etag: Option<Vec<u8>>,
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
