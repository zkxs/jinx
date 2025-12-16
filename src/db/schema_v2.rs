// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::db::{DB_V2_SCHEMA_VERSION_VALUE, SCHEMA_VERSION_KEY, helper};
use crate::error::JinxError;
use sqlx::{Executor, Pool, Sqlite};
use tokio::time::Instant;
use tracing::debug;

/// Set up the v2 database
pub(super) async fn init(pool: &Pool<Sqlite>) -> Result<(), JinxError> {
    let start = Instant::now();
    let mut connection = pool.acquire().await?;

    // simple key-value settings
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS "settings" (
                         key    TEXT PRIMARY KEY,
                         value  ANY NOT NULL
                     ) STRICT"#,
        )
        .await?;

    // guild information
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS guild (
                         guild_id               INTEGER PRIMARY KEY,
                         log_channel_id         INTEGER,
                         test                   INTEGER NOT NULL DEFAULT 0,
                         owner                  INTEGER NOT NULL DEFAULT 0,
                         gumroad_failure_count  INTEGER NOT NULL DEFAULT 0,
                         gumroad_nag_count      INTEGER NOT NULL DEFAULT 0,
                         blanket_role_id        INTEGER,
                         default_jinxxy_user    TEXT,
                         FOREIGN KEY            (default_jinxxy_user) REFERENCES jinxxy_user
                     ) STRICT"#,
        )
        .await?;

    // jinxxy store information
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS jinxxy_user (
                         jinxxy_user_id      TEXT PRIMARY KEY,
                         jinxxy_username     TEXT,
                         cache_time_unix_ms  INTEGER NOT NULL DEFAULT 0
                     ) STRICT"#,
        )
        .await?;

    // link between a store and a guild
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS jinxxy_user_guild (
                         guild_id              INTEGER NOT NULL,
                         jinxxy_user_id        TEXT NOT NULL,
                         jinxxy_api_key        TEXT NOT NULL,
                         jinxxy_api_key_valid  INTEGER NOT NULL DEFAULT TRUE,
                         PRIMARY KEY           (guild_id, jinxxy_user_id),
                         FOREIGN KEY           (guild_id)       REFERENCES guild,
                         FOREIGN KEY           (jinxxy_user_id) REFERENCES jinxxy_user
                     ) STRICT"#,
        )
        .await?;
    // guild -> stores lookup
    connection
        .execute(r#"CREATE INDEX IF NOT EXISTS store_lookup_by_guild ON jinxxy_user_guild (guild_id)"#)
        .await?;
    // store -> api_key lookup. This is needed to get an arbitrary API key for the cache warming job.
    connection
        .execute(r#"CREATE INDEX IF NOT EXISTS api_key_lookup_by_store ON jinxxy_user_guild (jinxxy_user_id) WHERE jinxxy_api_key_valid"#)
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
                         FOREIGN KEY     (jinxxy_user_id) REFERENCES jinxxy_user
                     ) STRICT"#,
        )
        .await?;
    // store -> products lookup
    connection
        .execute(r#"CREATE INDEX IF NOT EXISTS product_lookup_by_store ON product (jinxxy_user_id)"#)
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
                         FOREIGN KEY           (jinxxy_user_id) REFERENCES jinxxy_user
                     ) STRICT"#,
        )
        .await?;
    // store -> product_versions lookup
    connection
        .execute(r#"CREATE INDEX IF NOT EXISTS product_version_lookup_by_store ON product_version (jinxxy_user_id)"#)
        .await?;
    // product_id -> product_versions lookup
    connection
        .execute(r#"CREATE INDEX IF NOT EXISTS product_version_lookup_by_product ON product_version (jinxxy_user_id, product_id)"#)
        .await?;

    // role links for entire products (this includes any versions in the product as well!)
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS product_role (
                         guild_id               INTEGER NOT NULL,
                         jinxxy_user_id         TEXT NOT NULL,
                         product_id             TEXT NOT NULL,
                         role_id                INTEGER NOT NULL,
                         PRIMARY KEY            (guild_id, jinxxy_user_id, product_id, role_id),
                         FOREIGN KEY            (guild_id, jinxxy_user_id) REFERENCES jinxxy_user_guild
                     ) STRICT"#,
        )
        .await?;
    connection
        .execute(r#"CREATE INDEX IF NOT EXISTS role_lookup_by_product ON product_role (guild_id, jinxxy_user_id, product_id)"#)
        .await?;

    // product_version-specific role links
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS product_version_role (
                         guild_id               INTEGER NOT NULL,
                         jinxxy_user_id         TEXT NOT NULL,
                         product_id             TEXT NOT NULL,
                         version_id             TEXT NOT NULL,
                         role_id                INTEGER NOT NULL,
                         PRIMARY KEY            (guild_id, jinxxy_user_id, product_id, version_id, role_id),
                         FOREIGN KEY            (guild_id, jinxxy_user_id) REFERENCES jinxxy_user_guild
                     ) STRICT"#,
        )
        .await?;
    connection.execute(
        r#"CREATE INDEX IF NOT EXISTS role_lookup_by_product_version ON product_version_role (guild_id, jinxxy_user_id, product_id, version_id)"#
    ).await?;

    // local mirror of license activations. Source of truth is the Jinxxy API.
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS license_activation (
                         jinxxy_user_id         TEXT NOT NULL,
                         license_id             TEXT NOT NULL,
                         license_activation_id  TEXT NOT NULL,
                         user_id                INTEGER NOT NULL,
                         product_id             TEXT,
                         version_id             TEXT,
                         PRIMARY KEY            (jinxxy_user_id, license_id, license_activation_id, user_id),
                         FOREIGN KEY            (jinxxy_user_id) REFERENCES jinxxy_user
                     ) STRICT"#,
        )
        .await?;
    // index to look up all license_activation_id for a license
    connection.execute(r#"CREATE INDEX IF NOT EXISTS license_activation_lookup ON license_activation (jinxxy_user_id, license_id, user_id)"#).await?;

    // list of all discord users that are bot owners
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS "owner" (
                         owner_id               INTEGER PRIMARY KEY
                     ) STRICT"#,
        )
        .await?;

    let schema_version: i32 = helper::get_setting(&mut connection, SCHEMA_VERSION_KEY)
        .await?
        .unwrap_or(DB_V2_SCHEMA_VERSION_VALUE);

    // handle schema downgrade (or rather, DON'T handle it and throw an error)
    if schema_version > DB_V2_SCHEMA_VERSION_VALUE {
        let message = format!(
            "db schema version is v{schema_version}, which is newer than v{DB_V2_SCHEMA_VERSION_VALUE} which is the latest schema this Jinx build supports."
        );
        return Err(JinxError::new(message));
    }

    // Applications that use long-lived database connections should run "PRAGMA optimize=0x10002;" when the connection is first opened.
    // All applications should run "PRAGMA optimize;" after a schema change.
    connection.execute(r#"PRAGMA optimize = 0x10002"#).await?;

    // update the schema version value persisted to the DB
    helper::set_setting(&mut connection, SCHEMA_VERSION_KEY, DB_V2_SCHEMA_VERSION_VALUE).await?;

    let elapsed = start.elapsed();
    debug!(
        "initialized v2.{} db in {}ms",
        DB_V2_SCHEMA_VERSION_VALUE,
        elapsed.as_millis()
    );

    Ok(())
}

/// Copy all rows in the v1 db into the v2 db
#[allow(clippy::unused_async, unused_variables)] //TODO: remove
pub(super) async fn copy_from_v1(v1_pool: &Pool<Sqlite>, v2_pool: &Pool<Sqlite>) -> Result<(), JinxError> {
    todo!()
}
