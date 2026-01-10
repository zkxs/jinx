// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::SHOULD_RESTART;
use crate::bot::commands::guild_commands;
use crate::bot::util::{check_owner, error_reply, success_reply};
use crate::bot::{Context, util};
use crate::constants::{HOURS_PER_DAY, SECONDS_PER_HOUR};
use crate::db::ActivationCounts;
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::http::jinxxy::{GetProfileImageUrl as _, GetUsername as _};
use poise::{CreateReply, serenity_prelude as serenity};
use serenity::{Colour, CreateEmbed, CreateMessage, GuildId, GuildRef, UserId};
use std::sync::atomic;
use tokio::task::JoinSet;
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
    let db_size = context.data().db.size().await?;
    let db_total_bytes = db_size.total_bytes().div_ceil(1024);
    let db_free_bytes = db_size.free_bytes().div_ceil(1024);
    let configured_guild_count = context.data().db.configured_guild_count().await?;
    let guilds = context.serenity_context().cache.guilds(); // available AND unavailable guilds
    let stale_guild_count = context.data().db.get_stale_guilds(&guilds).await?.len();
    let license_activation_count = context.data().db.license_activation_count().await?;
    let distinct_user_count = context.data().db.distinct_user_count().await?;
    let blanket_role_count = context.data().db.blanket_role_count().await?;
    let product_role_count = context.data().db.product_role_count().await?;
    let product_version_role_count = context.data().db.product_version_role_count().await?;
    let api_cache_products = context.data().api_cache.product_count();
    let api_cache_product_versions = context.data().api_cache.product_version_count();
    let api_cache_len = context.data().api_cache.len();
    let api_cache_registered = context.data().api_cache.registered_stores();
    let log_channel_count = context.data().db.log_channel_count().await?;
    let available_guild_count = context.serenity_context().cache.guild_count(); // available guilds only
    let unavailable_guild_count = context.serenity_context().cache.unavailable_guilds().len(); // unavailable guilds only
    let shard_count = context.serenity_context().cache.shard_count();
    let mut shard_list = String::new();
    {
        for runner in context.serenity_context().runners.iter() {
            let shard_id = runner.key();
            let (info, _sender) = runner.value();
            shard_list.push_str(format!("\n- {} {:?} {}", shard_id, info.latency, info.stage).as_str());
        }
    }
    let tokio_metrics = tokio::runtime::Handle::current().metrics();
    let tokio_num_workers = tokio_metrics.num_workers();
    let tokio_num_alive_tasks = tokio_metrics.num_alive_tasks();
    let tokio_global_queue_depth = tokio_metrics.global_queue_depth();
    let elapsed_micros = start.elapsed().as_micros();

    let message = format!(
        "db size={db_total_bytes} KiB\n\
        db free={db_free_bytes} KiB\n\
        cached available guilds={available_guild_count}\n\
        cached unavailable guilds={unavailable_guild_count}\n\
        configured guilds={configured_guild_count}\n\
        stale guilds={stale_guild_count}\n\
        log channels={log_channel_count}\n\
        license activations={license_activation_count}\n\
        distinct activators={distinct_user_count}\n\
        wildcard role links={blanket_role_count}\n\
        product→role links={product_role_count}\n\
        product+version→role links={product_version_role_count}\n\
        API cache total products={api_cache_products}\n\
        API cache total product versions={api_cache_product_versions}\n\
        API cache stores={api_cache_len}\n\
        API cache registered={api_cache_registered}\n\
        shards={shard_count}{shard_list}\n\
        tokio num_workers={tokio_num_workers}\n\
        tokio num_alive_tasks={tokio_num_alive_tasks}\n\
        tokio global_queue_depth={tokio_global_queue_depth}\n\
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
    context.serenity_context().shutdown_all();
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
    context.serenity_context().shutdown_all();
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
        match message.clone().execute(context.as_ref(), channel).await {
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
                        name: guild.name.to_string(),
                        description: guild.description.as_deref().map(|s| s.to_string()),
                        thumbnail_url: guild.icon.map(|icon| {
                            format!(
                                "https://cdn.discordapp.com/icons/{}/{}.webp?size=128",
                                guild.id.get(),
                                icon
                            )
                        }),
                    }
                }

                let mut reply = CreateReply::default();
                if let Some(guild) = guild_id
                    .to_guild_cached(context.as_ref())
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

                    let guild_embed = {
                        let guild_name = guild.name;
                        let banned = context.data().db.get_guild_ban(guild_id).await?;
                        let note = context.data().db.get_guild_note(guild_id).await?.unwrap_or_default();
                        let guild_description = guild.description.unwrap_or_default();
                        let log_channel = context.data().db.get_log_channel(guild_id).await?.is_some();
                        let is_test = context.data().db.is_test_guild(guild_id).await?;
                        let permissions = {
                            let bot_member = util::bot_member(&context, guild_id).await?;
                            util::permissions(&context, &bot_member)?
                        };
                        let administrator = permissions.administrator();
                        let manage_roles = permissions.manage_roles();
                        let ActivationCounts {
                            day_7,
                            day_30,
                            day_90,
                            day_365,
                            lifetime,
                        } = context.data().db.guild_license_activation_count(guild_id).await?;
                        let gumroad_failure_count = context
                            .data()
                            .db
                            .get_gumroad_failure_count(guild_id)
                            .await?
                            .unwrap_or(0);
                        let gumroad_nag_count = context.data().db.get_gumroad_nag_count(guild_id).await?.unwrap_or(0);
                        let nag_role = util::highest_mentionable_role(&context, guild_id)?
                            .and_then(|role_id| util::role_name(&context, guild_id, role_id).ok().flatten())
                            .unwrap_or_default();
                        let top_channel_id = util::sorted_channels(&context, guild_id)?
                            .into_iter()
                            .next()
                            .map(|(_position, id)| id);
                        let top_channel_name = match top_channel_id {
                            Some(channel_id) => {
                                let guild = context
                                    .cache()
                                    .guild(guild_id)
                                    .ok_or_else(|| JinxError::new("expected guild to be in cache"))?;
                                let channel = guild.channel(channel_id.widen()).ok_or_else(|| {
                                    JinxError::new("could not find channel in cache that we learned of from cache")
                                })?;
                                Cow::Owned(channel.base().name.to_string())
                            }
                            None => Cow::Borrowed("`null`"),
                        };
                        let guild_embed = CreateEmbed::default().title("Guild Information").description(format!(
                            "Name={guild_name}\n\
                            Banned={banned}\n\
                            Note={note}\n\
                            Description={guild_description}\n\
                            Log channel={log_channel}\n\
                            Test={is_test}\n\
                            Admin={administrator}\n\
                            Manage Roles={manage_roles}\n\
                            license activations (7d)={day_7}\n\
                            license activations (30d)={day_30}\n\
                            license activations (90d)={day_90}\n\
                            license activations (1yr)={day_365}\n\
                            license activations (lifetime)={lifetime}\n\
                            failed gumroad licenses={gumroad_failure_count}\n\
                            gumroad nags={gumroad_nag_count}\n\
                            nag role={nag_role}\n\
                            top channel={top_channel_name}"
                        ));
                        if let Some(thumbnail_url) = guild.thumbnail_url {
                            guild_embed.thumbnail(thumbnail_url)
                        } else {
                            guild_embed
                        }
                    };
                    reply = reply.embed(verify_embed).embed(guild_embed);
                } else {
                    // guild was not cached
                    let embed = CreateEmbed::default()
                        .title("Guild Verification Error")
                        .color(Colour::RED)
                        .description("Guild not in cache");
                    reply = reply.embed(embed);
                }

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
                                embed.thumbnail(profile_image_url.to_owned())
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
                for api_embed in api_embeds {
                    reply = reply.embed(api_embed);
                }
                reply
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
    let mut guilds = context.cache().guilds();
    guilds.sort_unstable();
    for guild_id in guilds {
        const OK: char = ' ';
        let api_code = if !context.data().db.has_jinxxy_linked(guild_id).await? {
            'J' // no jinxxy link exists
        } else if context.data().db.has_invalid_jinxxy_api_key(guild_id).await? {
            'I' // link exists, but we've marked an API key as invalid
        } else {
            OK
        };
        let ban_code = if context.data().db.get_guild_ban(guild_id).await? {
            'B' // guild is banned
        } else {
            OK
        };
        let permission_code = if let Ok(bot_member) = util::bot_member(&context, guild_id).await
            && let Ok(permissions) = util::permissions(&context, &bot_member)
        {
            if permissions.administrator() {
                'A' // bot has Administrator
            } else if !permissions.manage_roles() {
                'M' // bot lacks Manage Roles
            } else {
                OK
            }
        } else {
            // We weren't able to get the bot member or perms. This can happen if the cache contains a guild we're not in.
            '?'
        };

        if api_code != OK || ban_code != OK || permission_code != OK {
            any_misconfigurations = true;
            let name = guild_id.name(context.as_ref());
            let name_str = name.as_deref().unwrap_or("");
            let guild_id = guild_id.get();
            lines.push_str(format!("{guild_id:20} {permission_code}{api_code}{ban_code} {name_str}\n").as_str());
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
                guild_commands::list_links_impl(context, guild_id, true).await?;
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
    let user_name = if let Some(discriminator) = user.discriminator {
        Cow::Owned(format!("{}#{:04}", user.name, discriminator))
    } else {
        Cow::Borrowed(user.name.as_str())
    };
    let display_name = if let Some(global_name) = &user.global_name {
        global_name.as_str()
    } else {
        ""
    };
    let banned = context.data().db.get_user_ban(user_id).await?;
    let note = context.data().db.get_user_note(user_id).await?.unwrap_or_default();
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
            format!(
                "User <@{}> name={}; display={}; banned={}; note={}; was found in cache:{}",
                user_id.get(),
                user_name,
                display_name,
                banned,
                note,
                results
            ),
        )
    } else {
        success_reply(
            "User Not Found",
            format!(
                "User <@{}> name={}; display={}; banned={}; note={}; was not found in cache",
                user_id.get(),
                user_name,
                display_name,
                banned,
                note,
            ),
        )
    };

    context.send(reply).await?;
    Ok(())
}

/// Globally ban a user from interacting with Jinx
#[poise::command(
    context_menu_command = "ban",
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn ban_user_context(
    context: Context<'_>,
    #[description = "user to ban"] user: serenity::User,
) -> Result<(), Error> {
    ban_user_impl(context, user, None).await
}

/// Globally ban a user from interacting with Jinx
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn ban_user(
    context: Context<'_>,
    #[description = "user to ban"] user: serenity::User,
    #[description = "Reason for ban"] reason: Option<String>,
) -> Result<(), Error> {
    ban_user_impl(context, user, reason).await
}

async fn ban_user_impl(context: Context<'_>, user: serenity::User, reason: Option<String>) -> Result<(), Error> {
    let user_id = user.id;
    context.data().db.set_user_ban(user_id, true, reason).await?;
    let reply = success_reply("User Banned", format!("<@{}> has been banned", user_id.get()));
    context.send(reply).await?;
    Ok(())
}

/// Globally unban a user from interacting with Jinx
#[poise::command(
    context_menu_command = "unban",
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn unban_user_context(
    context: Context<'_>,
    #[description = "user to ban"] user: serenity::User,
) -> Result<(), Error> {
    unban_user_impl(context, user).await
}

/// Globally unban a user from interacting with Jinx
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn unban_user(
    context: Context<'_>,
    #[description = "user to ban"] user: serenity::User,
) -> Result<(), Error> {
    unban_user_impl(context, user).await
}

async fn unban_user_impl(context: Context<'_>, user: serenity::User) -> Result<(), Error> {
    let user_id = user.id;
    context.data().db.set_user_ban(user_id, false, None).await?;
    let reply = success_reply("User Unbanned", format!("<@{}> has been unbanned", user_id.get()));
    context.send(reply).await?;
    Ok(())
}

/// Globally ban a guild from adding Jinx
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn ban_guild(
    context: Context<'_>,
    #[description = "ID of guild to ban"] guild_id: String,
    #[description = "Reason for ban"] reason: Option<String>,
) -> Result<(), Error> {
    match guild_id.parse::<u64>() {
        Ok(guild_id) => {
            if guild_id == 0 {
                // guild was invalid (0)
                let embed = CreateEmbed::default()
                    .title("ban_guild Error")
                    .color(Colour::RED)
                    .description("Guild was invalid (id of 0)");
                let reply = CreateReply::default().embed(embed);
                context.send(reply.ephemeral(true)).await?;
            } else {
                let guild_id = GuildId::new(guild_id);
                context.data().db.set_guild_ban(guild_id, true, reason).await?;
                let message = match context.http().leave_guild(guild_id).await {
                    Ok(_) => format!("Guild `{}` has been banned and left", guild_id.get()),
                    Err(_) => format!("Guild `{}` has been banned", guild_id.get()),
                };
                let reply = success_reply("Guild Banned", message);
                context.send(reply.ephemeral(true)).await?;
            }
        }
        Err(e) => {
            // guild was invalid (not a number)
            let embed = CreateEmbed::default()
                .title("ban_guild Error")
                .color(Colour::RED)
                .description(format!("Guild was invalid (parse error: {e})"));
            let reply = CreateReply::default().embed(embed);
            context.send(reply.ephemeral(true)).await?;
        }
    }

    Ok(())
}

/// Globally ban a guild from adding Jinx
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn unban_guild(
    context: Context<'_>,
    #[description = "ID of guild to ban"] guild_id: String,
) -> Result<(), Error> {
    match guild_id.parse::<u64>() {
        Ok(guild_id) => {
            if guild_id == 0 {
                // guild was invalid (0)
                let embed = CreateEmbed::default()
                    .title("unban_guild Error")
                    .color(Colour::RED)
                    .description("Guild was invalid (id of 0)");
                let reply = CreateReply::default().embed(embed);
                context.send(reply.ephemeral(true)).await?;
            } else {
                let guild_id = GuildId::new(guild_id);
                context.data().db.set_guild_ban(guild_id, false, None).await?;
                let reply = success_reply(
                    "Guild Unbanned",
                    format!("Guild `{}` has been unbanned", guild_id.get()),
                );
                context.send(reply.ephemeral(true)).await?;
            }
        }
        Err(e) => {
            // guild was invalid (not a number)
            let embed = CreateEmbed::default()
                .title("unban_guild Error")
                .color(Colour::RED)
                .description(format!("Guild was invalid (parse error: {e})"));
            let reply = CreateReply::default().embed(embed);
            context.send(reply.ephemeral(true)).await?;
        }
    }

    Ok(())
}

/// Delete stale guilds from the DB
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn delete_stale_guilds(
    context: Context<'_>,
    #[description = "max allowed stale guilds"] max: u64,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;
    let guilds = context.cache().guilds();
    let reply = match context.data().db.delete_stale_guilds(&guilds, max).await? {
        Some(deleted_stores) => {
            let store_len = deleted_stores.len();
            for jinxxy_user_id in deleted_stores {
                context
                    .data()
                    .api_cache
                    .unregister_store_in_cache(jinxxy_user_id)
                    .await?;
            }
            success_reply("Success", format!("Deleted {store_len} stores"))
        }
        None => error_reply("Failure", format!("More than max {max} stale guilds detected")),
    };
    context.send(reply).await?;
    Ok(())
}

/// Backfill missing license activation information
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn backfill_license_activation(context: Context<'_>) -> Result<(), Error> {
    const PARALLELISM: usize = 4;
    const PARALLELISM_THRESHOLD: usize = PARALLELISM * 8;
    context.defer_ephemeral().await?;
    let db_activations = context.data().db.get_activations_needing_backfill().await?;
    let parallelism = if db_activations.len() > PARALLELISM_THRESHOLD {
        PARALLELISM
    } else {
        1
    };
    let chunk_size = db_activations.len().div_ceil(parallelism);
    let mut join_set = JoinSet::<Result<(u64, u64), JinxError>>::new();
    for chunk in db_activations.chunks(chunk_size) {
        // tragically we must clone here because you can't just split an allocation
        // the only alternative would be some reference counting solution such as vecshard to drop once all shards are dropped
        let chunk = chunk.to_vec();
        let db = context.data().db.clone();
        join_set.spawn(async move {
            let mut backfill_count: u64 = 0;
            let mut skip_count: u64 = 0;
            for db_activation in chunk {
                match util::retry_thrice(|| {
                    jinxxy::get_license_activation(
                        &db_activation.jinxxy_api_key,
                        &db_activation.license_id,
                        &db_activation.license_activation_id,
                    )
                })
                .await
                {
                    Ok(Some(api_activation)) => {
                        let row_updated = db
                            .backfill_activation(
                                &db_activation.jinxxy_user_id,
                                &db_activation.license_id,
                                db_activation.activator_user_id,
                                &db_activation.license_activation_id,
                                &api_activation.created_at,
                            )
                            .await?;
                        if row_updated {
                            backfill_count += 1;
                        } else {
                            skip_count += 1;
                            warn!("someone beat me to a backfill! weird!");
                        }
                    }
                    Ok(None) => {
                        skip_count += 1;
                        warn!(
                            "DB activation {}:{}:{} did not exist in Jinxxy API",
                            &db_activation.jinxxy_user_id,
                            &db_activation.license_id,
                            &db_activation.license_activation_id
                        );
                    }
                    Err(e) => {
                        skip_count += 1;
                        warn!(
                            "DB activation {}:{}:{} threw Jinxxy error: {:?}",
                            &db_activation.jinxxy_user_id,
                            &db_activation.license_id,
                            &db_activation.license_activation_id,
                            e
                        );
                    }
                }
            }
            Ok((backfill_count, skip_count))
        });
    }

    let mut backfill_count: u64 = 0;
    let mut skip_count: u64 = 0;
    while let Some(result) = join_set.join_next().await {
        let (chunk_backfill_count, chunk_skip_count) = result??;
        backfill_count += chunk_backfill_count;
        skip_count += chunk_skip_count;
    }

    let reply = success_reply("Success", format!("Backfilled {backfill_count}, skipped {skip_count}."));
    context.send(reply).await?;
    Ok(())
}
