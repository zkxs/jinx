// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! Utils used by bot commands.

use crate::bot::Context;
use crate::http::jinxxy;
use crate::{bot, license};
use poise::serenity_prelude::GuildId;

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Check if the calling user is a bot owner
pub async fn check_owner(context: Context<'_>) -> Result<bool, Error> {
    Ok(context.data().db.is_user_owner(context.author().id.get()).await?)
}

/// Set (or reset) guild commands for this guild.
///
/// There is a global rate limit of 200 application command creates per day, per guild.
pub async fn set_guild_commands(context: Context<'_>, guild_id: GuildId, force_owner: Option<bool>, force_creator: Option<bool>) -> Result<(), crate::bot::Error> {
    bot::set_guild_commands(context, &context.data().db, guild_id, force_owner, force_creator).await
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
