// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::bot::commands::{LICENSE_KEY_ID, REGISTER_BUTTON_ID};
use crate::bot::util;
use crate::bot::util::MessageExtensions;
use crate::bot::{Data, Error, REGISTER_MODAL_ID};
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::license;
use crate::license::LicenseType;
use poise::serenity_prelude::{
    ActionRowComponent, Colour, CreateActionRow, CreateEmbed, CreateInputText, CreateInteractionResponse,
    CreateMessage, CreateModal, EditInteractionResponse, FullEvent, InputTextStyle, Interaction,
};
use poise::{FrameworkContext, serenity_prelude as serenity};
use regex::Regex;
use std::sync::LazyLock;
use tracing::{debug, error, info, warn};

static GLOBAL_EASTER_EGG_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:you'?re|ur) +(?:cute|a +cutie)\b", // uh, let me explain: I'm really bored right now and I thought it'd be funny if the bot did something silly if you call it cute.
    )
    .expect("Failed to compile GLOBAL_EASTER_EGG_REGEX")
});

thread_local! {
    // trick to avoid a subtle performance edge case: https://docs.rs/regex/latest/regex/index.html#sharing-a-regex-across-threads-can-result-in-contention
    static EASTER_EGG_REGEX: Regex = GLOBAL_EASTER_EGG_REGEX.clone();
}

/// Outer event handler layer for error handling. See [`event_handler_inner`] for the actual event handler implementation.
pub async fn event_handler<'a>(
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
        // bot was added to a guild
        FullEvent::GuildCreate { guild, is_new } => {
            // is_new == Some(false) when we're just restarting the bot
            // is_new == Some(true) when a new guild adds the bot
            if !matches!(is_new, Some(false)) {
                info!("GuildCreate guild={} is_new={:?}", guild.id.get(), is_new);
            }

            // reinstall guild commands
            //TODO: when the bot starts we might receive a flurry of GuildCreate events leading to ratelimit issues when we attempt to reinstall the commands with no delay.
            // I might be able to figure out some sort of work queue for if that ever becomes a problem. All we need is a serenity::Http and a GuildId so we should be good to handle this from a background task.
            let guild_command_reinstall_result =
                util::set_guild_commands(&context.http, &data.db, guild.id, None, None).await;
            if let Err(e) = guild_command_reinstall_result {
                error!("Error setting guild commands for guild {}: {:?}", guild.id.get(), e);
            }

            match data.db.get_jinxxy_api_key(guild.id).await {
                Ok(Some(_)) => {
                    // the guild has an API key
                    let register_guild_result = data.api_cache.register_guild_in_cache(guild.id).await;
                    if let Err(e) = register_guild_result {
                        error!(
                            "Error registering guild {} for background cache refresh: {:?}",
                            guild.id.get(),
                            e
                        );
                    }
                }
                Ok(None) => {
                    // guild had no API key set; do nothing
                }
                Err(e) => {
                    error!(
                        "Error checking API key before registering guild {} for background cache refresh: {:?}",
                        guild.id.get(),
                        e
                    );
                }
            }
        }
        // bot was removed from a guild (kick, ban, or guild deleted)
        FullEvent::GuildDelete { incomplete, full } => {
            // On startup, we get an event with `unavailable == false && full == None` for all guilds the bot used to be in but is kicked from
            if incomplete.unavailable || full.is_some() {
                let unregister_guild_result = data.api_cache.unregister_guild_in_cache(incomplete.id).await;
                if let Err(e) = unregister_guild_result {
                    error!(
                        "Error registering guild {} for background cache refresh: {:?}",
                        incomplete.id.get(),
                        e
                    );
                }

                info!("GuildDelete guild={:?} full={:?}", incomplete, full)
            }
        }
        /*
        the docs claim this happens "when the cache has received and inserted all data from
        guilds" and that "this process happens upon starting your bot". HOWEVER, it apparently
        ALSO happens every single time any new guild is added.
        */
        FullEvent::CacheReady { guilds } => {
            debug!("cache ready! {} guilds.", guilds.len());
        }
        // I'm curious if this ever happens. I'll debug log it for now and worry about it later.
        FullEvent::Ratelimit { data } => {
            warn!("Ratelimit event: {:?}", data);
        }
        // handle incoming messages (channel/DM/etc)
        FullEvent::Message { new_message } => {
            /*
            fun fact: even without MESSAGE_CONTENT intent, we still get message content in a few exceptional cases:
            - Content in messages that an app sends
            - Content in DMs with the app
            - Content in which the app is mentioned
            - Content of the message a message context menu command is used on

            So, basically any case where Discord thinks a user may actually intend for the bot to see the message.
            */

            if new_message.fixed_is_private(context).await {
                debug!(
                    "Received DM id={}; channel={}; author={}: {}",
                    new_message.id.get(),
                    new_message.channel_id.get(),
                    new_message.author.id.get(),
                    new_message.content,
                );

                if !new_message.author.bot {
                    let reply_content = "Jinx is a Discord bot that grants roles to users when they register Jinxxy license keys. \
                    It does not work from DMs: it needs to be set up in a server.\n\
                    For documentation, see <https://github.com/zkxs/jinx>\n\
                    For support, join https://discord.gg/aKkA6m26f9";
                    if let Err(e) = new_message.reply_ping(context, reply_content).await {
                        warn!("Unable to reply to DM. Error: {:?}", e);
                    }
                }
            } else if new_message.mentions_me(context).await.unwrap_or(false) {
                if let Some(guild_id) = new_message.guild_id {
                    debug!(
                        "Mentioned! guild={}; id={}; channel={}; author={}: {}",
                        guild_id.get(),
                        new_message.id.get(),
                        new_message.channel_id.get(),
                        new_message.author.id.get(),
                        new_message.content
                    );
                } else {
                    debug!(
                        "Mentioned in non-guild-context id={}; channel={}; author={}: {}",
                        new_message.id.get(),
                        new_message.channel_id.get(),
                        new_message.author.id.get(),
                        new_message.content,
                    );
                }

                // since we got mentioned we might as well do something funny here
                if !new_message.author.bot
                    && data.db.is_user_owner(new_message.author.id.get()).await?
                    && EASTER_EGG_REGEX.with(|regex| regex.is_match(new_message.content.as_str()))
                {
                    // Easter egg: when the owner says something matching a specific regex, try to reply
                    if let Err(e) = new_message.reply_ping(context, "no, you! 😳").await {
                        warn!(
                            "Unable to reply to owner easter-egg prompt. Falling back to reaction. Error: {:?}",
                            e
                        );
                        if let Err(e) = new_message.react(context, '😳').await {
                            warn!("Unable to react to owner easter-egg prompt: {:?}", e);
                        }
                    }
                } else {
                    // if anyone mentions the bot and we haven't already done the Easter egg, then try and add a reaction
                    let result = new_message.react(context, '👀').await;
                    if let Err(e) = result {
                        warn!("Unable to react to bot mention: {:?}", e);
                    }
                }
            }
        }
        // handle when messages are edited
        FullEvent::MessageUpdate {
            old_if_available,
            new,
            event,
        } => {
            // this MIGHT work on channel messages that mention the bot, but I haven't tested it.
            // this DOES work on DMs
            if event.fixed_is_private(context).await {
                if let Some(new) = new {
                    if let Some(old) = old_if_available {
                        debug!(
                            "DM {} updated:\nold: {}\nnew: {}",
                            event.id.get(),
                            old.content,
                            new.content
                        );
                    } else {
                        debug!("DM {} updated: {}", event.id.get(), new.content);
                    }
                } else {
                    debug!("DM {} updated", event.id.get());
                }
            }
        }
        // handle component interactions
        FullEvent::InteractionCreate {
            interaction: Interaction::Component(component_interaction),
        } => {
            #[allow(clippy::single_match)]
            // likely to add more matches later, so I'm leaving it like this because it's obnoxious to switch between `if let` and `match`
            match component_interaction.data.custom_id.as_str() {
                // create the register form when a user presses the register button
                REGISTER_BUTTON_ID => {
                    let components = vec![CreateActionRow::InputText(
                        CreateInputText::new(InputTextStyle::Short, "License Key", LICENSE_KEY_ID)
                            .placeholder("XXXX-cd071c534191"),
                    )];
                    let modal = CreateModal::new(REGISTER_MODAL_ID, "License Registration").components(components);
                    let response = CreateInteractionResponse::Modal(modal);
                    component_interaction.create_response(context, response).await?;
                }
                _ => {}
            }
        }
        // handle modal interactions
        FullEvent::InteractionCreate {
            interaction: Interaction::Modal(modal_interaction),
        } => {
            // this may take some time, so we defer the modal_interaction. If we don't ACK the interaction during the first 3s it is invalidated.
            modal_interaction.defer_ephemeral(context).await?;

            // likely to add more matches later, so I'm suppressing this lint because it's obnoxious to switch between `if let` and `match`
            #[allow(clippy::single_match)]
            match modal_interaction.data.custom_id.as_str() {
                // this is the code that handles a user submitting the register form. All the license activation logic lives here.
                REGISTER_MODAL_ID => {
                    let license_key = modal_interaction
                        .data
                        .components
                        .iter()
                        .flat_map(|row| row.components.iter())
                        .find_map(|component| {
                            if let ActionRowComponent::InputText(input_text) = component {
                                if input_text.custom_id == LICENSE_KEY_ID {
                                    input_text
                                        .value
                                        .as_deref()
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
                        let guild_id = modal_interaction
                            .guild_id
                            .ok_or_else(|| JinxError::new("expected to be in a guild"))?;
                        let user_id = modal_interaction.user.id;
                        let license_type = license::identify_license(license_key);

                        debug!(
                            "got license in {} from <@{}> which looks like {}",
                            guild_id.get(),
                            user_id.get(),
                            license_type
                        );

                        /*
                        Generic fail message. This message is deterministic based solely on the user-provided string,
                        which prevents leaking information regarding license validity. For example, different messages
                        for different contexts could let someone distinguish between:
                        - A valid license that has already been activated by someone else
                        - A valid, previously unactivated license that was activated by someone else while going through this flow
                        - An invalid license
                        */
                        let send_fail_message = async || {
                            if license_type.is_license() {
                                debug!(
                                    "failed to verify license in {} for <@{}> which looks like {}",
                                    guild_id.get(),
                                    user_id.get(),
                                    license_type
                                );
                            } else {
                                // if the user gave me something that I don't believe is a license, debug print it so I can learn if there's some weird case I need to handle
                                debug!(
                                    "failed to verify license \"{}\" in {} for <@{}> which looks like {}",
                                    license_key,
                                    guild_id.get(),
                                    user_id.get(),
                                    license_type
                                );
                            }

                            if matches!(license_type, LicenseType::Gumroad) {
                                data.db.increment_gumroad_failure_count(guild_id).await?;
                            }

                            let description = if license_type.is_jinxxy_license() {
                                "The provided Jinxxy license key was not valid or is already in use".to_string()
                            } else {
                                format!(
                                    "The provided Jinxxy license key was not valid or is already in use.\n\n\
                                    **This bot only supports Jinxxy keys**, but you appear to have provided {license_type}. \
                                    Please confirm you are providing the correct value to the correct bot. \
                                    Jinxxy keys should look like `XXXX-cd071c534191` or `3642d957-c5d8-4d18-a1ae-cd071c534191`."
                                )
                            };
                            let embed = CreateEmbed::default()
                                .title("Jinxxy Product Registration Failed")
                                .description(description)
                                .color(Colour::RED);
                            let edit = EditInteractionResponse::default().embed(embed);
                            modal_interaction.edit_response(context, edit).await?;
                            Ok::<(), Error>(())
                        };

                        if let Some(api_key) = data.db.get_jinxxy_api_key(guild_id).await? {
                            let license = license_type.create_untrusted_jinxxy_license(license_key);
                            let license_response = if let Some(license) = license {
                                jinxxy::check_license(&api_key, license, true).await?
                            } else {
                                // if the user has given us something that is very clearly not a Jinxxy license then don't even try hitting the API
                                None
                            };
                            if let Some(license_info) = license_response {
                                let member = modal_interaction
                                    .member
                                    .as_ref()
                                    .ok_or_else(|| JinxError::new("expected to be in a guild"))?;

                                let (activations, mut validation) = if license_info.activations == 0 {
                                    // API call saving check: we already know how many validations there are, so if there are 0 we don't need to query them
                                    (None, Default::default())
                                } else {
                                    let activations =
                                        jinxxy::get_license_activations(&api_key, &license_info.license_id).await?;
                                    let validation = license::validate_jinxxy_license_activation(user_id, &activations);
                                    (Some(activations), validation)
                                };

                                // verify no activations from unexpected users
                                if validation.other_user || validation.locked {
                                    // some other user has already activated this license. This is the NORMAL fail case. The other fail cases are abnormal.

                                    // send a notification to the guild owner bot log if it's set up for this guild
                                    if let Some(log_channel) = data.db.get_log_channel(guild_id).await? {
                                        let message = if validation.locked {
                                            format!(
                                                "<@{}> attempted to activate a locked license. An admin can unlock this license with the `/unlock_license` command.",
                                                user_id.get()
                                            )
                                        } else {
                                            let mut message = format!(
                                                "<@{}> attempted to activate a license that has already been used by:",
                                                user_id.get()
                                            );
                                            activations
                                                .iter()
                                                .flat_map(|vec| vec.iter())
                                                .flat_map(|activation| activation.try_into_user_id())
                                                .for_each(|user_id| {
                                                    message.push_str(format!("\n- <@{}>", user_id).as_str())
                                                });
                                            message
                                        };
                                        info!(
                                            "in {} for license id {}, {}",
                                            guild_id, license_info.license_id, message
                                        );
                                        let embed = CreateEmbed::default()
                                            .title("Activation Attempt Failed")
                                            .description(message)
                                            .color(Colour::ORANGE);
                                        let bot_log_message = CreateMessage::default().embed(embed);
                                        log_channel.send_message(context, bot_log_message).await?;
                                    }

                                    send_fail_message().await?;
                                } else {
                                    // log if multiple activations for this user
                                    if validation.multiple {
                                        warn!(
                                            "in {} <@{}> is about to activate {}. User already has multiple activations: {:?}",
                                            guild_id.get(),
                                            user_id.get(),
                                            license_info.license_id,
                                            activations
                                        );
                                    }

                                    // check our db to see if we have a record there
                                    let activation_present_in_db = data
                                        .db
                                        .has_user_license_activations(
                                            guild_id,
                                            user_id.get(),
                                            license_info.license_id.clone(),
                                        )
                                        .await?;

                                    // calculate if we should grant roles
                                    let grant_roles = if validation.own_user {
                                        // if already activated grant roles now and skip next steps

                                        if !activation_present_in_db {
                                            if let Some(activation) = activations.iter().flatten().next() {
                                                data.db
                                                    .activate_license(
                                                        guild_id,
                                                        license_info.license_id.clone(),
                                                        activation.id.clone(),
                                                        user_id.get(),
                                                        Some(license_info.product_id.clone()),
                                                        license_info.version_id().map(|str| str.to_string()),
                                                    )
                                                    .await?;
                                                warn!(
                                                    "in {} <@{}> just activated {}, but it was not in the DB! That's weird. Restored via {}",
                                                    guild_id.get(),
                                                    user_id.get(),
                                                    license_info.license_id,
                                                    activation.id
                                                );
                                            } else {
                                                warn!(
                                                    "This should be impossible: we JUST validated this activation but now it is empty."
                                                )
                                            }
                                        }
                                        true
                                    } else {
                                        // we aren't activated, so we need to create the activation... and then check again to prevent race conditions
                                        let new_activation_id = jinxxy::create_license_activation(
                                            &api_key,
                                            &license_info.license_id,
                                            user_id.get(),
                                        )
                                        .await?;
                                        data.db
                                            .activate_license(
                                                guild_id,
                                                license_info.license_id.clone(),
                                                new_activation_id.clone(),
                                                user_id.get(),
                                                Some(license_info.product_id.clone()),
                                                license_info.version_id().map(|str| str.to_string()),
                                            )
                                            .await?;
                                        let activations =
                                            jinxxy::get_license_activations(&api_key, &license_info.license_id).await?;
                                        validation = license::validate_jinxxy_license_activation(user_id, &activations);

                                        // log if multiple activations for different users
                                        if validation.multiple {
                                            warn!(
                                                "in {} <@{}> just activated {} via {}. User already has multiple activations: {:?}",
                                                guild_id.get(),
                                                user_id.get(),
                                                license_info.license_id,
                                                new_activation_id,
                                                activations
                                            );
                                        }

                                        // create roles if no non-us activations
                                        !(validation.other_user || validation.locked)
                                    };
                                    if validation.deadlocked() {
                                        // Two different people just race-conditioned their way to multiple activations so this license is now rendered unusable ever again.
                                        // A moderator can use `/deactivate_license` to fix this manually.
                                        warn!(
                                            "in {} license {} is deadlocked: multiple different users have somehow managed to activate it, rendering it unusable",
                                            guild_id.get(),
                                            license_info.license_id
                                        );

                                        // also send a notification to the guild owner bot log if it's set up for this guild
                                        if let Some(log_channel) = data.db.get_log_channel(guild_id).await? {
                                            let message = format!(
                                                "<@{}> attempted to activate a deadlocked license. It shouldn't be possible, but multiple users have already activated this license. An admin can use the `/deactivate_license` command to fix this manually.",
                                                user_id.get()
                                            );
                                            let embed = CreateEmbed::default()
                                                .title("Activation Error")
                                                .description(message)
                                                .color(Colour::RED);
                                            let bot_log_message = CreateMessage::default().embed(embed);
                                            log_channel.send_message(context, bot_log_message).await?;
                                        }
                                    }

                                    if grant_roles {
                                        let roles = data
                                            .db
                                            .get_role_grants(guild_id, license_info.new_product_version_id())
                                            .await?;

                                        let product_display_name = if let Some(product_version_info) =
                                            license_info.product_version_info
                                        {
                                            format!(
                                                "{} (version {})",
                                                license_info.product_name, product_version_info.product_version_name
                                            )
                                        } else {
                                            license_info.product_name
                                        };
                                        if roles.is_empty() {
                                            let embed = CreateEmbed::default()
                                                .title("Registration Partial Success")
                                                .description(format!("You have registered {}, but there are no configured role links. Please notify the server owner and then try again after role links have been configured.", product_display_name))
                                                .color(Colour::GOLD);

                                            /*
                                            Let the user know what happened.
                                            Note that this can fail if the interaction has been invalidated, which happens in some cases:
                                            - 3s after a non-acked interaction
                                            - 15m after an acked interaction
                                             */
                                            let edit = EditInteractionResponse::default().embed(embed);
                                            let user_notification_result =
                                                modal_interaction.edit_response(context, edit).await;
                                            if let Err(error) = user_notification_result {
                                                error!("Error notifying user of license activation: {:?}", error);
                                            }

                                            // also send a notification to the guild owner bot log if it's set up for this guild
                                            if let Some(log_channel) = data.db.get_log_channel(guild_id).await? {
                                                let owner_message = format!(
                                                    "<@{}> has registered the {} product, which has no configured roles!",
                                                    user_id.get(),
                                                    product_display_name
                                                );
                                                let embed = CreateEmbed::default()
                                                    .title("License Activation")
                                                    .color(Colour::GOLD)
                                                    .description(owner_message);
                                                let bot_log_message = CreateMessage::default().embed(embed);
                                                let bot_log_result =
                                                    log_channel.send_message(context, bot_log_message).await;
                                                if let Err(e) = bot_log_result {
                                                    warn!(
                                                        "Error logging to log channel in {}: {:?}",
                                                        guild_id.get(),
                                                        e
                                                    );
                                                }
                                            }
                                        } else {
                                            let mut client_message = format!(
                                                "Congratulations, you are now registered as an owner of the {} product and have been granted the following roles:",
                                                product_display_name
                                            );
                                            let mut owner_message = format!(
                                                "<@{}> has registered the {} product and has been granted the following roles:",
                                                user_id.get(),
                                                product_display_name
                                            );
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
                                                        warn!("in {} error granting role: {:?}", guild_id.get(), e);
                                                    }
                                                }
                                            }
                                            let embed = if errors.is_empty() {
                                                CreateEmbed::default()
                                                    .title("Registration Success")
                                                    .description(client_message)
                                                    .color(Colour::DARK_GREEN)
                                            } else {
                                                let message = format!(
                                                    "{}\n\nFailed to grant access to roles:{}\nThe bot may lack permission to grant the above roles. Contact your server administrator for support.",
                                                    client_message, errors
                                                );
                                                CreateEmbed::default()
                                                    .title("Registration Partial Success")
                                                    .description(message)
                                                    .color(Colour::ORANGE)
                                            };

                                            /*
                                            Let the user know what happened.
                                            Note that this can fail if the interaction has been invalidated, which happens in some cases:
                                            - 3s after a non-acked interaction
                                            - 15m after an acked interaction
                                             */
                                            let edit = EditInteractionResponse::default().embed(embed);
                                            let user_notification_result =
                                                modal_interaction.edit_response(context, edit).await;
                                            if let Err(error) = user_notification_result {
                                                error!("Error notifying user of license activation: {:?}", error);
                                            }

                                            // also send a notification to the guild owner bot log if it's set up for this guild
                                            if let Some(log_channel) = data.db.get_log_channel(guild_id).await? {
                                                let embed = CreateEmbed::default()
                                                    .title("License Activation")
                                                    .description(owner_message);
                                                let bot_log_message = CreateMessage::default().embed(embed);
                                                let bot_log_message = if errors.is_empty() {
                                                    bot_log_message
                                                } else {
                                                    let error_embed = CreateEmbed::default()
                                                        .title("Role Grant Error")
                                                        .description(format!("Failed to grant <@{}> access to the following roles:{}\nPlease check bot permissions.", user_id.get(), errors))
                                                        .color(Colour::RED);
                                                    bot_log_message.embed(error_embed)
                                                };
                                                let bot_log_result =
                                                    log_channel.send_message(context, bot_log_message).await;
                                                if let Err(e) = bot_log_result {
                                                    warn!(
                                                        "Error logging to log channel in {}: {:?}",
                                                        guild_id.get(),
                                                        e
                                                    );
                                                }
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
                                .description(
                                    "Jinxxy API key is not set: please contact the server administrator for support.",
                                )
                                .color(Colour::RED);
                            let edit = EditInteractionResponse::default().embed(embed);
                            modal_interaction.edit_response(context, edit).await?;
                        }
                    } else {
                        // User did not provide a license string, or provided all whitespace or something weird like that.
                        let embed = CreateEmbed::default()
                            .title("Registration Failure")
                            .description("You must provide a license key")
                            .color(Colour::RED);
                        let edit = EditInteractionResponse::default().embed(embed);
                        modal_interaction.edit_response(context, edit).await?;
                    }
                }
                _ => {}
            }
        }
        FullEvent::InteractionCreate {
            interaction: Interaction::Command(command_interaction),
        } => {
            debug!(
                "command \"{}\" invoked in {:?} by <@{}>",
                command_interaction.data.name,
                command_interaction.guild_id.map(|guild| guild.get()),
                command_interaction.user.id.get()
            );
        }
        _ => {}
    }

    Ok(())
}
