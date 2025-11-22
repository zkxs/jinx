// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::bot::util;
use crate::bot::util::{error_reply, success_reply};
use crate::bot::{Context, MISSING_API_KEY_MESSAGE};
use crate::db::LinkSource;
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::http::jinxxy::{GetProfileImageUrl as _, GetProfileUrl as _};
use crate::license::LOCKING_USER_ID;
use ahash::HashSet;
use poise::CreateReply;
use poise::serenity_prelude as serenity;
use serenity::{
    ButtonStyle, ChannelId, Colour, CreateActionRow, CreateAutocompleteResponse, CreateButton, CreateEmbed,
    CreateMessage, GuildId, RoleId,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write;
use tokio::join;
use tokio::task::JoinSet;
use tracing::{error, warn};

// discord component ids
pub(in crate::bot) const REGISTER_BUTTON_ID: &str = "jinx_register_button";
pub(in crate::bot) const LICENSE_KEY_ID: &str = "jinx_license_key_input";

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Get statistics about license activations
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn stats(context: Context<'_>) -> Result<(), Error> {
    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;
    let license_activation_count = context.data().db.guild_license_activation_count(guild_id).await?;
    let gumroad_failure_count = context
        .data()
        .db
        .get_gumroad_failure_count(guild_id)
        .await?
        .unwrap_or(0);

    let message = format!(
        "license activations={license_activation_count}\n\
        failed gumroad licenses={gumroad_failure_count}"
    );
    let embed = CreateEmbed::default().title("Jinx Stats").description(message);
    context
        .send(CreateReply::default().embed(embed).ephemeral(true))
        .await?;
    Ok(())
}

/// Set (or unset) channel for bot to log to.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn set_log_channel(
    context: Context<'_>,
    #[description = "user to query licenses for"] channel: Option<ChannelId>, // we can't use Channel here because it throws FrameworkError::ArgumentParse on access problems, which cannot be handled cleanly.
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let reply = match channel {
        Some(channel) => {
            // if setting a channel, then attempt to write a test log to the channel
            let embed = CreateEmbed::default()
                .title("Configuration Changed")
                .description("I will now log to this channel.");
            let message = CreateMessage::default().embed(embed);
            let test_result = channel.send_message(context, message).await.map(|_| ());

            match test_result {
                Ok(()) => {
                    // test log worked, so set the channel
                    context.data().db.set_log_channel(guild_id, Some(channel)).await?;

                    // let the user know what we just did
                    let message = format!("Bot log channel set to <#{}>.", channel.get());
                    success_reply("Success", message)
                }
                Err(e) => {
                    // test log failed, so let the user know
                    warn!("Error sending message to test log channel: {:?}", e);
                    error_reply(
                        "Error Setting Log Channel",
                        format!(
                            "Log channel not set because there was an error sending a message to <#{}>: {}. Please check bot and channel permissions.",
                            channel.get(),
                            e
                        ),
                    )
                }
            }
        }
        None => {
            context.data().db.set_log_channel(guild_id, None).await?;
            success_reply("Success", "Bot log channel unset.")
        }
    };

    context.send(reply).await?;
    Ok(())
}

/// Create post with buttons to register product keys
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn create_post(context: Context<'_>) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let channel = context.channel_id();

    let components = vec![CreateActionRow::Buttons(vec![
        CreateButton::new(REGISTER_BUTTON_ID)
            .label("Register")
            .style(ButtonStyle::Primary),
    ])];

    let api_key = context
        .data()
        .db
        .get_jinxxy_api_key(
            context
                .guild_id()
                .ok_or_else(|| JinxError::new("expected to be in a guild"))?,
        )
        .await?
        .ok_or_else(|| JinxError::new("Jinxxy API key is not set"))?;
    let reply = match jinxxy::get_own_user(&api_key).await {
        Ok(jinxxy_user) => {
            let profile_url = jinxxy_user.profile_url();
            let jinxxy_user: jinxxy::DisplayUser = jinxxy_user.into(); // convert into just the data we need for this command
            let display_name = jinxxy_user.name_possessive();
            let display_name = if let Some(profile_url) = profile_url {
                format!("[{display_name}](<{profile_url}>)")
            } else {
                display_name
            };
            let embed = CreateEmbed::default()
                .title("Jinxxy Product Registration")
                .description(format!("Press the button below to register a Jinxxy license key for any of {display_name} products. You can find your license key in your email receipt or at [jinxxy.com](<https://jinxxy.com/my/inventory>)."));
            let embed = if let Some(profile_image_url) = jinxxy_user.profile_image_url() {
                embed.thumbnail(profile_image_url)
            } else {
                embed
            };

            let message = CreateMessage::default().embed(embed).components(components);

            if let Err(e) = channel.send_message(context, message).await {
                warn!("Error in /create_post when sending message: {:?}", e);
                error_reply(
                    "Error Creating Post",
                    "Post not created because there was an error sending a message to this channel. Please check bot and channel permissions.",
                )
            } else {
                success_reply("Success", "Registration post created!")
            }
        }
        Err(e) => error_reply(
            "Error Creating Post",
            format!("Could not get info for your Jinxxy user: {e}"),
        ),
    };

    context.send(reply).await?;
    Ok(())
}

// requires MANAGE_GUILD permission because it can print license keys and a bunch of other customer information
/// Query license information for a user
#[poise::command(
    context_menu_command = "List Jinxxy licenses",
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub async fn user_info(
    context: Context<'_>,
    #[description = "user to query licenses for"] user: serenity::User,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let reply = if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
        // look up licenses from the local DB, which is the only way we can do this without scraping every license activation from Jinxxy
        let license_ids = context.data().db.get_user_licenses(guild_id, user.id.get()).await?;
        let message = if license_ids.is_empty() {
            format!("<@{}> has no license activations.", user.id.get())
        } else {
            let license_id_count = license_ids.len();

            // start looking up each license in jinxxy to get the associated product info
            // TODO: this is static data, so backfill it into the local DB to save some API calls here
            let mut license_check_join_set = JoinSet::new();
            for license_id in license_ids {
                let api_key = api_key.clone();
                let license_id = license_id.clone();
                license_check_join_set.spawn(async move {
                    jinxxy::check_license_id(&api_key, &license_id, false)
                        .await
                        .map(|option| option.ok_or(license_id))
                });
            }

            // figure out which product versions we need version names for and start those requests in the background
            let mut product_lookup_join_set = JoinSet::new();
            let mut products_requiring_lookup = HashSet::with_hasher(ahash::RandomState::default());
            let mut license_infos: Vec<_> = Vec::with_capacity(license_id_count);
            while let Some(result) = license_check_join_set.join_next().await {
                let license_info = result??;
                if let Ok(license_info) = &license_info
                    && license_info.product_version_info.is_some()
                {
                    let newly_inserted = products_requiring_lookup.insert(license_info.product_id.clone());
                    if newly_inserted {
                        let api_key = api_key.clone();
                        let product_id = license_info.product_id.clone();
                        product_lookup_join_set
                            .spawn(async move { jinxxy::get_product_uncached(&api_key, &product_id).await });
                    }
                }
                license_infos.push(license_info);
            }

            // for each product that we needed version info for extract version name into a map
            // map structure: {product_id -> version_id -> version_name}
            let mut product_version_name_cache: HashMap<
                String,
                HashMap<String, String, ahash::RandomState>,
                ahash::RandomState,
            > = Default::default();
            while let Some(result) = product_lookup_join_set.join_next().await {
                let product = result??;
                for version in product.versions {
                    product_version_name_cache
                        .entry(product.id.clone())
                        .or_default()
                        .insert(version.id, version.name);
                }
            }

            let mut message = format!("Licenses for <@{}>:", user.id.get());
            for license_info in license_infos {
                match license_info {
                    Ok(license_info) => {
                        let product_version_name = license_info
                            .product_version_info
                            .as_ref()
                            .map(|version| {
                                product_version_name_cache
                                    .get(license_info.product_id.as_str())
                                    .and_then(|inner| inner.get(&version.product_version_id))
                                    .map(|string| string.as_str())
                                    .unwrap_or("`ERROR`")
                            })
                            .unwrap_or("`null`");

                        let locked = context
                            .data()
                            .db
                            .is_license_locked(guild_id, license_info.license_id.clone())
                            .await?;

                        let username = if let Some(username) = &license_info.username {
                            format!(
                                "[{}](<{}>)",
                                username,
                                license_info.profile_url().ok_or_else(|| JinxError::new(
                                    "expected profile_url to exist when username is set"
                                ))?
                            )
                        } else {
                            format!("`{}`", license_info.user_id)
                        };

                        message.push_str(
                            format!(
                                "\n- `{}` activations={} locked={} user={} product=\"{}\" version={}",
                                license_info.short_key,
                                license_info.activations, // this field came from Jinxxy and is up to date
                                locked,                   // this field came from the local DB and may be out of sync
                                username,
                                license_info.product_name,
                                product_version_name
                            )
                            .as_str(),
                        );
                    }
                    Err(license_id) => {
                        // we had a license ID in our local DB, but could not find info on it in the Jinxxy API
                        message.push_str(format!("\n- ID=`{license_id}` (no data found)").as_str());
                    }
                }
            }
            message
        };
        success_reply("User Info", message)
    } else {
        error_reply("Error Getting User Info", MISSING_API_KEY_MESSAGE)
    };

    context.send(reply).await?;
    Ok(())
}

/// Deactivate a license. Does not revoke any granted roles.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub async fn deactivate_license(
    context: Context<'_>,
    #[description = "user to deactivate license for"] user: serenity::User,
    #[description = "Jinxxy license to deactivate for user"] license: String,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let reply = if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
        let license_id = util::license_to_id(&api_key, &license).await?;
        if let Some(license_id) = license_id {
            let activations = context
                .data()
                .db
                .get_user_license_activations(guild_id, user.id.get(), license_id.clone())
                .await?;
            for activation_id in activations {
                let license_id = license_id.clone();
                jinxxy::delete_license_activation(&api_key, &license_id, &activation_id).await?;
                context
                    .data()
                    .db
                    .deactivate_license(guild_id, license_id, activation_id, user.id.get())
                    .await?;
            }
            success_reply(
                "Success",
                format!(
                    "All of <@{}>'s activations for `{}` have been deleted.",
                    user.id.get(),
                    license
                ),
            )
        } else {
            error_reply(
                "Error Deactivating License",
                format!(
                    "License `{license}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server."
                ),
            )
        }
    } else {
        error_reply("Error Deactivating License", MISSING_API_KEY_MESSAGE)
    };
    context.send(reply).await?;
    Ok(())
}

/// Query activation information for a license
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub async fn license_info(
    context: Context<'_>,
    #[description = "Jinxxy license to query activations for"] license: String,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let reply = if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
        let license_id = util::license_to_id(&api_key, &license).await?;
        if let Some(license_id) = license_id {
            // look up license usage info from local DB and from Jinxxy concurrently
            let (local_license_users, license_info, remote_license_users) = join!(
                context.data().db.get_license_users(guild_id, license_id.clone()),
                async {
                    let api_key = api_key.clone();
                    let license_id = license_id.clone();
                    jinxxy::check_license_id(&api_key, &license_id, true).await
                },
                async {
                    let api_key = api_key.clone();
                    let license_id = license_id.clone();
                    jinxxy::get_license_activations(&api_key, &license_id).await
                }
            );
            let mut local_license_users: Vec<u64> = local_license_users?;
            local_license_users.sort_unstable();
            let mut remote_license_users: Vec<u64> = remote_license_users?
                .into_iter()
                .flat_map(|activation| activation.try_into_user_id())
                .collect();
            remote_license_users.sort_unstable();

            if let Some(license_info) = license_info? {
                // license is valid

                let product_name = license_info.product_name.as_str();
                let version_name = license_info
                    .product_version_info
                    .as_ref()
                    .map(|info| info.product_version_name.as_str())
                    .unwrap_or("`null`");

                let message = if local_license_users.is_empty() {
                    format!(
                        "ID: `{}`\nShort: `{}`\nLong: `{}`\nValid for {} {}\n\nNo registered users.",
                        license_info.license_id, license_info.short_key, license_info.key, product_name, version_name
                    )
                } else {
                    let mut message = format!(
                        "ID: `{}`\nShort: `{}`\nLong: `{}`\nValid for {} {}\n\nRegistered users:",
                        license_info.license_id, license_info.short_key, license_info.key, product_name, version_name
                    );
                    for user_id in &local_license_users {
                        if *user_id == 0 {
                            message.push_str("\n- **LOCKED** (prevents further use)");
                        } else {
                            message.push_str(format!("\n- <@{user_id}>").as_str());
                        }
                    }
                    message
                };
                let reply = success_reply("License Info", message);
                if local_license_users == remote_license_users {
                    reply
                } else {
                    let embed = CreateEmbed::default()
                        .title("Activator mismatch")
                        .description("The local and remote activator lists do not match. This is really weird and you should tell the bot dev about it, because chances are you are the first person seeing this message ever.")
                        .color(Colour::RED);
                    reply.embed(embed)
                }
            } else {
                // license is invalid... but we somehow found it in the license list search by key?
                // that or an ID was provided directly
                let message = if local_license_users.is_empty() {
                    format!(
                        "License `{license} not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server.`"
                    )
                } else {
                    let mut message = format!("License `{license}` not found, but somehow has users:");
                    for user_id in local_license_users {
                        if user_id == 0 {
                            message.push_str("\n- **LOCKED** (prevents further use)");
                        } else {
                            message.push_str(format!("\n- <@{user_id}>").as_str());
                        }
                    }
                    message.push_str("\nThis may indicate that the license has been revoked on the Jinxxy side.");
                    message
                };
                error_reply("License Info Validation Error", message)
            }
        } else {
            error_reply(
                "Error Getting License Info",
                format!(
                    "License `{license}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server."
                ),
            )
        }
    } else {
        error_reply("Error Getting License Info", MISSING_API_KEY_MESSAGE)
    };
    context.send(reply).await?;
    Ok(())
}

/// Lock a license, preventing it from being used to grant roles.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub async fn lock_license(
    context: Context<'_>,
    #[description = "Jinxxy license to lock"] license: String,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let reply = if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
        let license_id = util::license_to_id(&api_key, &license).await?;
        if let Some(license_id) = license_id {
            let activation_id = jinxxy::create_license_activation(&api_key, &license_id, LOCKING_USER_ID).await?;
            context
                .data()
                .db
                .activate_license(guild_id, license_id, activation_id, LOCKING_USER_ID, None, None)
                .await?;
            success_reply(
                "Success",
                format!("License `{license}` is now locked and cannot be used to grant roles."),
            )
        } else {
            error_reply(
                "Error Locking License",
                format!(
                    "License `{license}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server."
                ),
            )
        }
    } else {
        error_reply("Error Locking License", MISSING_API_KEY_MESSAGE)
    };
    context.send(reply).await?;
    Ok(())
}

/// Unlock a license, allowing it to be used to grant roles.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub async fn unlock_license(
    context: Context<'_>,
    #[description = "Jinxxy license to unlock"] license: String,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let reply = if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
        let license_id = util::license_to_id(&api_key, &license).await?;
        if let Some(license_id) = license_id {
            let activations = jinxxy::get_license_activations(&api_key, &license_id).await?;
            let lock_activation_id = activations
                .into_iter()
                .find(|activation| activation.is_lock())
                .map(|activation| activation.id);

            let message = if let Some(lock_activation_id) = lock_activation_id {
                jinxxy::delete_license_activation(&api_key, &license_id, &lock_activation_id).await?;
                context
                    .data()
                    .db
                    .deactivate_license(guild_id, license_id, lock_activation_id, LOCKING_USER_ID)
                    .await?;
                format!("License `{license}` is now unlocked and may be used to grant roles.")
            } else {
                format!(
                    "License `{license}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server."
                )
            };

            success_reply("Success", message)
        } else {
            error_reply(
                "Error Unlocking License",
                format!(
                    "License `{license}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server."
                ),
            )
        }
    } else {
        error_reply("Error Unlocking License", MISSING_API_KEY_MESSAGE)
    };
    context.send(reply).await?;
    Ok(())
}

/// Initializes autocomplete data, and then does the product autocomplete
async fn product_autocomplete(context: Context<'_>, product_prefix: &str) -> CreateAutocompleteResponse {
    match context.guild_id() {
        Some(guild_id) => {
            match context
                .data()
                .api_cache
                .autocomplete_product_names_with_prefix(&context.data().db, guild_id, product_prefix)
                .await
            {
                Ok(result) => util::create_autocomplete_response(result.into_iter()),
                Err(e) => {
                    warn!("Failed to read API cache: {:?}", e);
                    CreateAutocompleteResponse::new()
                }
            }
        }
        None => {
            error!("someone is somehow doing product autocomplete without being in a guild");
            CreateAutocompleteResponse::new()
        }
    }
}

/// Initializes autocomplete data, and then does the product version autocomplete
async fn product_version_autocomplete(context: Context<'_>, product_prefix: &str) -> CreateAutocompleteResponse {
    match context.guild_id() {
        Some(guild_id) => {
            match context
                .data()
                .api_cache
                .autocomplete_product_version_names_with_prefix(&context.data().db, guild_id, product_prefix)
                .await
            {
                Ok(result) => util::create_autocomplete_response(result.into_iter()),
                Err(e) => {
                    warn!("Failed to read API cache: {:?}", e);
                    CreateAutocompleteResponse::new()
                }
            }
        }
        None => {
            error!("someone is somehow doing product version autocomplete without being in a guild");
            CreateAutocompleteResponse::new()
        }
    }
}

/// Set a catch-all role which will be granted by all license registrations
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn set_wildcard_role(
    context: Context<'_>,
    #[description = "Role to link"] role: RoleId, // note that Discord does not presently support variadic arguments: https://github.com/discord/discord-api-docs/discussions/3286
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;
    let assignable_roles = util::assignable_roles(&context, guild_id).await?;

    let mut unassignable_roles: Option<RoleId> = None;
    context.data().db.set_blanket_role_id(guild_id, Some(role)).await?;
    if !assignable_roles.contains(&role) {
        unassignable_roles = Some(role);
    }

    let embed = CreateEmbed::default()
        .title("Wildcard Set Successful")
        .description(format!("Any product will now grant <@&{}>", role.get()))
        .color(Colour::DARK_GREEN);
    let reply = CreateReply::default().embed(embed).ephemeral(true);
    let reply = if let Some(embed) = util::create_role_warning_from_unassignable(unassignable_roles.into_iter()) {
        reply.embed(embed)
    } else {
        reply
    };

    context.send(reply).await?;
    Ok(())
}

/// Stop granting the catch-all role for all license registrations
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn unset_wildcard_role(context: Context<'_>) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    context.data().db.set_blanket_role_id(guild_id, None).await?;

    let embed = CreateEmbed::default()
        .title("Wildcard Unset Successful")
        .description(
            "A role will no longer be granted for any product: now roles are only granted on a per-product basis.",
        )
        .color(Colour::DARK_GREEN);
    let reply = CreateReply::default().embed(embed).ephemeral(true);

    context.send(reply).await?;
    Ok(())
}

/// Link all versions of a product to a role grant.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn link_product(
    context: Context<'_>,
    #[description = "Product to modify role links for"]
    #[autocomplete = "product_autocomplete"]
    product: String,
    #[description = "Role to link"] role: RoleId, // note that Discord does not presently support variadic arguments: https://github.com/discord/discord-api-docs/discussions/3286
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let product_ids = context
        .data()
        .api_cache
        .product_name_to_ids(&context.data().db, guild_id, &product)
        .await?;

    let reply = if product_ids.is_empty() {
        error_reply("Error Linking Product", "Product not found.")
    } else {
        let assignable_roles = util::assignable_roles(&context, guild_id).await?;
        let mut unassignable_roles: Option<RoleId> = None;
        if !assignable_roles.contains(&role) {
            unassignable_roles = Some(role);
        }

        let mut roles_set = HashSet::with_hasher(ahash::RandomState::new());
        for product_id in product_ids {
            context
                .data()
                .db
                .link_product(guild_id, product_id.clone(), role)
                .await?;

            let roles = context
                .data()
                .db
                .get_linked_roles_for_product(guild_id, product_id)
                .await?;
            roles_set.extend(roles);
        }

        let mut message_lines = String::new();
        for role in roles_set {
            message_lines.push_str(format!("\n- <@&{}>", role.get()).as_str());
        }

        let embed = CreateEmbed::default()
            .title("Product Link Successful")
            .description(format!("{product} will now grant the following roles:{message_lines}"))
            .color(Colour::DARK_GREEN);
        let reply = CreateReply::default().embed(embed).ephemeral(true);
        if let Some(embed) = util::create_role_warning_from_unassignable(unassignable_roles.into_iter()) {
            reply.embed(embed)
        } else {
            reply
        }
    };

    context.send(reply).await?;
    Ok(())
}

/// Unlink a product from a role grant.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn unlink_product(
    context: Context<'_>,
    #[description = "Product to modify role links for"]
    #[autocomplete = "product_autocomplete"]
    product: String,
    #[description = "Role to unlink"] role: RoleId, // note that Discord does not presently support variadic arguments: https://github.com/discord/discord-api-docs/discussions/3286
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let product_ids = context
        .data()
        .api_cache
        .product_name_to_ids(&context.data().db, guild_id, &product)
        .await?;

    let reply = if product_ids.is_empty() {
        error_reply("Error Unlinking Product", "Product not found.")
    } else {
        let assignable_roles = util::assignable_roles(&context, guild_id).await?;

        let mut roles_set = HashSet::with_hasher(ahash::RandomState::new());
        for product_id in product_ids {
            context
                .data()
                .db
                .unlink_product(guild_id, product_id.clone(), role)
                .await?;

            let roles = context
                .data()
                .db
                .get_linked_roles_for_product(guild_id, product_id)
                .await?;
            roles_set.extend(roles);
        }

        let mut message_lines = String::new();
        for role in &roles_set {
            message_lines.push_str(format!("\n- <@&{}>", role.get()).as_str());
        }

        let embed = CreateEmbed::default()
            .title("Product Unlink Successful")
            .description(format!("{product} will now grant the following roles:{message_lines}"))
            .color(Colour::DARK_GREEN);
        let reply = CreateReply::default().embed(embed).ephemeral(true);
        if let Some(embed) = util::create_role_warning_from_roles(&assignable_roles, roles_set.into_iter()) {
            reply.embed(embed)
        } else {
            reply
        }
    };

    context.send(reply).await?;
    Ok(())
}

/// Link a specific product version to a role grant.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn link_product_version(
    context: Context<'_>,
    #[description = "Product & version to modify role links for"]
    #[autocomplete = "product_version_autocomplete"]
    product_version: String,
    #[description = "Role to link"] role: RoleId, // note that Discord does not presently support variadic arguments: https://github.com/discord/discord-api-docs/discussions/3286
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let product_version_ids = context
        .data()
        .api_cache
        .product_version_name_to_version_ids(&context.data().db, guild_id, &product_version)
        .await?;

    let reply = if product_version_ids.is_empty() {
        error_reply("Error Linking Product Version", "Product version not found.")
    } else {
        let assignable_roles = util::assignable_roles(&context, guild_id).await?;
        let mut unassignable_roles: Option<RoleId> = None;
        if !assignable_roles.contains(&role) {
            unassignable_roles = Some(role);
        }

        let mut roles_set = HashSet::with_hasher(ahash::RandomState::new());
        for product_version_id in product_version_ids {
            context
                .data()
                .db
                .link_product_version(guild_id, product_version_id.clone(), role)
                .await?;

            let roles = context
                .data()
                .db
                .get_linked_roles_for_product_version(guild_id, product_version_id)
                .await?;
            roles_set.extend(roles);
        }

        let mut message_lines = String::new();
        for role in roles_set {
            message_lines.push_str(format!("\n- <@&{}>", role.get()).as_str());
        }

        let embed = CreateEmbed::default()
            .title("Product Version Link Successful")
            .description(format!(
                "{product_version} will now grant the following roles:{message_lines}"
            ))
            .color(Colour::DARK_GREEN);
        let reply = CreateReply::default().embed(embed).ephemeral(true);
        if let Some(embed) = util::create_role_warning_from_unassignable(unassignable_roles.into_iter()) {
            reply.embed(embed)
        } else {
            reply
        }
    };

    context.send(reply).await?;
    Ok(())
}

/// Unlink a specific product version from a role grant.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn unlink_product_version(
    context: Context<'_>,
    #[description = "Product & version to modify role links for"]
    #[autocomplete = "product_version_autocomplete"]
    product_version: String,
    #[description = "Role to unlink"] role: RoleId, // note that Discord does not presently support variadic arguments: https://github.com/discord/discord-api-docs/discussions/3286
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let product_version_ids = context
        .data()
        .api_cache
        .product_version_name_to_version_ids(&context.data().db, guild_id, &product_version)
        .await?;

    let reply = if product_version_ids.is_empty() {
        error_reply("Error Unlinking Product Version", "Product version not found.")
    } else {
        let assignable_roles = util::assignable_roles(&context, guild_id).await?;

        let mut roles_set = HashSet::with_hasher(ahash::RandomState::new());
        for product_version_id in product_version_ids {
            context
                .data()
                .db
                .unlink_product_version(guild_id, product_version_id.clone(), role)
                .await?;

            let roles = context
                .data()
                .db
                .get_linked_roles_for_product_version(guild_id, product_version_id)
                .await?;
            roles_set.extend(roles);
        }

        let mut message_lines = String::new();
        for role in &roles_set {
            message_lines.push_str(format!("\n- <@&{}>", role.get()).as_str());
        }

        let embed = CreateEmbed::default()
            .title("Product Version Unlink Successful")
            .description(format!(
                "{product_version} will now grant the following roles:{message_lines}"
            ))
            .color(Colour::DARK_GREEN);
        let reply = CreateReply::default().embed(embed).ephemeral(true);
        if let Some(embed) = util::create_role_warning_from_roles(&assignable_roles, roles_set.into_iter()) {
            reply.embed(embed)
        } else {
            reply
        }
    };

    context.send(reply).await?;
    Ok(())
}

/// List all product→role links
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn list_links(context: Context<'_>) -> Result<(), Error> {
    context.defer_ephemeral().await?;
    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;
    list_links_impl(context, guild_id).await
}

/// internal list links implementation. You MUST `context.defer_ephemeral()` on your own before calling this.
pub(in crate::bot) async fn list_links_impl(context: Context<'_>, guild_id: GuildId) -> Result<(), Error> {
    let assignable_roles = util::assignable_roles(&context, guild_id).await?;
    let links = context.data().db.get_links(guild_id).await?;
    static NO_LINKS_MESSAGE: &str = "No product→role links configured";
    let message = if links.is_empty() {
        NO_LINKS_MESSAGE.to_string()
    } else {
        // We always do a cached read here: even in the case of a cache miss we just fall back to IDs instead of names.
        // The idea is that users should have a warmed cache already when /list_links is invoked. This is because both
        // /init and /link_product do background cache warming.

        let mut linked_roles: Vec<RoleId> = links.keys().copied().collect();

        // handle deleted roles
        let deleted_roles = util::deleted_roles(&context, guild_id, linked_roles.iter().copied())?;
        for deleted_role in &deleted_roles {
            context.data().db.delete_role(guild_id, *deleted_role).await?;
        }
        linked_roles.retain(|role| !deleted_roles.contains(role));

        if linked_roles.is_empty() {
            NO_LINKS_MESSAGE.to_string()
        } else {
            linked_roles.sort_unstable(); // make sure the roles are listed in a consistent order and not subject to HashMap randomization
            context
                .data()
                .api_cache
                .get(&context.data().db, guild_id, |cache| {
                    let mut first_line = true;
                    let mut message = String::new();
                    for role in linked_roles {
                        if first_line {
                            first_line = false;
                        } else {
                            message.push('\n');
                        }
                        message.push_str(format!("- <@&{}> granted by:", role.get()).as_str());
                        let link_sources = links.get(&role).expect(
                            "we just queried a map with its own key list, how the hell is it missing an entry now?",
                        );
                        for link_source in link_sources {
                            // In the majority of cases we get a name &str here, so we use a Cow<str> to avoid a bunch of copies.
                            // Only in the case where we have to show an un-cached product version do we need to build an owned String.
                            let name = match link_source {
                                LinkSource::GlobalBlanket => Cow::Borrowed("`*`"),
                                LinkSource::ProductBlanket { product_id } => {
                                    let name = cache.product_id_to_name(product_id).unwrap_or(product_id.as_str());
                                    Cow::Borrowed(name)
                                }
                                LinkSource::ProductVersion(product_version_id) => cache
                                    .product_version_id_to_name(product_version_id)
                                    .map(Cow::Borrowed)
                                    .unwrap_or_else(|| Cow::Owned(format!("{product_version_id}"))),
                            };
                            message.push_str("\n  - ");
                            message.push_str(&name);
                        }
                    }

                    // discord has a message length limit of "4096 characters", but they do not specify if the mean
                    // codepoints or code units (bytes) by "characters". We check here to see if we're more than 80%
                    // (> 3276 bytes) of the way to 4096.
                    const MESSAGE_LENGTH_WARN_THRESHOLD: usize = 3276;
                    if message.len() > MESSAGE_LENGTH_WARN_THRESHOLD {
                        warn!(
                            "/list_links in {} had length of {}, which is getting dangerously close to the limit",
                            guild_id.get(),
                            message.len()
                        );
                    }

                    message
                })
                .await?
        }
    };

    let unassignable_embed = util::create_role_warning_from_roles(&assignable_roles, links.keys().copied());
    let embed = CreateEmbed::default()
        .title("All product→role links")
        .description(message);
    let reply = CreateReply::default().embed(embed).ephemeral(true);
    let reply = if let Some(embed) = unassignable_embed {
        reply.embed(embed)
    } else {
        reply
    };

    context.send(reply).await?;
    Ok(())
}

/// Grant a role to any users who have a license but are missing the linked role.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn grant_missing_roles(
    context: Context<'_>,
    #[description = "Role to grant"] role: Option<RoleId>,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;
    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let roles = if let Some(role) = role {
        vec![role]
    } else {
        context.data().db.get_linked_roles(guild_id).await?
    };

    // handle each role
    let mut reply_message = String::new();
    let mut after_first = false;
    for role in roles {
        if after_first {
            // handle delimiters between messages
            reply_message.push('\n');
        }
        after_first = true;

        let users = context.data().db.get_users_for_role(guild_id, role).await?;
        let total_users = users.len();
        let mut missing_users: usize = 0;
        let mut message_postfix = String::new();
        for user in users {
            let member = guild_id.member(context, user).await?;
            if !member.roles.contains(&role) {
                message_postfix.push_str(format!("\n- <@{}>", user.get()).as_str());
                missing_users += 1;
                member.add_role(context, role).await?;
            }
        }
        write!(
            reply_message,
            "{}/{} users were missing <@&{}>:{}",
            missing_users,
            total_users,
            role.get(),
            message_postfix
        )
        .expect("somehow failed writing into a String");
    }

    context
        .send(success_reply("Missing Roles Granted", reply_message.as_str()))
        .await?;

    // also send a notification to the guild owner bot log if it's set up for this guild
    if let Some(log_channel) = context.data().db.get_log_channel(guild_id).await? {
        let log_message = format!(
            "<@{}> granted missing roles:\n{}",
            context.author().id.get(),
            reply_message
        );
        let embed = CreateEmbed::default()
            .title("Missing Roles Granted")
            .description(log_message);
        let bot_log_message = CreateMessage::default().embed(embed);
        let bot_log_result = log_channel.send_message(context, bot_log_message).await;
        if let Err(e) = bot_log_result {
            warn!("Error logging to log channel in {}: {:?}", guild_id.get(), e);
        }
    }

    Ok(())
}
