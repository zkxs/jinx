// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::bot::util::{check_owner, success_reply};
use crate::bot::Context;
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::http::jinxxy::{GetProfileImageUrl as _, GetProfileUrl as _};
use crate::SHOULD_RESTART;
use poise::serenity_prelude as serenity;
use poise::CreateReply;
use serenity::{Colour, CreateEmbed, CreateMessage, GuildId, GuildRef, UserId};
use std::sync::atomic;
use tokio::time::{Duration, Instant};
use tracing::{info, warn};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Get statistics about bot load and performance
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn owner_stats(context: Context<'_>) -> Result<(), Error> {
    let start = Instant::now();
    let db_size = context.data().db.size().await.unwrap().div_ceil(1024);
    let configured_guild_count = context.data().db.guild_count().await.unwrap();
    let license_activation_count = context.data().db.license_activation_count().await.unwrap();
    let product_role_count = context.data().db.product_role_count().await.unwrap();
    let api_cache_products = context.data().api_cache.product_count();
    let api_cache_len = context.data().api_cache.len();
    let api_cache_capacity = context.data().api_cache.capacity();
    let log_channel_count = context.data().db.log_channel_count().await.unwrap();
    let user_count = context.serenity_context().cache.user_count();
    let cached_guild_count = context.serenity_context().cache.guild_count();
    let shard_count = context.serenity_context().cache.shard_count();
    let mut shard_list = String::new();
    {
        let shard_manager = context.framework().shard_manager();
        let lock = shard_manager.runners.lock().await;
        for (shard_id, info) in &*lock {
            shard_list
                .push_str(format!("\n- {} {:?} {}", shard_id, info.latency, info.stage).as_str());
        }
    }
    let tokio_metrics = tokio::runtime::Handle::current().metrics();
    let tokio_num_workers = tokio_metrics.num_workers();
    let tokio_num_alive_tasks = tokio_metrics.num_alive_tasks();
    let tokio_global_queue_depth = tokio_metrics.global_queue_depth();
    let elapsed_micros = start.elapsed().as_micros();

    let message = format!(
        "db_size={db_size} KiB\n\
        users={user_count}\n\
        cached guilds={cached_guild_count}\n\
        configured guilds={configured_guild_count}\n\
        log channels={log_channel_count}\n\
        license activations={license_activation_count}\n\
        product→role links={product_role_count}\n\
        API cache products={api_cache_products}\n\
        API cache len={api_cache_len}\n\
        API cache capacity={api_cache_capacity}\n\
        shards={shard_count}{shard_list}\n\
        tokio_num_workers={tokio_num_workers}\n\
        tokio_num_alive_tasks={tokio_num_alive_tasks}\n\
        tokio_global_queue_depth={tokio_global_queue_depth}\n\
        query time={elapsed_micros}μs" // this is a shitty metric of db load
    );
    let embed = CreateEmbed::default()
        .title("Jinx Owner Stats")
        .description(message);
    context
        .send(CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

/// Remotely shuts down the bot. If you do not have access to restart the bot this is PERMANENT.
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn exit(context: Context<'_>) -> Result<(), Error> {
    info!("starting shutdown…");
    context
        .send(success_reply("Success", "Shutting down now!"))
        .await?;
    context.framework().shard_manager.shutdown_all().await;
    Ok(())
}

/// Remotely restarts down the bot. If you do not have access to restart the bot this is PERMANENT.
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn restart(context: Context<'_>) -> Result<(), Error> {
    info!("starting restart…");
    context
        .send(success_reply("Success", "Restarting now!"))
        .await?;
    SHOULD_RESTART.store(true, atomic::Ordering::Release);
    context.framework().shard_manager.shutdown_all().await;
    Ok(())
}

/// Send an announcement to ALL bot log channels.
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn announce(
    context: Context<'_>,
    #[description = "Message title"] title: Option<String>,
    #[description = "Message to broadcast"] message: String,
) -> Result<(), Error> {
    announce_internal::<false>(context, title, message).await
}

/// Send an announcement to all test server bot log channels.
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn announce_test(
    context: Context<'_>,
    #[description = "Message title"] title: Option<String>,
    #[description = "Message to broadcast"] message: String,
) -> Result<(), Error> {
    announce_internal::<true>(context, title, message).await
}

/// Internal implementation of the announce command that handles whether to use test servers or ALL servers
async fn announce_internal<const TEST_ONLY: bool>(
    context: Context<'_>,
    title: Option<String>,
    message: String,
) -> Result<(), Error> {
    context.defer_ephemeral().await?; // gives us 15 minutes to complete our work

    let message = message.replace(r"\n", "\n");
    let embed = CreateEmbed::default().description(message);
    let embed = if let Some(title) = title {
        embed.title(title)
    } else if TEST_ONLY {
        embed.title("Test Announcement")
    } else {
        embed.title("Announcement")
    };

    let message = CreateMessage::default().embed(embed);
    let channels = context.data().db.get_log_channels::<TEST_ONLY>().await?;
    let channel_count = channels.len();
    let mut successful_messages: usize = 0;
    for channel in channels {
        match channel.send_message(context, message.clone()).await {
            Ok(_) => successful_messages += 1,
            Err(e) => warn!("Error sending message to {}: {:?}", channel, e),
        }
        tokio::time::sleep(Duration::from_millis(50)).await; // rate limit to 20 TPS
    }
    context
        .send(success_reply(
            "Success",
            format!("Sent announcement to {successful_messages}/{channel_count} channels"),
        ))
        .await?;
    Ok(())
}

/// Set or unset this guild as a test guild
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn set_test(
    context: Context<'_>,
    #[description = "is this a test guild?"] test: bool,
) -> Result<(), Error> {
    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    context.data().db.set_test(guild_id, test).await?;

    let message = if test {
        "this guild is now set as a test guild"
    } else {
        "this guild is now set as a production guild"
    };
    context.send(success_reply("Success", message)).await?;
    Ok(())
}

/// Verify guild ownership
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn verify_guild(
    context: Context<'_>,
    #[description = "ID of guild"] guild_id: String,
    #[description = "Optional ID of expected owner"] owner_id: Option<UserId>,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let reply = match guild_id.parse::<u64>() {
        Ok(guild_id) => {
            if guild_id == 0 {
                // guild was invalid (0)
                let embed = CreateEmbed::default()
                    .title("Guild Verification Error")
                    .color(Colour::RED)
                    .description("Guild was invalid (id of 0)");
                CreateReply::default().embed(embed)
            } else {
                let guild_id = GuildId::new(guild_id);

                // 1-off struct to contain the guild data we want, as GuildRef cannot pass await boundaries
                struct GuildData {
                    id: GuildId,
                    owner_id: UserId,
                    name: String,
                    description: Option<String>,
                    thumbnail_url: Option<String>,
                }
                fn to_guild_data(guild: GuildRef) -> GuildData {
                    GuildData {
                        id: guild.id,
                        owner_id: guild.owner_id,
                        name: guild.name.clone(),
                        description: guild.description.clone(),
                        thumbnail_url: guild.icon.map(|icon| {
                            format!(
                                "https://cdn.discordapp.com/icons/{}/{}.webp?size=128",
                                guild.id.get(),
                                icon
                            )
                        }),
                    }
                }

                if let Some(guild) = guild_id
                    .to_guild_cached(&context)
                    .map(|guild| to_guild_data(guild))
                {
                    // guild was valid!
                    let verify_embed = if let Some(expected_owner_id) = owner_id {
                        // we have an expected owner to check
                        if guild.owner_id == expected_owner_id {
                            CreateEmbed::default()
                                .title("Guild Verification Success")
                                .color(Colour::DARK_GREEN)
                                .description("Provided user owns that guild.")
                        } else {
                            CreateEmbed::default()
                                .title("Guild Verification Failure")
                                .color(Colour::ORANGE)
                                .description(format!(
                                    "Provided user does not own that guild. Actual owner is <@{}>.",
                                    guild.owner_id.get()
                                ))
                        }
                    } else {
                        // we don't have an expected owner to check, so just print the actual owner
                        CreateEmbed::default()
                            .title("Guild Verification Result")
                            .color(Colour::DARK_GREEN)
                            .description(format!("Guild owned by <@{}>", guild.owner_id.get()))
                    };

                    let api_embed = if let Some(api_key) =
                        context.data().db.get_jinxxy_api_key(guild.id).await?
                    {
                        match jinxxy::get_own_user(&api_key).await {
                            Ok(auth_user) => {
                                let embed = CreateEmbed::default()
                                    .title("API Verification Success")
                                    .color(Colour::DARK_GREEN);
                                let embed = if let Some(profile_image_url) =
                                    auth_user.profile_image_url()
                                {
                                    embed.thumbnail(profile_image_url)
                                } else {
                                    embed
                                };

                                let scopes = format!("{:?}", auth_user.scopes);
                                let profile_url = auth_user.profile_url();
                                let display_name = auth_user.into_display_name();
                                let message = if let Some(profile_url) = profile_url {
                                    format!("[{display_name}]({profile_url}) has scopes {scopes}")
                                } else {
                                    format!("{display_name} has scopes {scopes}")
                                };

                                embed.description(message)
                            }
                            Err(e) => CreateEmbed::default()
                                .title("API Verification Error")
                                .color(Colour::RED)
                                .description(format!("API key invalid: {}", e)),
                        }
                    } else {
                        CreateEmbed::default()
                            .title("API Verification Skipped")
                            .color(Colour::ORANGE)
                            .description("API key was unset")
                    };

                    let guild_embed = {
                        let guild_name = guild.name;
                        let guild_description = guild.description.unwrap_or_default();
                        let log_channel =
                            context.data().db.get_log_channel(guild_id).await?.is_some();
                        let is_test = context.data().db.is_test_guild(guild_id).await?;
                        let license_activation_count = context
                            .data()
                            .db
                            .guild_license_activation_count(guild_id)
                            .await?;
                        let gumroad_failure_count = context
                            .data()
                            .db
                            .get_gumroad_failure_count(guild_id)
                            .await?
                            .unwrap_or(0);
                        let gumroad_nag_count = context
                            .data()
                            .db
                            .get_gumroad_nag_count(guild_id)
                            .await?
                            .unwrap_or(0);
                        let product_role_count =
                            context.data().db.guild_product_role_count(guild_id).await?;
                        let guild_embed = CreateEmbed::default()
                            .title("Guild Information")
                            .description(format!(
                                "Name={guild_name}\n\
                                Description={guild_description}\n\
                                Log channel={log_channel}\n\
                                Test={is_test}\n\
                                license activations={license_activation_count}\n\
                                failed gumroad licenses={gumroad_failure_count}\n\
                                gumroad nags={gumroad_nag_count}\n\
                                product→role links={product_role_count}"
                            ));
                        if let Some(thumbnail_url) = guild.thumbnail_url {
                            guild_embed.thumbnail(thumbnail_url)
                        } else {
                            guild_embed
                        }
                    };

                    CreateReply::default()
                        .embed(verify_embed)
                        .embed(api_embed)
                        .embed(guild_embed)
                } else {
                    // guild was not cached
                    let embed = CreateEmbed::default()
                        .title("Guild Verification Error")
                        .color(Colour::RED)
                        .description("Guild not in cache");
                    CreateReply::default().embed(embed)
                }
            }
        }
        Err(e) => {
            // guild was invalid (not a number)
            let embed = CreateEmbed::default()
                .title("Guild Verification Error")
                .color(Colour::RED)
                .description(format!("Guild was invalid (parse error: {})", e));
            CreateReply::default().embed(embed)
        }
    };

    context.send(reply.ephemeral(true)).await?;
    Ok(())
}
