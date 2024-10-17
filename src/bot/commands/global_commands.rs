// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::bot::util::{check_owner, error_reply, set_guild_commands, success_reply};
use crate::bot::Context;
use crate::constants;
use crate::error::JinxError;
use crate::http::{jinxxy, update_checker};
use poise::serenity_prelude as serenity;
use poise::CreateReply;
use regex::Regex;
use serenity::{Colour, CreateEmbed};
use std::sync::LazyLock;
use tracing::debug;

type Error = Box<dyn std::error::Error + Send + Sync>;

static GLOBAL_JINXXY_API_KEY_REGEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(
    r"^sk_[a-f0-9]{32}$", // jinxxy API key `sk_9bba2064ee8c20aa4fd6b015eed2001a`
).unwrap()); // in case you are wondering the above is not a real key: it's only an example

thread_local! {
    // trick to avoid a subtle performance edge case: https://docs.rs/regex/latest/regex/index.html#sharing-a-regex-across-threads-can-result-in-contention
    static JINXXY_API_KEY_REGEX: Regex = GLOBAL_JINXXY_API_KEY_REGEX.clone();
}

/// Shows bot help
#[poise::command(
    slash_command,
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn help(
    context: Context<'_>,
) -> Result<(), Error> {
    let embed = CreateEmbed::default()
        .title("Jinx Help")
        .description(
            "Jinx is a Discord bot that grants roles to users when they register Jinxxy license keys.\n\
            For documentation, see https://github.com/zkxs/jinx\n\
            For support, join https://discord.gg/aKkA6m26f9"
        );
    let reply = CreateReply::default()
        .ephemeral(true)
        .embed(embed);
    context.send(reply).await?;
    Ok(())
}

/// Shows bot version
#[poise::command(
    slash_command,
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn version(
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

/// Set up Jinx for this Discord server
#[poise::command(
    slash_command,
    guild_only,
    default_member_permissions = "MANAGE_GUILD",
    install_context = "Guild",
    interaction_context = "Guild"
)]
pub(in crate::bot) async fn init(
    context: Context<'_>,
    #[description = "Jinxxy API key"] api_key: Option<String>,
) -> Result<(), Error> {
    let guild_id = context.guild_id().ok_or(JinxError::new("expected to be in a guild"))?;

    // handle trimming the string
    let api_key = api_key
        .map(|api_key| api_key.trim().to_string())
        .filter(|api_key| !api_key.is_empty());

    let reply = if let Some(api_key) = api_key {
        // here we have a bit of an easter-egg to install owner commands
        if api_key == "install_owner_commands" {
            if check_owner(context).await? {
                context.data().db.set_owner_guild(guild_id, true).await?;

                //TODO: for some reason this sometimes times out and gives a 404 if the commands have
                // previously been deleted in the same bot process; HOWEVER it actually still succeeds.
                // I suspect this is a discord/serenity/poise bug.
                // For some <id>, <nonce>, this looks like:
                // Http(UnsuccessfulRequest(ErrorResponse { status_code: 404, url: "https://discord.com/api/v10/interactions/<id>/<nonce>/callback", method: POST, error: DiscordJsonError { code: 10062, message: "Unknown interaction", errors: [] } }))
                set_guild_commands(&context, &context.data().db, guild_id, Some(true), None).await?;

                success_reply("Success", "Owner commands installed.")
            } else {
                error_reply("Not an owner")
            }
        } else if api_key == "uninstall_owner_commands" {
            if check_owner(context).await? {
                context.data().db.set_owner_guild(guild_id, false).await?;
                set_guild_commands(&context, &context.data().db, guild_id, Some(false), None).await?;
                success_reply("Success", "Owner commands uninstalled.")
            } else {
                error_reply("Not an owner")
            }
        } else if JINXXY_API_KEY_REGEX.with(|regex| regex.is_match(api_key.as_str())) {
            // normal /init <key> use ends up in this branch
            match jinxxy::get_own_user(&api_key).await {
                Ok(auth_user) => {
                    let has_required_scopes = auth_user.has_required_scopes();
                    let display_name = auth_user.into_display_name();
                    context.data().db.set_jinxxy_api_key(guild_id, api_key.trim().to_string()).await?;
                    set_guild_commands(&context, &context.data().db, guild_id, None, Some(true)).await?;
                    let reply = success_reply("Success", format!("Welcome, {display_name}! API key set and additional slash commands enabled. Please continue bot setup."));
                    if has_required_scopes {
                        reply
                    } else {
                        let embed = CreateEmbed::default()
                            .title("Permission Warning")
                            .color(Colour::ORANGE)
                            .description("Provided API key is missing at least one of the mandatory scopes. Jinx commands may not work correctly. Please double-check your API key setup against the documentation [here](<https://github.com/zkxs/jinx#installation>).");
                        reply.embed(embed)
                    }
                }
                Err(e) => {
                    error_reply(format!("Error verifying API key: {e}"))
                }
            }
        } else {
            // user has given us some mystery garbage value for their API key
            debug!("invalid API key provided: \"{}\"", api_key); // log it to try and diagnose why people have trouble with the initial setup
            error_reply("Provided API key appears to be invalid. API keys should look like `sk_9bba2064ee8c20aa4fd6b015eed2001a`. If you need help, bot setup documentation can be found [here](<https://github.com/zkxs/jinx#installation>).")
        }
    } else if context.data().db.get_jinxxy_api_key(guild_id).await?.is_some() {
        // re-initialize commands but only if API key is already set
        set_guild_commands(&context, &context.data().db, guild_id, None, Some(true)).await?;

        success_reply("Success", "Commands reinstalled.")
    } else {
        error_reply("Please provide a Jinxxy API key")
    };

    context.send(reply).await?;

    Ok(())
}
