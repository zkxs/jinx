# Self-Hosting Jinx

If you just want to use the bot without self-hosting, see [the readme](../README.md). These instructions are for running
your own, independently operated instance of Jinx. This gives you complete control over your data.

> [!NOTE]
> Jinx stores all of its data in a sqlite database in the working directory named `jinx.sqlite`. You should try not to
> lose this file, but because license activations are stored remotely in Jinxxy, local database loss is not catastrophic.

1. [Create a new Discord App](https://discord.com/developers/applications)
2. Record your bot's API token. You can reset this in the "Bot" tab if you lose it.
3. In the "Installation" tab, check the User and Guild checkboxes and set Install Link to "None"
4. In the "Bot" tab, uncheck the "Public Bot" checkbox.
5. In the "OAuth2" tab, check "application.commands", "bot", "Manage Roles", "Send Messages", and
   "Send Messages in Threads", set the Integration Type to "Guild", then copy the link. Use this link to add the bot to
   servers.
6. Clone this project
7. [Install Rust](https://www.rust-lang.org/tools/install)
8. Run `cargo install --path .` to build the project and install the `jinx` command.
9. To perform one-time setup, run `jinx init <DISCORD_TOKEN>` or `DISCORD_TOKEN=<DISCORD_TOKEN> jinx init` (the second
   option is more secure from process list snooping).
10. Finally, run `jinx`
11. You can exit Jinx by sending the process a SIGINT/SIGTERM/Ctrl+C, or by using the `/exit` command in a DM with the
    bot.

> [!WARNING]
> Avoid running multiple instances of the bot. It is not designed to work with multiple instances running.

Optionally, to gain access to special owner commands `/stats` and `/exit`, do the following:
1. In your terminal, add yourself as a bot owner with `jinx owner add <DISCORD_USER_ID>`. You may do this while the bot
   is running!
2. In your Discord server, run `/init install_owner_commands`. You may undo this later with
   `/init uninstall_owner_commands`.
