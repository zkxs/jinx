// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! Internal DTOs used only by Jinxxy API response parsing logic

use crate::http::jinxxy::{DISCORD_PREFIX, GetProfileImageUrl, GetUsername, ProductVersionInfo, Username};
use crate::license::LOCKING_USER_ID;
use ahash::HashSet;
use jiff::Timestamp;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use tracing::{error, warn};

static GLOBAL_JINXXY_ACTIVATION_DESCRIPTION_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(format!(r"^{DISCORD_PREFIX}(\d+)$").as_str())
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

#[derive(Debug, Deserialize)]
pub struct OrderInfo {
    id: String,
    #[allow(dead_code)] // debug printed
    payment_status: String,
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
            order_id: license.inventory_item.order.map(|order| order.id),
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
    /// Order metadata
    order: Option<OrderInfo>,
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
    /// Unique userid
    pub id: String,
    /// No sure what this is, but it can be null or empty (and often is). I think this is custom display name?
    name: Option<String>,
    /// Account's username; used in profile URL. Ought to be set for all sellers.
    username: Option<String>,
    profile_image: Option<ProfileImage>,
    /// API scopes
    pub scopes: HashSet<String>,
}

impl AuthUser {
    pub fn as_display_name(&self) -> &str {
        match &self.name {
            Some(name) if !name.is_empty() && !name.trim().is_empty() => name,
            _ => self.username.as_deref().unwrap_or("`null`"),
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
    fn username(&self) -> Username<'_> {
        Username(self.username.as_deref())
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

impl<'a> From<&'a AuthUser> for super::DisplayUser<'a> {
    fn from(auth_user: &'a AuthUser) -> Self {
        let profile_image_url = auth_user
            .profile_image
            .as_ref()
            .map(|profile_image| profile_image.url.as_str())
            .filter(|url| !url.is_empty());
        Self {
            display_name: auth_user.as_display_name(),
            profile_image_url,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ProfileImage {
    url: String,
}

/// While part of the Jinxxy API this is also very useful as an external DTO
#[derive(Debug, Deserialize)]
pub struct PartialProduct {
    /// Product ID
    pub id: String,

    /// Product Name
    pub name: String,

    /// All versions of this product
    pub versions: Vec<ProductVersion>,
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
    /// All versions of this product
    pub versions: Vec<ProductVersion>,
}

impl From<FullProduct> for PartialProduct {
    fn from(product: FullProduct) -> Self {
        Self {
            id: product.id,
            name: product.name,
            versions: product.versions,
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
    results: Vec<PartialProduct>,
}

impl ProductList {
    pub fn products(self) -> Vec<PartialProduct> {
        self.results
    }
}

#[derive(Debug, Deserialize)]
pub struct LicenseActivationList {
    pub results: Vec<LicenseActivation>,
}

impl LicenseActivationList {
    pub fn len(&self) -> usize {
        self.results.len()
    }
}

/// While part of the Jinxxy API this is also very useful as an external DTO
#[derive(Debug, Deserialize)]
pub struct LicenseActivation {
    /// ID of this license activation
    pub id: String,
    /// Custom description describing what this activation is for.
    pub description: String,
    /// Time this activation was created
    pub created_at: Timestamp,
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
            description: format!("{DISCORD_PREFIX}{user_id}"),
        }
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
            created_at: "2023-12-01T01:52:15.816Z".parse().expect("Expected valid Timestamp"),
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
            created_at: "2023-12-01T01:52:15.816Z".parse().expect("Expected valid Timestamp"),
        };

        let fail = activation.try_into_user_id().is_none();
        assert!(fail);
    }

    #[test]
    fn test_description_parse_fail_nan() {
        let activation = LicenseActivation {
            id: "3557172628961625518".to_string(),
            description: "discord_17781foo".to_string(),
            created_at: "2023-12-01T01:52:15.816Z".parse().expect("Expected valid Timestamp"),
        };

        let fail = activation.try_into_user_id().is_none();
        assert!(fail);
    }

    #[test]
    fn test_description_parse_fail_no_match() {
        let activation = LicenseActivation {
            id: "3557172628961625518".to_string(),
            description: "foo".to_string(),
            created_at: "2023-12-01T01:52:15.816Z".parse().expect("Expected valid Timestamp"),
        };

        let fail = activation.try_into_user_id().is_none();
        assert!(fail);
    }
}
