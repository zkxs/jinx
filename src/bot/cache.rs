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

//TODO: add cache size estimate to owner_len()

use std::collections::HashMap;
use crate::bot::commands::MISSING_API_KEY_MESSAGE;
use crate::bot::Context;
use crate::error::JinxError;
use crate::http::jinxxy;
use dashmap::{DashMap, Entry};
use poise::serenity_prelude::GuildId;
use radix_trie::{Trie, TrieCommon};
use std::time::Duration;
use tokio::time::Instant;
use tracing::debug;

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
        let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;
        let result = match self.map.entry(guild_id) {
            Entry::Occupied(entry) => {
                let cache_entry = entry.get();
                if cache_entry.is_expired() {
                    debug!("updating product cache due to expiry in {}", guild_id.get());
                    let cache_entry = GuildCache::new(context, guild_id).await?;
                    let mut entry = entry;
                    entry.insert(cache_entry);
                    f(entry.get())
                } else {
                    f(cache_entry)
                }
            }
            Entry::Vacant(entry) => {
                debug!("initializing product cache in {}", guild_id.get());
                let cache_entry = GuildCache::new(context, guild_id).await?;
                let entry_ref = entry.insert(cache_entry);
                f(entry_ref.value())
            }
        };

        Ok(result)
    }

    pub fn len(&self) -> usize {
        self.map.iter()
            .map(|entry| entry.value().len())
            .sum()
    }
}

pub struct GuildCache {
    product_name_to_id_map: HashMap<String, String, ahash::RandomState>,
    product_name_trie: Trie<String, ()>,
    create_time: Instant,
}

impl GuildCache {
    async fn new(context: &Context<'_>, guild_id: GuildId) -> Result<GuildCache, Error> {
        if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
            let products = jinxxy::get_products(&api_key).await?;

            // build trie
            let mut product_name_trie = Trie::new();
            for product_name in products.iter().map(|product| product.name.clone()) {
                product_name_trie.insert(product_name, ());
            }

            // build map
            let product_name_to_id_map = products.into_iter()
                .map(|product| (product.name, product.id))
                .collect();

            let create_time = Instant::now();

            Ok(GuildCache {
                product_name_to_id_map,
                product_name_trie,
                create_time,
            })
        } else {
            Err(JinxError::boxed(MISSING_API_KEY_MESSAGE))
        }
    }

    pub fn product_names_with_prefix(&self, prefix: &str) -> Box<dyn Iterator<Item = String> + Send + Sync> {
        if let Some(subtrie) = self.product_name_trie.subtrie(prefix) {
            let vec: Vec<String> = subtrie.keys().cloned().collect();
            Box::new(vec.into_iter())
        } else {
            Box::new(std::iter::empty())
        }
    }

    pub fn product_name_to_id(&self, product_name: &str) -> Option<&str> {
        self.product_name_to_id_map.get(product_name).map(|str| str.as_str())
    }

    fn len(&self) -> usize {
        self.product_name_to_id_map.len()
    }

    fn is_expired(&self) -> bool {
        self.create_time.elapsed() > CACHE_EXPIRY_TIME
    }
}