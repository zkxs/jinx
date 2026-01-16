# Maintenance Notes

## Do Before Each Release

- Make sure you didn't forget any license notices: `rg -g '*.rs' --files-without-match -F 'GNU AGPL v3.0'`
- Make sure you didn't introduce any lint warnings: `cargo clippy`

# How Jinx Works

## License Activation

1. users provide license key, which I look up and get id from using `GET /licenses?short_key=foo` or `GET /licenses?key=foo`. I don't specify limit here, because I only expect to see 0 or 1 result
2. I call `GET /licenses/<id>` to get additional information, including the total activation count. A 200 response here indicates the license is valid.
3. If there are nonzero activations I call `GET /licenses/<id>/activations` to check the activation descriptions against the ones I create
4. If there are no conflicting activations, I call `POST /licenses/<id>/activations` to create the activation
5. I then call `GET /licenses/<id>/activations` _again_ to detect if a race condition occurred and two distinct users managed to do step 4 concurrently

## Product Cache

There's also a background job that periodically enumerates all product and product-version names for stores linked to
Jinx. I need those locally because I use them for text autocompletion, which needs to be as low-latency as possible.
This job calls `GET /products` for every store about once every 24h and caches the results in the local DB. This cache
is a bit unusual in that it does not expire entries ever. It will queue a priority cache warm if it notices a user is
actively using the cache for a store and the cache is more than 60s old.

# Vocab and Concepts

## Guilds

Internally, Discord calls a server a "guild". I use this term anywhere non-user facing, because the word "server"
is very ambiguous and "Discord server" is a lot to type.

## Stale Guilds

A guild is considered to be **stale** if it the bot is no longer in it but Jinx still has references to the guild in its
DB. Stale guilds are always pending deletion, but due to a couple scary ambiguities in Serenity where it's not clear if
a guild has been permanently left or is only temporarily unavailable this deletion is not always performed
automatically.

## Invalid API Keys

Jinxxy API keys are added at the guild level. These API keys are marked as **invalid** if they return a 401 or a 403
during use in the background cache warming job. This invalid bit prevents the API key from being tried again in this
job. The high-priority cache flow both ignores the invalid bit and clears it if a request succeeds.

## Registered vs Activated

These terms have nearly the same meaning. I prefer using **registered** in text shown to the activating user, and
**activated** everywhere else.
