// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use super::util::check_owner;
use crate::bot::Context;
use crate::error::JinxError;
use crate::SHOULD_RESTART;
use poise::serenity_prelude as serenity;
use poise::CreateReply;
use serenity::{Colour, CreateEmbed, CreateMessage, GuildId, UserId};
use std::sync::atomic;
use std::time::Duration;
use tracing::{debug, info, warn};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Get statistics about bot load and performance
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn owner_stats(
    context: Context<'_>,
) -> Result<(), Error> {
    let db_size = context.data().db.size().await.unwrap().div_ceil(1024);
    let configured_guild_count = context.data().db.guild_count().await.unwrap();
    let license_activation_count = context.data().db.license_activation_count().await.unwrap();
    let product_role_count = context.data().db.product_role_count().await.unwrap();
    let api_cache_len = context.data().api_cache.len();
    let log_channel_count = context.data().db.log_channel_count().await.unwrap();
    let user_count = context.serenity_context().cache.user_count();
    let cached_guild_count = context.serenity_context().cache.guild_count();
    let shard_count = context.serenity_context().cache.shard_count();
    let mut shard_list = String::new();
    {
        let shard_manager = context.framework().shard_manager();
        let lock = shard_manager.runners.lock().await;
        for (shard_id, info) in &*lock {
            shard_list.push_str(format!("\n- {} {:?} {}", shard_id, info.latency, info.stage).as_str());
        }
    }
    let message = format!(
        "db_size={db_size} KiB\n\
        users={user_count}\n\
        cached guilds={cached_guild_count}\n\
        configured guilds={configured_guild_count}\n\
        log channels={log_channel_count}\n\
        license activations={license_activation_count}\n\
        product→role links={product_role_count}\n\
        API cache len={api_cache_len}\n\
        shards={shard_count}{shard_list}"
    );
    let embed = CreateEmbed::default()
        .title("Jinx Owner Stats")
        .description(message);
    context.send(
        CreateReply::default()
            .embed(embed)
            .ephemeral(true)
    ).await?;
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
pub(in crate::bot) async fn exit(
    context: Context<'_>,
) -> Result<(), Error> {
    info!("starting shutdown…");
    context.send(CreateReply::default().content("Shutting down now!").ephemeral(true)).await?;
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
pub(in crate::bot) async fn restart(
    context: Context<'_>,
) -> Result<(), Error> {
    info!("starting restart…");
    context.send(CreateReply::default().content("Restarting now!").ephemeral(true)).await?;
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
    #[description = "Message to broadcast"] message: String,
) -> Result<(), Error> {
    announce_internal::<false>(context, message).await
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
    #[description = "Message to broadcast"] message: String,
) -> Result<(), Error> {
    announce_internal::<true>(context, message).await
}

/// Internal implementation of the announce command that handles whether to use test servers or ALL servers
async fn announce_internal<const TEST_ONLY: bool>(
    context: Context<'_>,
    message: String,
) -> Result<(), Error> {
    let message = CreateMessage::default().content(message);
    let channels = context.data().db.get_log_channels::<TEST_ONLY>().await?;
    let channel_count = channels.len();
    context.defer_ephemeral().await?; // gives us 15 minutes to complete our work
    let mut successful_messages: usize = 0;
    for channel in channels {
        match channel.send_message(context, message.clone()).await {
            Ok(_) => successful_messages += 1,
            Err(e) => warn!("Error sending message to {}: {:?}", channel, e),
        }
        tokio::time::sleep(Duration::from_millis(50)).await; // rate limit to 20 TPS
    }
    let reply = CreateReply::default()
        .ephemeral(true)
        .content(format!("Sent announcement to {successful_messages}/{channel_count} channels"));
    context.send(reply).await?;
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
    let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;

    context.data().db.set_test(guild_id, test).await?;

    let message = if test {
        "this guild is now set as a test guild"
    } else {
        "this guild is now set as a production guild"
    };
    let reply = CreateReply::default()
        .ephemeral(true)
        .content(message);
    context.send(reply).await?;

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
    let embed = match guild_id.parse::<u64>() {
        Ok(guild_id) => {
            if guild_id == 0 {
                // guild was invalid (0)
                CreateEmbed::default()
                    .title("Guild Verification Error")
                    .color(Colour::RED)
                    .description("Guild was invalid (id of 0)")
            } else if let Some(guild) = GuildId::new(guild_id).to_guild_cached(&context) {
                // guild was valid!
                if let Some(expected_owner_id) = owner_id {
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
                            .description(format!("Provided user does not own that guild. Actual owner is <@{}>.", guild.owner_id.get()))
                    }
                } else {
                    // we don't have an expected owner to check, so just print the actual owner
                    debug!("GUILD DEBUG: {:?}", *guild);
                    CreateEmbed::default()
                        .title("Guild Verification Result")
                        .color(Colour::DARK_GREEN)
                        .description(format!("Guild owned by <@{}>", guild.owner_id.get()))
                }
            } else {
                // guild was not cached
                CreateEmbed::default()
                    .title("Guild Verification Error")
                    .color(Colour::RED)
                    .description("Guild not in cache")
            }
        }
        Err(e) => {
            // guild was invalid (not a number)
            CreateEmbed::default()
                .title("Guild Verification Error")
                .color(Colour::RED)
                .description(format!("Guild was invalid (parse error: {})", e))
        }
    };

    context.send(
        CreateReply::default()
            .embed(embed)
            .ephemeral(true)
    ).await?;
    Ok(())
}
