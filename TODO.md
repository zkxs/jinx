# Planned Features

## Do Before Release

- Make sure you didn't forget any license notices: `rg -g '*.rs' --files-without-match -F 'GNU AGPL v3.0'`
- Make sure you didn't introduce any lint warnings: `cargo clippy`
- validation (need Jinxxy API access first)
  - pagination
  - Jinxxy/GitHub Ratelimiting?
- bump project version
- indices on the sqlite tables
- add a `/help` command

## Goals

- Caching? API gives etags... do they work for paginated responses?
  - if caching doesn't work out, then every time we receive activation data we should sync it back to the `license_activation` table
- ability to scan and revoke roles in a background job
- Evaluate if a single thread can handle load or if this needs the full tokio multithreaded executor

## Stretch Goals

- admin features
  - ability to edit post (not a priority as you can just delete and recreate)
  - bot log to channel
  - recover from lost DB
    - some kind of `/rebuild_database` command to enumerate all licenses and activations to rebuild the `license_activation` table
    - admin will need to manually rerun `/init`
    - admin will need to manually re-link products and roles
  - source other fields for the info commands. This would require figuring out the `search_query` parameter
    - customers API
      - loyalty_amount
    - orders API
      - email
      - payment
        - time
        - status
        - total
- owner features
  - ability to ban bot users (not a priority until someone causes problems)
    - by guild ID
    - by Jinxxy ID (username and name can both change)
    - by user ID (we don't actually record this for creators or unsuccessful license registrations)
