// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! A guild-level cache of Jinxxy API results.
//!
//! This is needed because there are some cases, such as autocomplete, where we need API results
//! however autocomplete needs results at high frequency with low latency: in other words a naive
//! API fetch for each autocomplete would be extremely spammy.
//!
//! The idea here is we have a cache with a short expiry time (maybe 60s) and we reuse the results.
//! I can clear the cache with some kind of background task that checks timestamps ever 60s or so.

use crate::bot::{util, SECONDS_PER_DAY};
use crate::bot::{Context, MISSING_API_KEY_MESSAGE};
use crate::db::JinxDb;
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::http::jinxxy::{FullProduct, PartialProduct, ProductVersionId};
use dashmap::DashMap;
use poise::serenity_prelude::GuildId;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::time::{Duration, Instant};
use tracing::{debug, info, warn};
use trie_rs::map::{Trie, TrieBuilder};

type Error = Box<dyn std::error::Error + Send + Sync>;
type MapType = Arc<DashMap<GuildId, GuildCache, ahash::RandomState>>;

const HIGH_PRIORITY_CACHE_EXPIRY_TIME: Duration = Duration::from_secs(60); // 1 minute
const LOW_PRIORITY_CACHE_EXPIRY_TIME: Duration = Duration::from_secs(SECONDS_PER_DAY); // 24 hours

#[derive(Clone)]
pub struct ApiCache {
    map: MapType,
    high_priority_tx: mpsc::Sender<GuildId>,
    refresh_register_tx: mpsc::Sender<GuildId>,
    refresh_deregister_tx: mpsc::Sender<GuildId>,
}

impl ApiCache {
    pub fn new(db: Arc<JinxDb>) -> Self {
        let map: MapType = Default::default();

        const QUEUE_SIZE: usize = 1024;
        let (high_priority_tx, mut high_priority_rx) = mpsc::channel(QUEUE_SIZE);
        let (refresh_register_tx, mut refresh_register_rx) = mpsc::channel(QUEUE_SIZE);
        let (refresh_deregister_tx, mut refresh_deregister_rx) = mpsc::channel(QUEUE_SIZE);

        // high priority refresh task
        {
            let db = db.clone();
            let map = map.clone();
            tokio::task::spawn(async move {
                while let Some(guild_id) = high_priority_rx.recv().await {
                    // first, check to make sure this entry is still expired
                    let needs_refresh = map
                        .get(&guild_id)
                        .map(|entry| entry.is_expired_high_priority())
                        .unwrap_or(true);

                    if needs_refresh {
                        match db.get_jinxxy_api_key(guild_id).await {
                            Ok(api_key) => match api_key {
                                Some(api_key) => match GuildCache::new(&api_key).await {
                                    Ok(guild_cache) => {
                                        map.insert(guild_id, guild_cache);
                                    }
                                    Err(e) => {
                                        warn!("Error initializing API cache during high-priority refresh for {}: {:?}", guild_id.get(), e);
                                    }
                                },
                                None => {
                                    warn!("High-priority refresh was somehow triggered for guild {}, which has no api key set!", guild_id);
                                }
                            },
                            Err(e) => {
                                warn!("Error retrieving API key during high-priority refresh for {}: {:?}", guild_id.get(), e);
                            }
                        }
                    }
                }
            });
        }

        // low priority refresh task. Used for initial cache warm and refresh
        {
            let db = db;
            let map = map.clone();
            tokio::task::spawn(async move {
                let mut guild_set = HashSet::with_hasher(ahash::RandomState::default());
                let mut queue = VecDeque::new();
                'outer: loop {
                    // the first thing we do when we wake is check for newly registered guilds
                    match refresh_register_rx.try_recv() {
                        Ok(guild_id) => {
                            // new guilds cut in line and go to the front of the queue
                            if guild_set.insert(guild_id) {
                                queue.push_front(guild_id);
                            }
                        }
                        Err(TryRecvError::Empty) => {
                            // the second thing we do is check for deregistered guilds
                            'inner: loop {
                                match refresh_deregister_rx.try_recv() {
                                    Ok(guild_id) => {
                                        if guild_set.remove(&guild_id) {
                                            queue.retain(|queue_guild_id| {
                                                *queue_guild_id != guild_id
                                            });
                                        }
                                    }
                                    Err(TryRecvError::Empty) => {
                                        // we've caught up, so return to the outer loop
                                        break 'inner;
                                    }
                                    Err(TryRecvError::Disconnected) => {
                                        // channel is broken, so stop this task
                                        break 'outer;
                                    }
                                }
                            }
                            // end of inner loop
                            // all of the event processing is now done
                            // process a single guild

                            // find the first guild that needs a refresh
                            if let Some((index, guild_id)) =
                                queue.iter().enumerate().find(|(_index, guild_id)| {
                                    map.get(guild_id)
                                        .map(|entry| entry.is_expired_low_priority())
                                        .unwrap_or(true)
                                })
                            {
                                let guild_id = *guild_id;
                                // refresh that guild
                                let guild_ok = match db.get_jinxxy_api_key(guild_id).await {
                                    Ok(api_key) => {
                                        match api_key {
                                            Some(api_key) => {
                                                debug!(
                                                    "starting low priority refresh of cache for {}",
                                                    guild_id.get()
                                                );
                                                match GuildCache::new(&api_key).await {
                                                    Ok(guild_cache) => {
                                                        map.insert(guild_id, guild_cache);
                                                        true
                                                    }
                                                    Err(e) => {
                                                        warn!("Error initializing API cache during low-priority refresh for {}: {:?}", guild_id.get(), e);

                                                        match jinxxy::get_own_user(&api_key).await {
                                                            Ok(auth_user) => {
                                                                if auth_user.has_required_scopes() {
                                                                    // okay must have been a weird fluke, we'll leave this guild registered
                                                                    true
                                                                } else {
                                                                    false
                                                                }
                                                            }
                                                            Err(e) => {
                                                                info!("error checking /me for guild {}, will deregister now: {:?}", guild_id.get(), e);
                                                                false
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                            None => {
                                                info!("low-priority refresh was triggered for guild {}, which has no api key set! Deregistering now.", guild_id.get());
                                                false
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        warn!("Error retrieving API key during low-priority refresh for {}. Deregistering now: {:?}", guild_id.get(), e);
                                        false
                                    }
                                };

                                // pop all the guilds we just passed over and push them to the end
                                // if the 0th item was process we need to rotate 1 time, hence the `index + 1` expression
                                queue.rotate_left(index + 1);
                                if !guild_ok {
                                    guild_set.remove(&guild_id);
                                    queue.pop_back();
                                }
                            }

                            // wait a few seconds before doing another low-priority event
                            tokio::time::sleep(Duration::from_secs(60)).await;
                        }
                        Err(TryRecvError::Disconnected) => {
                            // channel is broken, so stop this task
                            break 'outer;
                        }
                    }
                }
                // end of outer loop
                // channel is broken, so we're stopping the task
            });
        }

        Self {
            map,
            high_priority_tx,
            refresh_register_tx,
            refresh_deregister_tx,
        }
    }

    /// Trigger a one-time high-priority refresh of this guild in the cache.
    pub async fn refresh_guild_in_cache(&self, guild_id: GuildId) -> Result<(), Error> {
        self.high_priority_tx.send(guild_id).await?;
        Ok(())
    }

    /// Register a guild in the cache. The guild will have its cache entry periodically warmed automatically.
    pub async fn register_guild_in_cache(&self, guild_id: GuildId) -> Result<(), Error> {
        self.refresh_register_tx.send(guild_id).await?;
        Ok(())
    }

    /// Deregister a guild in the cache. The guild will no longer have its cache entry periodically warmed automatically.
    pub async fn deregister_guild_in_cache(&self, guild_id: GuildId) -> Result<(), Error> {
        self.refresh_deregister_tx.send(guild_id).await?;
        Ok(())
    }

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

        if let Some(cache_entry) = self.map.get(&guild_id) {
            if cache_entry.is_expired_high_priority() {
                debug!(
                    "queuing priority product cache refresh for {} due to expiry",
                    guild_id.get()
                );
                self.high_priority_tx.send(guild_id).await?;
            }

            // got an entry; return it immediately, even if it's expired
            Ok(f(cache_entry.value()))
        } else {
            // expired or vacant entry
            debug!("initializing product cache in {}", guild_id.get());
            let api_key = &context
                .data()
                .db
                .get_jinxxy_api_key(guild_id)
                .await?
                .ok_or_else(|| JinxError::new(MISSING_API_KEY_MESSAGE))?;
            let guild_cache = GuildCache::new(api_key).await?;

            // You might wonder why I don't use the same dashmap entry here as I do above in the initial lookup.
            // I purposefully drop the dashmap lock (aka the entry) across the .await to avoid deadlocks, which DO happen.
            let guild_cache = self.map.entry(guild_id).insert(guild_cache);
            Ok(f(&guild_cache))
        }
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
            .retain(|_guild_id, cache_entry| !cache_entry.is_expired_high_priority());

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

    pub async fn product_name_to_ids(
        &self,
        context: &Context<'_>,
        product_name: &str,
    ) -> Result<Vec<String>, Error> {
        self.get(context, |cache_entry| {
            cache_entry.product_name_to_ids(product_name).to_vec()
        })
        .await
    }

    pub async fn product_version_name_to_version_ids(
        &self,
        context: &Context<'_>,
        product_name: &str,
    ) -> Result<Vec<ProductVersionId>, Error> {
        self.get(context, |cache_entry| {
            cache_entry
                .product_version_name_to_version_ids(product_name)
                .to_vec()
        })
        .await
    }
}

pub struct GuildCache {
    /// id to name
    product_id_to_name_map: HashMap<String, String, ahash::RandomState>,
    /// name to id
    product_name_to_id_map: HashMap<String, Vec<String>, ahash::RandomState>,
    /// completes lowercase name to name with correct case
    product_name_trie: Trie<u8, String>,
    /// number of products
    product_count: usize,
    /// version_id to name
    product_version_id_to_name_map: HashMap<ProductVersionId, String, ahash::RandomState>,
    /// name to version_id
    product_name_to_version_id_map: HashMap<String, Vec<ProductVersionId>, ahash::RandomState>,
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
    async fn new(api_key: &str) -> Result<GuildCache, Error> {
        let partial_products: Vec<PartialProduct> = jinxxy::get_products(api_key).await?;

        let products: Vec<FullProduct> = jinxxy::get_full_products(api_key, partial_products)
            .await?
            .into_iter()
            .filter(|product| !product.name.is_empty()) // products with empty names are kinda weird, so I'm just gonna filter them to avoid any potential pitfalls
            .collect();

        // convert into map tuples for products without versions
        let product_name_info: Vec<NameInfo> = products
            .iter()
            .map(|product| {
                let id = product.id.clone();
                let product_name = util::truncate_string_for_discord_autocomplete(&product.name);
                NameInfo { id, product_name }
            })
            .collect();

        // convert into map tuples for product versions
        let product_version_name_info: Vec<VersionNameInfo> = products
            .into_iter()
            .flat_map(|product| {
                let null_name_info = VersionNameInfo {
                    id: ProductVersionId::from_product_id(&product.id),
                    product_version_name: util::product_display_name(&product.name, None),
                };
                let null_iter = std::iter::once(null_name_info);

                let iter = product.versions.into_iter().map(move |version| {
                    let id = ProductVersionId {
                        product_id: product.id.clone(),
                        product_version_id: Some(version.id.clone()),
                    };
                    let product_version_name =
                        util::product_display_name(&product.name, Some(version.name.as_str()));
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
        let mut product_name_to_id_map: HashMap<String, Vec<String>, ahash::RandomState> =
            Default::default();
        for name_info in product_name_info {
            product_name_to_id_map
                .entry(name_info.product_name)
                .or_default()
                .push(name_info.id);
        }

        // build reverse map with versions
        let mut product_name_to_version_id_map: HashMap<
            String,
            Vec<ProductVersionId>,
            ahash::RandomState,
        > = Default::default();
        for name_info in product_version_name_info {
            product_name_to_version_id_map
                .entry(name_info.product_version_name)
                .or_default()
                .push(name_info.id);
        }

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

    fn product_name_to_ids(&self, product_name: &str) -> &[String] {
        self.product_name_to_id_map
            .get(product_name)
            .map(|vec| vec.as_slice())
            .unwrap_or_default()
    }

    fn product_version_name_to_version_ids(&self, product_name: &str) -> &[ProductVersionId] {
        self.product_name_to_version_id_map
            .get(product_name)
            .map(|vec| vec.as_slice())
            .unwrap_or_default()
    }

    fn product_count(&self) -> usize {
        self.product_count
    }

    fn product_version_count(&self) -> usize {
        self.product_version_count
    }

    /// check if the entry is a wee bit expired
    fn is_expired_high_priority(&self) -> bool {
        self.create_time.elapsed() > HIGH_PRIORITY_CACHE_EXPIRY_TIME
    }

    /// check if the entry is _very_ expired
    fn is_expired_low_priority(&self) -> bool {
        self.create_time.elapsed() > LOW_PRIORITY_CACHE_EXPIRY_TIME
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
