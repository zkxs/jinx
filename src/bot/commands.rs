// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use super::Context;
use crate::error::JinxError;
use crate::http::{jinxxy, update_checker};
use crate::license::LOCKING_USER_ID;
use crate::{constants, license};
use poise::serenity_prelude as serenity;
use poise::CreateReply;
use serenity::{ButtonStyle, Colour, ComponentInteractionDataKind, CreateActionRow, CreateButton, CreateEmbed, CreateInteractionResponse, CreateMessage, CreateSelectMenu, CreateSelectMenuKind, CreateSelectMenuOption, Role, RoleId};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use poise::serenity_prelude::GuildId;
use tracing::{info, warn};
use super::check_owner;

// discord component ids
pub(super) const REGISTER_BUTTON_ID: &str = "jinx_register_button";
pub(super) const LICENSE_KEY_ID: &str = "jinx_license_key_input";
const PRODUCT_SELECT_ID: &str = "product_select";
const ROLE_SELECT_ID: &str = "role_select";
const LINK_PRODUCT_BUTTON: &str = "link_product_button";
const UNLINK_PRODUCT_BUTTON: &str = "unlink_product_button";

const MAX_SELECT_VALUES: u8 = 25;

/// Message shown to admins when the Jinxxy API key is missing
static MISSING_API_KEY_MESSAGE: &str = "Jinxxy API key is not set: please use the `/init` command to set it.";

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Shows bot version
#[poise::command(
    slash_command,
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(super) async fn version(
    context: Context<'_>,
) -> Result<(), Error> {
    let embed = CreateEmbed::default()
        .title("Version Check")
        .description(constants::DISCORD_BOT_VERSION);
    let reply = CreateReply::default()
        .ephemeral(true)
        .embed(embed);
    let version_check = update_checker::check_for_update().await;
    let reply = if version_check.is_warn() {
        let embed = CreateEmbed::default()
            .title("Warning")
            .color(Colour::ORANGE)
            .description(version_check.to_string());
        reply.embed(embed)
    } else if version_check.is_error() {
        let embed = CreateEmbed::default()
            .title("Error")
            .color(Colour::RED)
            .description(version_check.to_string());
        reply.embed(embed)
    } else {
        reply
    };
    context.send(reply).await?;
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
pub(super) async fn exit(
    context: Context<'_>,
) -> Result<(), Error> {
    info!("starting shutdown...");
    context.send(CreateReply::default().content("Shutting down now!").ephemeral(true)).await?;
    context.framework().shard_manager.shutdown_all().await;
    Ok(())
}

/// Get statistics about bot load and performance
#[poise::command(
    slash_command,
    default_member_permissions = "MANAGE_GUILD",
    check = "check_owner",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(super) async fn stats(
    context: Context<'_>,
) -> Result<(), Error> {
    let db_size = context.data().db.size().await.unwrap().div_ceil(1024);
    let users_len = context.serenity_context().cache.user_count();
    let guilds_len = context.serenity_context().cache.guild_count();
    let shards_len = context.serenity_context().cache.shard_count();
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
        users={users_len}\n\
        guilds={guilds_len}\n\
        shards={shards_len}{shard_list}"
    );
    context.send(
        CreateReply::default()
            .content(message)
            .ephemeral(true)
    ).await?;
    Ok(())
}

/// Set up Jinx for this Discord server
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(super) async fn init(
    context: Context<'_>,
    #[description = "Jinxxy API key"] api_key: Option<String>,
) -> Result<(), Error> {
    let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;

    // handle trimming the string
    let api_key = api_key
        .map(|api_key| api_key.trim().to_string())
        .filter(|api_key| !api_key.is_empty());

    if let Some(api_key) = api_key {
        // here we have a bit of an easter-egg to install owner commands
        if api_key == "install_owner_commands" {
            if check_owner(context).await? {
                let creator = context.data().db.get_jinxxy_api_key(guild_id).await?.is_some();

                //TODO: for some reason this sometimes times out and gives a 404 if the commands have
                // previously been deleted in the same bot process; HOWEVER it actually still succeeds.
                // I suspect this is a discord/serenity/poise bug.
                // For some <id>, <nonce>, this looks like:
                // Http(UnsuccessfulRequest(ErrorResponse { status_code: 404, url: "https://discord.com/api/v10/interactions/<id>/<nonce>/callback", method: POST, error: DiscordJsonError { code: 10062, message: "Unknown interaction", errors: [] } }))
                set_guild_commands(context, guild_id, true, creator).await?;

                context.send(CreateReply::default().content("Commands installed.").ephemeral(true)).await?;
            } else {
                context.send(CreateReply::default().content("Not an owner").ephemeral(true)).await?;
            }
        } else if api_key == "uninstall_owner_commands" {
            if check_owner(context).await? {
                let creator = context.data().db.get_jinxxy_api_key(guild_id).await?.is_some();
                set_guild_commands(context, guild_id, false, creator).await?;
                context.send(CreateReply::default().content("Commands uninstalled.").ephemeral(true)).await?;
            } else {
                context.send(CreateReply::default().content("Not an owner").ephemeral(true)).await?;
            }
        } else {
            // normal /init <key> use ends up in this branch
            context.data().db.set_jinxxy_api_key(guild_id, api_key.trim().to_string()).await?;
            let owner = check_owner(context).await?;
            set_guild_commands(context, guild_id, owner, true).await?;
            context.send(CreateReply::default().content("Done!").ephemeral(true)).await?;
        }
    } else if context.data().db.get_jinxxy_api_key(guild_id).await?.is_some() {
        // re-initialize commands but only if API key is already set
        let owner = check_owner(context).await?;
        set_guild_commands(context, guild_id, owner, true).await?;
        context.send(CreateReply::default().content("Commands reinstalled.").ephemeral(true)).await?;
    } else {
        context.send(CreateReply::default().content("Please provide a Jinxxy API key").ephemeral(true)).await?;
    }

    Ok(())
}

async fn set_guild_commands(context: Context<'_>, guild_id: GuildId, owner: bool, creator: bool) -> Result<(), Error> {
    let owner_commands = owner.then_some(super::OWNER_COMMANDS.iter()).into_iter().flatten();
    let creator_commands = creator.then_some(super::CREATOR_COMMANDS.iter()).into_iter().flatten();
    let command_iter = owner_commands.chain(creator_commands);
    let commands = poise::builtins::create_application_commands(command_iter);
    guild_id.set_commands(context, commands).await?;
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
pub(super) async fn create_post(
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
                .description(format!("Press the button below to register a Jinxxy license key for any of {} products.", jinxxy_user.name_possessive()));
            let embed = if let Some(profile_image_url) = jinxxy_user.profile_image_url {
                embed.thumbnail(profile_image_url)
            } else {
                embed
            };

            let message = CreateMessage::default()
                .embed(embed)
                .components(components);
            channel.send_message(context.serenity_context(), message).await?;

            let reply = CreateReply::default()
                .ephemeral(true)
                .content("Registration post created!");

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
pub(super) async fn list_links(
    context: Context<'_>,
) -> Result<(), Error> {
    let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;
    if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
        let links = context.data().db.get_links(guild_id).await?;
        let mut message: String = "All product→role links:".to_string();
        for (product_id, role) in links {
            let products = jinxxy::get_products(&api_key).await?;
            let products: HashMap<String, String, ahash::RandomState> = products.into_iter()
                .map(|product| (product.id, product.name))
                .collect();
            let product_name = products.get(&product_id)
                .map(|name| format!("\"{}\"", name))
                .unwrap_or_else(|| product_id.clone());
            message.push_str(format!("\n- {} grants <@&{}>", product_name, role.get()).as_str());
        }
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
                    .and_then(|cache| cache.get(&license_info.product_version_id))
                    .map(|version| format!("\"{}\"", version))
                    .unwrap_or("`null`".to_string());

                let locked = context.data().db.is_license_locked(guild_id, license_id.clone()).await?;

                message.push_str(format!(
                    "\n- ID=`{}` short=`{}` long=`{}` activations={} locked={} [{}](<{}>) product=\"{}\" version={}",
                    license_id,
                    license_info.short_key,
                    license_info.key,
                    license_info.activations, // this field came from Jinxxy and is up to date
                    locked, // this field came from the local DB and may be out of sync
                    license_info.username,
                    license_info.profile_url(),
                    license_info.product_name,
                    product_version_name
                ).as_str());
            } else {
                message.push_str(format!("\n- ID=`{}` (no data found)", license_id).as_str());
            }
        }

        let reply = CreateReply::default()
            .ephemeral(true)
            .content(message);
        context.send(reply).await?;
    } else {
        context.send(CreateReply::default().content(MISSING_API_KEY_MESSAGE).ephemeral(true)).await?;
    }
    Ok(())
}

/// Query license information for a user
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
                jinxxy::delete_license_activation(&api_key, &license_id, &activation_id).await?; //TODO: might error if the activation doesn't exist
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
            let mut message = format!("Users for `{}`:", license);
            for user_id in license_users {
                if user_id == 0 {
                    message.push_str("\n- **LOCKED**. This entry prevents the license from being used to grant roles.");
                } else {
                    message.push_str(format!("\n- <@{}>", user_id).as_str());
                }
            }
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

/// get a license ID from whatever the heck the user provided
async fn license_to_id(api_key: &str, license: &str) -> Result<Option<String>, Error> {
    let license_type = license::identify_license(license);
    let license_id = if license_type.is_integer() {
        Some(license.to_string())
    } else {
        // convert short/long key into ID
        let license_key = license_type.create_jinxxy_license(license);
        if let Some(license_key) = license_key {
            jinxxy::get_license_id(api_key, license_key).await?
        } else {
            None
        }
    };
    Ok(license_id)
}

/// Link a product to a role. Activating a license for the product will grant linked roles.
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_ROLES",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(super) async fn link_product(
    context: Context<'_>,
) -> Result<(), Error> {
    let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;
    if let Some(api_key) = context.data().db.get_jinxxy_api_key(guild_id).await? {
        let products = jinxxy::get_products(&api_key).await?;
        if products.is_empty() {
            context.send(CreateReply::default().content("No products found. Add some products in Jinxxy before using this command.").ephemeral(true)).await?;
        } else {
            let product_select_options: Vec<CreateSelectMenuOption> = products.iter()
                .map(|product| CreateSelectMenuOption::new(&product.name, &product.id))
                .collect();
            let product_select_options_len = product_select_options.len();
            let product_select_options_len = if product_select_options_len > MAX_SELECT_VALUES as usize {
                MAX_SELECT_VALUES
            } else {
                product_select_options_len as u8
            };

            let product_lookup: HashMap<String, String, ahash::RandomState> = products.into_iter()
                .map(|product| (product.id, product.name))
                .collect();

            let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;
            let mut assignable_roles: HashSet<RoleId, ahash::RandomState> = {
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

            let id_prefix = format!("{}_", context.id());
            let product_select_id = format!("{}{}", id_prefix, PRODUCT_SELECT_ID);
            let role_select_id = format!("{}{}", id_prefix, ROLE_SELECT_ID);
            let link_button_id = format!("{}{}", id_prefix, LINK_PRODUCT_BUTTON);
            let unlink_button_id = format!("{}{}", id_prefix, UNLINK_PRODUCT_BUTTON);
            let custom_ids = vec![
                product_select_id.clone(),
                role_select_id.clone(),
                link_button_id.clone(),
                unlink_button_id.clone(),
            ];
            let components = vec![
                CreateActionRow::SelectMenu(CreateSelectMenu::new(product_select_id.clone(), CreateSelectMenuKind::String { options: product_select_options }).placeholder("Product Name").min_values(1).max_values(product_select_options_len)),
                CreateActionRow::SelectMenu(CreateSelectMenu::new(role_select_id.clone(), CreateSelectMenuKind::Role { default_roles: None }).placeholder("Role to Grant").min_values(1).max_values(MAX_SELECT_VALUES)),
                CreateActionRow::Buttons(vec![CreateButton::new(link_button_id.clone()).label("Link"), CreateButton::new(unlink_button_id.clone()).label("Unlink")]),
            ];
            let reply = CreateReply::default()
                .ephemeral(true)
                .content("Select products and roles to link. All selected products will grant all selected roles.")
                .components(components);
            let reply_handle = context.send(reply).await?;

            let mut selected_products: Option<Vec<String>> = None;
            let mut selected_roles: Option<Vec<RoleId>> = None;

            fn assign_selection_result<T: Clone>(target: &mut Option<Vec<T>>, values: &[T]) {
                if !values.is_empty() {
                    *target = Some(values.to_vec());
                } else {
                    *target = None;
                }
            }

            while let Some(component_interaction) = serenity::ComponentInteractionCollector::new(context)
                .author_id(context.author().id)
                .channel_id(context.channel_id())
                .timeout(Duration::from_secs(600)) // 10 minute timeout on the form
                .custom_ids(custom_ids.clone()) // wtf, this API is trash
                .await
            {
                // some absolutely ridiculous trick to get the select values out of Discord's javascript-centric API
                let custom_id = component_interaction.data.custom_id.as_str();
                match &component_interaction.data.kind {
                    ComponentInteractionDataKind::StringSelect { values } if custom_id == product_select_id.as_str() => {
                        assign_selection_result(&mut selected_products, values)
                    }
                    ComponentInteractionDataKind::RoleSelect { values } if custom_id == role_select_id.as_str() => {
                        assign_selection_result(&mut selected_roles, values)
                    }
                    ComponentInteractionDataKind::Button => {
                        let link = custom_id == link_button_id.as_str();
                        let unlink = custom_id == unlink_button_id.as_str();
                        if link | unlink {
                            // user pressed either submit button
                            if selected_products.is_some() && selected_roles.is_some() {
                                let product_ids = selected_products.take().unwrap();
                                let roles = selected_roles.take().unwrap();

                                let mut message_lines = String::new();
                                let mut warning_lines = String::new();
                                for product_id in product_ids {
                                    let product_name = product_lookup.get(product_id.as_str())
                                        .map(|name| format!("\"{}\"", name))
                                        .unwrap_or_else(|| product_id.clone());
                                    for role in &roles {
                                        if link {
                                            context.data().db.link_product(guild_id, product_id.clone(), *role).await?;
                                        } else {
                                            context.data().db.unlink_product(guild_id, product_id.clone(), *role).await?;
                                        }
                                        message_lines.push_str(format!("\n- {}→<@&{}>", product_name, role.get()).as_str());
                                        // if we're in link mode, then generate warnings
                                        if link && !assignable_roles.remove(role) {
                                            warning_lines.push_str(format!("\n- <@&{}>", role.get()).as_str());
                                        }
                                    }
                                }

                                let (action_name, action_verb) = if link {
                                    ("Link", "created")
                                } else {
                                    ("Unlink", "removed")
                                };
                                let embed = CreateEmbed::default()
                                    .title(format!("Product {} Successful", action_name))
                                    .description(format!("The following product→role links have been {}:{}", action_verb, message_lines))
                                    .color(Colour::DARK_GREEN);
                                let reply = CreateReply::default()
                                    .content("")
                                    .embed(embed)
                                    .components(Default::default());
                                let reply = if warning_lines.is_empty() {
                                    reply
                                } else {
                                    // warn if the roles cannot be assigned (too high, or we lack the perm)
                                    let embed = CreateEmbed::default()
                                        .title("Warning")
                                        .description(format!("I don't currently have access to grant the following roles. Please check bot permissions.{}", warning_lines))
                                        .color(Colour::ORANGE);
                                    reply.embed(embed)
                                };
                                reply_handle.edit(context, reply).await?;
                            } else {
                                let embed = CreateEmbed::default()
                                    .title("Product Link Failed")
                                    .description("Please try again, and select both a product and a role to link.")
                                    .color(Colour::RED);

                                reply_handle.edit(
                                    context,
                                    CreateReply::default()
                                        .content("")
                                        .embed(embed)
                                        .components(Default::default()),
                                ).await?;
                            }
                        }
                    }
                    _ => {}
                }

                // regardless of what component got poked we acknowledge it
                component_interaction.create_response(context, CreateInteractionResponse::Acknowledge).await?;
            }
        }
    } else {
        context.send(CreateReply::default().content(MISSING_API_KEY_MESSAGE).ephemeral(true)).await?;
    }

    Ok(())
}
