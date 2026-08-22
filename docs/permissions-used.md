# Permissions Used by Jinx

## Jinxxy API Permissions

| Permission           | Explanation                                                                                                                  |
| -------------------- | ---------------------------------------------------------------------------------------------------------------------------- |
| products_read        | Used to list products so you can assign Discord roles to them                                                                |
| licenses_read        | Used to verify license keys                                                                                                  |
| licenses_write       | Used to link a Discord user to a license key                                                                                 |
| discount_codes_write | Used for Gumroad→Jinxxy product transfer. You don't need to grant this if you don't use the bot's product transfer feature. |

## Discord Bot Permissions

| Permission               | Explanation                                                        |
| ------------------------ | ------------------------------------------------------------------ |
| Manage Roles             | Used to assign users the role matching their license key's product |
| Send Messages            | Used to send responses to some slash commands                      |
| Send Messages in Threads | Used to send responses to some slash commands                      |
