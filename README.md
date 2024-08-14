# Jinx

Jinx is a Discord bot that grants roles to users in your server when they register [Jinxxy](https://jinxxy.com/)
license keys.

<!-- For support, [open an issue][issues] or [join our Discord][discord].-->

> [!WARNING]
> Jinx is in a pre-release state and has only been partially validated against the Jinxxy API.
> You may experience bugs with this software: please report them [here][issues] or [in our Discord][discord].

> [!IMPORTANT]
> **[Click here to install the bot][bot install]**  
> <small>(and then go follow the [installation instructions](#installation))</small>

## User Experience

A user clicks the "Register" button:

![Registration Message](docs/register_message.png)

Next, the user is presented with a prompt to enter a license key:

![Registration Dialog](docs/register_modal.png)

Finally, if a valid license was provided then the user is granted any roles associated to their product. A confirmation
message is shown:

![Registration Success](docs/register_success.png)

## Installation

> [!IMPORTANT]
> **[Click here to install the bot][bot install]**  
> <small>(if you haven't already done so)</small>

When installing the bot, a "jinx" role will be automatically created in your server.
**You must ensure sure the "jinx" role is listed above any roles you want Jinx to manage.**
For example, in the screenshot below Jinx can only manage "test-secret-role" and "test-secret-role-2".

![Role Management UI](docs/manage_roles.png)

Next, go to [Jinxxy's API Keys page](https://jinxxy.com/my/dashboard/settings/api-keys) and create a new
API key with products_read, licenses_read, and licenses_write. Uncheck the expiration checkbox. Make note of the API
key when you create it: we'll need it shortly. The form should look like this:

![API Key creation](docs/create_api_key.png)

Finally, back in your Discord server run the following slash commands:

1. Run the `/init <api_key>` command in your Sever and provide your API key. This is one-time setup.
2. Run the `/link_product` command for each Jinxxy product you want to link to a role. You may have multiple products
   that grant the same role, and products can grant multiple rows.
3. Check your work using `/list_links`
4. When you're ready, run `/create_post` in the channel of your choosing to have Jinx create a button users can click to
   register license keys. You may create multiple posts this way. If you update your Jinxxy username or profile picture
   you may want to delete and recreate the post to update it.

I recommend testing everything with a test license. You can create a 100% discount code on an unlisted test product to
create test licenses.

### Self-hosting

You may also wish to self-host this bot. Instructions are provided, but the process is moderately technical.

<details>
<summary>Click to show advanced instructions</summary>

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

</details>

## Administrator Commands

Jinx comes with several slash commands for server administrators and moderators.

| Command                                | Required Permission | Description                                                                             |
|----------------------------------------|---------------------|-----------------------------------------------------------------------------------------|
| `/init [api_key]`                      | Manage Server       | Set up Jinx for this Discord server.                                                    |
| `/link_product`                        | Manage Roles        | Link a product to a role. Activating a license for the product will grant linked roles. |
| `/list_links`                          | Manage Roles        | List all product→role links.                                                            |
| `/create_post`                         | Manage Roles        | Create post with buttons to register product keys.                                      |
| `/user_info <user>`                    | Manage Server       | Query license information for a Discord user.                                           |
| `/license_info <license>`              | Manage Roles        | Query activation information for a license.                                             |
| `/lock_license <license>`              | Manage Roles        | Lock a license, preventing it from being used to grant roles.                           |
| `/unlock_license <license>`            | Manage Roles        | Unlock a license, allowing it to be used to grant roles.                                |
| `/deactivate_license <user> <license>` | Manage Roles        | Remove a user's activation of a license. This does not remove roles!                    |
| `/version`                             | None                | Shows version information about Jinx.                                                   |

> [!TIP]
> - The required permission/role for a command can be customized in the server's Integration settings.
> - `/user_info` can also be used from the context menu: look for "Apps"/"List Jinxxy licenses" when you right-click a
>   user in your server.
> - In the event that a Jinx update causes commands to become outdated, you can run `/init` again with no parameters to reinstall all
>   commands to your server.

## Permissions Used

### Jinxxy API Permissions

| Permission     | Explanation                                                   |
|----------------|---------------------------------------------------------------|
| products_read  | Used to list products so you can assign Discord roles to them |
| licenses_read  | Used to verify license keys                                   |
| licenses_write | Used to link a Discord user to a license key                  |

### Discord Bot Permissions

| Permission    | Explanation                                                        |
|---------------|--------------------------------------------------------------------|
| Manage Roles  | Used to assign users the role matching their license key's product |
| Send Messages | Used to send responses to some slash commands                      |

## License & Legal

jinx is free software: you can redistribute it and/or modify it under the terms of the
[GNU Affero General Public License](LICENSE) as published by the Free Software Foundation, either version 3 of the
License, or (at your option) any later version.

jinx is distributed in the hope that it will be useful, but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the [GNU Affero General Public License](LICENSE) for more
details.

A full list of dependencies is available in [Cargo.toml](Cargo.toml), or a breakdown of dependencies by license can be
generated with `cargo deny list`.

---

The [publicly installable bot][bot install] provided by us is available under our [Terms of Service](TERMS.md) and [Privacy Policy](PRIVACY.md).

[bot install]: https://discord.com/oauth2/authorize?client_id=1270708639145001052
[discord]: https://discord.gg/aKkA6m26f9
[issues]: https://github.com/zkxs/jinx/issues
