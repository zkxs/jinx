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

    pub async fn product_name_to_id(
        &self,
        context: &Context<'_>,
        product_name: &str,
    ) -> Result<Option<ProductVersionId>, Error> {
        self.get(context, |cache_entry| {
            cache_entry.product_name_to_id(product_name).cloned()
        })
        .await
    }
}

/// Get a display name from a product name and a version name
fn product_display_name(product_name: &str, product_version_name: Option<&str>) -> String {
    let mut name = match product_version_name {
        Some(product_version_name) => format!("{product_name} {product_version_name}"),
        None => product_name.to_string(),
    };

    // truncate name to meet discord's 100 character autocomplete limit
    if name.len() > 100 {
        debug!("\"{}\".len() > 100; truncating…", name);

        // byte len is > 100 so there must be at least one char, so we can disregard that edge case

        // get the index after the 100th) char
        let last_char_index = name
            .char_indices()
            .nth(100)
            .map(|index| index.0) // start index of char 101 == index after char 100
            .unwrap_or_else(|| name.len()); // index after end of last char

        name.truncate(last_char_index);
    }

    name
}

#[derive(Clone)]
pub struct GuildCache {
    /// id to name
    product_id_to_name_map: HashMap<ProductVersionId, String, ahash::RandomState>,
    /// name to id
    product_name_to_id_map: HashMap<String, ProductVersionId, ahash::RandomState>,
    /// completes lowercase name to name with correct case
    product_name_trie: Trie<u8, String>,
    create_time: Instant,
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

            // convert into map tuples
            let product_id_name_tuples: Vec<(ProductVersionId, String)> = products
                .into_iter()
                .flat_map(|product| {
                    let null_id = ProductVersionId {
                        product_id: product.id.clone(),
                        product_version_id: None,
                    };
                    let null_name = product_display_name(&product.name, None);
                    let null_iter = std::iter::once((null_id, null_name));
                    let iter = product.versions.into_iter().map(move |version| {
                        let id = ProductVersionId {
                            product_id: product.id.clone(),
                            product_version_id: Some(version.id),
                        };
                        let name = product_display_name(&product.name, Some(&version.name));
                        (id, name)
                    });
                    null_iter.chain(iter)
                })
                .collect();

            // check for duplicate product names
            {
                let mut dupe_set: HashSet<&str, ahash::RandomState> = Default::default();
                product_id_name_tuples.iter().for_each(|(id, name)| {
                    if !dupe_set.insert(name.as_str()) {
                        warn!(
                            "product {:?} \"{}\" has the same name as some other product",
                            id, name
                        )
                    }
                });
            }

            // build trie
            let mut trie_builder = TrieBuilder::new();
            for (_id, name) in product_id_name_tuples.iter() {
                trie_builder.push(name.to_lowercase(), name.to_string());
            }
            let product_name_trie = trie_builder.build();

            // build forward map
            let product_id_to_name_map = product_id_name_tuples.iter().cloned().collect();

            // build reverse map
            let product_name_to_id_map = product_id_name_tuples
                .into_iter()
                .map(|(id, name)| (name, id))
                .collect();

            let create_time = Instant::now();

            Ok(GuildCache {
                product_id_to_name_map,
                product_name_to_id_map,
                product_name_trie,
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

    pub fn product_id_to_name(&self, product_id: &ProductVersionId) -> Option<&str> {
        self.product_id_to_name_map
            .get(product_id)
            .map(|str| str.as_str())
    }

    fn product_name_to_id(&self, product_name: &str) -> Option<&ProductVersionId> {
        self.product_name_to_id_map.get(product_name)
    }

    fn product_count(&self) -> usize {
        self.product_name_to_id_map.len()
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
