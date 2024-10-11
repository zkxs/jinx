// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::bot::commands::util::license_to_id;
use crate::bot::{Context, MISSING_API_KEY_MESSAGE};
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::license::LOCKING_USER_ID;
use poise::serenity_prelude as serenity;
use poise::CreateReply;
use serenity::{ButtonStyle, ChannelId, Colour, CreateActionRow, CreateButton, CreateEmbed, CreateMessage, Role, RoleId};
use std::collections::{HashMap, HashSet};
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
pub(in crate::bot) async fn stats(
    context: Context<'_>,
) -> Result<(), Error> {
    let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;
    let license_activation_count = context.data().db.guild_license_activation_count(guild_id).await.unwrap();
    let product_role_count = context.data().db.guild_product_role_count(guild_id).await.unwrap();

    let message = format!(
        "license activations={license_activation_count}\n\
        product→role links={product_role_count}"
    );
    let embed = CreateEmbed::default()
        .title("Jinx Stats")
        .description(message);
    context.send(
        CreateReply::default()
            .embed(embed)
            .ephemeral(true)
    ).await?;
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
    let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;

    // if setting a channel, then attempt to write a test log to the channel
    let test_result = match channel {
        Some(channel) => {
            let message = CreateMessage::default()
                .content("I will now log to this channel.");
            channel.send_message(context, message).await.map(|_| ())
        }
        None => {
            Ok(())
        }
    };

    match test_result {
        Ok(()) => {
            // test log worked, so set the channel
            context.data().db.set_log_channel(guild_id, channel).await?;

            // let the user know what we just did
            let message = if let Some(channel) = channel {
                format!("Bot log channel set to <#{}>.", channel.get())
            } else {
                "Bot log channel unset.".to_string()
            };
            let reply = CreateReply::default()
                .ephemeral(true)
                .content(message);
            context.send(reply).await?;
        }
        Err(e) => {
            // test log failed, so let the user know
            let reply = CreateReply::default()
                .ephemeral(true)
                .content(format!("Log channel not set because there was an error sending a message to <#{}>: {}. Please check bot and channel permissions.", channel.unwrap().get(), e));
            context.send(reply).await?;
            warn!("Error sending message to test log channel: {:?}", e);
        }
    }

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
pub(in crate::bot) async fn create_post(
    context: Context<'_>,
) -> Result<(), Error> {
    let channel = context.channel_id();

    let components = vec![
        CreateActionRow::Buttons(vec![CreateButton::new(REGISTER_BUTTON_ID).label("Register").style(ButtonStyle::Primary)]),
    ];

    let api_key = context.data().db.get_jinxxy_api_key(context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?).await?
        .ok_or(JinxError::new("Jinxxy API key is not set"))?;
    match jinxxy::get_own_user(&api_key).await {
        Ok(jinxxy_user) => {
            let embed = CreateEmbed::default()
                .title("Jinxxy Product Registration")
                .description(format!("Press the button below to register a Jinxxy license key for any of {} products. You can find your license key in your email receipt or at [jinxxy.com](<https://jinxxy.com/my/inventory>).", jinxxy_user.name_possessive()));
            let embed = if let Some(profile_image_url) = jinxxy_user.profile_image_url {
                embed.thumbnail(profile_image_url)
            } else {
                embed
            };

            let message = CreateMessage::default()
                .embed(embed)
                .components(components);

            let message = if let Err(e) = channel.send_message(context, message).await {
                warn!("Error in /create_post when sending message: {:?}", e);
                "Post not created because there was an error sending a message to this channel. Please check bot and channel permissions."
            } else {
                "Registration post created!"
            };
            let reply = CreateReply::default()
                .ephemeral(true)
                .content(message);
            context.send(reply).await?;
        }
        Err(e) => {
            let reply = CreateReply::default()
                .ephemeral(true)
                .content(format!("Could not get info for your Jinxxy user: {}", e));
            context.send(reply).await?;
        }
    }
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
pub(in crate::bot) async fn list_links(
    context: Context<'_>,
) -> Result<(), Error> {
    let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;
    if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
        let mut links = context.data().db.get_links(guild_id).await?;
        let message = if links.is_empty() {
            "No product→role links configured".to_string()
        } else {
            links.sort_unstable_by(|a, b| a.1.cmp(&b.1)); // sort by role
            let mut message: String = "All product→role links:".to_string();
            let mut current_role = None;
            let products = jinxxy::get_products(&api_key).await?;
            let products: HashMap<String, String, ahash::RandomState> = products.into_iter()
                .map(|product| (product.id, product.name))
                .collect();
            for (product_id, role) in links {
                let product_name = products.get(&product_id)
                    .map(|name| format!("\"{}\"", name))
                    .unwrap_or_else(|| product_id.clone());
                if current_role != Some(role) {
                    current_role = Some(role);
                    message.push_str(format!("\n- <@&{}> grants {}", role.get(), product_name).as_str());
                } else {
                    message.push_str(format!(", {}", product_name).as_str());
                }
            }
            message
        };
        context.send(CreateReply::default().content(message).ephemeral(true)).await?;
    } else {
        context.send(CreateReply::default().content(MISSING_API_KEY_MESSAGE).ephemeral(true)).await?;
    }
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
    let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;

    if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
        let license_ids = context.data().db.get_user_licenses(guild_id, user.id.get()).await?;
        let message = if license_ids.is_empty() {
            format!("<@{}> has no license activations.", user.id.get())
        } else {
            let mut message = format!("Licenses for <@{}>:", user.id.get());

            // build a cache of product versions that we need names for
            // Map structure: product_id -> {product_version_id -> product_version_name}
            let mut product_cache: HashMap<String, Option<HashMap<String, String, ahash::RandomState>>, ahash::RandomState> = Default::default();

            for license_id in license_ids {
                let license_info = jinxxy::check_license_id(&api_key, &license_id).await?;
                if let Some(license_info) = license_info {
                    let product_version_cache = if let Some(product) = product_cache.get(&license_info.product_id) {
                        product.as_ref()
                    } else {
                        let result = jinxxy::get_product(&api_key, &license_info.product_id).await;
                        if let Err(e) = &result {
                            warn!("Error looking up product info for {}, which is in license {}: {:?}", license_info.product_id, license_id, e);
                        }
                        let result = result.ok().map(|product| {
                            product.versions.into_iter()
                                .map(|version| (version.id, version.name))
                                .collect()
                        });
                        product_cache.entry(license_info.product_id.clone())
                            .or_insert(result).as_ref() // kind of a weird use of this API because there's an extra empty check but oh well. We can't use or_insert_with because async reasons.
                    };
                    let product_version_name = product_version_cache
                        .and_then(|cache| license_info.product_version_id.as_ref().and_then(|version_id| cache.get(version_id)))
                        .map(|version| format!("\"{}\"", version))
                        .unwrap_or("`null`".to_string());

                    let locked = context.data().db.is_license_locked(guild_id, license_id.clone()).await?;

                    let username = if let Some(username) = &license_info.username {
                        format!("[{}](<{}>)", username, license_info.profile_url().ok_or_else(|| JinxError::new("expected profile_url to exist when username is set"))?)
                    } else {
                        format!("`{}`", license_info.user_id)
                    };

                    message.push_str(format!(
                        "\n- `{}` activations={} locked={} user={} product=\"{}\" version={}",
                        license_info.short_key,
                        license_info.activations, // this field came from Jinxxy and is up to date
                        locked, // this field came from the local DB and may be out of sync
                        username,
                        license_info.product_name,
                        product_version_name
                    ).as_str());
                } else {
                    // we had a license ID in our local DB, but could not find info on it in the Jinxxy API
                    message.push_str(format!("\n- ID=`{}` (no data found)", license_id).as_str());
                }
            }
            message
        };

        let reply = CreateReply::default()
            .ephemeral(true)
            .content(message);
        context.send(reply).await?;
    } else {
        context.send(CreateReply::default().content(MISSING_API_KEY_MESSAGE).ephemeral(true)).await?;
    }
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
    let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;

    if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
        let license_id = license_to_id(&api_key, &license).await?;
        if let Some(license_id) = license_id {
            let activations = context.data().db.get_user_license_activations(guild_id, user.id.get(), license_id.clone()).await?;
            for activation_id in activations {
                let license_id = license_id.clone();
                jinxxy::delete_license_activation(&api_key, &license_id, &activation_id).await?;
                context.data().db.deactivate_license(guild_id, license_id, activation_id, user.id.get()).await?;
            }
            let reply = CreateReply::default()
                .ephemeral(true)
                .content(format!("All of <@{}>'s activations for `{}` have been deleted.", user.id.get(), license));
            context.send(reply).await?;
        } else {
            let reply = CreateReply::default()
                .ephemeral(true)
                .content(format!("License `{}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server.", license));
            context.send(reply).await?;
        }
    } else {
        context.send(CreateReply::default().content(MISSING_API_KEY_MESSAGE).ephemeral(true)).await?;
    }
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
    let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;

    if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
        let license_id = license_to_id(&api_key, &license).await?;
        if let Some(license_id) = license_id {
            // look up license usage info from local DB: this avoids doing some expensive Jinxxy API requests
            let license_users = context.data().db.get_license_users(guild_id, license_id).await?;
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
            let reply = CreateReply::default()
                .ephemeral(true)
                .content(message);
            context.send(reply).await?;
        } else {
            let reply = CreateReply::default()
                .ephemeral(true)
                .content(format!("License `{}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server.", license));
            context.send(reply).await?;
        }
    } else {
        context.send(CreateReply::default().content(MISSING_API_KEY_MESSAGE).ephemeral(true)).await?;
    }
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
    let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;

    if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
        let license_id = license_to_id(&api_key, &license).await?;
        if let Some(license_id) = license_id {
            let activation_id = jinxxy::create_license_activation(&api_key, &license_id, LOCKING_USER_ID).await?;
            context.data().db.activate_license(guild_id, license_id, activation_id, LOCKING_USER_ID).await?;
            let reply = CreateReply::default()
                .ephemeral(true)
                .content(format!("License `{}` is now locked and cannot be used to grant roles.", license));
            context.send(reply).await?;
        } else {
            let reply = CreateReply::default()
                .ephemeral(true)
                .content(format!("License `{}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server.", license));
            context.send(reply).await?;
        }
    } else {
        context.send(CreateReply::default().content(MISSING_API_KEY_MESSAGE).ephemeral(true)).await?;
    }
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
    let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;

    if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
        let license_id = license_to_id(&api_key, &license).await?;
        if let Some(license_id) = license_id {
            let activations = jinxxy::get_license_activations(&api_key, &license_id).await?;
            let lock_activation_id = activations.into_iter()
                .find(|activation| activation.is_lock())
                .map(|activation| activation.id);

            let message = if let Some(lock_activation_id) = lock_activation_id {
                jinxxy::delete_license_activation(&api_key, &license_id, &lock_activation_id).await?;
                context.data().db.deactivate_license(guild_id, license_id, lock_activation_id, LOCKING_USER_ID).await?;
                format!("License `{}` is now unlocked and may be used to grant roles.", license)
            } else {
                format!("License `{}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server.", license)
            };

            let reply = CreateReply::default()
                .ephemeral(true)
                .content(message);
            context.send(reply).await?;
        } else {
            let reply = CreateReply::default()
                .ephemeral(true)
                .content(format!("License `{}` not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server.", license));
            context.send(reply).await?;
        }
    } else {
        context.send(CreateReply::default().content(MISSING_API_KEY_MESSAGE).ephemeral(true)).await?;
    }
    Ok(())
}

/// Initializes autocomplete data, and then does the product autocomplete
async fn product_autocomplete(context: Context<'_>, product_prefix: &str) -> impl Iterator<Item=String> {
    match context.data().api_cache.product_names_with_prefix(&context, product_prefix).await {
        Ok(result) => result.into_iter(),
        Err(e) => {
            warn!("Failed to read API cache: {:?}", e);
            Vec::new().into_iter()
        }
    }
}

/// Link a product to roles. Activating a license for the product will grant linked roles.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild",
)]
pub(in crate::bot) async fn link_product(
    context: Context<'_>,
    #[description = "Product to modify role links for"]
    #[autocomplete = "product_autocomplete"] product: String,
    #[description = "Roles to link"] roles: Vec<RoleId>,
) -> Result<(), Error> {
    let product_id = context.data().api_cache.product_name_to_id(&context, &product).await?;

    if let Some(product_id) = product_id {
        let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;
        let assignable_roles: HashSet<RoleId, ahash::RandomState> = {
            let bot_id = context.framework().bot_id;
            let bot_member = guild_id.member(context, bot_id).await?;
            let permissions = bot_member.permissions(context)?;

            // for some reason if the scope of `guild` is too large the compiler loses its mind
            if permissions.manage_roles() {
                let guild = context.guild().ok_or(JinxError::new("expected to be in a guild"))?;
                let highest_role = guild.member_highest_role(&bot_member);
                if let Some(highest_role) = highest_role {
                    let everyone_id = guild.role_by_name("@everyone").map(|role| role.id);
                    let mut roles: Vec<&Role> = guild.roles.values()
                        .filter(|role| Some(role.id) != everyone_id) // @everyone is weird, don't use it
                        .filter(|role| role.position < highest_role.position) // roles above our highest can't be managed
                        .filter(|role| !role.managed) // managed roles can't be managed
                        .collect();
                    roles.sort_unstable_by(|a, b| u16::cmp(&b.position, &a.position));
                    roles.into_iter()
                        .map(|role| role.id)
                        .collect()
                } else {
                    Default::default()
                }
            } else {
                Default::default()
            }
        };

        let mut warned_roles: HashSet<u64, ahash::RandomState> = HashSet::with_hasher(Default::default());
        for role in &roles {
            context.data().db.link_product(guild_id, product_id.clone(), *role).await?;
            if !assignable_roles.contains(role) && !warned_roles.contains(&role.get()) {
                warned_roles.insert(role.get());
            }
        }

        let roles = context.data().db.get_roles(guild_id, product_id).await?;
        let mut message_lines = String::new();
        for role in roles {
            message_lines.push_str(format!("\n- <@&{}>", role.get()).as_str());
        }

        let embed = CreateEmbed::default()
            .title("Product Link Successful")
            .description(format!("{} will now grant the following roles:{}", product, message_lines))
            .color(Colour::DARK_GREEN);
        let reply = CreateReply::default()
            .embed(embed)
            .components(Default::default())
            .ephemeral(true);
        let reply = if warned_roles.is_empty() {
            reply
        } else {
            // warn if the roles cannot be assigned (too high, or we lack the perm)
            let mut warning_lines = String::new();
            for role in warned_roles {
                warning_lines.push_str(format!("\n- <@&{}>", role).as_str());
            }
            let embed = CreateEmbed::default()
                .title("Warning")
                .description(format!("I don't currently have access to grant the following roles. Please check bot permissions.{}", warning_lines))
                .color(Colour::ORANGE);
            reply.embed(embed)
        };
        context.send(reply).await?;
    } else {
        context.send(CreateReply::default().content("Product not found.").ephemeral(true)).await?;
    }

    Ok(())
}

/// Unlink a product from roles.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild",
)]
pub(in crate::bot) async fn unlink_product(
    context: Context<'_>,
    #[description = "Product to modify role links for"]
    #[autocomplete = "product_autocomplete"] product: String,
    #[description = "Roles to unlink"] roles: Vec<RoleId>,
) -> Result<(), Error> {
    let product_id = context.data().api_cache.product_name_to_id(&context, &product).await?;

    if let Some(product_id) = product_id {
        let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;

        for role in &roles {
            context.data().db.unlink_product(guild_id, product_id.clone(), *role).await?;
        }

        let roles = context.data().db.get_roles(guild_id, product_id).await?;
        let mut message_lines = String::new();
        for role in roles {
            message_lines.push_str(format!("\n- <@&{}>", role.get()).as_str());
        }

        let embed = CreateEmbed::default()
            .title("Product Link Successful")
            .description(format!("{} will now grant the following roles:{}", product, message_lines))
            .color(Colour::DARK_GREEN);
        let reply = CreateReply::default()
            .embed(embed)
            .components(Default::default())
            .ephemeral(true);
        context.send(reply).await?;
    } else {
        context.send(CreateReply::default().content("Product not found.").ephemeral(true)).await?;
    }

    Ok(())
}
