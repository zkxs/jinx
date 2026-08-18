# Maintenance Notes

## Do Before Each Release

- Make sure you didn't forget any license notices: `rg -g '*.rs' --files-without-match -F 'GNU AGPL v3.0'`
- Make sure you didn't introduce any lint warnings: `cargo clippy`

## sqlx issues with cargo?

Usually no special action should be needed running cargo commands, as I ship cached SQL query analysis using
[sqlx's offline mode](https://github.com/transact-rs/sqlx/blob/main/sqlx-cli/README.md#enable-building-in-offline-mode-with-query).

By setting a `DATABASE_URL` in your shell to a valid jinx database, offline mode is not needed:

```shell
export DATABASE_URL=sqlite://jinx2.sqlite
```

If you want to build your own sqlx cache, you'll first need sqlx-cli:
```shell
cargo install sqlx-cli
```

Then prepare the cache:
```shell
DATABASE_URL=sqlite://jinx2.sqlite cargo sqlx prepare
```

## Updating Dependencies

The serenity/poise dependencies are difficult, as `cargo update` does not provide a way to ignore them and incorrectly
treats them as non-breaking. The following evil command will skip serenity and poise while updating everything else:

```shell
cargo update --dry-run |& rg '\->' | awk '{print $2"@"substr($3,2)}' | rg -wv 'serenity|poise|poise_macros' | xargs cargo update --verbose
```

## Updating Sqlite

sqlite dependency comes in via an unfortunately complex chain:
sqlx -> sqlx-sqlite -> libsqlite3-sys

Check this with `cargo tree --invert libsqlite3-sys`.

[libsqlite3-sys](https://crates.io/crates/libsqlite3-sys/versions) is a semi-internal part of rusqlite: https://github.com/rusqlite/rusqlite/tree/master/libsqlite3-sys

There is no documentation on which versions of libsqlite3-sys correspond to which versions of sqlite.
The only way to find this is to examine `/sqlite3/bindgen_bundled_version.rs` in some version of
libsqlite3-sys and look at the `SQLITE_VERSION` const. For example, for libsqlite3-sys 0.35.0:
https://crates.io/crates/libsqlite3-sys/0.35.0/code/sqlite3/bindgen_bundled_version.rs

I have manually collected the mapping for selected versions:

| libsqlite3-sys | sqlite |
|----------------|--------|
| 0.34.0         | 3.49.2 |
| 0.35.0         | 3.50.2 |
| 0.36.0         | 3.51.1 |
| 0.37.0         | 3.51.3 |
| 0.38.0         | 3.53.1 |
| 0.38.1         | 3.53.2 |
| 0.38.2         | 3.53.2 |

Relevant pages with information on sqlite versions:
- https://sqlite.org/changes.html
- https://sqlite.org/chronology.html
- https://sqlite.org/news.html

To patch jinx for sqlite bugs:
1. Go to your https://github.com/zkxs/sqlx checkout
2. `git checkout main`
3. `git pull`
4. `git push origin`
4. `git branch libsqlite3-0.38-3.53`
5. `git checkout libsqlite3-0.38-3.53`
6. Update Cargo.toml to reference libsqlite3 0.38
6. `cargo test -p sqlx-sqlite --features bundled,deserialize,load-extension,unlock-notify`
7. `cargo test -p sqlx --lib --features macros,sqlite,runtime-tokio`
8. `git push`
9. Go back to your https://github.com/zkxs/jinx checkout
10. Update Cargo.toml to reference the new sqlx branch

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

A guild is considered to be **stale** if the bot is no longer in it but Jinx still has references to the guild in its
DB. This cannot happen unless the bot misses a GuildDelete event.

Stale guilds are always pending deletion, but it is ambiguous if the guild is not joined because it is temporarily
unavailable or because the bot has actually been removed. Due to this ambiguity, stale guild deletion is not performed
automatically: instead the `/delete_stale_guilds` must be manually ran to perform this cleanup.

Note that a stale guild has never been observed in production since GuildDelete event monitoring was implemented.

## Invalid API Keys

Jinxxy API keys are added at the guild level. These API keys are marked as **invalid** if they return a 401 or a 403
during use in the background cache warming job. This invalid bit prevents the API key from being tried again in this
job. The high-priority cache flow both ignores the invalid bit and clears it if a request succeeds.

## Registered vs Activated

These terms have nearly the same meaning. I prefer using **registered** in text shown to the activating user, and
**activated** everywhere else.
