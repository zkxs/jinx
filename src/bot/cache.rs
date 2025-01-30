// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! A guild-level cache of Jinxxy API results.
//!
//! This is needed because there are some cases, such as autocomplete, where we need API results
//! however autocomplete needs results at high frequency with low latency: in other words a naive
//! API fetch for each autocomplete would be extremely spammy.
//!
//! The idea here is we have a cache with a short expiry time (maybe 60s) and we reuse the results.
//! I can clear the cache with some kind of background task that checks timestamps ever 60s or so.

use crate::bot::util::{product_display_name, truncate_string_for_discord_autocomplete};
use crate::bot::{Context, MISSING_API_KEY_MESSAGE};
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::http::jinxxy::{FullProduct, PartialProduct, ProductVersionId};
use dashmap::{DashMap, Entry};
use poise::serenity_prelude::GuildId;
use std::collections::{HashMap, HashSet};
use tokio::time::{Duration, Instant};
use tracing::{debug, warn};
use trie_rs::map::{Trie, TrieBuilder};

type Error = Box<dyn std::error::Error + Send + Sync>;

const CACHE_EXPIRY_TIME: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct ApiCache {
    map: DashMap<GuildId, GuildCache, ahash::RandomState>,
}

impl ApiCache {
    /// Get a cache line and run some process on it, returning the result.
    ///
    /// If the cache is empty or expired, the underlying API will be hit.
    pub async fn get<F, T>(&self, context: &Context<'_>, f: F) -> Result<T, Error>
    where
        F: FnOnce(&GuildCache) -> T,
    {
        let guild_id = context
            .guild_id()
            .ok_or_else(|| JinxError::new("expected to be in a guild"))?;
        let lookup_result = match self.map.entry(guild_id) {
            Entry::Occupied(entry) => {
                let cache_entry = entry.get();
                if cache_entry.is_expired() {
                    debug!("updating product cache due to expiry in {}", guild_id.get());
                    None
                } else {
                    Some(entry.get().clone())
                }
            }
            Entry::Vacant(_entry) => {
                debug!("initializing product cache in {}", guild_id.get());
                None
            }
        };

        // purposefully drop dashmap lock across await to avoid deadlocks
        let guild_cache = if let Some(guild_cache) = lookup_result {
            // got an unexpired entry
            guild_cache
        } else {
            // expired or vacant entry
            let guild_cache = GuildCache::new(context, guild_id).await?;
            self.map.insert(guild_id, guild_cache.clone());
            guild_cache
        };

        Ok(f(&guild_cache))
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn capacity(&self) -> usize {
        self.map.capacity()
    }

    pub fn product_count(&self) -> usize {
        self.map
            .iter()
            .map(|entry| entry.value().product_count())
            .sum()
    }

    pub fn product_version_count(&self) -> usize {
        self.map
            .iter()
            .map(|entry| entry.value().product_version_count())
            .sum()
    }

    /// Remove expired cache entries
    pub fn clean(&self) {
        self.map
            .retain(|_guild_id, cache_entry| !cache_entry.is_expired());

        // if the capacity is much larger than the actual usage, then try shrinking
        let len = self.map.len();
        let capacity = self.map.capacity();

        let shrink = if len == 0 {
            capacity > 16 // edge case to avoid dividing by zero
        } else {
            capacity / len >= 16 // if load factor is beyond some arbitrary threshold
        };

        if shrink {
            self.map.shrink_to_fit();
        }
    }

    pub async fn product_names_with_prefix(
        &self,
        context: &Context<'_>,
        prefix: &str,
    ) -> Result<Vec<String>, Error> {
        self.get(context, |cache_entry| {
            cache_entry.product_names_with_prefix(prefix).collect()
        })
        .await
    }

    pub async fn product_version_names_with_prefix(
        &self,
        context: &Context<'_>,
        prefix: &str,
    ) -> Result<Vec<String>, Error> {
        self.get(context, |cache_entry| {
            cache_entry
                .product_version_names_with_prefix(prefix)
                .collect()
        })
        .await
    }

    pub async fn product_name_to_id(
        &self,
        context: &Context<'_>,
        product_name: &str,
    ) -> Result<Option<String>, Error> {
        self.get(context, |cache_entry| {
            cache_entry
                .product_name_to_id(product_name)
                .map(|str| str.to_string())
        })
        .await
    }

    pub async fn product_version_name_to_version_id(
        &self,
        context: &Context<'_>,
        product_name: &str,
    ) -> Result<Option<ProductVersionId>, Error> {
        self.get(context, |cache_entry| {
            cache_entry
                .product_version_name_to_version_id(product_name)
                .cloned()
        })
        .await
    }
}

#[derive(Clone)]
pub struct GuildCache {
    /// id to name
    product_id_to_name_map: HashMap<String, String, ahash::RandomState>,
    /// name to id
    product_name_to_id_map: HashMap<String, String, ahash::RandomState>,
    /// completes lowercase name to name with correct case
    product_name_trie: Trie<u8, String>,
    /// number of products
    product_count: usize,
    /// version_id to name
    product_version_id_to_name_map: HashMap<ProductVersionId, String, ahash::RandomState>,
    /// name to version_id
    product_name_to_version_id_map: HashMap<String, ProductVersionId, ahash::RandomState>,
    /// completes lowercase version name to version name with correct case
    product_version_name_trie: Trie<u8, String>,
    /// Number of product versions, including null versions
    product_version_count: usize,
    /// Time this cache was constructed
    create_time: Instant,
}

/// Internal struct for holding name info
struct NameInfo {
    id: String,
    product_name: String,
}

/// Internal struct for holding version name info
struct VersionNameInfo {
    id: ProductVersionId,
    product_version_name: String,
}

impl GuildCache {
    async fn new(context: &Context<'_>, guild_id: GuildId) -> Result<GuildCache, Error> {
        if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
            let partial_products: Vec<PartialProduct> = jinxxy::get_products(&api_key).await?;

            let products: Vec<FullProduct> = jinxxy::get_full_products(&api_key, partial_products)
                .await?
                .into_iter()
                .filter(|product| !product.name.is_empty()) // products with empty names are kinda weird, so I'm just gonna filter them to avoid any potential pitfalls
                .collect();

            // convert into map tuples for products without versions
            let product_name_info: Vec<NameInfo> = products
                .iter()
                .map(|product| {
                    let id = product.id.clone();
                    let product_name =
                        truncate_string_for_discord_autocomplete(product.name.clone());
                    NameInfo { id, product_name }
                })
                .collect();

            // convert into map tuples for product versions
            let product_version_name_info: Vec<VersionNameInfo> = products
                .into_iter()
                .flat_map(|product| {
                    let product_name = truncate_string_for_discord_autocomplete(product.name);

                    let null_name_info = VersionNameInfo {
                        id: ProductVersionId::from_product_id(&product.id),
                        product_version_name: product_name.clone(),
                    };
                    let null_iter = std::iter::once(null_name_info);

                    let iter = product.versions.into_iter().map(move |version| {
                        let id = ProductVersionId {
                            product_id: product.id.clone(),
                            product_version_id: Some(version.id.clone()),
                        };
                        let product_version_name = truncate_string_for_discord_autocomplete(
                            product_display_name(&product_name, Some(version.name.as_str())),
                        );
                        VersionNameInfo {
                            id,
                            product_version_name,
                        }
                    });
                    null_iter.chain(iter)
                })
                .collect();

            let product_count = product_name_info.len();
            let product_version_count = product_version_name_info.len();

            // check for duplicate product names without versions
            {
                let mut dupe_set: HashSet<&str, ahash::RandomState> = Default::default();
                product_name_info.iter().for_each(|name_info| {
                    if !dupe_set.insert(name_info.product_name.as_str()) {
                        warn!(
                            "in {} product {} \"{}\" has the same name as some other product",
                            guild_id.get(),
                            name_info.id,
                            name_info.product_name
                        )
                    }
                });
            }

            // check for duplicate product names with versions
            {
                let mut dupe_set: HashSet<&str, ahash::RandomState> = Default::default();
                product_version_name_info
                    .iter()
                    .for_each(|name_info| {
                        if !dupe_set.insert(name_info.product_version_name.as_str()) {
                            warn!(
                                "in {} product {} \"{}\" has the same name as some other product+version",
                                guild_id.get(), name_info.id, name_info.product_version_name
                            )
                        }
                    });
            }

            // build trie without versions
            let product_name_trie = {
                let mut trie_builder = TrieBuilder::new();
                for name_info in product_name_info.iter() {
                    let name = &name_info.product_name;
                    trie_builder.push(name.to_lowercase(), name.to_string());
                }
                trie_builder.build()
            };

            // build trie with versions
            let product_version_name_trie = {
                let mut trie_builder = TrieBuilder::new();
                for name_info in product_version_name_info.iter() {
                    let name = &name_info.product_version_name;
                    trie_builder.push(name.to_lowercase(), name.to_string());
                }
                trie_builder.build()
            };

            // build forward map without versions
            let product_id_to_name_map = product_name_info
                .iter()
                .map(|name_info| (name_info.id.clone(), name_info.product_name.clone()))
                .collect();

            // build forward map with versions
            let product_version_id_to_name_map = product_version_name_info
                .iter()
                .map(|name_info| (name_info.id.clone(), name_info.product_version_name.clone()))
                .collect();

            // build reverse map without versions
            let product_name_to_id_map = product_name_info
                .into_iter()
                .map(|name_info| (name_info.product_name, name_info.id))
                .collect();

            // build reverse map with versions
            let product_name_to_version_id_map = product_version_name_info
                .into_iter()
                .map(|name_info| (name_info.product_version_name, name_info.id))
                .collect();

            let create_time = Instant::now();

            Ok(GuildCache {
                product_id_to_name_map,
                product_name_to_id_map,
                product_name_trie,
                product_count,
                product_version_id_to_name_map,
                product_name_to_version_id_map,
                product_version_name_trie,
                product_version_count,
                create_time,
            })
        } else {
            Err(JinxError::boxed(MISSING_API_KEY_MESSAGE))
        }
    }

    fn product_names_with_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Iterator<Item = String> + 'a {
        self.product_name_trie
            .predictive_search(prefix.to_lowercase())
            .map(|(_key, value): (Vec<u8>, &String)| value.to_string())
    }

    fn product_version_names_with_prefix<'a>(
        &'a self,
        prefix: &'a str,
    ) -> impl Iterator<Item = String> + 'a {
        self.product_version_name_trie
            .predictive_search(prefix.to_lowercase())
            .map(|(_key, value): (Vec<u8>, &String)| value.to_string())
    }

    pub fn product_id_to_name(&self, product_id: &str) -> Option<&str> {
        self.product_id_to_name_map
            .get(product_id)
            .map(|str| str.as_str())
    }

    pub fn product_version_id_to_name(
        &self,
        product_version_id: &ProductVersionId,
    ) -> Option<&str> {
        self.product_version_id_to_name_map
            .get(product_version_id)
            .map(|str| str.as_str())
    }

    fn product_name_to_id(&self, product_name: &str) -> Option<&str> {
        self.product_name_to_id_map
            .get(product_name)
            .map(|str| str.as_str())
    }

    fn product_version_name_to_version_id(&self, product_name: &str) -> Option<&ProductVersionId> {
        self.product_name_to_version_id_map.get(product_name)
    }

    fn product_count(&self) -> usize {
        self.product_count
    }

    fn product_version_count(&self) -> usize {
        self.product_version_count
    }

    fn is_expired(&self) -> bool {
        self.create_time.elapsed() > CACHE_EXPIRY_TIME
    }
}

#[cfg(test)]
mod test {
    use trie_rs::map::TrieBuilder;

    #[test]
    fn test_trie_empty_prefix() {
        let tuples = [
            ("foo", "foo_data"),
            ("bar", "bar_data"),
            ("baz", "baz_data"),
        ];

        let mut trie_builder = TrieBuilder::new();
        for (key, value) in tuples.iter() {
            trie_builder.push(key, value.to_string());
        }
        let trie = trie_builder.build();

        let results: Vec<String> = trie
            .predictive_search("")
            .map(|(_key, value): (Vec<u8>, &String)| value)
            .map(|value| value.to_string())
            .collect();

        assert_eq!(
            tuples.len(),
            results.len(),
            "actual and expected result lengths did not match"
        );

        for tuple in tuples {
            let (_, expected) = tuple;
            assert!(
                results.iter().any(|actual| actual == expected),
                "could not find expected value: {}",
                expected
            );
        }
    }
}
