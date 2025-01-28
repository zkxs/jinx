// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::bot::util::{error_reply, generate_nonce};
use crate::bot::{Context, Data, Error};
use poise::{serenity_prelude as serenity, FrameworkError};
use std::fmt::Debug;
use tracing::error;

enum SomeContext<'a> {
    Serenity(&'a serenity::client::Context),
    Framework(Context<'a>),
}

struct PoiseError<'a> {
    title: &'static str,
    diagnostic: Option<String>,
    context: SomeContext<'a>,
}

impl<'a> PoiseError<'a> {
    fn new_cmd(title: &'static str, context: Context<'a>) -> Option<Self> {
        Some(Self {
            title,
            diagnostic: None,
            context: SomeContext::Framework(context),
        })
    }

    fn debug<T: Debug>(
        title: &'static str,
        context: &'a serenity::client::Context,
        diagnostic: T,
    ) -> Option<Self> {
        Some(Self {
            title,
            diagnostic: Some(format!("{:?}", diagnostic)),
            context: SomeContext::Serenity(context),
        })
    }

    fn debug_cmd<T: Debug>(
        title: &'static str,
        context: Context<'a>,
        diagnostic: T,
    ) -> Option<Self> {
        Some(Self {
            title,
            diagnostic: Some(format!("{:?}", diagnostic)),
            context: SomeContext::Framework(context),
        })
    }

    fn string_cmd(title: &'static str, context: Context<'a>, diagnostic: String) -> Option<Self> {
        Some(Self {
            title,
            diagnostic: Some(diagnostic),
            context: SomeContext::Framework(context),
        })
    }
}

/// Error handler to add extra, custom logging for Poise/Serenity errors.
pub async fn error_handler(error: FrameworkError<'_, Data, Error>) {
    let error: Option<PoiseError> = match error {
        FrameworkError::Setup { ctx, error, .. } => PoiseError::debug("Setup", ctx, error),
        FrameworkError::EventHandler { ctx, error, .. } => {
            PoiseError::debug("Event handler", ctx, error)
        }
        FrameworkError::Command { ctx, error, .. } => PoiseError::debug_cmd("Command", ctx, error),
        FrameworkError::SubcommandRequired { ctx, .. } => {
            PoiseError::new_cmd("Subcommand required", ctx)
        }
        FrameworkError::CommandPanic { ctx, payload, .. } => {
            // this is a really weird one, so I don't want to do ANYTHING beyond logging it
            error!("Command panic in {}: {:?}", ctx.command().name, payload);
            None
        }
        FrameworkError::ArgumentParse {
            ctx, input, error, ..
        } => PoiseError::string_cmd(
            "Argument parse error",
            ctx,
            format!("{:?} {:?}", input, error),
        ),
        FrameworkError::CommandStructureMismatch { description, .. } => {
            // this technically has a context, but it's a weird 1-off type
            error!("Command structure mismatch: {:}", description);
            None
        }
        FrameworkError::CooldownHit { ctx, .. } => PoiseError::new_cmd("Cooldown hit", ctx),
        FrameworkError::MissingBotPermissions { ctx, .. } => {
            PoiseError::new_cmd("Missing bot permissions", ctx)
        }
        FrameworkError::MissingUserPermissions { ctx, .. } => {
            PoiseError::new_cmd("Missing user permissions", ctx)
        }
        FrameworkError::NotAnOwner { ctx, .. } => PoiseError::new_cmd("Not an owner", ctx),
        FrameworkError::GuildOnly { ctx, .. } => PoiseError::new_cmd("Guild only", ctx),
        FrameworkError::DmOnly { ctx, .. } => PoiseError::new_cmd("DM only", ctx),
        FrameworkError::NsfwOnly { ctx, .. } => PoiseError::new_cmd("NSFW only", ctx),
        FrameworkError::CommandCheckFailed { ctx, error, .. } => {
            PoiseError::debug_cmd("Command check failed", ctx, error)
        }
        FrameworkError::DynamicPrefix { error, .. } => {
            // this technically has a context, but it's a weird 1-off type
            error!("Dynamic prefix error: {:?}", error);
            None
        }
        FrameworkError::UnknownCommand { ctx, trigger, .. } => {
            PoiseError::debug("Unknown prefix command", ctx, trigger)
        }
        FrameworkError::UnknownInteraction {
            ctx, interaction, ..
        } => PoiseError::debug("Unknown interaction", ctx, interaction),
        FrameworkError::NonCommandMessage { ctx, error, .. } => {
            PoiseError::debug("Non-command message", ctx, error)
        }
        FrameworkError::__NonExhaustive(_) => {
            error!("poise dev has done something weird and thrown a __NonExhaustive error");
            None
        }
    };
    if let Some(error) = error {
        match error.context {
            SomeContext::Serenity(_context) => {
                if let Some(diagnostic) = error.diagnostic {
                    error!("{} error: {}", error.title, diagnostic);
                } else {
                    error!("{} error", error.title);
                }
            }
            SomeContext::Framework(context) => {
                let nonce: u64 = generate_nonce();
                let nonce = format!("{:016X}", nonce);
                let user = context.author();

                if let Some(diagnostic) = error.diagnostic {
                    error!(
                        "NONCE[{}] {} error encountered in {}: Caused by {:?}. {}",
                        nonce,
                        error.title,
                        context.command().name,
                        user,
                        diagnostic
                    );
                } else {
                    error!(
                        "NONCE[{}] {} error encountered in {}. Caused by {:?}.",
                        nonce,
                        error.title,
                        context.command().name,
                        user
                    );
                }

                let result = context.send(error_reply(format!("{} Error", error.title), format!("An unexpected error has occurred. Please report this to the bot developer with error code `{}`\n\nBugs can be reported on [our GitHub](<https://github.com/zkxs/jinx/issues>) or in [our Discord](<https://discord.gg/aKkA6m26f9>).", nonce))).await;
                if let Err(e) = result {
                    error!("Error sending error message: {:?}", e);
                }
            }
        }
    };
}
