// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::SHOULD_RESTART;
use crate::bot::commands::guild_commands;
use crate::bot::util::{check_owner, error_reply, success_reply};
use crate::bot::{Context, HOURS_PER_DAY, SECONDS_PER_HOUR, util};
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::http::jinxxy::{GetProfileImageUrl as _, GetUsername};
use poise::CreateReply;
use poise::serenity_prelude as serenity;
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
pub(in crate::bot) async fn owner_stats(
    context: Context<'_>,
    #[description = "if \"False\", response will be public. Defaults to True."] ephemeral: Option<bool>,
) -> Result<(), Error> {
    let start = Instant::now();
    let db_size = context.data().db.size().await?.div_ceil(1024);
    let configured_guild_count = context.data().db.guild_count().await?;
    let license_activation_count = context.data().db.license_activation_count().await?;
    let distinct_user_count = context.data().db.distinct_user_count().await?;
    let blanket_role_count = context.data().db.blanket_role_count().await?;
    let product_role_count = context.data().db.product_role_count().await?;
    let product_version_role_count = context.data().db.product_version_role_count().await?;
    let api_cache_products = context.data().api_cache.product_count();
    let api_cache_product_versions = context.data().api_cache.product_version_count();
    let api_cache_len = context.data().api_cache.len();
    let log_channel_count = context.data().db.log_channel_count().await?;
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
    let tokio_metrics = tokio::runtime::Handle::current().metrics();
    let tokio_num_workers = tokio_metrics.num_workers();
    let tokio_num_alive_tasks = tokio_metrics.num_alive_tasks();
    let tokio_global_queue_depth = tokio_metrics.global_queue_depth();
    let elapsed_micros = start.elapsed().as_micros();

    let message = format!(
        "db_size={db_size} KiB\n\
        cached users={user_count}\n\
        cached guilds={cached_guild_count}\n\
        configured guilds={configured_guild_count}\n\
        log channels={log_channel_count}\n\
        license activations={license_activation_count}\n\
        distinct activators={distinct_user_count}\n\
        wildcard role links={blanket_role_count}\n\
        product→role links={product_role_count}\n\
        product+version→role links={product_version_role_count}\n\
        API cache total products={api_cache_products}\n\
        API cache total product versions={api_cache_product_versions}\n\
        API cache guilds={api_cache_len}\n\
        shards={shard_count}{shard_list}\n\
        tokio_num_workers={tokio_num_workers}\n\
        tokio_num_alive_tasks={tokio_num_alive_tasks}\n\
        tokio_global_queue_depth={tokio_global_queue_depth}\n\
        query time={elapsed_micros}μs" // this is a shitty metric of db load
    );
    let embed = CreateEmbed::default().title("Jinx Owner Stats").description(message);
    context
        .send(CreateReply::default().embed(embed).ephemeral(ephemeral.unwrap_or(true)))
        .await?;
    Ok(())
}

/// Ensure all stores are registered in the API cache, even if they previously had Jinxxy API failures.
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn unfuck_cache(context: Context<'_>) -> Result<(), Error> {
    for jinxxy_user_id in context.data().db.get_all_stores().await? {
        context.data().api_cache.register_store_in_cache(jinxxy_user_id).await?;
    }
    let reply = success_reply("Success", "All stores re-registered in cache refresh worker");
    context.send(reply).await?;
    Ok(())
}

/// Delete the API cache from memory and disk. It will be expensive to rebuild.
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn clear_cache(context: Context<'_>) -> Result<(), Error> {
    let start = Instant::now();

    let before_api_cache_products = context.data().api_cache.product_count();
    let before_api_cache_product_versions = context.data().api_cache.product_version_count();
    let before_api_cache_len = context.data().api_cache.len();
    let time_after_metrics_1 = Instant::now();

    context.data().db.clear_cache().await?;
    let time_after_db_clear = Instant::now();

    context.data().api_cache.clear();
    let time_after_cache_clear = Instant::now();

    let after_api_cache_products = context.data().api_cache.product_count();
    let after_api_cache_product_versions = context.data().api_cache.product_version_count();
    let after_api_cache_len = context.data().api_cache.len();
    let time_after_metrics_2 = Instant::now();

    // calculate how long each step took
    let metrics_1_elapsed = time_after_metrics_1.duration_since(start).as_micros();
    let db_clear_elapsed = time_after_db_clear.duration_since(time_after_metrics_1).as_millis();
    let cache_clear_elapsed = time_after_cache_clear.duration_since(time_after_db_clear).as_micros();
    let metrics_2_elapsed = time_after_metrics_2.duration_since(time_after_cache_clear).as_micros();

    let message = format!(
        "**Before:**\n\
        API cache total products={before_api_cache_products}\n\
        API cache total product versions={before_api_cache_product_versions}\n\
        API cache guilds={before_api_cache_len}\n\
        **After:**\n\
        API cache total products={after_api_cache_products}\n\
        API cache total product versions={after_api_cache_product_versions}\n\
        API cache guilds={after_api_cache_len}\n\
        **Elapsed:**\n\
        m1={metrics_1_elapsed}μs\n\
        db={db_clear_elapsed}ms\n\
        mem={cache_clear_elapsed}μs\n\
        m2={metrics_2_elapsed}μs"
    );

    let embed = CreateEmbed::default().title("API Cache Cleared").description(message);
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
    context.send(success_reply("Success", "Shutting down now!")).await?;
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
    context.send(success_reply("Success", "Restarting now!")).await?;
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
                    owner_id: UserId,
                    name: String,
                    description: Option<String>,
                    thumbnail_url: Option<String>,
                }
                fn to_guild_data(guild: GuildRef) -> GuildData {
                    GuildData {
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

                if let Some(guild) = guild_id.to_guild_cached(&context).map(|guild| to_guild_data(guild)) {
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

                    let store_links = context.data().db.get_store_links(guild_id).await?;
                    let mut api_embeds = Vec::with_capacity(store_links.len());
                    for linked_store in store_links {
                        let unique_name = linked_store.jinxxy_username.unwrap_or(linked_store.jinxxy_user_id);
                        let api_embed = match jinxxy::get_own_user(&linked_store.jinxxy_api_key).await {
                            Ok(auth_user) => {
                                let embed = CreateEmbed::default()
                                    .title(format!("API Verification Success: {unique_name}"))
                                    .color(Colour::DARK_GREEN);
                                let embed = if let Some(profile_image_url) = auth_user.profile_image_url() {
                                    embed.thumbnail(profile_image_url)
                                } else {
                                    embed
                                };

                                let scopes = format!("{:?}", auth_user.scopes);
                                let profile_url = auth_user.username().profile_url();
                                let display_name = auth_user.as_display_name();
                                let message = if let Some(profile_url) = profile_url {
                                    format!("[{display_name}]({profile_url}) has scopes {scopes}")
                                } else {
                                    format!("{display_name} has scopes {scopes}")
                                };

                                embed.description(message)
                            }
                            Err(e) => CreateEmbed::default()
                                .title(format!("API Verification Error: {unique_name}"))
                                .color(Colour::RED)
                                .description(format!("API key invalid: {e}")),
                        };
                        api_embeds.push(api_embed);
                    }

                    let guild_embed = {
                        let guild_name = guild.name;
                        let guild_description = guild.description.unwrap_or_default();
                        let log_channel = context.data().db.get_log_channel(guild_id).await?.is_some();
                        let is_test = context.data().db.is_test_guild(guild_id).await?;
                        let is_administrator = match util::is_administrator(&context, guild_id).await {
                            Ok(true) => "true",
                            Ok(false) => "false",
                            Err(_) => "error",
                        };
                        let license_activation_count =
                            context.data().db.guild_license_activation_count(guild_id).await?;
                        let gumroad_failure_count = context
                            .data()
                            .db
                            .get_gumroad_failure_count(guild_id)
                            .await?
                            .unwrap_or(0);
                        let gumroad_nag_count = context.data().db.get_gumroad_nag_count(guild_id).await?.unwrap_or(0);
                        let guild_embed = CreateEmbed::default().title("Guild Information").description(format!(
                            "Name={guild_name}\n\
                                Description={guild_description}\n\
                                Log channel={log_channel}\n\
                                Test={is_test}\n\
                                Admin={is_administrator}\n\
                                license activations={license_activation_count}\n\
                                failed gumroad licenses={gumroad_failure_count}\n\
                                gumroad nags={gumroad_nag_count}"
                        ));
                        if let Some(thumbnail_url) = guild.thumbnail_url {
                            guild_embed.thumbnail(thumbnail_url)
                        } else {
                            guild_embed
                        }
                    };

                    let mut reply = CreateReply::default().embed(verify_embed).embed(guild_embed);
                    for api_embed in api_embeds {
                        reply = reply.embed(api_embed);
                    }
                    reply
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
                .description(format!("Guild was invalid (parse error: {e})"));
            CreateReply::default().embed(embed)
        }
    };

    context.send(reply.ephemeral(true)).await?;
    Ok(())
}

/// Scan for misconfigured guilds
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn misconfigured_guilds(context: Context<'_>) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let mut lines = "```\n".to_string();
    let mut any_misconfigurations = false;
    for guild_id in context.cache().guilds() {
        let is_administrator = util::is_administrator(&context, guild_id).await;
        if *is_administrator.as_ref().unwrap_or(&true) {
            any_misconfigurations = true;
            let admin_code = match is_administrator {
                Ok(true) => "A",
                Err(_) => "E",
                _ => "?",
            };
            let name = guild_id.name(context);
            let name_str = name.as_deref().unwrap_or("");
            lines.push_str(format!("{:20} {admin_code} {name_str}\n", guild_id.get()).as_str())
        }
    }

    let reply = if any_misconfigurations {
        lines.push_str("```");
        error_reply("Misconfigured Guild Report", lines)
    } else {
        success_reply("Misconfigured Guild Report", "No misconfigured guilds!")
    };
    context.send(reply.ephemeral(true)).await?;
    Ok(())
}

/// Run list_links in a different guild. This is an evil hack.
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn sudo_list_links(
    context: Context<'_>,
    #[description = "ID of guild"] guild_id: String,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    match guild_id.parse::<u64>() {
        Ok(guild_id) => {
            if guild_id == 0 {
                // guild was invalid (0)
                let embed = CreateEmbed::default()
                    .title("sudo_list_links Error")
                    .color(Colour::RED)
                    .description("Guild was invalid (id of 0)");
                let reply = CreateReply::default().embed(embed);
                context.send(reply.ephemeral(true)).await?;
            } else {
                let guild_id = GuildId::new(guild_id);

                // horrible evil hack to reuse all the logic with minimal work
                guild_commands::list_links_impl(context, guild_id).await?;
            }
        }
        Err(e) => {
            // guild was invalid (not a number)
            let embed = CreateEmbed::default()
                .title("sudo_list_links Error")
                .color(Colour::RED)
                .description(format!("Guild was invalid (parse error: {e})"));
            let reply = CreateReply::default().embed(embed);
            context.send(reply.ephemeral(true)).await?;
        }
    }

    Ok(())
}

/// List product names cached for a target guild.
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn debug_product_cache(
    context: Context<'_>,
    #[description = "ID of guild"] guild_id: String,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    match guild_id.parse::<u64>() {
        Ok(guild_id) => {
            if guild_id == 0 {
                // guild was invalid (0)
                let embed = CreateEmbed::default()
                    .title("debug_product_cache Error")
                    .color(Colour::RED)
                    .description("Guild was invalid (id of 0)");
                let reply = CreateReply::default().embed(embed);
                context.send(reply.ephemeral(true)).await?;
            } else {
                let guild_id = GuildId::new(guild_id);

                let mut message = String::new();
                context
                    .data()
                    .api_cache
                    .for_all_in_guild(&context.data().db, guild_id, |linked_store, cache| {
                        let unique_store_name = linked_store
                            .jinxxy_username
                            .as_deref()
                            .unwrap_or(linked_store.jinxxy_user_id.as_str());
                        for (index, product_name) in cache.product_name_iter().enumerate() {
                            if index != 0 {
                                message.push('\n');
                            }
                            message.push_str("- ");
                            message.push_str(unique_store_name);
                            message.push_str(": ");
                            message.push_str(product_name);
                        }
                    })
                    .await?;
                let embed = CreateEmbed::default().title("").description(message);
                let reply = CreateReply::default().embed(embed);
                context.send(reply.ephemeral(true)).await?;
            }
        }
        Err(e) => {
            // guild was invalid (not a number)
            let embed = CreateEmbed::default()
                .title("debug_product_cache Error")
                .color(Colour::RED)
                .description(format!("Guild was invalid (parse error: {e})"));
            let reply = CreateReply::default().embed(embed);
            context.send(reply.ephemeral(true)).await?;
        }
    }

    Ok(())
}

/// Set the expiry time for the low-priority product cache
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn set_cache_expiry_time(
    context: Context<'_>,
    #[description = "expiry time in hours"] expiry_time_hours: u64,
) -> Result<(), Error> {
    // we're rounding this for display, so a bit of precision loss is fine
    #[allow(clippy::cast_precision_loss)]
    let days: f64 = expiry_time_hours as f64 / HOURS_PER_DAY as f64;

    let low_priority_cache_expiry_time = Duration::from_secs(expiry_time_hours * SECONDS_PER_HOUR);
    context
        .data()
        .db
        .set_low_priority_cache_expiry_time(low_priority_cache_expiry_time)
        .await?;
    context.data().api_cache.bump().await?;
    context
        .send(success_reply(
            "Success",
            format!("Cache will now expire every {days:.2} days"),
        ))
        .await?;
    Ok(())
}

/// whois
#[poise::command(
    context_menu_command = "whois",
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn whois(
    context: Context<'_>,
    #[description = "user to look up"] user: serenity::User,
) -> Result<(), Error> {
    let user_id = user.id;
    let cache = context.serenity_context().cache.as_ref();
    let db_guilds = context.data().db.get_user_guilds(user_id.get()).await?;
    let mut line_iter = cache
        .guilds()
        .into_iter()
        .filter_map(|guild_id| {
            if let Some(guild) = cache.guild(guild_id) {
                let membership = if guild.owner_id == user_id {
                    Some("owner")
                } else if guild.members.contains_key(&user_id) {
                    Some("member")
                } else if db_guilds.contains(&guild_id) {
                    Some("activator")
                } else {
                    None
                };
                let guild_id = guild_id.get();
                let guild_name = guild.name.as_str();
                membership.map(|membership| format!("\n- `{guild_id}` {membership} {guild_name}"))
            } else {
                None
            }
        })
        .peekable();

    let reply = if line_iter.peek().is_some() {
        let results: String = line_iter.collect();
        success_reply(
            "User Found",
            format!("User <@{}> was found in cache:{}", user_id.get(), results),
        )
    } else {
        success_reply(
            "User Not Found",
            format!("User <@{}> was not found in cache", user_id.get()),
        )
    };

    context.send(reply).await?;
    Ok(())
}
