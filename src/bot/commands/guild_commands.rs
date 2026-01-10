// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::bot::util::{error_reply, success_reply};
use crate::bot::{Context, MISSING_STORE_LINK_MESSAGE, util};
use crate::db::{ActivationCounts, LinkSource};
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::http::jinxxy::{GetProfileImageUrl as _, GetUsername as _, Username};
use crate::license::LOCKING_USER_ID;
use ahash::HashSet;
use jiff::Timestamp;
use poise::{CreateReply, serenity_prelude as serenity};
use serenity::{
    ButtonStyle, Colour, CreateActionRow, CreateAutocompleteResponse, CreateButton, CreateComponent, CreateEmbed,
    CreateMessage, GenericChannelId, GuildId, RoleId,
};
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt::Write;
use tokio::task::JoinSet;
use tracing::{error, info, warn};

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

    let message = format!(
        "license activations (7d)={day_7}\n\
        license activations (30d)={day_30}\n\
        license activations (90d)={day_90}\n\
        license activations (1yr)={day_365}\n\
        license activations (lifetime)={lifetime}\n\
        gumroad licenses rejected={gumroad_failure_count}"
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
    #[description = "user to query licenses for"] channel: Option<GenericChannelId>, // we can't use Channel here because it throws FrameworkError::ArgumentParse on access problems, which cannot be handled cleanly.
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
            let test_result = message.execute(context.as_ref(), channel).await.map(|_| ());

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

/// Initializes autocomplete data, and then does the store name autocomplete
async fn store_name_autocomplete<'a>(context: Context<'_>, store_name_prefix: &str) -> CreateAutocompleteResponse<'a> {
    match context.guild_id() {
        Some(guild_id) => {
            match context
                .data()
                .db
                .autocomplete_jinxxy_username(guild_id, store_name_prefix)
                .await
            {
                Ok(result) => util::create_autocomplete_response(result.into_iter()),
                Err(e) => {
                    warn!("Failed to autocomplete Jinxxy username: {:?}", e);
                    CreateAutocompleteResponse::new()
                }
            }
        }
        None => {
            error!("someone is somehow doing store name autocomplete without being in a guild");
            CreateAutocompleteResponse::new()
        }
    }
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
    #[description = "store this license belongs to"]
    #[autocomplete = "store_name_autocomplete"]
    store_name: String,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let channel = context.channel_id();

    let guild = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let reply = if let Some(store) = context.data().db.get_store_link_by_username(guild, &store_name).await? {
        match jinxxy::get_own_user(&store.jinxxy_api_key).await {
            Ok(jinxxy_user) => {
                // might as well do a sanity check while I've got the data in scope. This shouldn't ever fail.
                if jinxxy_user.id != store.jinxxy_user_id {
                    /*
                    Hello, dear reader. If you're here it means that somehow an API key in the database currently has a user ID
                    that does not match the user ID stored alongside that API key in the database. This should not be possible,
                    as the user ID is assumed to be immutable and the database is assumed to be uncorruptible.
                     */
                    let nonce: u64 = util::generate_nonce();
                    error!(
                        "NONCE[{}] ID MISMATCH MAJOR FUCKUP!!! API user_id: \"{}\" DB jinxxy_user_id: \"{}\"",
                        nonce, jinxxy_user.id, store.jinxxy_user_id
                    );
                    // if it DOES somehow fail, we should stop here.
                    Err(JinxError::new(format!(
                        "A critical sanity-check has failed. Please report this to the bot developer with error code `{}`",
                        nonce
                    )))?;
                }

                let display_user = jinxxy::DisplayUser::from(&jinxxy_user); // convert into just the data we need for this command
                let display_name = display_user.name_possessive();
                let display_name = if let Some(profile_url) = jinxxy_user.username().profile_url() {
                    format!("[{display_name}](<{profile_url}>)")
                } else {
                    display_name
                };
                let embed = CreateEmbed::default()
                    .title("Jinxxy Product Registration")
                    .description(format!(
                        "Press the button below to register a Jinxxy license key for any of {display_name} products."
                    ));
                let embed = if let Some(profile_image_url) = display_user.profile_image_url() {
                    embed.thumbnail(profile_image_url)
                } else {
                    embed
                };

                // embed the store ID into the register button
                // note that custom id can be AT MOST 100 characters long or Discord will explode
                let buttons = [CreateButton::new(format!("{}:{}", REGISTER_BUTTON_ID, jinxxy_user.id))
                    .label("Register")
                    .style(ButtonStyle::Primary)];
                let action_row = CreateActionRow::Buttons(buttons.as_ref().into());
                let components = [CreateComponent::ActionRow(action_row)];
                let message = CreateMessage::default().embed(embed).components(&components);
                if let Err(e) = message.execute(context.as_ref(), channel).await {
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
        }
    } else {
        // happens if we couldn't find a linked store with that username
        error_reply("Error Creating Post", MISSING_STORE_LINK_MESSAGE)
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

    // look up licenses from the local DB, which is the only way we can do this without scraping every license activation from Jinxxy
    let license_ids = context.data().db.get_user_licenses(guild_id, user.id.get()).await?;
    let message = if license_ids.is_empty() {
        format!("<@{}> has no license activations.", user.id.get())
    } else {
        let license_id_count = license_ids.len();

        /*
        start looking up each license in jinxxy to get the associated product info
        this SEEMS like static data, we could backfill it into the local DB to save some API calls here, however
        in the case where a license has been deleted from Jinxxy but is known to the bot, it's nice to display it here
        */
        let mut license_check_join_set = JoinSet::new();
        for license_id in license_ids {
            license_check_join_set.spawn(async move {
                jinxxy::check_license_id(
                    license_id.jinxxy_api_key.as_str(),
                    license_id.license_id.as_str(),
                    false,
                )
                .await
                .map(|license_info| (license_id, license_info))
            });
        }

        // figure out which product versions we need version names for and start those requests in the background
        let mut product_lookup_join_set = JoinSet::new();
        let mut products_requiring_lookup = HashSet::with_hasher(ahash::RandomState::default());
        let mut license_infos: Vec<_> = Vec::with_capacity(license_id_count);
        while let Some(result) = license_check_join_set.join_next().await {
            let tuple = result??;
            let (license_id, license_info) = &tuple;
            if let Some(license_info) = license_info
                && license_info.product_version_info.is_some()
            {
                let key = (license_id.jinxxy_user_id.clone(), license_info.product_id.clone());
                let newly_inserted = products_requiring_lookup.insert(key);
                if newly_inserted {
                    let api_key = license_id.jinxxy_api_key.clone();
                    let jinxxy_user_id = license_id.jinxxy_user_id.clone();
                    let product_id = license_info.product_id.clone();
                    product_lookup_join_set.spawn(async move {
                        jinxxy::get_product(&api_key, &product_id)
                            .await
                            .map(|product_info| (jinxxy_user_id, product_info))
                    });
                }
            }
            license_infos.push(tuple);
        }

        // for each product that we needed version info for extract version name into a map
        // map structure: {store+product_id -> version_id -> version_name}
        let mut product_version_name_cache: HashMap<
            (String, String),
            HashMap<String, String, ahash::RandomState>,
            ahash::RandomState,
        > = Default::default();
        while let Some(result) = product_lookup_join_set.join_next().await {
            let (jinxxy_user_id, product) = result??;
            for version in product.versions {
                product_version_name_cache
                    .entry((jinxxy_user_id.clone(), product.id.clone()))
                    .or_default()
                    .insert(version.id, version.name);
            }
        }

        let mut message = format!("Licenses for <@{}>:", user.id.get());
        for license_info in license_infos {
            match license_info {
                (license_id, Some(license_info)) => {
                    let map_key = (license_id.jinxxy_user_id, license_info.product_id);
                    let (jinxxy_user_id, _product_id) = &map_key;
                    let product_version_name = license_info
                        .product_version_info
                        .as_ref()
                        .map(|version| {
                            product_version_name_cache
                                .get(&map_key)
                                .and_then(|inner| inner.get(&version.product_version_id))
                                .map(|string| string.as_str())
                                .unwrap_or("`ERROR`")
                        })
                        .unwrap_or("`null`");

                    let locked = context
                        .data()
                        .db
                        .is_license_locked(jinxxy_user_id.as_str(), license_info.license_id.as_str())
                        .await?
                        .is_some();

                    let username =
                        Username::format_discord_display_name(&license_info.user_id, license_info.username.as_deref());
                    let store_identifier =
                        Username::format_discord_display_name(jinxxy_user_id, license_id.jinxxy_username.as_deref());

                    message.push_str(
                        format!(
                            "\n- `{}` store={} activations={} locked={} user={} product=\"{}\" version={}",
                            license_info.short_key,
                            store_identifier,
                            license_info.activations, // this field came from Jinxxy and is up to date
                            locked,                   // this field came from the local DB and may be out of sync
                            username,
                            license_info.product_name,
                            product_version_name
                        )
                        .as_str(),
                    );
                }
                (license_id, None) => {
                    // we had a license ID in our local DB, but could not find info on it in the Jinxxy API
                    message.push_str(
                        format!(
                            "\n- store=`{}` id=`{}` (no data found)",
                            license_id.jinxxy_user_id, license_id.license_id
                        )
                        .as_str(),
                    );
                }
            }
        }
        message
    };
    let reply = success_reply("User Info", message);
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
    #[description = "store this license belongs to"]
    #[autocomplete = "store_name_autocomplete"]
    store_name: String,
    #[description = "Jinxxy license to deactivate for user"] license: String,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let reply = if let Some(store) = context
        .data()
        .db
        .get_store_link_by_username(guild_id, &store_name)
        .await?
    {
        let license_id = util::trusted_license_to_id(&store.jinxxy_api_key, &license).await?;
        if let Some(license_id) = license_id {
            let activations = context
                .data()
                .db
                .get_user_license_activations(&store.jinxxy_user_id, user.id.get(), &license_id)
                .await?;
            for activation_id in activations {
                let license_id = license_id.clone();
                jinxxy::delete_license_activation(&store.jinxxy_api_key, &license_id, &activation_id).await?;
                context
                    .data()
                    .db
                    .deactivate_license(&store.jinxxy_user_id, &license_id, &activation_id, user.id.get())
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
        error_reply("Error Deactivating License", MISSING_STORE_LINK_MESSAGE)
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
    #[description = "store this license belongs to"]
    #[autocomplete = "store_name_autocomplete"]
    store_name: String,
    #[description = "Jinxxy license to query activations for"] license: String,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let reply = if let Some(store) = context
        .data()
        .db
        .get_store_link_by_username(guild_id, &store_name)
        .await?
    {
        let license_id = util::trusted_license_to_id(&store.jinxxy_api_key, &license).await?;
        if let Some(license_id) = license_id {
            // look up license usage info from local DB and from Jinxxy concurrently
            let db_copy = context.data().db.clone();
            let (local_license_users, license_info, remote_license_users) = tokio::join!(
                db_copy.get_license_users(&store.jinxxy_user_id, &license_id),
                async {
                    let api_key = store.jinxxy_api_key.to_owned();
                    let license_id = license_id.clone();
                    jinxxy::check_license_id(&api_key, &license_id, true).await
                },
                async {
                    let api_key = store.jinxxy_api_key.to_owned();
                    let license_id = license_id.clone();
                    jinxxy::get_license_activations(&api_key, &license_id, None).await
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

                let message = if remote_license_users.is_empty() {
                    format!(
                        "ID: `{}`\nShort: `{}`\nLong: `{}`\nValid for {} {}\n\nNo registered users.",
                        license_info.license_id, license_info.short_key, license_info.key, product_name, version_name
                    )
                } else {
                    let mut message = format!(
                        "ID: `{}`\nShort: `{}`\nLong: `{}`\nValid for {} {}\n\nRegistered users:",
                        license_info.license_id, license_info.short_key, license_info.key, product_name, version_name
                    );
                    for user_id in &remote_license_users {
                        if *user_id == LOCKING_USER_ID {
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
                // that or an invalid ID was provided directly
                let message = if remote_license_users.is_empty() {
                    format!(
                        "License `{license} not found: please verify that the key is correct and belongs to the Jinxxy account linked to this Discord server.`"
                    )
                } else {
                    let mut message = format!("License `{license}` not found, but somehow has users:");
                    for user_id in &remote_license_users {
                        if *user_id == LOCKING_USER_ID {
                            message.push_str("\n- **LOCKED** (prevents further use)");
                        } else {
                            message.push_str(format!("\n- <@{user_id}>").as_str());
                        }
                    }
                    message.push_str("\nThis may indicate that the license has been revoked on the Jinxxy side.");
                    message
                };
                let reply = error_reply("License Info Validation Error", message);
                if local_license_users == remote_license_users {
                    reply
                } else {
                    let embed = CreateEmbed::default()
                        .title("Activator mismatch")
                        .description("The local and remote activator lists do not match. This is really weird and you should tell the bot dev about it, because chances are you are the first person seeing this message ever.")
                        .color(Colour::RED);
                    reply.embed(embed)
                }
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
        error_reply("Error Getting License Info", MISSING_STORE_LINK_MESSAGE)
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
    #[description = "store this license belongs to"]
    #[autocomplete = "store_name_autocomplete"]
    store_name: String,
    #[description = "Jinxxy license to lock"] license: String,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let reply = if let Some(store) = context
        .data()
        .db
        .get_store_link_by_username(guild_id, &store_name)
        .await?
    {
        let license_id = util::trusted_license_to_id(&store.jinxxy_api_key, &license).await?;
        if let Some(license_id) = license_id {
            let activations = jinxxy::get_license_activations(
                &store.jinxxy_api_key,
                &license_id,
                Some(jinxxy::LOCKING_ACTIVATION_DESCRIPTION),
            )
            .await?;
            let mut lock_activation_ids = activations.into_iter().filter(|activation| activation.is_lock()); // should not be needed with the search_query set

            let (activation_id, created_at) = if let Some(activation) = lock_activation_ids.next() {
                // we'll bump the existing DB entry
                (activation.id, activation.created_at)
            } else {
                // we'll create a new DB entry
                let activation_id =
                    jinxxy::create_license_activation(&store.jinxxy_api_key, &license_id, LOCKING_USER_ID).await?;
                let created_at = Timestamp::now();
                (activation_id, created_at)
            };
            context
                .data()
                .db
                .activate_license(
                    &store.jinxxy_user_id,
                    &license_id,
                    &activation_id,
                    LOCKING_USER_ID,
                    None,
                    None,
                    &created_at,
                )
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
        error_reply("Error Locking License", MISSING_STORE_LINK_MESSAGE)
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
    #[description = "store this license belongs to"]
    #[autocomplete = "store_name_autocomplete"]
    store_name: String,
    #[description = "Jinxxy license to unlock"] license: String,
) -> Result<(), Error> {
    context.defer_ephemeral().await?;

    let guild_id = context
        .guild_id()
        .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

    let reply = if let Some(store) = context
        .data()
        .db
        .get_store_link_by_username(guild_id, &store_name)
        .await?
    {
        let license_id = util::trusted_license_to_id(&store.jinxxy_api_key, &license).await?;
        if let Some(license_id) = license_id {
            let remote_activations = jinxxy::get_license_activations(
                &store.jinxxy_api_key,
                &license_id,
                Some(jinxxy::LOCKING_ACTIVATION_DESCRIPTION),
            )
            .await?;
            let mut remote_lock_activation_ids = remote_activations
                .into_iter()
                .filter(|activation| activation.is_lock()) // should not be needed with the search_query set
                .map(|activation| activation.id)
                .peekable();

            let message = if remote_lock_activation_ids.peek().is_none() {
                // make sure local DB is clean too!
                let delete_count = context
                    .data()
                    .db
                    .unlock_license(&store.jinxxy_user_id, &license_id)
                    .await?;
                if delete_count == 0 {
                    format!("No locks for `{license}` were found.")
                } else {
                    format!(
                        "License `{license}` state was out of sync in Jinx's cache and in the Jinxxy API. This has been fixed, and the license is now unlcoed and may be used to grant role."
                    )
                }
            } else {
                for lock_activation_id in remote_lock_activation_ids {
                    jinxxy::delete_license_activation(&store.jinxxy_api_key, &license_id, &lock_activation_id).await?;
                }
                // make sure our DB is a completely clean slate
                context
                    .data()
                    .db
                    .unlock_license(&store.jinxxy_user_id, &license_id)
                    .await?;
                format!("License `{license}` is now unlocked and may be used to grant roles.")
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
        error_reply("Error Unlocking License", MISSING_STORE_LINK_MESSAGE)
    };
    context.send(reply).await?;
    Ok(())
}

/// Initializes autocomplete data, and then does the product autocomplete
async fn product_autocomplete<'a>(context: Context<'_>, product_prefix: &str) -> CreateAutocompleteResponse<'a> {
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
async fn product_version_autocomplete<'a>(
    context: Context<'_>,
    product_prefix: &str,
) -> CreateAutocompleteResponse<'a> {
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
        for id in product_ids {
            context
                .data()
                .db
                .link_product(&id.jinxxy_user_id, guild_id, &id.product_id, role)
                .await?;

            let roles = context
                .data()
                .db
                .get_linked_roles_for_product(&id.jinxxy_user_id, guild_id, &id.product_id)
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
        for id in product_ids {
            context
                .data()
                .db
                .unlink_product(&id.jinxxy_user_id, guild_id, &id.product_id, role)
                .await?;

            let roles = context
                .data()
                .db
                .get_linked_roles_for_product(&id.jinxxy_user_id, guild_id, &id.product_id)
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
        for id in product_version_ids {
            context
                .data()
                .db
                .link_product_version(&id.jinxxy_user_id, guild_id, &id.product_version_id, role)
                .await?;

            let roles = context
                .data()
                .db
                .get_linked_roles_for_product_version(&id.jinxxy_user_id, guild_id, &id.product_version_id)
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
        for id in product_version_ids {
            context
                .data()
                .db
                .unlink_product_version(&id.jinxxy_user_id, guild_id, &id.product_version_id, role)
                .await?;

            let roles = context
                .data()
                .db
                .get_linked_roles_for_product_version(&id.jinxxy_user_id, guild_id, &id.product_version_id)
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
    list_links_impl(context, guild_id, false).await
}

/// internal list links implementation. You MUST `context.defer_ephemeral()` on your own before calling this.
pub(in crate::bot) async fn list_links_impl(context: Context<'_>, guild_id: GuildId, sudo: bool) -> Result<(), Error> {
    let assignable_roles = util::assignable_roles(&context, guild_id).await?;
    // all links from the db for this guild, even across multiple stores
    let link_info = context.data().db.get_role_links(guild_id).await?;

    // handle deleted roles
    let mut linked_roles: Vec<RoleId> = link_info.links.keys().copied().collect();
    let deleted_roles = util::deleted_roles(&context, guild_id, linked_roles.iter().copied())?;
    if !deleted_roles.is_empty() {
        info!("Unlinking {} deleted roles in {}", deleted_roles.len(), guild_id);
    }
    for deleted_role in &deleted_roles {
        context.data().db.delete_role(guild_id, *deleted_role).await?;
    }
    linked_roles.retain(|role| !deleted_roles.contains(role));

    let message = if linked_roles.is_empty() {
        "No product→role links configured".to_string()
    } else {
        // We always do a cached read here: even in the case of a cache miss we just fall back to IDs instead of names.
        // The idea is that users should have a warmed cache already when /list_links is invoked. This is because both
        // /add_store and /link_product do background cache warming.
        linked_roles.sort_unstable(); // make sure the roles are listed in a consistent order and not subject to HashMap randomization
        let mut message = String::new();
        let mut first_line = true;
        for role in &linked_roles {
            if first_line {
                first_line = false;
            } else {
                message.push('\n');
            }
            if sudo {
                let role_name = util::role_name(&context, guild_id, *role)?;
                if let Some(role_name) = role_name {
                    message.push_str(format!("- `{}` granted by:", role_name).as_str());
                } else {
                    message.push_str(format!("- `<@&{}>` granted by:", role.get()).as_str());
                }
            } else {
                message.push_str(format!("- <@&{}> granted by:", role.get()).as_str());
            }
            let link_sources = link_info
                .links
                .get(role)
                .expect("we just queried a map with its own key list, how the hell is it missing an entry now?");
            for link_source in link_sources {
                // In the majority of cases we get a name &str here, so we use a Cow<str> to avoid a bunch of copies.
                // Only in the case where we have to show an un-cached product version do we need to build an owned String.
                let name = match link_source {
                    LinkSource::GlobalBlanket => Cow::Borrowed("`*`"),
                    LinkSource::ProductBlanket {
                        jinxxy_user_id,
                        product_id,
                    } => {
                        let store_display_name = link_info
                            .stores
                            .get(jinxxy_user_id)
                            .map(|s| s.as_str())
                            .unwrap_or(jinxxy_user_id.as_str());
                        context
                            .data()
                            .api_cache
                            .get(&context.data().db, guild_id, jinxxy_user_id, |cache| {
                                let product_name = cache.product_id_to_name(product_id).unwrap_or(product_id.as_str());
                                Cow::Owned(format!("{store_display_name}: {product_name}"))
                            })
                            .await?
                    }
                    LinkSource::ProductVersion {
                        jinxxy_user_id,
                        product_version_id,
                    } => {
                        let store_display_name = link_info
                            .stores
                            .get(jinxxy_user_id)
                            .map(|s| s.as_str())
                            .unwrap_or(jinxxy_user_id.as_str());
                        context
                            .data()
                            .api_cache
                            .get(&context.data().db, guild_id, jinxxy_user_id, |cache| {
                                let product_version_name = cache
                                    .product_version_id_to_name(product_version_id)
                                    .map(Cow::Borrowed)
                                    .unwrap_or_else(|| Cow::Owned(format!("{product_version_id}")));
                                Cow::Owned(format!("{store_display_name}: {product_version_name}"))
                            })
                            .await?
                    }
                };

                message.push_str("\n  - ");
                message.push_str(&name);
            }
        }
        message
    };

    let unassignable_embed = util::create_role_warning_from_roles(&assignable_roles, linked_roles.into_iter());
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
                member
                    .add_role(context.as_ref(), role, Some("/grant_missing_roles"))
                    .await?;
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
        let bot_log_result = bot_log_message.execute(context.as_ref(), log_channel).await;
        if let Err(e) = bot_log_result {
            warn!("Error logging to log channel in {}: {:?}", guild_id.get(), e);
        }
    }

    Ok(())
}
