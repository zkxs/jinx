// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! Utils used by bot commands.

use crate::bot::{Context, CREATOR_COMMANDS, OWNER_COMMANDS};
use crate::db::JinxDb;
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::license;
use poise::serenity_prelude::Http;
use poise::{serenity_prelude as serenity, CreateReply};
use serenity::{Colour, CreateEmbed, GuildId, Role, RoleId};
use std::collections::HashSet;

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Check if the calling user is a bot owner
pub(super) async fn check_owner(context: Context<'_>) -> Result<bool, Error> {
    Ok(context.data().db.is_user_owner(context.author().id.get()).await?)
}

/// Set (or reset) guild commands for this guild.
///
/// There is a global rate limit of 200 application command creates per day, per guild.
pub async fn set_guild_commands(http: impl AsRef<Http>, db: &JinxDb, guild_id: GuildId, force_owner: Option<bool>, force_creator: Option<bool>) -> Result<(), Error> {
    let owner = if let Some(owner) = force_owner {
        owner
    } else {
        db.is_owner_guild(guild_id).await?
    };
    let creator = if let Some(creator) = force_creator {
        creator
    } else {
        db.get_jinxxy_api_key(guild_id).await?.is_some()
    };
    let owner_commands = owner.then_some(OWNER_COMMANDS.iter()).into_iter().flatten();
    let creator_commands = creator.then_some(CREATOR_COMMANDS.iter()).into_iter().flatten();
    let command_iter = owner_commands.chain(creator_commands);
    let commands = poise::builtins::create_application_commands(command_iter);
    guild_id.set_commands(http, commands).await?;
    Ok(())
}

/// Get a license ID from whatever the heck the user provided. This can proxy IDs through, so it may
/// not be suitable for untrusted applications where you don't want to allow users to pass IDs directly.
pub async fn license_to_id(api_key: &str, license: &str) -> Result<Option<String>, Error> {
    let license_type = license::identify_license(license);
    let license_id = if license_type.is_integer() {
        Some(license.to_string())
    } else {
        // convert short/long key into ID
        let license_key = license_type.create_trusted_jinxxy_license(license);
        if let Some(license_key) = license_key {
            jinxxy::get_license_id(api_key, license_key).await?
        } else {
            None
        }
    };
    Ok(license_id)
}

pub(super) async fn assignable_roles(context: &Context<'_>, guild_id: GuildId) -> Result<HashSet<RoleId, ahash::RandomState>, Error> {
    let bot_id = context.framework().bot_id;
    let bot_member = guild_id.member(context, bot_id).await?;
    let permissions = bot_member.permissions(context)?;

    let assignable_roles = if permissions.manage_roles() {
        // for some reason if the scope of `guild` is too large the compiler loses its mind. Probably something with calling await when it's in scope?
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
    };

    Ok(assignable_roles)
}

/// warn if the roles cannot be assigned (too high, or we lack the perm)
pub fn create_role_warning_from_roles<T: Iterator<Item=RoleId>>(assignable_roles: &HashSet<RoleId, ahash::RandomState>, roles: T) -> Option<CreateEmbed> {
    let roles: HashSet<RoleId, ahash::RandomState> = roles.into_iter().collect();
    let mut unassignable_roles: Vec<RoleId> = roles.difference(assignable_roles).copied().collect();
    create_role_warning(&mut unassignable_roles)
}

/// warn if the roles cannot be assigned (too high, or we lack the perm)
pub fn create_role_warning_from_unassignable<T: Iterator<Item=RoleId>>(unassignable_roles: T) -> Option<CreateEmbed> {
    let mut unassignable_roles: Vec<RoleId> = unassignable_roles.into_iter().collect();
    create_role_warning(&mut unassignable_roles)
}

/// warn if the roles cannot be assigned (too high, or we lack the perm)
fn create_role_warning(unassignable_roles: &mut Vec<RoleId>) -> Option<CreateEmbed> {
    if unassignable_roles.is_empty() {
        None
    } else {
        unassignable_roles.sort_unstable();

        let mut warning_lines = String::new();
        for role in unassignable_roles {
            warning_lines.push_str(format!("\n- <@&{}>", role).as_str());
        }
        let embed = CreateEmbed::default()
            .title("Warning")
            .description(format!("I don't currently have access to grant the following roles. Please check bot permissions.{}", warning_lines))
            .color(Colour::ORANGE);
        Some(embed)
    }
}

/// Create a simple success reply
pub fn success_reply(title: impl Into<String>, message: impl Into<String>) -> CreateReply {
    let embed = CreateEmbed::default()
        .title(title)
        .description(message)
        .color(Colour::DARK_GREEN);
    CreateReply::default()
        .ephemeral(true)
        .embed(embed)
}

/// Create a simple error reply
pub fn error_reply(message: impl Into<String>) -> CreateReply {
    let embed = CreateEmbed::default()
        .title("Error")
        .description(message)
        .color(Colour::RED);
    CreateReply::default()
        .ephemeral(true)
        .embed(embed)
}
