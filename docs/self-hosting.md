# Self-Hosting Jinx

If you just want to use the bot you do not need to self-host and you should instead follow the instructions in
[the readme](../README.md#installation). The instructions on this page are for hosting your own, independently operated
instance of Jinx. This gives you complete control over your data, but you need a server with 24/7 uptime.

> [!NOTE]
> Jinx stores all of its data in a sqlite database in the working directory named `jinx.sqlite`. You should try not to
> lose this file. Because license activations are stored remotely in Jinxxy local database loss is not completely
> catastrophic.

## Setup Instructions

1. Register the Discord bot
   1. [Create a new Discord App](https://discord.com/developers/applications)
   2. Record your bot's API token. You can reset this in the "Bot" tab if you lose it.
   3. In the "Installation" tab, check the Guild checkbox and set Install Link to "None"
   4. In the "Bot" tab, uncheck the "Public Bot" checkbox.
   5. In the "OAuth2" tab, check "application.commands", "bot", "Manage Roles", "Send Messages", and
      "Send Messages in Threads", set the Integration Type to "Guild", then copy the link. Use this link to add the bot to
      servers.
2. Install a jinx binary
   1. [Install Rust](https://www.rust-lang.org/tools/install)
   2. git clone this project
   3. Run `cargo install --path .` to build the project and install the `jinx` command.
3. 1st-time setup
   1. run `jinx init <DISCORD_TOKEN>` or `DISCORD_TOKEN=<DISCORD_TOKEN> jinx init` (the second option is more secure
      from process list snooping).
   2. add yourself as a bot owner with `jinx owner add <DISCORD_USER_ID>`. This works even if the bot process is running!
4. Finally, run jinx via `./run.sh`. If you run jinx directly the `/restart` command will not function correctly.
5. You can exit Jinx by sending the process a SIGINT/SIGTERM/Ctrl+C, or by using the `/exit` command.

> [!WARNING]
> Avoid running multiple instances of the bot. It is not designed to work with multiple instances running.

## Keeping Jinx Updated

1. Use `jinx update-check` on the command line or `/version` in Discord to check for new versions.
2. `git pull`
3. `cargo install --path .`
4. Run `/restart` in Discord

## Jinx Binary Help

The output of `jinx --help`:

```
Discord bot that handles Jinxxy license registration. If ran with no subcommands the bot will start.

Usage: jinx [COMMAND]

Commands:
  init          Initialize DB with a Discord bot token and exit
  update-check  Check GitHub for updates
  owner         Modify bot owners
  help          Print this message or the help of the given subcommand(s)

Options:
  -h, --help
          Print help (see a summary with '-h')

  -V, --version
          Print version
```

## Owner Commands

As the bot owner, you have access to additional owner-only commands:

| Command                                     | Description                                                                                                                       |
| ------------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------- |
| `/init install_owner_commands`              | Installs bot-owner slash commands into this server                                                                                |
| `/init uninstall_owner_commands`            | Uninstalls bot-owner slash commands from this guild                                                                               |
| `/exit`                                     | Exit the bot. It will NOT restart automatically.                                                                                  |
| `/restart`                                  | Exit the bot. It will restart automatically if the bot is running from `./run.sh`                                                 |
| `/owner_stats`                              | Display bot-wide usage and performance statistics.                                                                                |
| `/set_test <True/False>`                    | Set/unset this server as a test server.                                                                                           |
| `/announce_test <message> [title]`          | Send an announcement to the log channels for all servers marked as test servers using the `/set_test` command.                    |
| `/announce <message> [title]`               | Send an announcement to the log channels for all servers                                                                          |
| `/verify_guild <guild_id> <guild_owner_id>` | Verify if the provided guild ID is owned by the provided user ID. Intended use is to verify guild ownership for support requests. |
