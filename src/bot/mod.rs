// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

mod cache;
mod commands;
mod error_handler;
mod event_handler;
pub mod util;

use crate::bot::cache::ApiCache;
use crate::bot::error_handler::error_handler;
use crate::db::{JinxDb, SqliteWalCheckpoint};
use crate::error::JinxError;
use commands::*;
use poise::{Command, PrefixFrameworkOptions, serenity_prelude as serenity};
use serenity::{
    ActivityData, Client, ClientBuilder, Colour, CreateEmbed, CreateMessage, GatewayIntents, ShardRunnerMessage, Token,
};
use std::sync::{Arc, LazyLock};
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

/// Number of items to limit lists used in autocompletion to.
///
/// Exceeding the limit results in:
/// ```
/// WARN poise::dispatch::slash: couldn't send autocomplete response: Invalid Form Body (data.choices: Must be 25 or fewer in length.)
/// ```
pub const AUTOCOMPLETE_RESULT_LIMIT: usize = 25;
/// Maximum character length in a single autocompletion result. Discord docs are ambiguous if this is characters or bytes.
pub const AUTOCOMPLETE_CHARACTER_LIMIT: usize = 100;
/// Maximum character length in a `custom_id` field. Discord docs are ambiguous if this is characters or bytes.
pub const CUSTOM_ID_CHARACTER_LIMIT: usize = 100;

const SECONDS_PER_MINUTE: u64 = 60;
const MINUTES_PER_HOUR: u64 = 60;
const HOURS_PER_DAY: u64 = 24;
const SECONDS_PER_DAY: u64 = SECONDS_PER_MINUTE * MINUTES_PER_HOUR * HOURS_PER_DAY;
const SECONDS_PER_HOUR: u64 = SECONDS_PER_MINUTE * MINUTES_PER_HOUR;

/// Message shown to admins when the Jinxxy API key is missing
pub static MISSING_API_KEY_MESSAGE: &str = "Jinxxy API key is not set: please use the `/add_store` command to set it.";
/// Message shown to admins when there's no store link for the username they provided via some command
pub static MISSING_STORE_LINK_MESSAGE: &str = "No linked store with that username was found.";

const REGISTER_MODAL_ID: &str = "jinx_register_modal";

/// Version number for the guild commands that is incremented whenever a command's definition (not code!) changes,
/// or a new command is added
const GUILD_COMMAND_VERSION: i64 = 0;

/// commands to be installed globally
const GLOBAL_COMMANDS: &[fn() -> Command<Data, Error>] = &[add_store, help, version];

/// commands to be installed only after successful Jinxxy init
const CREATOR_COMMANDS: &[fn() -> Command<Data, Error>] = &[
    create_post,
    deactivate_license,
    grant_missing_roles,
    license_info,
    link_product,
    link_product_version,
    list_links,
    lock_license,
    set_log_channel,
    set_wildcard_role,
    stats,
    unlink_product,
    unlink_product_version,
    unlock_license,
    unset_wildcard_role,
    user_info,
];

/// commands to be installed only for owner-owned guilds
const OWNER_COMMANDS: &[fn() -> Command<Data, Error>] = &[
    announce,
    announce_test,
    clear_cache,
    debug_product_cache,
    exit,
    misconfigured_guilds,
    owner_stats,
    restart,
    set_cache_expiry_time,
    set_test,
    sudo_list_links,
    unfuck_cache,
    verify_guild,
    whois,
];

static BAKED_GLOBAL_COMMANDS: LazyLock<Vec<Command<Data, Error>>> =
    LazyLock::new(|| GLOBAL_COMMANDS.iter().map(|f| f()).collect());
static BAKED_CREATOR_COMMANDS: LazyLock<Vec<Command<Data, Error>>> =
    LazyLock::new(|| CREATOR_COMMANDS.iter().map(|f| f()).collect());
static BAKED_OWNER_COMMANDS: LazyLock<Vec<Command<Data, Error>>> =
    LazyLock::new(|| OWNER_COMMANDS.iter().map(|f| f()).collect());

/// User data, which is stored and accessible in all command invocations. Cloning is by-reference.
#[derive(Clone)]
struct Data {
    db: JinxDb,
    api_cache: ApiCache,
}

struct BotBuilder {
    client_builder: ClientBuilder,
    db: JinxDb,
}

pub struct Bot {
    client: Client,
    db: JinxDb,
}

impl BotBuilder {
    pub async fn new() -> Result<Self, Error> {
        let db = JinxDb::open().await?;
        let discord_token = db.get_discord_token().await?.ok_or_else(|| {
            JinxError::new(
                "discord token not provided. Re-run the application with the `init` subcommand to run first-time setup.",
            )
        })?;
        let discord_token = Token::try_from(discord_token)?;
        let intents = GatewayIntents::GUILDS
            .union(GatewayIntents::GUILD_MESSAGES)
            .union(GatewayIntents::DIRECT_MESSAGES);
        let commands = GLOBAL_COMMANDS
            .iter()
            .chain(CREATOR_COMMANDS.iter())
            .chain(OWNER_COMMANDS.iter())
            .map(|c| c())
            .collect();
        let options = poise::FrameworkOptions {
            commands, // all commands must appear in this list otherwise poise won't recognize interactions for them
            on_error: |e| Box::pin(error_handler(e)),
            initialize_owners: false, // `initialize_owners: true` is broken. serenity::http::client::get_current_application_info has a deserialization bug
            prefix_options: PrefixFrameworkOptions {
                // obnoxiously the defaults on this make it do things even if I have no prefix commands configured
                prefix: None,
                additional_prefixes: Vec::new(),
                dynamic_prefix: None,
                stripped_dynamic_prefix: None,
                mention_as_prefix: false,
                edit_tracker: None,
                execute_untracked_edits: false,
                ignore_edits_if_not_yet_responded: true,
                execute_self_messages: false,
                ignore_bots: true,
                ignore_thread_creation: true,
                case_insensitive_commands: false,
                non_command_message: None,
                ..Default::default()
            },
            ..Default::default()
        };
        let framework = poise::Framework::new(options);
        let distinct_user_count = db
            .distinct_user_count()
            .await
            .expect("Failed to read distinct user count from DB");
        debug!("fetched initial distinct_user_count");

        let client_builder = ClientBuilder::new(discord_token, intents)
            .activity(ActivityData::custom(get_activity_string(distinct_user_count)))
            .framework(Box::new(framework));

        let bot = Self { client_builder, db };
        Ok(bot)
    }

    pub async fn build(self) -> Result<Bot, Error> {
        let BotBuilder { client_builder, db } = self;

        debug!("Building client…");
        let api_cache = ApiCache::new(db.clone());
        let data = Arc::new(Data {
            db: db.clone(),
            api_cache: api_cache.clone(),
        });
        let client_builder = client_builder.data(data.clone()).event_handler(data);
        let client = client_builder.await.expect("Failed to create bot client");
        debug!("Client built… Starting background jobs…");

        // set up the task to periodically optimize the DB
        {
            let db = db.clone();
            tokio::task::spawn(async move {
                loop {
                    // 1 day per optimize. Startup optimize is handled on DB init.
                    tokio::time::sleep(Duration::from_secs(SECONDS_PER_DAY)).await;
                    let start = Instant::now();
                    if let Err(e) = db.optimize().await {
                        error!("Error optimizing DB: {:?}", e);
                    }
                    let elapsed = start.elapsed();
                    info!("optimized db in {}ms", elapsed.as_millis());
                }
            });
        }

        // set up the task to periodically checkpoint the DB
        {
            let db = db.clone();
            tokio::task::spawn(async move {
                // an extra 30 minute phase shift so this doesn't align with the 24h optimize
                tokio::time::sleep(Duration::from_secs(30 * SECONDS_PER_MINUTE)).await;
                loop {
                    // 1 hour per checkpoint. We do not checkpoint on startup.
                    tokio::time::sleep(Duration::from_secs(SECONDS_PER_HOUR)).await;
                    let start = Instant::now();
                    let result = db.checkpoint(SqliteWalCheckpoint::Passive).await;
                    let elapsed = start.elapsed();
                    match result {
                        Ok(result) => info!("checkpointed db in {}ms. {}", elapsed.as_millis(), result),
                        Err(e) => error!("error checkpointing db after {}ms: {:?}", elapsed.as_millis(), e),
                    }
                }
            });
        }

        // set up the task to periodically perform gumroad nags
        {
            let db = db.clone();
            let http = client.http.clone();
            tokio::task::spawn(async move {
                // initial delay of 60 seconds before the first nag wave
                tokio::time::sleep(Duration::from_secs(60)).await;
                loop {
                    let start = Instant::now();

                    let mut sent_nag_count: usize = 0;
                    match db.get_guilds_pending_gumroad_nag().await {
                        Ok(pending_nags) => {
                            for pending_nag in pending_nags {
                                let message = format!(
                                    "Jinx has detected that a significant number ({}) of your users are providing Gumroad license keys to Jinx. \
                                    This may indicate confusion between GumCord and Jinx. To improve your user experience, please consider adding \
                                    documentation messages in your server to help direct users to the correct bots. **This is the only time this alert \
                                    will appear**: in the future you can use the `/stats` command to view the current count of failed Gumroad activation \
                                    attempts Jinx has seen.",
                                    pending_nag.gumroad_failure_count
                                );
                                let embed = CreateEmbed::default()
                                    .title("Jinxxy/Gumroad Confusion Alert")
                                    .description(message)
                                    .color(Colour::ORANGE);
                                let message = CreateMessage::default().embed(embed);
                                match message.execute(http.as_ref(), pending_nag.log_channel_id).await {
                                    Ok(_message) => match db.increment_gumroad_nag_count(pending_nag.guild_id).await {
                                        Ok(()) => {
                                            sent_nag_count += 1;
                                        }
                                        Err(e) => {
                                            error!(
                                                "failed to increment gumroad nag count for {}: {:?}",
                                                pending_nag.guild_id.get(),
                                                e
                                            );
                                        }
                                    },
                                    Err(e) => {
                                        error!(
                                            "failed to send nag message for {}: {:?}",
                                            pending_nag.guild_id.get(),
                                            e
                                        );
                                    }
                                }

                                // rate limit to 20 TPS
                                tokio::time::sleep(Duration::from_millis(50)).await;
                            }
                        }
                        Err(e) => {
                            error!("Error getting pending gumroad nags: {:?}", e);
                        }
                    }

                    let elapsed = start.elapsed();
                    const EXPECTED_DURATION: Duration = Duration::from_millis(15);
                    if sent_nag_count != 0 || elapsed > EXPECTED_DURATION {
                        info!("sent {} gumroad nags in {}ms", sent_nag_count, elapsed.as_millis());
                    }

                    // wait 1 hour for each subsequent nag wave
                    tokio::time::sleep(Duration::from_secs(SECONDS_PER_HOUR)).await;
                }
            });
        }

        // set up the task to periodically set the bot's status
        {
            let db = db.clone();
            let runners = client.shard_manager.runners.clone();
            tokio::task::spawn(async move {
                let mut distinct_user_count = db
                    .distinct_user_count()
                    .await
                    .expect("Failed to read distinct user count from DB");

                loop {
                    // update once a minute
                    tokio::time::sleep(Duration::from_secs(60)).await;

                    let start = Instant::now();
                    match db.distinct_user_count().await {
                        Ok(new_distinct_user_count) => {
                            let updated = if new_distinct_user_count != distinct_user_count {
                                // only do the expensive bit if the count has actually changed
                                distinct_user_count = new_distinct_user_count;
                                let custom_activity = get_activity_string(new_distinct_user_count);
                                for runner in runners.iter() {
                                    let (_, sender) = runner.value();
                                    let result = sender.unbounded_send(ShardRunnerMessage::SetPresence {
                                        activity: Some(Some(ActivityData::custom(custom_activity.as_str()))),
                                        status: None,
                                    });
                                    if let Err(e) = result {
                                        warn!("Error updating bot activity: {e:?}");
                                    }
                                }
                                true
                            } else {
                                false
                            };

                            let elapsed = start.elapsed();
                            const EXPECTED_DURATION: Duration = Duration::from_millis(10);
                            if elapsed > EXPECTED_DURATION {
                                info!(
                                    "updated bot activity in {}μs, real_update={}",
                                    elapsed.as_micros(),
                                    updated
                                )
                            }
                        }
                        Err(e) => {
                            error!("Error reading distinct user count from DB: {e:?}")
                        }
                    }
                }
            });
        }

        debug!("Background jobs started. Starting API cache registration…");
        // register all stores in API cache
        for jinxxy_user_id in db.get_all_stores().await? {
            api_cache.register_store_in_cache(jinxxy_user_id).await?;
        }
        debug!("API cache registration complete.");

        Ok(Bot { client, db })
    }
}

impl Bot {
    pub async fn new() -> Result<Self, Error> {
        BotBuilder::new().await?.build().await
    }

    pub async fn start(&mut self) -> Result<(), Error> {
        debug!("Starting client event handler…");

        // note that client.start() does NOT do sharding. If sharding is needed you need to use one of the alternative start functions
        // https://docs.rs/serenity/latest/serenity/gateway/index.html#sharding
        // https://discord.com/developers/docs/topics/gateway#sharding
        self.client.start().await?;

        debug!("client stopped itself. Closing DB…");
        self.db.close().await;
        debug!("DB closed!");

        Ok(())
    }

    /// Shutdown all bot shards
    pub fn get_shutdown_trigger(&self) -> impl FnOnce() + Send + use<> {
        let shutdown_shards = self.client.shard_manager.get_shutdown_trigger();
        move || {
            shutdown_shards();
        }
    }
}

fn get_activity_string(distinct_user_count: u64) -> String {
    format!("Helping {distinct_user_count} users register Jinxxy products")
}
