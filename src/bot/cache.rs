// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! A guild-level cache of Jinxxy API results.
//!
//! This is needed because there are some cases, such as autocomplete, where we need API results
//! however autocomplete needs results at high frequency with low latency: in other words a naive
//! API fetch for each autocomplete would be extremely spammy.
//!
//! The idea here is we have a cache with a short expiry time (maybe 60s) and we reuse the results.

use crate::bot::MISSING_API_KEY_MESSAGE;
use crate::bot::{SECONDS_PER_DAY, util};
use crate::db;
use crate::db::JinxDb;
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::http::jinxxy::{
    LoadedProduct, PartialProduct, ProductNameInfo, ProductNameInfoValue, ProductVersionId, ProductVersionNameInfo,
};
use crate::time::SimpleTime;
use poise::serenity_prelude::GuildId;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;
use tokio::time::{Duration, timeout};
use tracing::{debug, error, info, warn};
use trie_rs::map::{Trie, TrieBuilder};

type Error = Box<dyn std::error::Error + Send + Sync>;
type MapType = Arc<papaya::HashMap<GuildId, GuildCache, ahash::RandomState>>;

/// How long before the high priority worker considers a cache entry expired. Currently 1 minute.
const HIGH_PRIORITY_CACHE_EXPIRY_TIME: Duration = Duration::from_secs(60);
/// How long before the low priority worker considers a cache entry expired. Currently 24 hours.
const DEFAULT_LOW_PRIORITY_CACHE_EXPIRY_TIME: Duration = Duration::from_secs(SECONDS_PER_DAY);
/// Some small quantity of time to wait on top of the low priority cache expiry time. This is intended to act as wiggle
/// room avoid waking right before an entry expires (I'd rather wake a bit after instead).
const LOW_PRIORITY_CACHE_EXPIRY_TIME_FUDGE_FACTOR: Duration = Duration::from_secs(60);
/// Minimum time the low priority worker will use as its poll timeout
const MIN_SLEEP_DURATION: Duration = Duration::from_millis(2000);
/// Number of items to limit lists used in autocompletion to.
///
/// Exceeding the limit results in:
/// ```
/// WARN poise::dispatch::slash: couldn't send autocomplete response: Invalid Form Body (data.choices: Must be 25 or fewer in length.)
/// ```
const AUTOCOMPLETE_RESULT_LIMIT: usize = 25;

/// Missless cache for Jinxxy product and version names.
///
/// "Missless" means cache reads will _always_ hit the cache: even for expired entries. Expired entries are not deleted.
/// This cache has no retention time: instead expiry is handled by a background task which re-warms old cache entries.
///
/// This admittedly unusual design is due to the Jinxxy API's extraordinarily high latencies. On a _good_ day it can
/// take >3s to enumerate all product and version names for a guild. On a bad day it can take up to 15s, and beyond that
/// point the API will actually time out server-side and start returning 500s. We need product and version names for
/// text autocompletion which absolutely _must_ be low-latency to feel usable. Sub-second latency is a must, and faster
/// is always better, especially because network latency from the bot to the user's Discord client will already be
/// adding a noticeable delay.
///
/// Clones of this struct reference the same underlying data: you do not need to wrap this in an Arc.
#[derive(Clone)]
pub struct ApiCache {
    map: MapType,
    high_priority_tx: mpsc::Sender<GuildId>,
    /// sending a None is considered a "bump", indicating we should re-check the next queued item
    refresh_register_tx: mpsc::Sender<Option<GuildId>>,
    refresh_unregister_tx: mpsc::Sender<GuildId>,
}

impl ApiCache {
    pub fn new(db: Arc<JinxDb>) -> Self {
        let map: MapType = Default::default();

        const QUEUE_SIZE: usize = 1024;
        let (high_priority_tx, mut high_priority_rx) = mpsc::channel(QUEUE_SIZE);
        let (refresh_register_tx, mut refresh_register_rx) = mpsc::channel(QUEUE_SIZE);
        let (refresh_unregister_tx, mut refresh_unregister_rx) = mpsc::channel(QUEUE_SIZE);

        /* High priority refresh task.

        This is used when we have strong reason to believe a user will be needing the cache imminently: for example,
        initializing the guild or linking a product is a strong hint the user will need a product list soon.

        As long as the substantially shorter HIGH_PRIORITY_CACHE_EXPIRY_TIME is not exceeded, this always hits the
        Jinxxy API directly if the memory cache is cold (in comparison to the low-priority refresh which does a DB read
        if the memory cache misses).

        This is handled via a simple FIFO queue with no delays. The effect is that only one high-priority refresh can be
        in-flight at a time.

        High-priority refreshes are moderately costly, as every single product in the guild will be queried in parallel.
        This has been measured to have a substantial negative impact on Jinxxy API response times, as it struggles to
        handle concurrent requests for the same API key. It is unknown if this issue is localized to an API key: it
        could be that Jinxxy simply cannot handle concurrent requests globally.
        */
        {
            let db = db.clone();
            let map = map.clone();
            tokio::task::spawn(async move {
                while let Some(guild_id) = high_priority_rx.recv().await {
                    // first, check to make sure this entry is still expired
                    let needs_refresh = map
                        .pin()
                        .get(&guild_id)
                        .map(|entry| entry.is_expired_high_priority())
                        .unwrap_or(true);

                    if needs_refresh {
                        match db.get_jinxxy_api_key(guild_id).await {
                            Ok(api_key) => match api_key {
                                Some(api_key) => {
                                    // the high-priority API hit
                                    match GuildCache::from_jinxxy_api::<true>(&db, &api_key, guild_id).await {
                                        Ok(guild_cache) => {
                                            map.pin().insert(guild_id, guild_cache);
                                        }
                                        Err(e) => {
                                            warn!(
                                                "Error initializing API cache during high-priority refresh for {}: {:?}",
                                                guild_id.get(),
                                                e
                                            );
                                        }
                                    }
                                }
                                None => {
                                    warn!(
                                        "High-priority refresh was somehow triggered for guild {}, which has no api key set!",
                                        guild_id
                                    );
                                }
                            },
                            Err(e) => {
                                warn!(
                                    "Error retrieving API key during high-priority refresh for {}: {:?}",
                                    guild_id.get(),
                                    e
                                );
                            }
                        }
                    }
                }
                debug!("high-priority cache worker task is shutting down");
            });
        }

        /* Low priority refresh task

        Handles periodic background cache warming. We have no reason to suspect the user will need the data soon, so
        this task works at a relaxed pace. The task will load a single guild at a time, and each product in the guild is
        loaded serially which is far less intensive on the Jinxxy API than a parallel load.

        On bot start, each guild is registered, causing a cache load in this task. This is guaranteed to miss the memory
        cache (remember, the bot just started), so it falls back to a DB cache read. This is the mechanism by which the
        cache is re-warmed from disk.

        This is a priority queue, where the oldest cache line is at the head of the queue. The task wakes when a guild
        is registered, unregistered, or after the calculated delay to the queue head expiring. This delay is updated
        every time the task wakes. Additionally, the task is required to sleep a certain minimum interval every time it
        finishes work in order to prevent API spam and spinning in the case of bugs... there is a lot of fiddly
        timekeeping math at play here, so this code has been the subject of a disproportionate number of bugs.


         */
        {
            let db = db;
            let map = map.clone();
            tokio::task::spawn(async move {
                // set of all registered guilds ids
                let mut guild_set = HashSet::with_hasher(ahash::RandomState::default());

                // priority queue of all registered guilds. Returns oldest entry first.
                let mut queue = BinaryHeap::new();

                // time we wait on a new entry to show up before we run the timeout task, which pops the queue
                // by default we sleep until we get an event. In some cases we sleep for a certain maximum time.
                let mut sleep_duration: Option<Duration> = None;

                'outer: loop {
                    let received_event = if let Some(sleep_duration) = sleep_duration {
                        // note: there is an undocumented edge case where tokio's timeout function treats 0 as "no timeout"
                        // because we have a min sleep time we dodge, this but be wary of passing Duration::ZERO in there.

                        // do a receive or a timeout, whatever happens first
                        let sleep_duration = MIN_SLEEP_DURATION.max(sleep_duration);
                        debug!("sleeping for {}s", sleep_duration.as_secs());
                        timeout(sleep_duration, refresh_register_rx.recv())
                            .await
                            .map_err(|_| ())
                    } else {
                        // we have no data yet, so there is no reason to have a timeout
                        Ok(refresh_register_rx.recv().await)
                    };
                    let low_priority_cache_expiry_time = match db.get_low_priority_cache_expiry_time().await {
                        Ok(Some(expiry_time)) => expiry_time,
                        Ok(None) => DEFAULT_LOW_PRIORITY_CACHE_EXPIRY_TIME,
                        Err(e) => {
                            // this is kind of a big problem, as if the default is lower than what we're SUPPOSED to
                            // have read from the DB, then we'd spam the heck out of the API if we just fell back to the
                            // default.
                            error!(
                                "Error reading low_priority_cache_expiry_time from DB. Stopping cache refresh task now! {e:?}"
                            );
                            break 'outer;
                        }
                    };
                    match received_event {
                        Ok(Some(Some(guild_id))) => {
                            // new guild has appeared; insert it into the queue (as long as it isn't a duplicate)
                            if guild_set.insert(guild_id) {
                                queue.push(GuildQueueRef {
                                    guild_id,
                                    create_time: SimpleTime::UNIX_EPOCH, // some arbitrarily old placeholder value
                                });

                                // update the sleep time
                                let next_queue_entry =
                                    queue.peek().expect("queue should not be empty immediately after push");
                                // remaining time until the entry hits the expiration time, or 0 if it's already expired
                                let remaining = next_queue_entry.remaining_time_until_low_priority_expiry(
                                    low_priority_cache_expiry_time,
                                    SimpleTime::now(),
                                );
                                // the queue is not empty, so we'll time out around the time the next entry is supposed to expire
                                debug!(
                                    "new guild {} registered; low-priority worker will sleep for {}s",
                                    guild_id,
                                    remaining.as_secs()
                                );
                                sleep_duration = Some(remaining);
                            }
                        }
                        Ok(Some(None)) => {
                            // cache bump! we've been requested to take a look at the next cache item to see if the sleep time is still correct
                            if let Some(next_queue_entry) = queue.peek() {
                                // remaining time until the entry hits the expiration time, or 0 if it's already expired
                                let remaining = next_queue_entry.remaining_time_until_low_priority_expiry(
                                    low_priority_cache_expiry_time,
                                    SimpleTime::now(),
                                );
                                // the queue is not empty, so we'll time out around the time the next entry is supposed to expire
                                debug!(
                                    "cache bumped; low-priority worker will sleep for {}s",
                                    remaining.as_secs()
                                );
                                sleep_duration = Some(remaining);
                            } else {
                                // queue is totally empty, so sleep until we get an event
                                sleep_duration = None
                            }
                        }
                        Ok(None) => {
                            // channel is broken, so stop this task
                            break 'outer;
                        }
                        Err(()) => {
                            // ok, we got a timeout.
                            // first, handle deregistration in an inner loop
                            'inner: loop {
                                match refresh_unregister_rx.try_recv() {
                                    Ok(guild_id) => {
                                        if guild_set.remove(&guild_id) {
                                            queue.retain(|queue_entry| queue_entry.guild_id != guild_id);
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
                            // the event processing is now done, so process guilds until none are expired in a "work loop"

                            let mut now = SimpleTime::now(); // initialize it to some arbitrary default: we set it later.
                            let mut work_remaining = true;
                            let mut work_counter = 0;
                            let mut touched_guild_set = HashSet::with_hasher(ahash::RandomState::default());
                            const MAX_WORK_COUNT: u16 = 250;
                            while work_remaining {
                                // find the first guild that needs a refresh. This is a simple queue pop.
                                if let Some(mut queue_entry) = queue.pop() {
                                    work_counter += 1;

                                    // if we've processed too many work items this loop, then stop
                                    work_remaining &= work_counter < MAX_WORK_COUNT;

                                    // if the queue is now empty no reason to go again
                                    let queue_empty = queue.is_empty();
                                    if queue_empty {
                                        warn!("stopping work loop because queue is empty!");
                                    }
                                    work_remaining &= !queue_empty;

                                    // record that we've touched this guild ID in the work loop
                                    touched_guild_set.insert(queue_entry.guild_id.get());

                                    // update the current time: we'll need it a couple of times and I want it to keep the same reading
                                    now = SimpleTime::now();

                                    // figure out what state this entry is in
                                    let (load, try_db_load) = map
                                        .pin()
                                        .get(&queue_entry.guild_id)
                                        .map(|cache_entry| {
                                            // sync our thread-local copy of the create time with the source of truth (the cache)
                                            queue_entry.create_time = cache_entry.create_time;

                                            if cache_entry.is_expired_low_priority(low_priority_cache_expiry_time, now)
                                            {
                                                // entry exists and was expired
                                                // no need to do a DB load because we obviously already have data in memory
                                                (true, false)
                                            } else {
                                                // entry exists and was not expired
                                                // this can happen if that entry was touched externally (e.g. a high priority refresh) before we saw it
                                                debug!("skipping unexpired guild {}", queue_entry.guild_id.get());
                                                (false, false)
                                            }
                                        })
                                        .unwrap_or((true, true)); // or else entry did NOT exist in memory, so it's worth trying a db load

                                    let guild_valid = if load {
                                        // refresh that guild
                                        match db.get_jinxxy_api_key(queue_entry.guild_id).await {
                                            Ok(api_key) => {
                                                match api_key {
                                                    Some(api_key) => {
                                                        debug!(
                                                            "starting low priority refresh of cache for {}",
                                                            queue_entry.guild_id.get()
                                                        );

                                                        let guild_cache = if try_db_load {
                                                            // try a DB load instead of an API load
                                                            let db_result =
                                                                GuildCache::from_db(&db, queue_entry.guild_id).await;
                                                            match db_result {
                                                                Ok(Some(cache_entry)) => {
                                                                    // DB read worked: just return it
                                                                    if cache_entry.is_expired_low_priority(
                                                                        low_priority_cache_expiry_time,
                                                                        now,
                                                                    ) {
                                                                        debug!(
                                                                            "DB cache hit trying to initialize API cache for {}, but was expired. It will be refreshed once we loop around the guild list again.",
                                                                            queue_entry.guild_id.get()
                                                                        );
                                                                    }
                                                                    Ok(cache_entry)
                                                                }
                                                                Ok(None) => {
                                                                    // DB had no data
                                                                    debug!(
                                                                        "DB cache miss trying to initialize API cache for {}. Falling back to API load.",
                                                                        queue_entry.guild_id.get()
                                                                    );
                                                                    GuildCache::from_jinxxy_api::<false>(
                                                                        &db,
                                                                        &api_key,
                                                                        queue_entry.guild_id,
                                                                    )
                                                                    .await
                                                                }
                                                                Err(e) => {
                                                                    // uh this is probably bad because DB read shouldn't fail
                                                                    // fall back to an API load anyways
                                                                    error!(
                                                                        "DB read failed when trying to initialize API cache for {}. Falling back to API load: {:?}",
                                                                        queue_entry.guild_id.get(),
                                                                        e
                                                                    );
                                                                    GuildCache::from_jinxxy_api::<false>(
                                                                        &db,
                                                                        &api_key,
                                                                        queue_entry.guild_id,
                                                                    )
                                                                    .await
                                                                }
                                                            }
                                                        } else {
                                                            GuildCache::from_jinxxy_api::<false>(
                                                                &db,
                                                                &api_key,
                                                                queue_entry.guild_id,
                                                            )
                                                            .await
                                                        };

                                                        match guild_cache {
                                                            Ok(cache_entry) => {
                                                                // we got a new cache entry from either db or jinxxy!

                                                                // update our queue entry so it's corrected before we re-insert it into the queue
                                                                queue_entry.create_time = cache_entry.create_time;

                                                                // actually update the dang cache!
                                                                map.pin().insert(queue_entry.guild_id, cache_entry);
                                                                true
                                                            }
                                                            Err(e) => {
                                                                warn!(
                                                                    "Error initializing API cache during low-priority refresh for {}: {:?}",
                                                                    queue_entry.guild_id.get(),
                                                                    e
                                                                );

                                                                match jinxxy::get_own_user(&api_key).await {
                                                                    Ok(auth_user) => {
                                                                        // we were able to do an API request with this key...
                                                                        // okay must have been a weird fluke, we'll leave this guild registered
                                                                        if !auth_user.has_required_scopes() {
                                                                            warn!(
                                                                                "Could not initialize API cache for guild {}, possibly because it lacks required scopes. Will try it again later.",
                                                                                queue_entry.guild_id.get()
                                                                            );
                                                                        }
                                                                        true
                                                                    }
                                                                    Err(e) => {
                                                                        info!(
                                                                            "error checking /me for guild {}, will unregister now: {:?}",
                                                                            queue_entry.guild_id.get(),
                                                                            e
                                                                        );
                                                                        false
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                    None => {
                                                        info!(
                                                            "low-priority refresh was triggered for guild {}, which has no api key set! Unregistering now.",
                                                            queue_entry.guild_id.get()
                                                        );
                                                        false
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                warn!(
                                                    "Error retrieving API key during low-priority refresh for {}. Unregistering now: {:?}",
                                                    queue_entry.guild_id.get(),
                                                    e
                                                );
                                                false
                                            }
                                        }
                                    } else {
                                        // if we didn't need to load the guild, then treat it as still valid
                                        true
                                    };

                                    if guild_valid {
                                        // done loading the guild; time to put it back in the queue
                                        queue.push(queue_entry);
                                    } else {
                                        // something about the guild was screwed up so we're just going to unregister it
                                        guild_set.remove(&queue_entry.guild_id);
                                    }

                                    // if we haven't touched the next guild ID yet, then go again (true)
                                    // if there is no next guild, then do not go again (false)
                                    let next_guild_pending_work = queue
                                        .peek()
                                        .map(|next_guild| !touched_guild_set.contains(&next_guild.guild_id.get()))
                                        .unwrap_or(false);
                                    if !next_guild_pending_work {
                                        debug!("stopping work loop because next guild has already been touched");
                                    }
                                    work_remaining &= next_guild_pending_work;
                                } else {
                                    warn!("ended low-priority work loop due to empty work queue!");
                                    work_remaining = false;
                                }
                            } // end work loop

                            if work_counter >= MAX_WORK_COUNT {
                                warn!("ended low-priority work loop due to exceeding maximum loop count!");
                            }

                            // update the sleep time
                            if let Some(next_queue_entry) = queue.peek() {
                                // remaining time until the entry hits the expiration time, or 0 if it's already expired
                                let remaining = next_queue_entry
                                    .remaining_time_until_low_priority_expiry(low_priority_cache_expiry_time, now);

                                // the queue is not empty, so we'll time out around the time the next entry is supposed to expire
                                if !remaining.is_zero() {
                                    debug!(
                                        "low-priority worker caught up; will sleep for {}s. Next up is {}",
                                        remaining.as_secs(),
                                        next_queue_entry.guild_id.get()
                                    );
                                }

                                // this is the normal case for setting `sleep_duration`
                                sleep_duration = Some(remaining);
                            } else {
                                // the queue was empty, so we can actually sleep forever (or rather until the rx triggers) as there is no work to do
                                debug!("low-priority worker has ran out of work!");
                                sleep_duration = None;
                            }
                        }
                    }
                }
                // end of outer loop
                // channel is broken, so we're stopping the task
                debug!("low-priority cache worker task is shutting down");
            });
        }

        Self {
            map,
            high_priority_tx,
            refresh_register_tx,
            refresh_unregister_tx,
        }
    }

    /// Trigger a one-time high-priority refresh of this guild in the cache.
    async fn refresh_guild_in_cache(&self, guild_id: GuildId) -> Result<(), Error> {
        self.high_priority_tx.send(guild_id).await?;
        Ok(())
    }

    /// Register a guild in the cache. The guild will have its cache entry periodically warmed automatically.
    pub async fn register_guild_in_cache(&self, guild_id: GuildId) -> Result<(), Error> {
        self.refresh_register_tx.send(Some(guild_id)).await?;
        Ok(())
    }

    /// Bump the low priority cache queue.
    pub async fn bump(&self) -> Result<(), Error> {
        self.refresh_register_tx.send(None).await?;
        Ok(())
    }

    /// Unregister a guild in the cache. The guild will no longer have its cache entry periodically warmed automatically.
    pub async fn unregister_guild_in_cache(&self, guild_id: GuildId) -> Result<(), Error> {
        self.refresh_unregister_tx.send(guild_id).await?;
        Ok(())
    }

    /// Get a cache line and run some process on it, returning the result.
    ///
    /// If the cache is empty or expired, the underlying API will be hit.
    pub async fn get<F, T>(&self, db: &JinxDb, guild_id: GuildId, f: F) -> Result<T, Error>
    where
        F: FnOnce(&GuildCache) -> T,
    {
        let (result, expired) = if let Some(entry) = self.map.pin().get(&guild_id) {
            // got an entry; return it immediately, even if it's expired
            let result = f(entry);
            let expired = entry.is_expired_high_priority();
            (result, expired)
        } else {
            info!(
                "cache missed! Falling back to direct API request for {}",
                guild_id.get()
            );
            let api_key = db
                .get_jinxxy_api_key(guild_id)
                .await?
                .ok_or_else(|| JinxError::new(MISSING_API_KEY_MESSAGE))?;

            // we had a cache miss, implying that there's no reason to load from db so we go straight through to the jinxxy API
            let guild_cache = GuildCache::from_jinxxy_api::<true>(db, api_key.as_str(), guild_id).await?;
            let result = f(&guild_cache);

            // update the cache
            self.map.pin().insert(guild_id, guild_cache);

            // it is nonsensical for a freshly-created entry to be expired, so hardcode expired=false
            (result, false)
        };

        // ensure expired guild gets a priority refresh, as it has a very high chance of having get() called again soon
        if expired {
            debug!(
                "queuing priority product cache refresh for {} due to expiry",
                guild_id.get()
            );
            self.refresh_guild_in_cache(guild_id).await?;
        }

        // ensure this guild is registered in the cache
        self.register_guild_in_cache(guild_id).await?;

        Ok(result)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn product_count(&self) -> usize {
        let map = self.map.pin();
        map.values().map(|guild_cache| guild_cache.product_count()).sum()
    }

    pub fn product_version_count(&self) -> usize {
        let map = self.map.pin();
        map.values()
            .map(|guild_cache| guild_cache.product_version_count())
            .sum()
    }

    /// Remove all cache entries
    pub fn clear(&self) {
        let map = self.map.pin();
        map.clear();
    }

    /// Get product names with prefix, up to Discord's limit
    pub async fn autocomplete_product_names_with_prefix(
        &self,
        db: &JinxDb,
        guild_id: GuildId,
        prefix: &str,
    ) -> Result<Vec<String>, Error> {
        self.get(db, guild_id, |cache_entry| {
            cache_entry
                .product_names_with_prefix(prefix)
                .take(AUTOCOMPLETE_RESULT_LIMIT)
                .collect()
        })
        .await
    }

    /// Get product version names with prefix, up to Discord's limit
    pub async fn autocomplete_product_version_names_with_prefix(
        &self,
        db: &JinxDb,
        guild_id: GuildId,
        prefix: &str,
    ) -> Result<Vec<String>, Error> {
        self.get(db, guild_id, |cache_entry| {
            cache_entry
                .product_version_names_with_prefix(prefix)
                .take(AUTOCOMPLETE_RESULT_LIMIT)
                .collect()
        })
        .await
    }

    pub async fn product_name_to_ids(
        &self,
        db: &JinxDb,
        guild_id: GuildId,
        product_name: &str,
    ) -> Result<Vec<String>, Error> {
        self.get(db, guild_id, |cache_entry| {
            cache_entry.product_name_to_ids(product_name).to_vec()
        })
        .await
    }

    pub async fn product_version_name_to_version_ids(
        &self,
        db: &JinxDb,
        guild_id: GuildId,
        product_name: &str,
    ) -> Result<Vec<ProductVersionId>, Error> {
        self.get(db, guild_id, |cache_entry| {
            cache_entry.product_version_name_to_version_ids(product_name).to_vec()
        })
        .await
    }
}

/// A reference to a guild cache entry to be kept in a max-heap. This is NOT the actual cache entry!
struct GuildQueueRef {
    /// ID of the guild in the actual cache
    guild_id: GuildId,
    /// A copy of the last-known create time for this cache entry. This is NOT the actual cache entry create time, so it may desync!
    create_time: SimpleTime,
}

/// We want lower create_time to have higher priority, so we reverse the ord.
/// This will cause BinaryHeap to yield the smallest create_time, aka the earliest create_time
impl Ord for GuildQueueRef {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .create_time
            .cmp(&self.create_time)
            .then(other.guild_id.cmp(&self.guild_id))
    }
}

impl PartialOrd<Self> for GuildQueueRef {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq<Self> for GuildQueueRef {
    fn eq(&self, other: &Self) -> bool {
        self.create_time == other.create_time && self.guild_id == other.guild_id
    }
}

impl Eq for GuildQueueRef {}

impl GuildQueueRef {
    /// remaining time until the entry hits the expiration time, or 0 if it's already expired
    fn remaining_time_until_low_priority_expiry(
        &self,
        low_priority_cache_expiry_time: Duration,
        now: SimpleTime,
    ) -> Duration {
        let elapsed = now.duration_since(self.create_time);
        (low_priority_cache_expiry_time + LOW_PRIORITY_CACHE_EXPIRY_TIME_FUDGE_FACTOR)
            .checked_sub(elapsed)
            .unwrap_or_default()
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
    create_time: SimpleTime,
}

impl GuildCache {
    /// Create a cache entry by hitting the Jinxxy API. This is very costly and involves a lot of API hits.
    /// Upon success, it will automatically persist the retrieved data to the DB.
    async fn from_jinxxy_api<const PARALLEL: bool>(
        db: &JinxDb,
        api_key: &str,
        guild_id: GuildId,
    ) -> Result<GuildCache, Error> {
        // list products
        let partial_products: Vec<PartialProduct> = jinxxy::get_products(api_key).await?;

        // get details for each product
        let mut products: Vec<LoadedProduct> =
            jinxxy::get_full_products::<PARALLEL>(db, api_key, guild_id, partial_products)
                .await?
                .into_iter()
                .filter(|product| {
                    // products with empty names are kinda weird, so I'm just gonna filter them to avoid any potential pitfalls
                    match product {
                        LoadedProduct::Api(product) => !product.name.is_empty(),
                        LoadedProduct::Cached { .. } => true,
                    }
                })
                .collect();

        // convert into map tuples for products without versions
        let product_name_info: Vec<ProductNameInfo> = products
            .iter_mut()
            .map(|product| match product {
                LoadedProduct::Api(product) => {
                    let id = product.id.clone();
                    let product_name = util::truncate_string_for_discord_autocomplete(&product.name);
                    let etag = product.etag.clone();
                    ProductNameInfo {
                        id,
                        value: ProductNameInfoValue { product_name, etag },
                    }
                }
                LoadedProduct::Cached { product_info, .. } => product_info.take().expect(
                    "product_info is specifically in an option so I can take() it later, this should not have failed",
                ),
            })
            .collect();

        // convert into map tuples for product versions
        let product_version_name_info: Vec<ProductVersionNameInfo> = products
            .into_iter()
            .flat_map(|product| match product {
                LoadedProduct::Api(product) => {
                    let null_name_info = ProductVersionNameInfo {
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
                        ProductVersionNameInfo {
                            id,
                            product_version_name,
                        }
                    });
                    let iter = null_iter.chain(iter);
                    let iter: Box<dyn Iterator<Item = _>> = Box::new(iter);
                    iter
                }
                LoadedProduct::Cached { versions, .. } => Box::new(versions.into_iter()),
            })
            .collect();

        let create_time = SimpleTime::now();

        Self::persist(
            db,
            guild_id,
            product_name_info.clone(),
            product_version_name_info.clone(),
            create_time,
        )
        .await?;
        Self::from_products(product_name_info, product_version_name_info, create_time)
    }

    /// Attempt to create a cache entry from the DB. This is quite cheap compared to hitting Jinxxy.
    async fn from_db(db: &JinxDb, guild_id: GuildId) -> Result<Option<GuildCache>, Error> {
        let db_cache_entry = db.get_guild_cache(guild_id).await?;

        if db_cache_entry.product_name_info.is_empty()
            && db_cache_entry.product_version_name_info.is_empty()
            && db_cache_entry.cache_time.as_epoch_millis() == 0
        {
            // don't even try building this mildly expensive struct if we have no data
            Ok(None)
        } else {
            Ok(Some(Self::from_products(
                db_cache_entry.product_name_info,
                db_cache_entry.product_version_name_info,
                db_cache_entry.cache_time,
            )?))
        }
    }

    /// Persist cache state to DB. This needs owned values, because they're being moved into a different thread.
    async fn persist(
        db: &JinxDb,
        guild_id: GuildId,
        product_name_info: Vec<ProductNameInfo>,
        product_version_name_info: Vec<ProductVersionNameInfo>,
        cache_time: SimpleTime,
    ) -> Result<(), Error> {
        let db_cache_entry = db::GuildCache {
            product_name_info,
            product_version_name_info,
            cache_time,
        };
        db.persist_guild_cache(guild_id, db_cache_entry).await?;
        Ok(())
    }

    /// Create a cache entry from values.
    fn from_products(
        product_name_info: Vec<ProductNameInfo>,
        product_version_name_info: Vec<ProductVersionNameInfo>,
        create_time: SimpleTime,
    ) -> Result<GuildCache, Error> {
        let product_count = product_name_info.len();
        let product_version_count = product_version_name_info.len();

        // build trie without versions
        let product_name_trie = {
            let mut trie_builder = TrieBuilder::new();
            for name_info in product_name_info.iter() {
                let name = &name_info.value.product_name;
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
            .map(|name_info| (name_info.id.clone(), name_info.value.product_name.clone()))
            .collect();

        // build forward map with versions
        let product_version_id_to_name_map = product_version_name_info
            .iter()
            .map(|name_info| (name_info.id.clone(), name_info.product_version_name.clone()))
            .collect();

        // build reverse map without versions
        let mut product_name_to_id_map: HashMap<String, Vec<String>, ahash::RandomState> = Default::default();
        for name_info in product_name_info {
            product_name_to_id_map
                .entry(name_info.value.product_name)
                .or_default()
                .push(name_info.id);
        }

        // build reverse map with versions
        let mut product_name_to_version_id_map: HashMap<String, Vec<ProductVersionId>, ahash::RandomState> =
            Default::default();
        for name_info in product_version_name_info {
            product_name_to_version_id_map
                .entry(name_info.product_version_name)
                .or_default()
                .push(name_info.id);
        }

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

    fn product_names_with_prefix<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = String> + 'a {
        self.product_name_trie
            .predictive_search(prefix.to_lowercase())
            .map(|(_key, value): (Vec<u8>, &String)| value.to_string())
    }

    fn product_version_names_with_prefix<'a>(&'a self, prefix: &'a str) -> impl Iterator<Item = String> + 'a {
        self.product_version_name_trie
            .predictive_search(prefix.to_lowercase())
            .map(|(_key, value): (Vec<u8>, &String)| value.to_string())
    }

    pub fn product_id_to_name(&self, product_id: &str) -> Option<&str> {
        self.product_id_to_name_map.get(product_id).map(|str| str.as_str())
    }

    pub fn product_version_id_to_name(&self, product_version_id: &ProductVersionId) -> Option<&str> {
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
    fn is_expired_low_priority(&self, low_priority_cache_expiry_time: Duration, now: SimpleTime) -> bool {
        now.duration_since(self.create_time) > low_priority_cache_expiry_time
    }

    pub fn product_name_iter(&self) -> impl Iterator<Item = &str> {
        self.product_name_to_id_map.keys().map(|str| str.as_str())
    }
}

#[cfg(test)]
mod test {
    use trie_rs::map::TrieBuilder;

    #[test]
    fn test_trie_empty_prefix() {
        let tuples = [("foo", "foo_data"), ("bar", "bar_data"), ("baz", "baz_data")];

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
                "could not find expected value: {expected}"
            );
        }
    }
}
