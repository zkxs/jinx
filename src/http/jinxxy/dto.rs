// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! Internal DTOs used only by Jinxxy API response parsing logic

use crate::http::jinxxy::{DISCORD_PREFIX, GetProfileImageUrl, GetUsername, ProductVersionInfo};
use crate::license::LOCKING_USER_ID;
use ahash::HashSet;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use tracing::{error, warn};

static GLOBAL_JINXXY_ACTIVATION_DESCRIPTION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(format!(r"^{}(\d+)$", DISCORD_PREFIX).as_str())
        .expect("Failed to compile GLOBAL_JINXXY_ACTIVATION_DESCRIPTION_REGEX")
});

thread_local! {
    // trick to avoid a subtle performance edge case: https://docs.rs/regex/latest/regex/index.html#sharing-a-regex-across-threads-can-result-in-contention
    static JINXXY_ACTIVATION_DESCRIPTION_REGEX: Regex = GLOBAL_JINXXY_ACTIVATION_DESCRIPTION_REGEX.clone();
}

#[derive(Debug, Deserialize)]
pub struct LicenseList {
    pub results: Vec<LicenseListResult>,
}

#[derive(Debug, Deserialize)]
pub struct LicenseListResult {
    /// License ID
    pub id: String,
}

#[derive(Debug, Deserialize)]
pub struct License {
    /// ID of this license
    id: String,
    /// Short key
    short_key: String,
    /// Long key
    key: String,
    user: LicenseUser,
    inventory_item: LicenseInventoryItem,
    activations: LicenseActivations,
}

impl From<License> for super::LicenseInfo {
    fn from(license: License) -> Self {
        let product_version_info = license
            .inventory_item
            .target_version_id
            .map(|version_id| ProductVersionInfo {
                product_version_id: version_id,
                product_version_name: String::new(), // this gets injected later after some async stuff happens, don't worry about it for now
            });
        Self {
            license_id: license.id,
            short_key: license.short_key,
            key: license.key,
            user_id: license.user.id,
            username: license.user.username,
            product_id: license.inventory_item.target_id,
            product_name: license.inventory_item.item.name,
            product_version_info,
            activations: license.activations.total_count,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LicenseUser {
    /// User ID
    id: String,
    /// Account's username; used in profile URL
    username: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LicenseInventoryItem {
    // this also has an item `id` field which may be able to be cross-referenced with the order API
    /// Product ID
    target_id: String,
    /// Product version ID. None if the item did not have an associated version.
    target_version_id: Option<String>,
    /// More product metadata
    item: LicenseInventoryItemItem,
}

#[derive(Debug, Deserialize)]
pub struct LicenseInventoryItemItem {
    // yes I know this name is ridiculous, but it's how the API response is structured ¯\_(ツ)_/¯
    /// Product Name
    name: String,
}

#[derive(Debug, Deserialize)]
struct LicenseActivations {
    total_count: u32,
}

#[derive(Debug, Deserialize)]
pub struct AuthUser {
    /// No sure what this is, but it can be null or empty. I think this is custom display name?
    name: Option<String>,
    /// Account's username; used in profile URL
    username: Option<String>,
    profile_image: Option<ProfileImage>,
    /// API scopes
    pub scopes: HashSet<String>,
}

impl AuthUser {
    pub fn into_display_name(self) -> String {
        match self.name {
            Some(name) if !name.is_empty() && !name.trim().is_empty() => name,
            _ => self.username.unwrap_or_else(|| "`null`".to_string()),
        }
    }

    /// Check if this API key has all the required scopes
    pub fn has_required_scopes(&self) -> bool {
        self.has_scope_licenses_read() && self.has_scope_licenses_write() && self.has_scope_products_read()
    }

    /// Check if this API key has the `products_read` scope
    fn has_scope_products_read(&self) -> bool {
        self.scopes.contains("products_read")
    }

    // /// Check if this API key has the `orders_read` scope
    // fn has_scope_orders_read(&self) -> bool {
    //     self.scopes.contains("orders_read")
    // }

    // /// Check if this API key has the `discount_codes_read` scope
    // fn has_scope_discount_codes_read(&self) -> bool {
    //     self.scopes.contains("discount_codes_read")
    // }

    // /// Check if this API key has the `customers_read` scope
    // fn has_scope_customers_read(&self) -> bool {
    //     self.scopes.contains("customers_read")
    // }

    /// Check if this API key has the `licenses_read` scope
    fn has_scope_licenses_read(&self) -> bool {
        self.scopes.contains("licenses_read")
    }

    /// Check if this API key has the `licenses_write` scope
    fn has_scope_licenses_write(&self) -> bool {
        self.scopes.contains("licenses_write")
    }
}

impl GetUsername for AuthUser {
    fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }
}

impl GetProfileImageUrl for AuthUser {
    fn profile_image_url(&self) -> Option<&str> {
        self.profile_image
            .as_ref()
            .map(|inner| inner.url.as_str())
            .filter(|url| !url.is_empty())
    }
}

impl From<AuthUser> for super::DisplayUser {
    fn from(mut auth_user: AuthUser) -> Self {
        let profile_image_url = auth_user
            .profile_image
            .take()
            .map(|profile_image| profile_image.url)
            .filter(|url| !url.is_empty());
        Self {
            display_name: auth_user.into_display_name(),
            profile_image_url,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ProfileImage {
    url: String,
}

/// While part of the Jinxxy API this is also very useful as an external DTO
pub struct PartialProduct {
    /// Product ID
    pub id: String,
    /// Product Name
    pub name: String,
}

/// In addition to all the fields of [`PartialProduct`], this also contains price and version information
///
/// While part of the Jinxxy API this is also very useful as an external DTO
#[derive(Debug, Deserialize)]
pub struct FullProduct {
    /// Product ID
    pub id: String,
    /// Product name
    pub name: String,
    pub versions: Vec<ProductVersion>,
}

impl From<FullProduct> for PartialProduct {
    fn from(product: FullProduct) -> Self {
        Self {
            id: product.id,
            name: product.name,
        }
    }
}

/// While part of the Jinxxy API this is also useful as an external DTO
#[derive(Debug, Deserialize)]
pub struct ProductVersion {
    /// Product version ID
    pub id: String,
    /// Product version name
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct ProductList {
    results: Vec<ProductListResult>,
}

impl From<ProductList> for Vec<PartialProduct> {
    fn from(product_list: ProductList) -> Self {
        product_list.results.into_iter().map(|item| item.into()).collect()
    }
}

#[derive(Debug, Deserialize)]
struct ProductListResult {
    /// Product ID
    id: String,
    /// Product Name
    name: String,
}

impl From<ProductListResult> for PartialProduct {
    fn from(product: ProductListResult) -> Self {
        Self {
            id: product.id,
            name: product.name,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct LicenseActivationList {
    pub results: Vec<LicenseActivation>,
}

/// While part of the Jinxxy API this is also very useful as an external DTO
#[derive(Debug, Deserialize)]
pub struct LicenseActivation {
    /// ID of this license activation
    pub id: String,
    /// Custom description describing what this activation is for.
    pub description: String,
}

impl LicenseActivation {
    /// Try to extract a Discord user ID from this license activation
    pub fn try_into_user_id(&self) -> Option<u64> {
        JINXXY_ACTIVATION_DESCRIPTION_REGEX
            .with(|regex| regex.captures(&self.description))
            .or_else(|| {
                warn!(
                    "JINXXY_ACTIVATION_DESCRIPTION_REGEX did not match Jinxxy license activation description \"{}\"",
                    self.description
                );
                None
            })
            .and_then(|captures| {
                let capture = captures.get(1);
                if capture.is_none() {
                    error!("JINXXY_ACTIVATION_DESCRIPTION_REGEX capture group 1 not found!");
                }
                capture
            })
            .and_then(|capture| match capture.as_str().parse::<u64>() {
                Ok(id) => Some(id),
                Err(e) => {
                    error!("error parsing activation description \"{}\": {:?}", self.description, e);
                    None
                }
            })
    }

    /// Check if this activation is a lock
    pub fn is_lock(&self) -> bool {
        self.try_into_user_id().map(|id| id == LOCKING_USER_ID).unwrap_or(false)
    }
}

#[derive(Debug, Serialize)]
pub struct CreateLicenseActivation {
    /// Custom description describing what this activation is for.
    description: String,
}

impl CreateLicenseActivation {
    pub fn from_user_id(user_id: u64) -> Self {
        Self {
            description: format!("{}{}", DISCORD_PREFIX, user_id),
        }
    }
}

/// Undocumented part of the Jinxxy API. JSON looks like this:
/// ```json
/// {
///   "status_code": 500,
///   "error": "Bad Request",
///   "message": "You are not authorized.",
///   "code": "GRAPHQL_ERROR"
/// }
/// ```
#[derive(Debug, Deserialize)]
pub struct JinxxyError {
    status_code: u16,
    error: String,
    message: String,
}

impl JinxxyError {
    /// Check if an error looks like a 403.
    ///
    /// For some reason Jinxxy does not return a reasonable status code, leaving it up to me to parse their 500 response JSON.
    pub fn looks_like_403(&self) -> bool {
        self.status_code == 403 || (self.error == "Bad Request" && self.message == "You are not authorized.")
    }

    /// Check if an error looks like a 404.
    ///
    /// For some reason Jinxxy does not return a reasonable status code, leaving it up to me to parse their 500 response JSON.
    pub fn looks_like_404(&self) -> bool {
        self.status_code == 404 || (self.error == "Bad Request" && self.message == "Resource not found.")
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use tracing_test::traced_test;

    #[test]
    #[traced_test]
    fn test_description_parse() {
        let expected = 177811898790707200u64;

        let activation = LicenseActivation {
            id: "3557172628961625518".to_string(),
            description: "discord_177811898790707200".to_string(),
        };

        let actual = activation
            .try_into_user_id()
            .expect("expected description parse to succeed");
        assert_eq!(expected, actual);
    }

    #[test]
    fn test_description_parse_fail_too_long() {
        let activation = LicenseActivation {
            id: "3557172628961625518".to_string(),
            description: "discord_17781189879070720575757890257892304570".to_string(),
        };

        let fail = activation.try_into_user_id().is_none();
        assert!(fail);
    }

    #[test]
    fn test_description_parse_fail_nan() {
        let activation = LicenseActivation {
            id: "3557172628961625518".to_string(),
            description: "discord_17781foo".to_string(),
        };

        let fail = activation.try_into_user_id().is_none();
        assert!(fail);
    }

    #[test]
    fn test_description_parse_fail_no_match() {
        let activation = LicenseActivation {
            id: "3557172628961625518".to_string(),
            description: "foo".to_string(),
        };

        let fail = activation.try_into_user_id().is_none();
        assert!(fail);
    }
}
