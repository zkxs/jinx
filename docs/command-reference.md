# Jinx Command Reference

> [!TIP]
> The required permission/role for a command can be customized in the server's Integration settings.

## Setup Commands

| Command                      | Required Permission | Description                                        |
| ---------------------------- | ------------------- | -------------------------------------------------- |
| `/init <api_key>`            | Manage Server       | Set up Jinx for this Discord server.               |
| `/set_log_channel [channel]` | Manage Server       | Set (or unset) channel for bot to log to.          |
| `/create_post`               | Manage Roles        | Create post with buttons to register product keys. |

## Role Management Commands

| Command                                            | Required Permission | Description                                                                                                                  |
| -------------------------------------------------- | ------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `/link_product <product> <role>`                   | Manage Roles        | Link a product to a role. Activating a license for any version of the product will grant the linked roles.                   |
| `/unlink_product <product> <role>`                 | Manage Roles        | Unlink product from roles.                                                                                                   |
| `/link_product_version <product_version> <role>`   | Manage Roles        | Link a product version to a role. Activating a license for that specific version of the product will grant the linked roles. |
| `/unlink_product_version <product_version> <role>` | Manage Roles        | Unlink a product version from a role.                                                                                        |
| `/set_wildcard_role <role>`                        | Manage Roles        | Set a wildcard role which will be granted for all products in your store.                                                    |
| `/unset_wildcard_role`                             | Manage Roles        | Unset the wildcard role.                                                                                                     |
| `/list_links`                                      | Manage Roles        | List all product→role links.                                                                                                 |
| `/grant_missing_roles [role]`                      | Manage Roles        | Grant a role to any users who have a license but are missing the linked role. Omit role parameter to run for all roles.      |

## License Management Commands

| Command                                | Required Permission | Description                                                                                                                          |
| -------------------------------------- | ------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `/user_info <user>`                    | Manage Server       | List all licenses linked to a Discord user.                                                                                          |
| `/license_info <license>`              | Manage Roles        | List activation information for a license.                                                                                           |
| `/lock_license <license>`              | Manage Roles        | Lock a license, preventing it from being used to grant roles.                                                                        |
| `/unlock_license <license>`            | Manage Roles        | Unlock a locked license, allowing it to be used to grant roles.                                                                      |
| `/deactivate_license <user> <license>` | Manage Roles        | Forget a user's activation of a license. This does not remove roles and allows a different Discord user to re-activate this license! |

> [!TIP]
> `/user_info` can also be used from the context menu: look for "Apps"/"List Jinxxy licenses" when you right-click a user in your server.

## Miscellaneous Commands

| Command    | Required Permission | Description                                                                              |
| ---------- | ------------------- | ---------------------------------------------------------------------------------------- |
| `/stats`   | Manage Server       | Display aggregate statistics on license activations, such as total number of activations |
| `/version` | None                | Shows version information about Jinx.                                                    |
| `/help`    | None                | Shows help information about Jinx.                                                       |
