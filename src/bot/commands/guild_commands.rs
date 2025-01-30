// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::bot::util::{
    assignable_roles, create_role_warning_from_roles, create_role_warning_from_unassignable,
    error_reply, license_to_id, success_reply,
};
use crate::bot::{Context, MISSING_API_KEY_MESSAGE};
use crate::db::LinkSource;
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::http::jinxxy::{GetProfileImageUrl as _, GetProfileUrl as _};
use crate::license::LOCKING_USER_ID;
use poise::serenity_prelude as serenity;
use poise::CreateReply;
use serenity::{
    ButtonStyle, ChannelId, Colour, CreateActionRow, CreateButton, CreateEmbed, CreateMessage,
    RoleId,
};
use std::collections::HashMap;
use tracing::warn;

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
    let product_role_count = context.data().db.guild_product_role_count(guild_id).await?;

    let message = format!(
        "license activations={license_activation_count}\n\
        failed gumroad licenses={gumroad_failure_count}\n\
        product→role links={product_role_count}"
    );
    let embed = CreateEmbed::default()
        .title("Jinx Stats")
        .description(message);
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
                    context
                        .data()
                        .db
                        .set_log_channel(guild_id, Some(channel))
                        .await?;

                    // let the user know what we just did
                    let message = format!("Bot log channel set to <#{}>.", channel.get());
                    success_reply("Success", message)
                }
                Err(e) => {
                    // test log failed, so let the user know
                    warn!("Error sending message to test log channel: {:?}", e);
                    error_reply("Error Setting Log Channel", format!("Log channel not set because there was an error sending a message to <#{}>: {}. Please check bot and channel permissions.", channel.get(), e))
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

    let components = vec![CreateActionRow::Buttons(vec![CreateButton::new(
        REGISTER_BUTTON_ID,
    )
    .label("Register")
    .style(ButtonStyle::Primary)])];

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
            let jinxxy_user: jinxxy::DisplayUser = jinxxy_user.into(); // convert into just the data we need for this command
            let embed = CreateEmbed::default()
                .title("Jinxxy Product Registration")
                .description(format!("Press the button below to register a Jinxxy license key for any of {} products. You can find your license key in your email receipt or at [jinxxy.com](<https://jinxxy.com/my/inventory>).", jinxxy_user.name_possessive()));
            let embed = if let Some(profile_image_url) = jinxxy_user.profile_image_url() {
                embed.thumbnail(profile_image_url)
            } else {
                embed
            };

            let message = CreateMessage::default().embed(embed).components(components);

            if let Err(e) = channel.send_message(context, message).await {
                warn!("Error in /create_post when sending message: {:?}", e);
                error_reply("Error Creating Post", "Post not created because there was an error sending a message to this channel. Please check bot and channel permissions.")
            } else {
                success_reply("Success", "Registration post created!")
            }
        }
        Err(e) => error_reply(
            "Error Creating Post",
            format!("Could not get info for your Jinxxy user: {}", e),
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
        let license_ids = context
            .data()
            .db
            .get_user_licenses(guild_id, user.id.get())
            .await?;
        let message = if license_ids.is_empty() {
            format!("<@{}> has no license activations.", user.id.get())
        } else {
            let mut message = format!("Licenses for <@{}>:", user.id.get());

            // build a cache of product versions that we need names for
            // Map structure: product_id -> {product_version_id -> product_version_name}
            let mut product_cache: HashMap<
                String,
                Option<HashMap<String, String, ahash::RandomState>>,
                ahash::RandomState,
            > = Default::default();

            for license_id in license_ids {
                let license_info = jinxxy::check_license_id(&api_key, &license_id).await?;
                if let Some(license_info) = license_info {
                    let product_version_cache = if let Some(product) =
                        product_cache.get(&license_info.product_id)
                    {
                        product.as_ref()
                    } else {
                        let result = jinxxy::get_product(&api_key, &license_info.product_id).await;
                        if let Err(e) = &result {
                            warn!("Error looking up product info for {}, which is in license {}: {:?}", license_info.product_id, license_id, e);
                        }
                        let result = result.ok().map(|product| {
                            product
                                .versions
                                .into_iter()
                                .map(|version| (version.id, version.name))
                                .collect()
                        });
                        product_cache
                            .entry(license_info.product_id.clone())
                            .or_insert(result)
                            .as_ref() // kind of a weird use of this API because there's an extra empty check but oh well. We can't use or_insert_with because async reasons.
                    };
                    let product_version_name = product_version_cache
                        .and_then(|cache| {
                            license_info
                                .product_version_info
                                .as_ref()
                                .map(|info| info.product_version_id.as_str())
                                .and_then(|version_id| cache.get(version_id))
                        })
                        .map(|version| format!("\"{}\"", version))
                        .unwrap_or("`null`".to_string());

                    let locked = context
                        .data()
                        .db
                        .is_license_locked(guild_id, license_id.clone())
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
                            locked, // this field came from the local DB and may be out of sync
                            username,
                            license_info.product_name,
                            product_version_name
                        )
                        .as_str(),
                    );
                } else {
                    // we had a license ID in our local DB, but could not find info on it in the Jinxxy API
                    message.push_str(format!("\n- ID=`{}` (no data found)", license_id).as_str());
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
        let license_id = license_to_id(&api_key, &license).await?;
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
            error_reply("Error Deactivating License", format!("License `{}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server.", license))
        }
    } else {
        error_reply("Error Deactivating License", MISSING_API_KEY_MESSAGE)
    };
    context.send(reply).await?;
    Ok(())
}

// only requires MANAGE_ROLES permission because it can't emit license key info
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
        let license_id = license_to_id(&api_key, &license).await?;
        if let Some(license_id) = license_id {
            // look up license usage info from local DB: this avoids doing some expensive Jinxxy API requests
            let license_users = context
                .data()
                .db
                .get_license_users(guild_id, license_id)
                .await?;
            let message = if license_users.is_empty() {
                format!("`{}` is valid, but has no registered users.", license)
            } else {
                let mut message = format!("Users for `{}`:", license);
                for user_id in license_users {
                    if user_id == 0 {
                        message.push_str("\n- **LOCKED** (prevents further use)");
                    } else {
                        message.push_str(format!("\n- <@{}>", user_id).as_str());
                    }
                }
                message
            };
            success_reply("License Info", message)
        } else {
            error_reply("Error Getting License Info", format!("License `{}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server.", license))
        }
    } else {
        error_reply("Error Getting License Info", MISSING_API_KEY_MESSAGE)
    };
    context.send(reply).await?;
    Ok(())
}

// only requires MANAGE_ROLES permission because it can't emit license key info
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
        let license_id = license_to_id(&api_key, &license).await?;
        if let Some(license_id) = license_id {
            let activation_id =
                jinxxy::create_license_activation(&api_key, &license_id, LOCKING_USER_ID).await?;
            context
                .data()
                .db
                .activate_license(guild_id, license_id, activation_id, LOCKING_USER_ID)
                .await?;
            success_reply(
                "Success",
                format!(
                    "License `{}` is now locked and cannot be used to grant roles.",
                    license
                ),
            )
        } else {
            error_reply("Error Locking License", format!("License `{}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server.", license))
        }
    } else {
        error_reply("Error Locking License", MISSING_API_KEY_MESSAGE)
    };
    context.send(reply).await?;
    Ok(())
}

// only requires MANAGE_ROLES permission because it can't emit license key info
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
        let license_id = license_to_id(&api_key, &license).await?;
        if let Some(license_id) = license_id {
            let activations = jinxxy::get_license_activations(&api_key, &license_id).await?;
            let lock_activation_id = activations
                .into_iter()
                .find(|activation| activation.is_lock())
                .map(|activation| activation.id);

            let message = if let Some(lock_activation_id) = lock_activation_id {
                jinxxy::delete_license_activation(&api_key, &license_id, &lock_activation_id)
                    .await?;
                context
                    .data()
                    .db
                    .deactivate_license(guild_id, license_id, lock_activation_id, LOCKING_USER_ID)
                    .await?;
                format!(
                    "License `{}` is now unlocked and may be used to grant roles.",
                    license
                )
            } else {
                format!("License `{}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server.", license)
            };

            success_reply("Success", message)
        } else {
            error_reply("Error Unlocking License", format!("License `{}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server.", license))
        }
    } else {
        error_reply("Error Unlocking License", MISSING_API_KEY_MESSAGE)
    };
    context.send(reply).await?;
    Ok(())
}

/// Initializes autocomplete data, and then does the product autocomplete
async fn product_autocomplete(
    context: Context<'_>,
    product_prefix: &str,
) -> impl Iterator<Item = String> {
    match context
        .data()
        .api_cache
        .product_names_with_prefix(&context, product_prefix)
        .await
    {
        Ok(result) => result.into_iter(),
        Err(e) => {
            warn!("Failed to read API cache: {:?}", e);
            Vec::new().into_iter()
        }
    }
}

/// Initializes autocomplete data, and then does the product version autocomplete
async fn product_version_autocomplete(
    context: Context<'_>,
    product_prefix: &str,
) -> impl Iterator<Item = String> {
    match context
        .data()
        .api_cache
        .product_version_names_with_prefix(&context, product_prefix)
        .await
    {
        Ok(result) => result.into_iter(),
        Err(e) => {
            warn!("Failed to read API cache: {:?}", e);
            Vec::new().into_iter()
        }
    }
}

/// Link all versions of a product to a role grant.
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
    let assignable_roles = assignable_roles(&context, guild_id).await?;

    let mut unassignable_roles: Option<RoleId> = None;
    context
        .data()
        .db
        .set_blanket_role_id(guild_id, Some(role))
        .await?;
    if !assignable_roles.contains(&role) {
        unassignable_roles = Some(role);
    }

    let embed = CreateEmbed::default()
        .title("Wildcard Set Successful")
        .description(format!("Any product will now grant <@&{}>", role.get()))
        .color(Colour::DARK_GREEN);
    let reply = CreateReply::default().embed(embed).ephemeral(true);
    let reply = if let Some(embed) =
        create_role_warning_from_unassignable(unassignable_roles.into_iter())
    {
        reply.embed(embed)
    } else {
        reply
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
pub(in crate::bot) async fn unset_wildcard_role(context: Context<'_>) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    context
        .data()
        .db
        .set_blanket_role_id(guild_id, None)
        .await?;

    let embed = CreateEmbed::default()
        .title("Wildcard Unset Successful")
        .description("A role will no longer be granted for any product: now roles are only granted on a per-product basis.")
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

    let product_id = context
        .data()
        .api_cache
        .product_name_to_id(&context, &product)
        .await?;

    let reply = if let Some(product_id) = product_id {
        let guild_id = context
            .guild_id()
            .ok_or_else(|| JinxError::new("expected to be in a guild"))?;
        let assignable_roles = assignable_roles(&context, guild_id).await?;

        let mut unassignable_roles: Option<RoleId> = None;
        context
            .data()
            .db
            .link_product(guild_id, product_id.clone(), role)
            .await?;
        if !assignable_roles.contains(&role) {
            unassignable_roles = Some(role);
        }

        let roles = context
            .data()
            .db
            .get_linked_roles_for_product(guild_id, product_id)
            .await?;
        let mut message_lines = String::new();
        for role in roles {
            message_lines.push_str(format!("\n- <@&{}>", role.get()).as_str());
        }

        let embed = CreateEmbed::default()
            .title("Product Link Successful")
            .description(format!(
                "{} will now grant the following roles:{}",
                product, message_lines
            ))
            .color(Colour::DARK_GREEN);
        let reply = CreateReply::default().embed(embed).ephemeral(true);
        if let Some(embed) = create_role_warning_from_unassignable(unassignable_roles.into_iter()) {
            reply.embed(embed)
        } else {
            reply
        }
    } else {
        error_reply("Error Linking Product", "Product not found.")
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

    let product_id = context
        .data()
        .api_cache
        .product_name_to_id(&context, &product)
        .await?;

    let reply = if let Some(product_id) = product_id {
        let guild_id = context
            .guild_id()
            .ok_or_else(|| JinxError::new("expected to be in a guild"))?;
        let assignable_roles = assignable_roles(&context, guild_id).await?;

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
        let mut message_lines = String::new();
        for role in &roles {
            message_lines.push_str(format!("\n- <@&{}>", role.get()).as_str());
        }

        let embed = CreateEmbed::default()
            .title("Product Unlink Successful")
            .description(format!(
                "{} will now grant the following roles:{}",
                product, message_lines
            ))
            .color(Colour::DARK_GREEN);
        let reply = CreateReply::default().embed(embed).ephemeral(true);
        if let Some(embed) = create_role_warning_from_roles(&assignable_roles, roles.into_iter()) {
            reply.embed(embed)
        } else {
            reply
        }
    } else {
        error_reply("Error Unlinking Product", "Product not found.")
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
    #[description = "Product to modify role links for"]
    #[autocomplete = "product_version_autocomplete"]
    product_version: String,
    #[description = "Role to link"] role: RoleId, // note that Discord does not presently support variadic arguments: https://github.com/discord/discord-api-docs/discussions/3286
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let product_version_id = context
        .data()
        .api_cache
        .product_version_name_to_version_id(&context, &product_version)
        .await?;

    let reply = if let Some(product_version_id) = product_version_id {
        let guild_id = context
            .guild_id()
            .ok_or_else(|| JinxError::new("expected to be in a guild"))?;
        let assignable_roles = assignable_roles(&context, guild_id).await?;

        let mut unassignable_roles: Option<RoleId> = None;
        context
            .data()
            .db
            .link_product_version(guild_id, product_version_id.clone(), role)
            .await?;
        if !assignable_roles.contains(&role) {
            unassignable_roles = Some(role);
        }

        let roles = context
            .data()
            .db
            .get_linked_roles_for_product_version(guild_id, product_version_id)
            .await?;
        let mut message_lines = String::new();
        for role in roles {
            message_lines.push_str(format!("\n- <@&{}>", role.get()).as_str());
        }

        let embed = CreateEmbed::default()
            .title("Product Version Link Successful")
            .description(format!(
                "{} will now grant the following roles:{}",
                product_version, message_lines
            ))
            .color(Colour::DARK_GREEN);
        let reply = CreateReply::default().embed(embed).ephemeral(true);
        if let Some(embed) = create_role_warning_from_unassignable(unassignable_roles.into_iter()) {
            reply.embed(embed)
        } else {
            reply
        }
    } else {
        error_reply(
            "Error Linking Product Version",
            "Product version not found.",
        )
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
    #[description = "Product to modify role links for"]
    #[autocomplete = "product_version_autocomplete"]
    product_version: String,
    #[description = "Role to unlink"] role: RoleId, // note that Discord does not presently support variadic arguments: https://github.com/discord/discord-api-docs/discussions/3286
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let product_version_id = context
        .data()
        .api_cache
        .product_version_name_to_version_id(&context, &product_version)
        .await?;

    let reply = if let Some(product_version_id) = product_version_id {
        let guild_id = context
            .guild_id()
            .ok_or_else(|| JinxError::new("expected to be in a guild"))?;
        let assignable_roles = assignable_roles(&context, guild_id).await?;

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
        let mut message_lines = String::new();
        for role in &roles {
            message_lines.push_str(format!("\n- <@&{}>", role.get()).as_str());
        }

        let embed = CreateEmbed::default()
            .title("Product Version Unlink Successful")
            .description(format!(
                "{} will now grant the following roles:{}",
                product_version, message_lines
            ))
            .color(Colour::DARK_GREEN);
        let reply = CreateReply::default().embed(embed).ephemeral(true);
        if let Some(embed) = create_role_warning_from_roles(&assignable_roles, roles.into_iter()) {
            reply.embed(embed)
        } else {
            reply
        }
    } else {
        error_reply(
            "Error Unlinking Product Version",
            "Product version not found.",
        )
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

    let assignable_roles = assignable_roles(&context, guild_id).await?;
    let links = context.data().db.get_links(guild_id).await?;
    let message = if links.is_empty() {
        "No product→role links configured".to_string()
    } else {
        let mut linked_roles: Vec<RoleId> = links.keys().copied().collect();
        linked_roles.sort_unstable(); // make sure the roles are listed in a consistent order and not subject to HashMap randomization
        context
            .data()
            .api_cache
            .get(&context, |cache| {
                let mut first_line = true;
                let mut message = String::new();
                for role in linked_roles {
                    let mut first_name_in_line = true;
                    if first_line {
                        first_line = false;
                    } else {
                        message.push('\n');
                    }
                    message.push_str(format!("- <@&{}> granted by ", role.get()).as_str());
                    let link_sources = links.get(&role).expect("we just queried a map with its own key list, how the hell is it missing an entry now?");
                    for link_source in link_sources {
                        let name = match link_source {
                            LinkSource::GlobalBlanket => "`*`".to_string(),
                            LinkSource::ProductBlanket { product_id } => {
                                cache.product_id_to_name(product_id).unwrap_or(product_id.as_str()).to_string()
                            }
                            LinkSource::ProductVersion(product_version_id) => {
                                // obnoxiously this one format requires this whole block to return String vs &str
                                cache.product_version_id_to_name(product_version_id)
                                    .map(|str| str.to_string())
                                    .unwrap_or_else(|| format!("{product_version_id}"))
                            }
                        };
                        if first_name_in_line {
                            first_name_in_line = false;
                            message.push_str(name.as_str());
                        } else {
                            message.push_str(", ");
                            message.push_str(name.as_str());
                        }
                    }
                }
                message
            }).await?
    };

    let unassignable_embed =
        create_role_warning_from_roles(&assignable_roles, links.keys().copied());
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
