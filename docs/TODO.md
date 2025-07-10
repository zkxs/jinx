# Planned Features

## Do Before Each Release

- Make sure you didn't forget any license notices: `rg -g '*.rs' --files-without-match -F 'GNU AGPL v3.0'`
- Make sure you didn't introduce any lint warnings: `cargo clippy`
- validation (need Jinxxy API access first)
  - pagination
  - Jinxxy/GitHub Ratelimiting?
- bump project version

## Goals

- Caching? API gives etags... do they work for paginated responses?
  - if caching doesn't work out, then every time we receive activation data we should sync it back to the `license_activation` table
  - I've confirmed that provided `Cache-Control: max-age=0` is used instead of `Cache-Control: no-cache`, `ETag`/`If-None-Match` work as expected.
  - Etags are set by the API and even "work" on paginated responses, but each page has its own etag which is rather suspicious
  - Etags are probably worth not worth doing for anything paginated, because the lack of consistency or documented result ordering makes the whole
    response suspect even before layering on the added complexity of caching.
  - Etags are probably worth it for the individual `GET /products/<id>` requests I have to spam to keep my cache warm.
- Evaluate if a single thread can handle load or if this needs the full tokio multithreaded executor
- indices on the sqlite tables
- foreign keys on the sqlite tables
- clean up message formatting (reduce information overload of `/user_info` and `/license_info`)
- consistency pass on "verify", "register", "activate" language

## Stretch Goals

- ability to scan and revoke roles in a background job. License invalidation may be possible today via buyer-initiated refunds?
- admin features
  - ability to edit post (not a priority as you can just delete and recreate)
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
- Gumroad product transfer. Requires https://jinxxy.canny.io/feature-requests/p/give-creators-the-ability-to-manually-assign-licenses
- Giveaway command. Requires https://jinxxy.canny.io/feature-requests/p/ability-to-create-discount-codes-via-api

### Other Stores

While possible to add other stores, it would require some significant refactoring and DB schema changes so I need to
convince myself that it's worth it. Below is a list of various stores and what their API is like:

- API is viable
  - **gumroad** has [an API](https://help.gumroad.com/article/76-license-keys.html) that works fine. [Gumcord](https://github.com/benaclejames/GumCord) already uses it. I'm not interested in competing with Gumcord.
  - **payhip** has [an API](https://help.payhip.com/article/114-software-license-keys), and it looks mostly fine. The increase/decrease stuff is weirdly non-atomic, but oh well.
  - **itch.io** has [an API](https://itch.io/docs/api/serverside), but it expects a fragment of the download URL which is weird and will be difficult to
    explain to users.
- API is not viable
  - **Ko-fi** has basic Discord roles support, but its binary "is this person a supporter" and can't do per-product role
    grants. No REST API, but it also provides a webhook which in theory could work, but in practice would be very
    obnoxious to implement anything on. I'm not going to jump through obnoxious hoops if Ko-Fi already has their own
    Discord support, even if their support is mediocre.
- No API
  - **booth** has no API, and does not issue license keys.
