// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::db::helper;
use crate::error::JinxError;
use poise::futures_util::TryStreamExt;
use sqlx::{Executor, Row, SqliteConnection};
use std::collections::HashMap;
use tokio::time::Instant;
use tracing::debug;

const SCHEMA_MINOR_VERSION_KEY: &str = "schema_minor_version";
const SCHEMA_PATCH_VERSION_KEY: &str = "schema_patch_version";
/// Increment this if there is a backwards-compatibility breaking schema change, such as deleting a column
const SCHEMA_MINOR_VERSION_VALUE: i32 = 0;
/// Increment this if there is a backwards-compatible change, such as adding a new column
const SCHEMA_PATCH_VERSION_VALUE: i32 = 0;

/// Set up the v2 database
pub(super) async fn init(connection: &mut SqliteConnection) -> Result<(), JinxError> {
    let start = Instant::now();

    // simple key-value settings
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS "settings" (
                   key    TEXT NOT NULL PRIMARY KEY,
                   value  ANY NOT NULL
               ) STRICT, WITHOUT ROWID"#,
        )
        .await?;

    // guild information
    // we intentionally use rowid as we have an integer pk
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS guild (
                   guild_id               INTEGER NOT NULL PRIMARY KEY,
                   log_channel_id         INTEGER,
                   test                   INTEGER NOT NULL DEFAULT 0,
                   owner                  INTEGER NOT NULL DEFAULT 0,
                   gumroad_failure_count  INTEGER NOT NULL DEFAULT 0,
                   gumroad_nag_count      INTEGER NOT NULL DEFAULT 0,
                   blanket_role_id        INTEGER,
                   default_jinxxy_user    TEXT,
                   FOREIGN KEY            (default_jinxxy_user) REFERENCES jinxxy_user ON DELETE CASCADE
               ) STRICT"#,
        )
        .await?;

    // jinxxy store information
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS jinxxy_user (
                   jinxxy_user_id      TEXT NOT NULL PRIMARY KEY,
                   jinxxy_username     TEXT,
                   cache_time_unix_ms  INTEGER NOT NULL DEFAULT 0
               ) STRICT, WITHOUT ROWID"#,
        )
        .await?;

    // link between a store and a guild
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS jinxxy_user_guild (
                   jinxxy_user_id        TEXT NOT NULL,
                   guild_id              INTEGER NOT NULL,
                   jinxxy_api_key        TEXT NOT NULL,
                   jinxxy_api_key_valid  INTEGER NOT NULL DEFAULT TRUE,
                   PRIMARY KEY           (jinxxy_user_id, guild_id),
                   FOREIGN KEY           (guild_id)       REFERENCES guild ON DELETE CASCADE,
                   FOREIGN KEY           (jinxxy_user_id) REFERENCES jinxxy_user ON DELETE CASCADE
               ) STRICT, WITHOUT ROWID"#,
        )
        .await?;
    // Index needed to look up all links by guild
    connection
        .execute(r#"CREATE INDEX IF NOT EXISTS api_key_lookup_by_guild ON jinxxy_user_guild (guild_id)"#)
        .await?;
    // Index needed to look up directly by api key for setting its validity
    connection
        .execute(r#"CREATE INDEX IF NOT EXISTS api_key_lookup ON jinxxy_user_guild (jinxxy_api_key)"#)
        .await?;

    // disk cache for product names
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS product (
                   jinxxy_user_id  TEXT NOT NULL,
                   product_id      TEXT NOT NULL,
                   product_name    TEXT NOT NULL,
                   etag            BLOB,
                   PRIMARY KEY     (jinxxy_user_id, product_id),
                   FOREIGN KEY     (jinxxy_user_id) REFERENCES jinxxy_user ON DELETE CASCADE
               ) STRICT, WITHOUT ROWID"#,
        )
        .await?;

    // disk cache for product version names
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS product_version (
                   jinxxy_user_id        TEXT NOT NULL,
                   product_id            TEXT NOT NULL,
                   version_id            TEXT NOT NULL,
                   product_version_name  TEXT NOT NULL,
                   PRIMARY KEY           (jinxxy_user_id, product_id, version_id),
                   FOREIGN KEY           (jinxxy_user_id) REFERENCES jinxxy_user ON DELETE CASCADE
               ) STRICT, WITHOUT ROWID"#,
        )
        .await?;

    // role links for entire products (this includes any versions in the product as well!)
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS product_role (
                   jinxxy_user_id         TEXT NOT NULL,
                   guild_id               INTEGER NOT NULL,
                   product_id             TEXT NOT NULL,
                   role_id                INTEGER NOT NULL,
                   PRIMARY KEY            (jinxxy_user_id, guild_id, product_id, role_id),
                   FOREIGN KEY            (jinxxy_user_id, guild_id) REFERENCES jinxxy_user_guild ON DELETE CASCADE
               ) STRICT, WITHOUT ROWID"#,
        )
        .await?;
    // for joining by guild
    connection
        .execute(r#"CREATE INDEX IF NOT EXISTS product_role_lookup_by_guild ON product_role (guild_id)"#)
        .await?;

    // product_version-specific role links
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS product_version_role (
                   jinxxy_user_id         TEXT NOT NULL,
                   guild_id               INTEGER NOT NULL,
                   product_id             TEXT NOT NULL,
                   version_id             TEXT NOT NULL,
                   role_id                INTEGER NOT NULL,
                   PRIMARY KEY            (jinxxy_user_id, guild_id, product_id, version_id, role_id),
                   FOREIGN KEY            (jinxxy_user_id, guild_id) REFERENCES jinxxy_user_guild ON DELETE CASCADE
               ) STRICT, WITHOUT ROWID"#,
        )
        .await?;
    // for joining by guild
    connection
        .execute(
            r#"CREATE INDEX IF NOT EXISTS product_version_role_lookup_by_guild ON product_version_role (guild_id)"#,
        )
        .await?;

    // local mirror of license activations. Source of truth is the Jinxxy API.
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS license_activation (
                   jinxxy_user_id         TEXT NOT NULL,
                   license_id             TEXT NOT NULL,
                   activator_user_id      INTEGER NOT NULL,
                   license_activation_id  TEXT NOT NULL,
                   product_id             TEXT NOT NULL,
                   version_id             TEXT,
                   PRIMARY KEY            (jinxxy_user_id, license_id, activator_user_id, license_activation_id),
                   FOREIGN KEY            (jinxxy_user_id) REFERENCES jinxxy_user ON DELETE CASCADE
               ) STRICT, WITHOUT ROWID"#,
        )
        .await?;
    // for searching for activations given an activator user ID
    connection
        .execute(
            r#"CREATE INDEX IF NOT EXISTS activation_lookup_by_activator ON license_activation (activator_user_id)"#,
        )
        .await?;

    // list of all discord users that are bot owners
    // we intentionally use rowid as we have an integer pk
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS "owner" (
                   owner_id               INTEGER NOT NULL PRIMARY KEY
               ) STRICT"#,
        )
        .await?;

    let schema_minor_version: i32 = helper::get_setting(connection, SCHEMA_MINOR_VERSION_KEY)
        .await?
        .unwrap_or(SCHEMA_MINOR_VERSION_VALUE);
    let schema_patch_version: i32 = helper::get_setting(connection, SCHEMA_PATCH_VERSION_KEY)
        .await?
        .unwrap_or(SCHEMA_PATCH_VERSION_VALUE);

    // handle schema downgrade (or rather, DON'T handle it and throw an error)
    if schema_minor_version > SCHEMA_MINOR_VERSION_VALUE {
        let message = format!(
            "db schema version is v2.{schema_minor_version}.{schema_patch_version}, which is newer than v2.{SCHEMA_MINOR_VERSION_VALUE} which is the latest schema this Jinx build supports."
        );
        return Err(JinxError::new(message));
    }

    // Applications that use long-lived database connections should run "PRAGMA optimize=0x10002;" when the connection is first opened.
    // All applications should run "PRAGMA optimize;" after a schema change.
    connection.execute(r#"PRAGMA optimize = 0x10002"#).await?;

    // update the schema version value persisted to the DB
    helper::set_setting(connection, SCHEMA_MINOR_VERSION_KEY, SCHEMA_MINOR_VERSION_VALUE).await?;
    helper::set_setting(connection, SCHEMA_PATCH_VERSION_KEY, SCHEMA_PATCH_VERSION_VALUE).await?;

    let elapsed = start.elapsed();
    debug!(
        "initialized v2.{}.{} db in {}ms",
        SCHEMA_MINOR_VERSION_VALUE,
        SCHEMA_PATCH_VERSION_VALUE,
        elapsed.as_millis()
    );

    Ok(())
}

/// Copy all rows in the v1 db into the v2 db
pub(super) async fn copy_from_v1(
    v1_connection: &mut SqliteConnection,
    v2_connection: &mut SqliteConnection,
) -> Result<(), JinxError> {
    // settings migration
    {
        debug!("starting settings migration");
        let discord_token: Option<String> =
            sqlx::query_scalar(r#"SELECT value FROM settings WHERE key = 'discord_token'"#)
                .fetch_optional(&mut *v1_connection)
                .await?;
        if let Some(discord_token) = discord_token {
            sqlx::query!(
                r#"INSERT INTO settings (key, value) VALUES ('discord_token', ?)"#,
                discord_token
            )
            .execute(&mut *v2_connection)
            .await?;
        }

        let low_priority_cache_expiry_seconds: Option<i64> =
            sqlx::query_scalar(r#"SELECT value FROM settings WHERE key = 'low_priority_cache_expiry_seconds'"#)
                .fetch_optional(&mut *v1_connection)
                .await?;
        if let Some(low_priority_cache_expiry_seconds) = low_priority_cache_expiry_seconds {
            sqlx::query!(
                r#"INSERT INTO settings (key, value) VALUES ('low_priority_cache_expiry_seconds', ?)"#,
                low_priority_cache_expiry_seconds
            )
            .execute(&mut *v2_connection)
            .await?;
        }
    }

    // all the old tables are guild-based so we'll need to be able to map them to the correct store
    let mut guild_to_store_map: HashMap<i64, String, ahash::RandomState> = Default::default();

    // guild migration
    {
        debug!("starting guild migration");
        let mut rows = sqlx::query(
            r#"SELECT guild_id, jinxxy_api_key, log_channel_id, test, owner, gumroad_failure_count, gumroad_nag_count,
                   cache_time_unix_ms, blanket_role_id, jinxxy_user_id, jinxxy_username FROM guild"#,
        )
        .fetch(&mut *v1_connection);
        while let Some(row) = rows.try_next().await? {
            let guild_id: i64 = row.get("guild_id");
            let jinxxy_api_key: Option<&str> = row.get("jinxxy_api_key");
            let log_channel_id: Option<i64> = row.get("log_channel_id");
            let test: i64 = row.get("test");
            let owner: i64 = row.get("owner");
            let gumroad_failure_count: i64 = row.get("gumroad_failure_count");
            let gumroad_nag_count: i64 = row.get("gumroad_nag_count");
            let cache_time_unix_ms: i64 = row.get("cache_time_unix_ms");
            let blanket_role_id: Option<i64> = row.get("blanket_role_id");
            let jinxxy_user_id: Option<&str> = row.get("jinxxy_user_id");
            let jinxxy_username: Option<&str> = row.get("jinxxy_username");

            let jinxxy_api_key = jinxxy_api_key.expect("Expected jinxxy_api_key to be non-null for all guilds");
            let jinxxy_user_id = jinxxy_user_id.expect("Expected jinxxy_user_id to be non-null for all guilds");

            // that's a surprise tool that can help us later
            guild_to_store_map.insert(guild_id, jinxxy_user_id.to_owned());

            // Some of the data is separated into the new jinxxy_user table, and must therefore be deduplicated.
            // This must be done first for foreign key constraint reasons.
            sqlx::query!(
                r#"INSERT INTO jinxxy_user (jinxxy_user_id, jinxxy_username, cache_time_unix_ms)
                   VALUES (?, ?, ?)
                   ON CONFLICT (jinxxy_user_id) DO UPDATE
                   SET cache_time_unix_ms = max(excluded.cache_time_unix_ms, cache_time_unix_ms),
                   jinxxy_username = ifnull(excluded.jinxxy_username, jinxxy_username)"#,
                jinxxy_user_id,
                jinxxy_username,
                cache_time_unix_ms
            )
            .execute(&mut *v2_connection)
            .await?;

            // most of the data goes into the new guild table
            sqlx::query!(
                r#"INSERT INTO guild (guild_id, log_channel_id, test, owner, gumroad_failure_count, gumroad_nag_count, blanket_role_id, default_jinxxy_user)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?)"#,
                guild_id,
                log_channel_id,
                test,
                owner,
                gumroad_failure_count,
                gumroad_nag_count,
                blanket_role_id,
                jinxxy_user_id,
            )
            .execute(&mut *v2_connection)
            .await?;

            // finally, the api key goes into jinxxy_user_guild. This must be done last for foreign key constraint reasons.
            sqlx::query!(
                r#"INSERT INTO jinxxy_user_guild (jinxxy_user_id, guild_id, jinxxy_api_key)
                   VALUES (?, ?, ?)"#,
                jinxxy_user_id,
                guild_id,
                jinxxy_api_key
            )
            .execute(&mut *v2_connection)
            .await?;
        }
    }

    // product migration
    {
        debug!("starting product migration");
        let mut rows =
            sqlx::query(r#"SELECT guild_id, product_id, product_name, etag FROM product"#).fetch(&mut *v1_connection);
        while let Some(row) = rows.try_next().await? {
            let guild_id: i64 = row.get("guild_id");
            let product_id: &str = row.get("product_id");
            let product_name: &str = row.get("product_name");
            let etag: Option<&[u8]> = row.get("etag");
            let jinxxy_user_id: &str = guild_to_store_map
                .get(&guild_id)
                .expect("jinxxy_user_id not found for guild");

            sqlx::query!(
                r#"INSERT INTO product (jinxxy_user_id, product_id, product_name, etag)
                   VALUES (?, ?, ?, ?)
                   ON CONFLICT (jinxxy_user_id, product_id) DO NOTHING"#,
                jinxxy_user_id,
                product_id,
                product_name,
                etag,
            )
            .execute(&mut *v2_connection)
            .await?;
        }
    }

    // product_version migration
    {
        debug!("starting product_version migration");
        let mut rows =
            sqlx::query(r#"SELECT guild_id, product_id, version_id, product_version_name FROM product_version"#)
                .fetch(&mut *v1_connection);
        while let Some(row) = rows.try_next().await? {
            let guild_id: i64 = row.get("guild_id");
            let product_id: &str = row.get("product_id");
            let version_id: &str = row.get("version_id");
            let product_version_name: &str = row.get("product_version_name");
            let jinxxy_user_id: &str = guild_to_store_map
                .get(&guild_id)
                .expect("jinxxy_user_id not found for guild");

            sqlx::query!(
                r#"INSERT INTO product_version (jinxxy_user_id, product_id, version_id, product_version_name)
                   VALUES (?, ?, ?, ?)
                   ON CONFLICT (jinxxy_user_id, product_id, version_id) DO NOTHING"#,
                jinxxy_user_id,
                product_id,
                version_id,
                product_version_name,
            )
            .execute(&mut *v2_connection)
            .await?;
        }
    }

    // product_role migration
    {
        debug!("starting product_role migration");
        let mut rows =
            sqlx::query(r#"SELECT guild_id, product_id, role_id FROM product_role"#).fetch(&mut *v1_connection);
        while let Some(row) = rows.try_next().await? {
            let guild_id: i64 = row.get("guild_id");
            let product_id: &str = row.get("product_id");
            let role_id: i64 = row.get("role_id");
            let jinxxy_user_id: &str = guild_to_store_map
                .get(&guild_id)
                .expect("jinxxy_user_id not found for guild");

            sqlx::query!(
                r#"INSERT INTO product_role (jinxxy_user_id, guild_id, product_id, role_id)
                   VALUES (?, ?, ?, ?)
                   ON CONFLICT (jinxxy_user_id, guild_id, product_id, role_id) DO NOTHING"#,
                jinxxy_user_id,
                guild_id,
                product_id,
                role_id,
            )
            .execute(&mut *v2_connection)
            .await?;
        }
    }

    // product_version_role migration
    {
        debug!("starting product_version_role migration");
        let mut rows = sqlx::query(r#"SELECT guild_id, product_id, version_id, role_id FROM product_version_role"#)
            .fetch(&mut *v1_connection);
        while let Some(row) = rows.try_next().await? {
            let guild_id: i64 = row.get("guild_id");
            let product_id: &str = row.get("product_id");
            let version_id: &str = row.get("version_id");
            let role_id: i64 = row.get("role_id");
            let jinxxy_user_id: &str = guild_to_store_map
                .get(&guild_id)
                .expect("jinxxy_user_id not found for guild");

            sqlx::query!(
                r#"INSERT INTO product_version_role (jinxxy_user_id, guild_id, product_id, version_id, role_id)
                   VALUES (?, ?, ?, ?, ?)
                   ON CONFLICT (jinxxy_user_id, guild_id, product_id, version_id, role_id) DO NOTHING"#,
                jinxxy_user_id,
                guild_id,
                product_id,
                version_id,
                role_id,
            )
            .execute(&mut *v2_connection)
            .await?;
        }
    }

    // license_activation migration
    {
        debug!("starting license_activation migration");
        let mut rows = sqlx::query(
            r#"SELECT guild_id, license_id, license_activation_id, user_id, product_id, version_id FROM license_activation"#
        )
            .fetch(&mut *v1_connection);
        while let Some(row) = rows.try_next().await? {
            let guild_id: i64 = row.get("guild_id");
            let license_id: &str = row.get("license_id");
            let license_activation_id: &str = row.get("license_activation_id");
            let activator_user_id: i64 = row.get("user_id");
            let product_id: Option<&str> = row.get("product_id");
            let version_id: Option<&str> = row.get("version_id");
            let jinxxy_user_id: &str = guild_to_store_map
                .get(&guild_id)
                .expect("jinxxy_user_id not found for guild");
            let product_id = product_id.expect("expected product_id to be non-null for all license_activation");
            let version_id = match version_id {
                Some("") => None, // replace empty version ids with null, as the meaning is semantically equivalent but empty string is more obnoxious
                Some(s) => Some(s),
                None => None,
            };

            sqlx::query!(
                r#"INSERT INTO license_activation (jinxxy_user_id, license_id, activator_user_id, license_activation_id, product_id, version_id)
                   VALUES (?, ?, ?, ?, ?, ?)
                   ON CONFLICT (jinxxy_user_id, license_id, activator_user_id, license_activation_id) DO UPDATE
                   SET product_id = ifnull(excluded.product_id, product_id),
                   version_id = ifnull(excluded.version_id, version_id)"#,
                jinxxy_user_id,
                license_id,
                activator_user_id,
                license_activation_id,
                product_id,
                version_id,
            )
            .execute(&mut *v2_connection)
            .await?;
        }
    }

    // owner migration
    {
        debug!("starting owner migration");
        let mut rows = sqlx::query_scalar(r#"SELECT owner_id FROM owner"#).fetch(&mut *v1_connection);
        while let Some(row) = rows.try_next().await? {
            let owner_id: i64 = row;
            sqlx::query!(r#"INSERT INTO owner (owner_id) VALUES (?)"#, owner_id)
                .execute(&mut *v2_connection)
                .await?;
        }
    }

    Ok(())
}
