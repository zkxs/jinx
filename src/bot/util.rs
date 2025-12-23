// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! Utils used by bot commands.

use crate::bot::{AUTOCOMPLETE_CHARACTER_LIMIT, CREATOR_COMMANDS, Context, OWNER_COMMANDS};
use crate::db::JinxDb;
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::license::LicenseType;
use poise::{CreateReply, serenity_prelude as serenity};
use rand::distr::{Distribution, StandardUniform};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};
use serenity::{
    AutocompleteChoice, Cache, CacheHttp, ChannelId, Colour, CreateAutocompleteResponse, CreateEmbed,
    Error as SerenityError, GuildId, Http, Message, MessageFlags, MessageType, MessageUpdateEvent, Role, RoleId,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt::Debug;
use std::future::Future;
use std::time::Duration;
use tracing::{debug, error, warn};

type Error = Box<dyn std::error::Error + Send + Sync>;

/// Check if the calling user is a bot owner
pub(super) async fn check_owner(context: Context<'_>) -> Result<bool, Error> {
    Ok(context.data().db.is_user_owner(context.author().id.get()).await?)
}

/// Set (or reset) guild commands for this guild.
///
/// There is a global rate limit of 200 application command creates per day, per guild.
///
/// If `force_owner` or `force_creator` are None, it infers using DB state. If they are Some(bool), the bool
/// will be used to forcibly set or unset the owner/creator flags for the purpose of installing (or uninstalling!)
/// commands.
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
        db.has_jinxxy_linked(guild_id).await?
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
pub async fn trusted_license_to_id(api_key: &str, license: &str) -> Result<Option<String>, Error> {
    let license_type = LicenseType::identify(license);
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

pub(super) fn highest_mentionable_role(cache: &impl AsRef<Cache>, guild_id: GuildId) -> Result<Option<RoleId>, Error> {
    let guild = guild_id
        .to_guild_cached(cache)
        .ok_or(JinxError::new("expected guild to be in the Discord client cache"))?;
    let roles = &guild.roles;
    let max_role = roles
        .iter()
        .filter_map(|(role_id, role)| role.mentionable.then_some((role.position, *role_id)))
        .max_by(|(position_a, role_id_a), (position_b, role_id_b)| {
            // bigger position is better, then smaller role_id is better
            position_a.cmp(position_b).then(role_id_b.cmp(role_id_a))
        })
        .map(|(_position, role_id)| role_id);
    Ok(max_role)
}

pub(super) fn sorted_channels(cache: &impl AsRef<Cache>, guild_id: GuildId) -> Result<Vec<(u16, ChannelId)>, Error> {
    let guild = guild_id
        .to_guild_cached(cache)
        .ok_or(JinxError::new("expected guild to be in the Discord client cache"))?;
    let mut channels = guild
        .channels
        .iter()
        .map(|(channel_id, channel)| (channel.position, *channel_id))
        .collect::<Vec<_>>();
    channels.sort_unstable_by(|(pos_a, id_a), (pos_b, id_b)| pos_a.cmp(pos_b).then(id_b.cmp(id_a)));
    Ok(channels)
}

pub(super) fn role_name(
    cache: &impl AsRef<Cache>,
    guild_id: GuildId,
    role_id: RoleId,
) -> Result<Option<String>, Error> {
    let guild = guild_id
        .to_guild_cached(cache)
        .ok_or(JinxError::new("expected guild to be in the Discord client cache"))?;
    let role_name = guild.roles.get(&role_id).map(|role| role.name.clone());
    Ok(role_name)
}

pub(super) async fn assignable_roles(
    context: &Context<'_>,
    guild_id: GuildId,
) -> Result<HashSet<RoleId, ahash::RandomState>, Error> {
    let bot_id = context.framework().bot_id();
    let bot_member = guild_id.member(context, bot_id).await?;

    // Serenity has deprecated getting guild-global permissions and is making providing a channel mandatory.
    // This is nonsensical because I want to see if a member has Manage Roles, which cannot be overridden at the channel level.
    // It also causes problems, as `context.guild_channel()` fails in threads and stage text channels: https://github.com/zkxs/jinx/issues/8
    #[allow(deprecated)]
    let permissions = bot_member.permissions(context)?;

    let assignable_roles: HashSet<RoleId, _> = if permissions.manage_roles() {
        // Despite the above deprecation text I pass a channel in regardless, here.
        // for some reason if the scope of `guild` is too large the compiler loses its mind. Probably something with calling await when it's in scope?
        let guild = guild_id
            .to_guild_cached(context)
            .ok_or(JinxError::new("expected guild to be in the Discord client cache"))?;
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
            error!("in {} bot has manage role perms but no roles!", guild_id.get());
            Default::default()
        }
    } else {
        // bot has no manage role perms
        Default::default()
    };

    Ok(assignable_roles)
}

pub(super) fn deleted_roles(
    cache: &impl AsRef<Cache>,
    guild_id: GuildId,
    known_roles: impl Iterator<Item = RoleId>,
) -> Result<Vec<RoleId>, Error> {
    let guild = guild_id
        .to_guild_cached(cache)
        .ok_or(JinxError::new("expected guild to be in the Discord client cache"))?;
    let roles = &guild.roles;
    Ok(known_roles.filter(|role| !roles.contains_key(role)).collect())
}

pub(super) async fn is_administrator(context: &Context<'_>, guild_id: GuildId) -> Result<bool, Error> {
    let bot_id = context.framework().bot_id();
    let bot_member = guild_id.member(context, bot_id).await?;

    // same deprecation warning as above in `assignable_roles`
    #[allow(deprecated)]
    let permissions = bot_member.permissions(context)?;
    Ok(permissions.administrator())
}

/// warn if the roles cannot be assigned (too high, or we lack the perm)
pub fn create_role_warning_from_roles(
    assignable_roles: &HashSet<RoleId, ahash::RandomState>,
    roles: impl Iterator<Item = RoleId>,
) -> Option<CreateEmbed> {
    let roles: HashSet<RoleId, ahash::RandomState> = roles.into_iter().collect();
    let mut unassignable_roles: Vec<RoleId> = roles.difference(assignable_roles).copied().collect();
    create_role_warning(&mut unassignable_roles)
}

/// warn if the roles cannot be assigned (too high, or we lack the perm)
pub fn create_role_warning_from_unassignable(unassignable_roles: impl Iterator<Item = RoleId>) -> Option<CreateEmbed> {
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
            warning_lines.push_str(format!("\n- <@&{role}>").as_str());
        }
        let embed = CreateEmbed::default()
            .title("Warning")
            .description(format!(
                "I don't currently have access to grant the following roles. Please check bot permissions.{warning_lines}"
            ))
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

/// Get a display name from a product name and a version name.
/// This is truncated to Discord's 100 character autocomplete limit.
/// I am assuming the 100 char limit is codepoints, not code units (bytes).
pub fn product_display_name(product_name: &str, product_version_name: Option<&str>) -> String {
    match product_version_name {
        Some(product_version_name) => {
            // we're going to delimit with a single space, so I use 1 char
            // this leaves 99 chars to work with
            const MAX_LENGTH: usize = AUTOCOMPLETE_CHARACTER_LIMIT - 1;

            // only used in some cases :/
            const PRODUCT_VERSION_MAX_LENGTH: usize = 33;

            let product_name_len = product_name.chars().count();
            let product_version_len = product_version_name.chars().count();

            if product_name_len + product_version_len > MAX_LENGTH {
                debug!(
                    "\"{}\" + \"{}\" .chars().count() > 100; truncating…",
                    product_name, product_version_name
                );

                // I have to trim either the product name or the product version name or both
                // It's not trivial to know what will be prettiest, so time for some shitty rules
                if product_name_len > MAX_LENGTH && product_version_len > MAX_LENGTH {
                    // everything is really freaking long
                    const LENGTH_A: usize = 50;
                    const LENGTH_B: usize = MAX_LENGTH - LENGTH_A;
                    let product_name: String = product_name.chars().take(LENGTH_A).collect();
                    let product_version_name: String = product_name.chars().take(LENGTH_B).collect();
                    format!("{product_name} {product_version_name}")
                } else if product_version_len <= PRODUCT_VERSION_MAX_LENGTH {
                    // product version seems short-ish, so I've arbitrarily decided it doesn't need trimming
                    let length_a = MAX_LENGTH - product_version_len;
                    let product_name: String = product_name.chars().take(length_a).collect();
                    format!("{product_name} {product_version_name}")
                } else {
                    // product version will be truncated to PRODUCT_VERSION_MAX_LENGTH because why the hell not
                    const LENGTH_A: usize = MAX_LENGTH - PRODUCT_VERSION_MAX_LENGTH;
                    let product_name: String = product_name.chars().take(LENGTH_A).collect();
                    let product_version_name: String =
                        product_version_name.chars().take(PRODUCT_VERSION_MAX_LENGTH).collect();
                    format!("{product_name} {product_version_name}")
                }
            } else {
                format!("{product_name} {product_version_name}")
            }
        }
        None => {
            // I use 15 chars
            const MAX_LENGTH: usize = AUTOCOMPLETE_CHARACTER_LIMIT - 15;
            if product_name.chars().count() > MAX_LENGTH {
                let truncated_product_name: String = product_name.chars().take(MAX_LENGTH).collect();
                format!("{truncated_product_name} (null version)")
            } else {
                format!("{product_name} (null version)")
            }
        }
    }
}

/// truncate a string to meet Discord's 100 character autocomplete limit
pub fn truncate_string_for_discord_autocomplete(string: &str) -> String {
    if string.chars().count() > AUTOCOMPLETE_CHARACTER_LIMIT {
        debug!("\"{}\".chars().count() > 100; truncating…", string);
        string.chars().take(AUTOCOMPLETE_CHARACTER_LIMIT).collect()
    } else {
        string.to_string()
    }
}

pub trait MessageExtensions {
    /// Fixed check for if a message is private.
    ///
    /// Message::is_private() is deprecated with: "Check if guild_id is None if the message is received from the gateway."
    ///
    /// Meanwhile, Message.guild_id says: "This value will only be present if this message was received over the gateway, therefore do not use this to check if message is in DMs, it is not a reliable method."
    ///
    /// Fuck me, I guess. This check attempts to fix Serenity's shit.
    async fn fixed_is_private(&self, cache_http: impl CacheHttp) -> Result<bool, SerenityError>;
}

impl<T> MessageExtensions for T
where
    T: GetChannelId + GetGuildId + GetMessageKind + GetMessageFlags,
{
    async fn fixed_is_private(&self, cache_http: impl CacheHttp) -> Result<bool, SerenityError> {
        if self.get_guild_id().is_none() {
            if self
                .get_kind()
                .map(|kind| matches!(kind, MessageType::Regular | MessageType::InlineReply))
                .unwrap_or(false)
            {
                if self
                    .get_flags()
                    .map(|flags| {
                        flags.intersects(
                            MessageFlags::IS_CROSSPOST
                                | MessageFlags::IS_VOICE_MESSAGE
                                | MessageFlags::EPHEMERAL
                                | MessageFlags::LOADING
                                | MessageFlags::URGENT,
                        )
                    })
                    .unwrap_or(false)
                {
                    // the message has some weird flags set, so even if it's TECHNICALLY a private message it's definitely not a normal one
                    Ok(false)
                } else {
                    // we couldn't get flags, or they looked normal
                    match cache_http.http().get_channel(self.get_channel_id()).await {
                        Ok(channel) => Ok(channel.private().is_some()),
                        Err(e) => {
                            warn!(
                                "Could not determine if {} is a private channel: {:?}",
                                self.get_channel_id().get(),
                                e
                            );
                            Err(e)
                        }
                    }
                }
            } else {
                // the message was not a regular message, or we couldn't get the message kind
                Ok(false)
            }
        } else {
            // guild is set, so it's definitely not a DM
            Ok(false)
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

/// Result of a retry check
pub enum RetryCheck {
    /// Do not retry, and immediately return the latest result
    DoNotRetry,
    /// Retry after the specified duration
    RetryAfter(Duration),
}

/// calls `provider` until one of its results returns `false` from `should_retry`
pub async fn retry<Provider, RetryChecker, Result, ResultFuture>(
    mut provider: Provider,
    mut should_retry: RetryChecker,
) -> Result
where
    Provider: FnMut() -> ResultFuture,
    RetryChecker: FnMut(&Result) -> RetryCheck,
    ResultFuture: Future<Output = Result> + Sized,
{
    loop {
        let result = provider().await;
        match should_retry(&result) {
            RetryCheck::DoNotRetry => {
                return result;
            }
            RetryCheck::RetryAfter(duration) => {
                if !duration.is_zero() {
                    tokio::time::sleep(duration).await;
                }
            }
        }
    }
}

/// Some item that is known to be either deterministic or non-deterministic. This behavior is useful to know, as if
/// recreating this item will always yield the same result then there is no reason to recreate the item.
pub trait IsDeterministic {
    /// Check if this item is deterministic or non-deterministic.
    fn is_deterministic(&self) -> bool;
}

/// Calls `provider` up to four times (three retry attempts maximum) until an `Ok` is returned.
/// Each retry attempt has a gradually increasing delay (0.3s, 3.0s, 10.0s).
pub async fn retry_thrice<Provider, T, ResultFuture, E>(provider: Provider) -> Result<T, E>
where
    Provider: FnMut() -> ResultFuture,
    ResultFuture: Future<Output = Result<T, E>> + Sized,
    E: Debug + IsDeterministic,
{
    let mut retry_count: u8 = 0;
    retry(provider, move |result| match result {
        Ok(_) => RetryCheck::DoNotRetry,
        Err(e) => {
            if e.is_deterministic() {
                warn!("aborting retry on something because it's deterministic: {:?}", e);
                RetryCheck::DoNotRetry
            } else {
                let retry_check = match retry_count {
                    0 => RetryCheck::RetryAfter(Duration::from_millis(300)),
                    1 => RetryCheck::RetryAfter(Duration::from_secs(3)),
                    2 => RetryCheck::RetryAfter(Duration::from_secs(10)),
                    _ => RetryCheck::DoNotRetry,
                };
                if !matches!(retry_check, RetryCheck::DoNotRetry) {
                    // set up for the retry we are about to perform
                    retry_count += 1;
                    warn!("retry attempt {} on something because: {:?}", retry_count, e);
                } // else, we're not retrying so there's no reason to do set-up
                retry_check
            }
        }
    })
    .await
}

/// Create an autocomplete response from strings
pub fn create_autocomplete_response<T: Into<String>>(choices: impl Iterator<Item = T>) -> CreateAutocompleteResponse {
    let choice_vec = choices
        .into_iter()
        .map(|choice| AutocompleteChoice::from(choice))
        .collect();
    CreateAutocompleteResponse::new().set_choices(choice_vec)
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
