// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::bot::commands::{LICENSE_KEY_ID, REGISTER_BUTTON_ID};
use crate::bot::util::set_guild_commands;
use crate::bot::{Data, Error, REGISTER_MODAL_ID};
use crate::error::JinxError;
use crate::http::jinxxy;
use crate::license;
use poise::serenity_prelude::{ActionRowComponent, Colour, CreateActionRow, CreateEmbed, CreateInputText, CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage, CreateModal, FullEvent, InputTextStyle, Interaction};
use poise::{serenity_prelude as serenity, FrameworkContext};
use tracing::{debug, error, info, warn};

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
        FullEvent::GuildCreate { guild, is_new } => {
            // is_new == Some(false) when we're just restarting the bot
            // is_new == Some(true) when a new guild adds the bot
            if !matches!(is_new, Some(false)) {
                info!("GuildCreate guild={} is_new={:?}", guild.id.get(), is_new);
            }

            if let Err(e) = set_guild_commands(&context.http, &data.db, guild.id, None, None).await {
                error!("Error setting guild commands for guild {}: {:?}", guild.id.get(), e);
            }
        }
        FullEvent::GuildDelete { incomplete, full } => {
            // On startup, we get an event with `unavailable == false && full == None` for all guilds the bot used to be in but is kicked from
            if incomplete.unavailable || full.is_some() {
                info!("GuildDelete guild={:?} full={:?}", incomplete, full)
            }
        }
        FullEvent::CacheReady { guilds } => {
            /* the docs claim this happens "when the cache has received and inserted all data from
             * guilds" and that "this process happens upon starting your bot". HOWEVER, it apparently
             * ALSO happens every single time any new guild is added.
             */
            debug!("cache ready! {} guilds.", guilds.len());
        }
        FullEvent::Ratelimit { data } => {
            warn!("Ratelimit event: {:?}", data);
        }
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
                        let user_id = modal_interaction.user.id;
                        debug!("got license from <@{}> which looks like {}", user_id, license_type);

                        /* Generic fail message. This message is deterministic based solely on the user-provided string,
                         * which prevents leaking information regarding license validity. For example, different messages
                         * for different contexts could let someone distinguish between:
                         * - A valid license that has already been activated by someone else
                         * - A valid, previously unactivated license that was activated by someone else while going through this flow
                         * - An invalid license
                         */
                        let send_fail_message = || async {
                            if license_type.is_license() {
                                debug!("failed to verify license for <@{}> which looks like {}", user_id.get(), license_type);
                            } else {
                                debug!("failed to verify license \"{}\" for <@{}> which looks like {}", license_key, user_id.get(), license_type);
                            }

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
                            let license = license_type.create_untrusted_jinxxy_license(license_key);
                            let license_response = if let Some(license) = license {
                                jinxxy::check_license(&api_key, license).await?
                            } else {
                                // if the user has given us something that is very clearly not a Jinxxy license then don't even try hitting the API
                                None
                            };
                            if let Some(license_info) = license_response {
                                let member = modal_interaction.member.as_ref().ok_or(JinxError::new("expected to be in a guild"))?;

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
                                        info!("{}", message);
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
                                        warn!("<@{}> is about to activate \"{}\". User already has multiple activations: {:?}", user_id, license_key, activations);
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
                                            warn!("<@{}> just activated \"{}\" in {}. User already has multiple activations: {:?}", user_id, license_key, new_activation_id, activations);
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
                                            let embed = CreateEmbed::default()
                                                .title("Activation Error")
                                                .description(message)
                                                .color(Colour::RED);
                                            let bot_log_message = CreateMessage::default().embed(embed);
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
                                            log_channel.send_message(context, bot_log_message).await?;
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
        FullEvent::InteractionCreate { interaction: Interaction::Command(command_interaction) } => {
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
