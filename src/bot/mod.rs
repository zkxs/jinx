// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

mod cache;
mod commands;
mod event_handler;
mod error_handler;
pub mod util;

use crate::bot::cache::ApiCache;
use crate::bot::error_handler::error_handler;
use crate::bot::event_handler::event_handler;
use crate::db::JinxDb;
use crate::error::JinxError;
use commands::*;
use poise::{serenity_prelude as serenity, Command, PrefixFrameworkOptions};
use serenity::GatewayIntents;
use std::sync::{Arc, LazyLock};
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

/// Message shown to admins when the Jinxxy API key is missing
pub static MISSING_API_KEY_MESSAGE: &str = "Jinxxy API key is not set: please use the `/init` command to set it.";

const REGISTER_MODAL_ID: &str = "jinx_register_modal";

/// commands to be installed globally
static GLOBAL_COMMANDS: LazyLock<Vec<Command<Data, Error>>> = LazyLock::new(|| {
    vec![
        help(),
        init(),
        version(),
    ]
});

/// commands to be installed only after successful Jinxxy init
static CREATOR_COMMANDS: LazyLock<Vec<Command<Data, Error>>> = LazyLock::new(|| {
    vec![
        create_post(),
        deactivate_license(),
        license_info(),
        link_product(),
        list_links(),
        lock_license(),
        set_log_channel(),
        stats(),
        unlink_product(),
        unlock_license(),
        user_info(),
    ]
});

/// commands to be installed only for owner-owned guilds
static OWNER_COMMANDS: LazyLock<Vec<Command<Data, Error>>> = LazyLock::new(|| {
    vec![
        announce(),
        announce_test(),
        exit(),
        owner_stats(),
        restart(),
        set_test(),
        verify_guild(),
    ]
});

/// User data, which is stored and accessible in all command invocations
struct Data {
    db: Arc<JinxDb>,
    api_cache: Arc<ApiCache>,
}

pub async fn run_bot() -> Result<(), Error> {
    let db = JinxDb::open().await?;
    debug!("DB opened");
    let discord_token = db.get_discord_token().await?
        .ok_or(JinxError::new("discord token not provided. Re-run the application with the `init` subcommand to run first-time setup."))?;
    let intents = GatewayIntents::GUILDS
        .union(GatewayIntents::GUILD_MESSAGES)
        .union(GatewayIntents::DIRECT_MESSAGES);

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            // all commands must appear in this list otherwise poise won't recognize interactions for them
            // this vec is terribly redundant, but because we can't clone Command and it ONLY takes a Vec<Command>, this is the only option.
            commands: vec![
                announce(),
                announce_test(),
                create_post(),
                deactivate_license(),
                exit(),
                help(),
                init(),
                license_info(),
                link_product(),
                list_links(),
                lock_license(),
                owner_stats(),
                restart(),
                set_log_channel(),
                set_test(),
                stats(),
                unlink_product(),
                unlock_license(),
                user_info(),
                verify_guild(),
                version(),
            ],
            event_handler: |ctx, event, framework, data| {
                Box::pin(event_handler(ctx, event, framework, data))
            },
            on_error: |e| {
                Box::pin(error_handler(e))
            },
            initialize_owners: false, // `initialize_owners: true` is broken. serenity::http::client::get_current_application_info has a deserialization bug
            prefix_options: PrefixFrameworkOptions { // obnoxiously the defaults on this make it do things even if I have no prefix commands configured
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
        })
        .setup(|ctx, _ready, _framework| {
            Box::pin(async move {
                let db = Arc::new(db);
                debug!("registering global commands…");
                let commands_to_create = poise::builtins::create_application_commands(GLOBAL_COMMANDS.as_slice());
                ctx.http.create_global_commands(&commands_to_create).await?;

                const SECONDS_PER_MINUTE: u64 = 60;
                const MINUTES_PER_HOUR: u64 = 60;
                const HOURS_PER_DAY: u64 = 24;
                const SECONDS_PER_DAY: u64 = SECONDS_PER_MINUTE * MINUTES_PER_HOUR * HOURS_PER_DAY;

                // set up the task to periodically optimize the DB
                {
                    let db_clone = db.clone();
                    tokio::task::spawn(async move {
                        loop {
                            tokio::time::sleep(Duration::from_secs(SECONDS_PER_DAY)).await;
                            let start = Instant::now();
                            if let Err(e) = db_clone.optimize().await {
                                error!("Error optimizing DB: {:?}", e);
                            }
                            let elapsed = start.elapsed();
                            info!("optimized db in {}ms", elapsed.as_millis());
                        }
                    });
                }

                let api_cache = Arc::new(ApiCache::default());

                // set up the task to periodically clean the API cache
                {
                    let api_cache_clone = api_cache.clone();
                    tokio::task::spawn(async move {
                        loop {
                            tokio::time::sleep(Duration::from_secs(5 * SECONDS_PER_MINUTE)).await;
                            let start = Instant::now();
                            api_cache_clone.clean();
                            let elapsed = start.elapsed();
                            const EXPECTED_DURATION: Duration = Duration::from_millis(5);
                            if elapsed > EXPECTED_DURATION {
                                info!("cleaned cache in {}ms", elapsed.as_millis());
                            }
                        }
                    });
                }

                debug!("framework setup complete");

                Ok(Data {
                    db,
                    api_cache,
                })
            })
        })
        .build();

    debug!("framework built");

    let mut client = serenity::ClientBuilder::new(discord_token, intents)
        .framework(framework)
        .await.unwrap();

    debug!("client built. Starting…");

    // note that client.start() does NOT do sharding. If sharding is needed you need to use one of the alternative start functions
    // https://docs.rs/serenity/latest/serenity/gateway/index.html#sharding
    // https://discord.com/developers/docs/topics/gateway#sharding
    client.start().await.unwrap();

    Ok(())
}
