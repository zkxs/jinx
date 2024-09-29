// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

mod commands;

use crate::bot::commands::*;
use crate::db::JinxDb;
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::license;
use poise::serenity_prelude::{CreateMessage, GuildId, Http};
use poise::{serenity_prelude as serenity, Command, CreateReply, FrameworkContext, FrameworkError};
use rand::prelude::*;
use serenity::{ActionRowComponent, Colour, CreateActionRow, CreateEmbed, CreateInputText, CreateInteractionResponse, CreateInteractionResponseMessage, CreateModal, FullEvent, InputTextStyle, Interaction};
use std::fmt::Debug;
use std::sync::{Arc, LazyLock};
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

const REGISTER_MODAL_ID: &str = "jinx_register_modal";

/// commands to be installed globally
static GLOBAL_COMMANDS: LazyLock<Vec<Command<Data, Error>>> = LazyLock::new(|| {
    vec![
        help(),
        init(),
        version(),
    ]
});

/// commands to be installed only after successful Jinxxy init
static CREATOR_COMMANDS: LazyLock<Vec<Command<Data, Error>>> = LazyLock::new(|| {
    vec![
        create_post(),
        deactivate_license(),
        license_info(),
        link_product(),
        list_links(),
        lock_license(),
        set_log_channel(),
        unlock_license(),
        user_info(),
    ]
});

/// commands to be installed only for owner-owned guilds
static OWNER_COMMANDS: LazyLock<Vec<Command<Data, Error>>> = LazyLock::new(|| {
    vec![
        announce(),
        announce_test(),
        exit(),
        restart(),
        set_test(),
        stats(),
    ]
});

/// User data, which is stored and accessible in all command invocations
struct Data {
    db: Arc<JinxDb>,
}

pub async fn run_bot() -> Result<(), Error> {
    let db = JinxDb::open().await?;
    debug!("DB opened");
    let discord_token = db.get_discord_token().await?
        .ok_or(JinxError::new("discord token not provided. Re-run the application with the `init` subcommand to run first-time setup."))?;
    let intents = serenity::GatewayIntents::non_privileged();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            // all commands must appear in this list otherwise poise won't recoginize interactions for them
            commands: vec![
                announce(),
                announce_test(),
                create_post(),
                deactivate_license(),
                exit(),
                help(),
                init(),
                license_info(),
                link_product(),
                list_links(),
                lock_license(),
                restart(),
                set_log_channel(),
                set_test(),
                stats(),
                unlock_license(),
                user_info(),
                version(),
            ],
            event_handler: |ctx, event, framework, data| {
                Box::pin(event_handler(ctx, event, framework, data))
            },
            on_error: |e| {
                Box::pin(error_handler(e))
            },
            initialize_owners: false, // `initialize_owners: true` is broken. serenity::http::client::get_current_application_info has a deserialization bug
            ..Default::default()
        })
        .setup(|ctx, _ready, _framework| {
            Box::pin(async move {
                let db = Arc::new(db);
                debug!("registering global commands...");
                let commands_to_create = poise::builtins::create_application_commands(GLOBAL_COMMANDS.as_slice());
                ctx.http.create_global_commands(&commands_to_create).await?;

                // set up the task to set guild commands
                {
                    let shard_id = ctx.shard_id.get();
                    let http = ctx.http.clone();
                    let cache = ctx.cache.clone();
                    let db_clone = db.clone();
                    tokio::task::spawn(async move {
                        let mut updated_guilds: usize = 0;
                        for guild in cache.guilds() {
                            if guild.shard_id(&cache) == shard_id {
                                tokio::time::sleep(Duration::from_millis(500)).await; // rate limit to 2 TPS
                                if let Err(e) = set_guild_commands(&http, &db_clone, guild, None, None).await {
                                    error!("Error setting guild commands for guild {}: {:?}", guild, e);
                                } else {
                                    updated_guilds += 1;
                                }
                            }
                        }
                        info!("Shard {} set up commands in {} guilds", shard_id, updated_guilds);
                    });
                }

                // set up the task to periodically optimize the DB
                {
                    let db_clone = db.clone();
                    tokio::task::spawn(async move {
                        loop {
                            const SECONDS_PER_MINUTE: u64 = 60;
                            const MINUTES_PER_HOUR: u64 = 60;
                            const HOURS_PER_DAY: u64 = 24;
                            const SECONDS_PER_DAY: u64 = SECONDS_PER_MINUTE * MINUTES_PER_HOUR * HOURS_PER_DAY;

                            tokio::time::sleep(Duration::from_secs(SECONDS_PER_DAY)).await;
                            let start = Instant::now();
                            if let Err(e) = db_clone.optimize().await {}
                            let elapsed = start.elapsed();
                            info!("optimized db in {}ms", elapsed.as_millis());
                        }
                    });
                }

                debug!("framework setup complete");

                Ok(Data {
                    db
                })
            })
        })
        .build();

    debug!("framework built");

    let mut client = serenity::ClientBuilder::new(discord_token, intents)
        .framework(framework)
        .await.unwrap();

    debug!("client built. Starting...");

    // note that client.start() does NOT do sharding. If sharding is needed you need to use one of the alternative start functions
    // https://docs.rs/serenity/latest/serenity/gateway/index.html#sharding
    // https://discord.com/developers/docs/topics/gateway#sharding
    client.start().await.unwrap();

    Ok(())
}

/// Set (or reset) guild commands for this guild.
///
/// There is a global rate limit of 200 application command creates per day, per guild.
async fn set_guild_commands(http: impl AsRef<Http>, db: &JinxDb, guild_id: GuildId, force_owner: Option<bool>, force_creator: Option<bool>) -> Result<(), Error> {
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

async fn check_owner(context: Context<'_>) -> Result<bool, Error> {
    Ok(context.data().db.is_user_owner(context.author().id.get()).await?)
}

/// Outer event handler layer for error handling. See [`event_handler_inner`] for the actual event handler implementation.
async fn event_handler<'a>(
    context: &'a serenity::Context,
    event: &'a FullEvent,
    framework_context: FrameworkContext<'a, Data, Error>,
    data: &'a Data,
) -> Result<(), Error> {
    let result = event_handler_inner(context, event, framework_context, data).await;
    if let Err(e) = &result {
        error!("Unhandled error in event handler: {:?}", e)
    }
    result
}

/// Inner event handler layer. See [`event_handler`] for the error handling layer.
async fn event_handler_inner<'a>(
    context: &'a serenity::Context,
    event: &'a FullEvent,
    _framework_context: FrameworkContext<'a, Data, Error>,
    data: &'a Data,
) -> Result<(), Error> {
    match event {
        FullEvent::InteractionCreate { interaction: Interaction::Component(component_interaction) } => {
            #[allow(
                clippy::single_match
            )] // likely to add more matches later, so I'm leaving it like this because it's obnoxious to switch between `if let` and `match`
            match component_interaction.data.custom_id.as_str() {
                REGISTER_BUTTON_ID => {
                    let components = vec![CreateActionRow::InputText(CreateInputText::new(InputTextStyle::Short, "License Key", LICENSE_KEY_ID).placeholder("XXXX-cd071c534191"))];
                    let modal = CreateModal::new(REGISTER_MODAL_ID, "License Registration")
                        .components(components);
                    let response = CreateInteractionResponse::Modal(modal);
                    component_interaction.create_response(context, response).await?;
                }
                _ => {}
            }
        }
        FullEvent::InteractionCreate { interaction: Interaction::Modal(modal_interaction) } => {
            #[allow(
                clippy::single_match
            )] // likely to add more matches later, so I'm leaving it like this because it's obnoxious to switch between `if let` and `match`
            match modal_interaction.data.custom_id.as_str() {
                REGISTER_MODAL_ID => {
                    let license_key = modal_interaction.data.components.iter()
                        .flat_map(|row| row.components.iter())
                        .find_map(|component| {
                            if let ActionRowComponent::InputText(input_text) = component {
                                if input_text.custom_id == LICENSE_KEY_ID {
                                    input_text.value.as_deref()
                                        .map(|value| value.trim())
                                        .filter(|value| !value.is_empty())
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        });
                    if let Some(license_key) = license_key {
                        let license_type = license::identify_license(license_key);
                        debug!("got license \"{}\" which looks like {}", license_key, license_type);

                        /* Generic fail message. This message is deterministic based solely on the user-provided string,
                         * which prevents leaking information regarding license validity. For example, different messages
                         * for different contexts could let someone distinguish between:
                         * - A valid license that has already been activated by someone else
                         * - A valid, previously unactivated license that was activated by someone else while going through this flow
                         * - An invalid license
                         */
                        let send_fail_message = || async {
                            let description = if license_type.is_jinxxy_license() {
                                "The provided license key was not valid or is already in use".to_string()
                            } else {
                                format!(
                                    "The provided license key was not valid or is already in use.\n\
                                    Hint: I expect a Jinxxy key, but you appear to have provided {}. Please confirm you are providing the correct value.",
                                    license_type
                                )
                            };
                            let embed = CreateEmbed::default()
                                .title("Registration Failure")
                                .description(description)
                                .color(Colour::RED);
                            let message = CreateInteractionResponseMessage::default()
                                .ephemeral(true)
                                .embed(embed);
                            modal_interaction.create_response(context, CreateInteractionResponse::Message(message)).await?;
                            Ok::<(), Error>(())
                        };

                        let guild_id = modal_interaction.guild_id.ok_or(JinxError::new("expected to be in a guild"))?;
                        if let Some(api_key) = data.db.get_jinxxy_api_key(guild_id).await? {
                            let license = license_type.create_jinxxy_license(license_key);
                            let license_response = if let Some(license) = license {
                                jinxxy::check_license(&api_key, license).await?
                            } else {
                                // if the user has given us something that is very clearly not a Jinxxy license then don't even try hitting the API
                                None
                            };
                            if let Some(license_info) = license_response {
                                let member = modal_interaction.member.as_ref().ok_or(JinxError::new("expected to be in a guild"))?;
                                let user_id = member.user.id;

                                let (activations, mut validation) = if license_info.activations == 0 {
                                    // API call saving check: we already know how many validations there are, so if there are 0 we don't need to query them
                                    (None, Default::default())
                                } else {
                                    let activations = jinxxy::get_license_activations(&api_key, &license_info.license_id).await?;
                                    let validation = license::validate_jinxxy_license_activation(user_id, &activations);
                                    (Some(activations), validation)
                                };

                                // verify no activations from unexpected users
                                if validation.other_user || validation.locked {
                                    // some other user has already activated this license. This is the NORMAL fail case. The other fail cases are abnormal.

                                    // send a notification to the guild owner bot log if it's set up for this guild
                                    if let Some(log_channel) = data.db.get_log_channel(guild_id).await? {
                                        let message = if validation.locked {
                                            format!("<@{}> attempted to activate a locked license. An admin can unlock this license with the `/unlock_license` command.", user_id.get())
                                        } else {
                                            let mut message = format!("<@{}> attempted to activate a license that has already been used by:", user_id.get());
                                            activations.iter()
                                                .flat_map(|vec| vec.iter())
                                                .flat_map(|activation| activation.try_into_user_id())
                                                .for_each(|user_id| message.push_str(format!("\n- <@{}>", user_id).as_str()));
                                            message
                                        };
                                        let bot_log_message = CreateMessage::default().content(message);
                                        log_channel.send_message(context, bot_log_message).await?;
                                    }

                                    send_fail_message().await?;
                                } else {
                                    // log if multiple activations for this user
                                    if validation.multiple {
                                        warn!("{} is about to activate \"{}\". User already has multiple activations: {:?}", user_id, license_key, activations);
                                    }

                                    // calculate if we should grant roles
                                    let grant_roles = if validation.own_user {
                                        // if already activated grant roles now and skip next steps
                                        true
                                    } else {
                                        // we aren't activated, so we need to create the activation... and then check again to prevent race conditions
                                        let new_activation_id = jinxxy::create_license_activation(&api_key, &license_info.license_id, user_id.get()).await?;
                                        data.db.activate_license(guild_id, license_info.license_id.clone(), new_activation_id.clone(), user_id.get()).await?;
                                        let activations = jinxxy::get_license_activations(&api_key, &license_info.license_id).await?;
                                        validation = license::validate_jinxxy_license_activation(user_id, &activations);

                                        // log if multiple activations for different users
                                        if validation.multiple {
                                            warn!("{} just activated \"{}\" in {}. User already has multiple activations: {:?}", user_id, license_key, new_activation_id, activations);
                                        }

                                        // create roles if no non-us activations
                                        !(validation.other_user || validation.locked)
                                    };
                                    if validation.deadlocked() {
                                        // Two different people just race-conditioned their way to multiple activations so this license is now rendered unusable ever again.
                                        // A moderator can use `/deactivate_license` to fix this manually.
                                        warn!("license \"{}\" is deadlocked: multiple different users have somehow managed to activate it, rendering it unusable", license_key);

                                        // also send a notification to the guild owner bot log if it's set up for this guild
                                        if let Some(log_channel) = data.db.get_log_channel(guild_id).await? {
                                            let message = format!("<@{}> attempted to activate a deadlocked license. It shouldn't be possible, but multiple users have already activated this license. An admin can use the `/deactivate_license` command to fix this manually.", user_id.get());
                                            let bot_log_message = CreateMessage::default().content(message);
                                            log_channel.send_message(context, bot_log_message).await?;
                                        }
                                    }

                                    if grant_roles {
                                        let roles = data.db.get_roles(guild_id, license_info.product_id).await?;
                                        let mut client_message = format!("Congratulations, you are now registered as an owner of the {} product and have been granted the following roles:", license_info.product_name);
                                        let mut owner_message = format!("<@{}> has registered the {} product and has been granted the following roles:", user_id.get(), license_info.product_name);
                                        let mut errors: String = String::new();
                                        for role in roles {
                                            match member.add_role(context, role).await {
                                                Ok(()) => {
                                                    let bullet_point = format!("\n- <@&{}>", role.get());
                                                    client_message.push_str(bullet_point.as_str());
                                                    owner_message.push_str(bullet_point.as_str());
                                                }
                                                Err(e) => {
                                                    errors.push_str(format!("\n- <@&{}>", role.get()).as_str());
                                                    warn!("error granting role: {:?}", e);
                                                }
                                            }
                                        }
                                        let embed = if errors.is_empty() {
                                            CreateEmbed::default()
                                                .title("Registration Success")
                                                .description(client_message)
                                                .color(Colour::DARK_GREEN)
                                        } else {
                                            let message = format!("{}\n\nFailed to grant access to roles:{}\nThe bot may lack permission to grant the above roles. Contact your server administrator for support.", client_message, errors);
                                            CreateEmbed::default()
                                                .title("Registration Partial Success")
                                                .description(message)
                                                .color(Colour::ORANGE)
                                        };

                                        // let the user know what happened
                                        let modal_response_message = CreateInteractionResponseMessage::default()
                                            .ephemeral(true)
                                            .embed(embed);
                                        modal_interaction.create_response(context, CreateInteractionResponse::Message(modal_response_message)).await?;

                                        // also send a notification to the guild owner bot log if it's set up for this guild
                                        if let Some(log_channel) = data.db.get_log_channel(guild_id).await? {
                                            let bot_log_message = CreateMessage::default().content(owner_message);
                                            log_channel.send_message(context, bot_log_message).await?;
                                            if !errors.is_empty() {
                                                let bot_log_error_message = CreateMessage::default()
                                                    .content(format!("Failed to grant <@{}> access to the following roles:{}\nPlease check bot permissions.", user_id.get(), errors));
                                                log_channel.send_message(context, bot_log_error_message).await?;
                                            }
                                        }
                                    } else {
                                        // license activation check failed. This happens if we created an activation but the double check failed due to finding a second user's activation.
                                        send_fail_message().await?;
                                    }
                                }
                            } else {
                                // could not find a matching license in Jinxxy
                                send_fail_message().await?;
                            }
                        } else {
                            let embed = CreateEmbed::default()
                                .title("Jinx Misconfiguration")
                                .description("Jinxxy API key is not set: please contact the server administrator for support.")
                                .color(Colour::RED);
                            let message = CreateInteractionResponseMessage::default()
                                .ephemeral(true)
                                .embed(embed);
                            modal_interaction.create_response(context, CreateInteractionResponse::Message(message)).await?;
                        }
                    } else {
                        // User did not provide a license string, or provided all whitespace or something weird like that.
                        let embed = CreateEmbed::default()
                            .title("Registration Failure")
                            .description("You must provide a license key")
                            .color(Colour::RED);
                        let message = CreateInteractionResponseMessage::default()
                            .ephemeral(true)
                            .embed(embed);
                        modal_interaction.create_response(context, CreateInteractionResponse::Message(message)).await?;
                    }
                }
                _ => {}
            }
        }
        _ => {}
    }

    Ok(())
}

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

    fn debug<T: Debug>(title: &'static str, context: &'a serenity::client::Context, diagnostic: T) -> Option<Self> {
        Some(Self {
            title,
            diagnostic: Some(format!("{:?}", diagnostic)),
            context: SomeContext::Serenity(context),
        })
    }

    fn debug_cmd<T: Debug>(title: &'static str, context: Context<'a>, diagnostic: T) -> Option<Self> {
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
async fn error_handler(error: FrameworkError<'_, Data, Error>) {
    let error: Option<PoiseError> = match error {
        FrameworkError::Setup { ctx, error, .. } => PoiseError::debug("Setup", ctx, error),
        FrameworkError::EventHandler { ctx, error, .. } => PoiseError::debug("Event handler", ctx, error),
        FrameworkError::Command { ctx, error, .. } => PoiseError::debug_cmd("Command", ctx, error),
        FrameworkError::SubcommandRequired { ctx, .. } => PoiseError::new_cmd("Subcommand required", ctx),
        FrameworkError::CommandPanic { ctx, payload, .. } => {
            // this is a really weird one, so I don't want to do ANYTHING beyond logging it
            error!("Command panic in {}: {:?}", ctx.command().name, payload);
            None
        }
        FrameworkError::ArgumentParse { ctx, input, error, .. } => PoiseError::string_cmd("Argument parse error", ctx, format!("{:?} {:?}", input, error)),
        FrameworkError::CommandStructureMismatch { description, .. } => {
            // this technically has a context, but it's a weird 1-off type
            error!("Command structure mismatch: {:}", description);
            None
        }
        FrameworkError::CooldownHit { ctx, .. } => PoiseError::new_cmd("Cooldown hit", ctx),
        FrameworkError::MissingBotPermissions { ctx, .. } => PoiseError::new_cmd("Missing bot permissions", ctx),
        FrameworkError::MissingUserPermissions { ctx, .. } => PoiseError::new_cmd("Missing user permissions", ctx),
        FrameworkError::NotAnOwner { ctx, .. } => PoiseError::new_cmd("Not an owner", ctx),
        FrameworkError::GuildOnly { ctx, .. } => PoiseError::new_cmd("Guild only", ctx),
        FrameworkError::DmOnly { ctx, .. } => PoiseError::new_cmd("DM only", ctx),
        FrameworkError::NsfwOnly { ctx, .. } => PoiseError::new_cmd("NSFW only", ctx),
        FrameworkError::CommandCheckFailed { ctx, error, .. } => PoiseError::debug_cmd("Command check failed", ctx, error),
        FrameworkError::DynamicPrefix { error, .. } => {
            // this technically has a context, but it's a weird 1-off type
            error!("Dynamic prefix error: {:?}", error);
            None
        }
        FrameworkError::UnknownCommand { ctx, trigger, .. } => PoiseError::debug("Unknown command", ctx, trigger),
        FrameworkError::UnknownInteraction { ctx, interaction, .. } => PoiseError::debug("Unknown interaction", ctx, interaction),
        FrameworkError::NonCommandMessage { ctx, error, .. } => PoiseError::debug("Non-command message", ctx, error),
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
                let nonce: u64 = random();
                let nonce = format!("{:016X}", nonce);
                let user = context.author();

                if let Some(diagnostic) = error.diagnostic {
                    error!("NONCE[{}] {} error encountered in {}: Caused by {:?}. {}", nonce, error.title, context.command().name, user, diagnostic);
                } else {
                    error!("NONCE[{}] {} error encountered in {}. Caused by {:?}.", nonce, error.title, context.command().name, user);
                }
                let result = context.send(CreateReply::default()
                    .ephemeral(true)
                    .content(format!("Error: {}. Additional data has been sent to the log. Please report this to the bot developer with error code `{}`", error.title, nonce))
                ).await;
                if let Err(e) = result {
                    error!("Error sending error message: {:?}", e);
                }
            }
        }
    };
}
