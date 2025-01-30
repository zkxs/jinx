// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! Utils used by bot commands.

use crate::bot::{Context, CREATOR_COMMANDS, OWNER_COMMANDS};
use crate::db::JinxDb;
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::license;
use poise::{serenity_prelude as serenity, CreateReply};
use rand::distr::{Distribution, StandardUniform};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serenity::{
    CacheHttp, ChannelId, Colour, CreateEmbed, GuildId, Http, Message, MessageFlags, MessageType,
    MessageUpdateEvent, Role, RoleId,
};
use std::cell::RefCell;
use std::collections::HashSet;
use tracing::{debug, error, warn};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Check if the calling user is a bot owner
pub(super) async fn check_owner(context: Context<'_>) -> Result<bool, Error> {
    Ok(context
        .data()
        .db
        .is_user_owner(context.author().id.get())
        .await?)
}

/// Set (or reset) guild commands for this guild.
///
/// There is a global rate limit of 200 application command creates per day, per guild.
pub async fn set_guild_commands(
    http: impl AsRef<Http>,
    db: &JinxDb,
    guild_id: GuildId,
    force_owner: Option<bool>,
    force_creator: Option<bool>,
) -> Result<(), Error> {
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
    let creator_commands = creator
        .then_some(CREATOR_COMMANDS.iter())
        .into_iter()
        .flatten();
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

pub(super) async fn assignable_roles(
    context: &Context<'_>,
    guild_id: GuildId,
) -> Result<HashSet<RoleId, ahash::RandomState>, Error> {
    let bot_id = context.framework().bot_id;
    let bot_member = guild_id.member(context, bot_id).await?;

    // Serenity has deprecated getting guild-global permissions and is making providing a channel mandatory.
    // This is nonsensical because I want to see if a member has Manage Roles, which cannot be overridden at the channel level.
    // It also causes problems, as `context.guild_channel()` fails in threads and stage text channels: https://github.com/zkxs/jinx/issues/8
    #[allow(deprecated)]
    let permissions = bot_member.permissions(context)?;

    let assignable_roles: HashSet<RoleId, _> = if permissions.manage_roles() {
        // Despite the above deprecation text I pass a channel in regardless, here.
        // for some reason if the scope of `guild` is too large the compiler loses its mind. Probably something with calling await when it's in scope?
        let guild = context
            .guild()
            .ok_or(JinxError::new("expected to be in a guild"))?;
        let highest_role = guild.member_highest_role(&bot_member);
        if let Some(highest_role) = highest_role {
            let everyone_id = guild.role_by_name("@everyone").map(|role| role.id);
            let mut roles: Vec<&Role> = guild
                .roles
                .values()
                .filter(|role| Some(role.id) != everyone_id) // @everyone is weird, don't use it
                .filter(|role| role.position < highest_role.position) // roles above our highest can't be managed
                .filter(|role| !role.managed) // managed roles can't be managed
                .collect();
            roles.sort_unstable_by(|a, b| u16::cmp(&b.position, &a.position));
            roles.into_iter().map(|role| role.id).collect()
        } else {
            // bot has no roles (this should not be possible)
            error!(
                "in {} bot has manage role perms but no roles!",
                guild_id.get()
            );
            Default::default()
        }
    } else {
        // bot has no manage role perms
        Default::default()
    };

    Ok(assignable_roles)
}

/// warn if the roles cannot be assigned (too high, or we lack the perm)
pub fn create_role_warning_from_roles<T: Iterator<Item = RoleId>>(
    assignable_roles: &HashSet<RoleId, ahash::RandomState>,
    roles: T,
) -> Option<CreateEmbed> {
    let roles: HashSet<RoleId, ahash::RandomState> = roles.into_iter().collect();
    let mut unassignable_roles: Vec<RoleId> = roles.difference(assignable_roles).copied().collect();
    create_role_warning(&mut unassignable_roles)
}

/// warn if the roles cannot be assigned (too high, or we lack the perm)
pub fn create_role_warning_from_unassignable<T: Iterator<Item = RoleId>>(
    unassignable_roles: T,
) -> Option<CreateEmbed> {
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
    CreateReply::default().ephemeral(true).embed(embed)
}

/// Create a simple error reply
pub fn error_reply(title: impl Into<String>, message: impl Into<String>) -> CreateReply {
    let embed = CreateEmbed::default()
        .title(title)
        .description(message)
        .color(Colour::RED);
    CreateReply::default().ephemeral(true).embed(embed)
}

/// Get a display name from a product name and a version name
pub fn product_display_name(product_name: &str, product_version_name: Option<&str>) -> String {
    match product_version_name {
        Some(product_version_name) => format!("{product_name} {product_version_name}"),
        None => product_name.to_string(),
    }
}

/// truncate a string to meet discord's 100 character autocomplete limit
pub fn truncate_string_for_discord_autocomplete(mut string: String) -> String {
    if string.len() > 100 {
        debug!("\"{}\".len() > 100; truncating…", string);

        // byte len is > 100 so there must be at least one char, so we can disregard that edge case

        // get the index after the 100th) char
        let last_char_index = string
            .char_indices()
            .nth(100)
            .map(|index| index.0) // start index of char 101 == index after char 100
            .unwrap_or_else(|| string.len()); // index after end of last char

        string.truncate(last_char_index);
    }

    string
}

pub trait MessageExtensions {
    /// Fixed check for if a message is private.
    ///
    /// Message::is_private() is deprecated with: "Check if guild_id is None if the message is received from the gateway."
    ///
    /// Meanwhile, Message.guild_id says: "This value will only be present if this message was received over the gateway, therefore do not use this to check if message is in DMs, it is not a reliable method."
    ///
    /// Fuck me, I guess. This check attempts to fix Serenity's shit.
    async fn fixed_is_private(&self, cache_http: impl CacheHttp) -> bool;
}

impl<T> MessageExtensions for T
where
    T: GetChannelId + GetGuildId + GetMessageKind + GetMessageFlags,
{
    async fn fixed_is_private(&self, cache_http: impl CacheHttp) -> bool {
        if self.get_guild_id().is_none() {
            if self
                .get_kind()
                .map(|kind| matches!(kind, MessageType::Regular | MessageType::InlineReply))
                .unwrap_or(false)
            {
                if self
                    .get_flags()
                    .map(|flags| {
                        matches!(
                            flags,
                            MessageFlags::IS_CROSSPOST
                                | MessageFlags::IS_VOICE_MESSAGE
                                | MessageFlags::EPHEMERAL
                                | MessageFlags::LOADING
                                | MessageFlags::URGENT
                        )
                    })
                    .unwrap_or(false)
                {
                    // the message has some weird flags set, so even if it's TECHNICALLY a private message it's definitely not a normal one
                    false
                } else {
                    // we couldn't get flags, or they looked normal
                    match self.get_channel_id().to_channel(cache_http).await {
                        Ok(channel) => channel.private().is_some(),
                        Err(e) => {
                            // couldn't get the channel from the cache
                            warn!(
                                "Could not determine if {} is a private channel: {:?}",
                                self.get_channel_id().get(),
                                e
                            );
                            false
                        }
                    }
                }
            } else {
                // the message was not a regular message, or we couldn't get the message kind
                false
            }
        } else {
            // guild is not set, so it's definitely not a DM
            false
        }
    }
}

trait GetChannelId {
    fn get_channel_id(&self) -> ChannelId;
}

impl GetChannelId for Message {
    fn get_channel_id(&self) -> ChannelId {
        self.channel_id
    }
}

impl GetChannelId for MessageUpdateEvent {
    fn get_channel_id(&self) -> ChannelId {
        self.channel_id
    }
}

trait GetGuildId {
    fn get_guild_id(&self) -> Option<GuildId>;
}

impl GetGuildId for Message {
    fn get_guild_id(&self) -> Option<GuildId> {
        self.guild_id
    }
}

impl GetGuildId for MessageUpdateEvent {
    fn get_guild_id(&self) -> Option<GuildId> {
        self.guild_id
    }
}

trait GetMessageKind {
    fn get_kind(&self) -> Option<MessageType>;
}

impl GetMessageKind for Message {
    fn get_kind(&self) -> Option<MessageType> {
        Some(self.kind)
    }
}
impl GetMessageKind for MessageUpdateEvent {
    fn get_kind(&self) -> Option<MessageType> {
        self.kind
    }
}

trait GetMessageFlags {
    fn get_flags(&self) -> Option<MessageFlags>;
}

impl GetMessageFlags for Message {
    fn get_flags(&self) -> Option<MessageFlags> {
        self.flags
    }
}

impl GetMessageFlags for MessageUpdateEvent {
    fn get_flags(&self) -> Option<MessageFlags> {
        self.flags.flatten()
    }
}

/// Seed a new StdRng from OS entropy
fn new_seeded_rng() -> StdRng {
    StdRng::from_os_rng()
}

thread_local! {
    /// Provides a local instance of StdRng for each thread
    static RNG: RefCell<StdRng> = RefCell::new(new_seeded_rng());
}

/// Generate a random nonce from a thread-local RNG
pub fn generate_nonce<T>() -> T
where
    StandardUniform: Distribution<T>,
{
    RNG.with_borrow_mut(|rng| rng.random())
}

#[cfg(test)]
mod test {
    use super::*;

    /// Generate some nonces just to make sure there's no panics or anything from the RefCell mutability.
    /// This should be totally safe, though.
    #[test]
    fn test_generate_nonce() {
        std::hint::black_box(generate_nonce::<u64>());
        std::hint::black_box(generate_nonce::<u64>());
        std::hint::black_box(generate_nonce::<u64>());
        std::hint::black_box(generate_nonce::<u64>());
        std::hint::black_box(generate_nonce::<u64>());
    }
}
