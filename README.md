# Jinx

Jinx is a free, open source Discord bot that grants roles to users in your server when they register [Jinxxy](https://jinxxy.com/)
license keys. Note that Jinx is not affiliated with Jinxxy: this is an independent project.

> [!IMPORTANT]
> **[Click here to add the bot to your server][bot install]**  
> <small>(and then go follow the [installation instructions](#installation))</small>

Features include:

- Licenses can only be activated by a single Discord user.
  - The server owner can deactivate a license. This may be useful if a buyer needs to change Discord accounts and wants to move the license over.
  - Jinx can log license activations and suspicious activity to a Discord channel.
  - Ability to lock a license, preventing it from being used to grant roles in the future.
- Flexible role configuration
  - Products can grant more than one role at a time. For example, you might have a shared role that all buyers get and then additional per-product roles.
  - For extremely simple setup, you can set a wildcard role granted for all of your products.
  - For fine-grained setup, you can configure different role grants for different versions of the same products.
  - No limit on number of linked products or roles. (Some similar bots support a maximum of 25 products due to a certain Discord limitation).

If you have suggestions, feedback, or bug reports please let us know [here on GitHub][issues] or [in our Discord][discord].

## User Experience

A user clicks the "Register" button:

![Registration Message](docs/images/register_message.png)

Next, the user is presented with a prompt to enter a license key:

![Registration Dialog](docs/images/register_modal.png)

Finally, if a valid license was provided then the user is granted any roles associated to their product. A confirmation
message is shown:

![Registration Success](docs/images/register_success.png)

## Installation

> [!IMPORTANT]
> **[Click here to add the bot to your server][bot install]**  
> <small>(if you haven't already done so)</small>

When installing the bot, a "jinx" role will be automatically created in your server.
**You must ensure sure the "jinx" role is listed above any roles you want Jinx to manage.**
For example, in the screenshot below Jinx can only manage "test-secret-role" and "test-secret-role-2".

![Role Management UI](docs/images/manage_roles.png)

Next, go to [Jinxxy's API Keys page](https://jinxxy.com/my/dashboard/settings/api-keys) and create a new
API key with products_read, licenses_read, and licenses_write (see
[explanation of permissions](docs/permissions-used.md) to learn why we need these). Uncheck the expiration checkbox.
Make note of the API key when you create it: we'll need it shortly. The form should look like this:

![API Key creation](docs/images/create_api_key.png)

Finally, back in your Discord server run the following slash commands:

1. Run the `/init <api_key>` command in your Sever and provide your API key. This is one-time setup.
2. Optionally, run `/set_log_channel [channel]` to tell the bot which channel to log events (such as license activations)
   to. I recommend you make this channel private so only you and your trusted moderators can see it. You will probably
   need to grant Jinx permission to send messages to this channel.
3. Run the `/link_product` command for each Jinxxy product you want to link to a role. You do not need a distinct role for
   each product. Any product can grant any role, or even multiple roles! If you make a mistake, use `/unlink_product` to fix it.
   For even more ways to link products to roles, check out the rest of the [role management commands](docs/command-reference.md#role-management-commands).
4. Check your work using `/list_links`
5. When you're ready, run `/create_post` in the channel of your choosing to have Jinx create a button users can click to
   register license keys. You may create multiple posts this way. If you update your Jinxxy username or profile picture
   you may want to delete and recreate the post, as it will not automatically update.

I recommend testing everything with a test license. You can create a 100% discount code or create an unlisted free
product to create test license keys.

### Self-hosting

You may also wish to self-host this bot. [Self-hosting instructions](docs/self-hosting.md) are provided, but the process
is moderately technical.

## Administrator Commands

Jinx features a variety of slash commands for server administrators and moderators. See the
[command reference](docs/command-reference.md) for a full list.

## Frequently Asked Questions

See the [FAQ page](docs/faq.md).

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
[app directory]: https://discord.com/application-directory/1270708639145001052
