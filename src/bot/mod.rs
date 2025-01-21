// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

mod cache;
mod commands;
mod error_handler;
mod event_handler;
pub mod util;

use crate::bot::cache::ApiCache;
use crate::bot::error_handler::error_handler;
use crate::bot::event_handler::event_handler;
use crate::db::JinxDb;
use crate::error::JinxError;
use commands::*;
use poise::{serenity_prelude as serenity, Command, PrefixFrameworkOptions};
use serenity::{Colour, CreateEmbed, CreateMessage, GatewayIntents};
use std::sync::{Arc, LazyLock};
use tokio::time::{Duration, Instant};
use tracing::{debug, error, info};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

const SECONDS_PER_MINUTE: u64 = 60;
const MINUTES_PER_HOUR: u64 = 60;
const HOURS_PER_DAY: u64 = 24;
const SECONDS_PER_DAY: u64 = SECONDS_PER_MINUTE * MINUTES_PER_HOUR * HOURS_PER_DAY;
const SECONDS_PER_HOUR: u64 = SECONDS_PER_MINUTE * MINUTES_PER_HOUR;

/// Message shown to admins when the Jinxxy API key is missing
pub static MISSING_API_KEY_MESSAGE: &str =
    "Jinxxy API key is not set: please use the `/init` command to set it.";

const REGISTER_MODAL_ID: &str = "jinx_register_modal";

/// commands to be installed globally
static GLOBAL_COMMANDS: LazyLock<Vec<Command<Data, Error>>> =
    LazyLock::new(|| vec![help(), init(), version()]);

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
        .ok_or_else(|| JinxError::new("discord token not provided. Re-run the application with the `init` subcommand to run first-time setup."))?;
    let intents = GatewayIntents::GUILDS
        .union(GatewayIntents::GUILD_MESSAGES)
        .union(GatewayIntents::DIRECT_MESSAGES);

    // we need this thing all over the place, so wrap it in an Arc
    let db = Arc::new(db);
    let framework_db_clone = db.clone();

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
        })
        .setup(|ctx, _ready, _framework| {
            Box::pin(async move {
                let db = framework_db_clone;
                debug!("registering global commands…");
                let commands_to_create =
                    poise::builtins::create_application_commands(GLOBAL_COMMANDS.as_slice());
                ctx.http.create_global_commands(&commands_to_create).await?;

                // set up the task to periodically optimize the DB
                {
                    let db = db.clone();
                    tokio::task::spawn(async move {
                        loop {
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

                let api_cache = Arc::new(ApiCache::default());

                // set up the task to periodically clean the API cache
                {
                    let api_cache = api_cache.clone();
                    tokio::task::spawn(async move {
                        loop {
                            tokio::time::sleep(Duration::from_secs(5 * SECONDS_PER_MINUTE)).await;
                            let start = Instant::now();
                            api_cache.clean();
                            let elapsed = start.elapsed();
                            const EXPECTED_DURATION: Duration = Duration::from_millis(5);
                            if elapsed > EXPECTED_DURATION {
                                info!("cleaned cache in {}ms", elapsed.as_millis());
                            }
                        }
                    });
                }

                debug!("framework setup complete");

                Ok(Data { db, api_cache })
            })
        })
        .build();

    debug!("framework built");

    let mut client = serenity::ClientBuilder::new(discord_token, intents)
        .framework(framework)
        .await
        .unwrap();

    // set up the task to periodically perform gumroad nags
    {
        let db = db.clone();
        let http = client.http.clone();
        let cache = client.cache.clone();
        tokio::task::spawn(async move {
            // initial delay of 60 seconds before the first nag wave
            let mut duration = Duration::from_secs(60);
            loop {
                tokio::time::sleep(duration).await;

                // wait 1 hour for each subsequent nag wave
                duration = Duration::from_secs(SECONDS_PER_HOUR);
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
                            match pending_nag
                                .log_channel_id
                                .send_message((&cache, http.as_ref()), message)
                                .await
                            {
                                Ok(_message) => {
                                    match db.increment_gumroad_nag_count(pending_nag.guild_id).await
                                    {
                                        Ok(()) => {
                                            sent_nag_count += 1;
                                        }
                                        Err(e) => {
                                            error!("failed to increment gumroad nag count for {}: {:?}", pending_nag.guild_id.get(), e);
                                        }
                                    }
                                }
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
                const EXPECTED_DURATION: Duration = Duration::from_millis(5);
                if sent_nag_count != 0 || elapsed > EXPECTED_DURATION {
                    info!(
                        "sent {} gumroad nags in {}ms",
                        sent_nag_count,
                        elapsed.as_millis()
                    );
                }
            }
        });
    }

    debug!("client built. Starting…");

    // note that client.start() does NOT do sharding. If sharding is needed you need to use one of the alternative start functions
    // https://docs.rs/serenity/latest/serenity/gateway/index.html#sharding
    // https://discord.com/developers/docs/topics/gateway#sharding
    client.start().await.unwrap();

    Ok(())
}
