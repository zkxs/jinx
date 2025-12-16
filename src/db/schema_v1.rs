// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::db::{DB_V1_SCHEMA_VERSION_VALUE, SCHEMA_VERSION_KEY, helper};
use crate::error::JinxError;
use sqlx::{Executor, Pool, Sqlite};
use tokio::time::Instant;
use tracing::debug;

/// Set up the database
pub(super) async fn init(pool: &Pool<Sqlite>) -> Result<(), JinxError> {
    let start = Instant::now();
    let mut connection = pool.acquire().await?;

    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS "settings" (
                         key                    TEXT PRIMARY KEY,
                         value                  ANY
                     ) STRICT"#,
        )
        .await?;

    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS guild (
                         guild_id               INTEGER PRIMARY KEY,
                         jinxxy_api_key         TEXT,
                         log_channel_id         INTEGER,
                         test                   INTEGER NOT NULL DEFAULT 0,
                         owner                  INTEGER NOT NULL DEFAULT 0,
                         gumroad_failure_count  INTEGER NOT NULL DEFAULT 0,
                         gumroad_nag_count      INTEGER NOT NULL DEFAULT 0,
                         cache_time_unix_ms     INTEGER NOT NULL DEFAULT 0,
                         blanket_role_id        INTEGER
                     ) STRICT"#,
        )
        .await?;

    // disk cache for product names
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS product (
                         guild_id               INTEGER NOT NULL,
                         product_id             TEXT NOT NULL,
                         product_name           TEXT NOT NULL,
                         etag                   BLOB,
                         PRIMARY KEY            (guild_id, product_id)
                     ) STRICT"#,
        )
        .await?;
    connection
        .execute(r#"CREATE INDEX IF NOT EXISTS product_lookup_by_guild ON product (guild_id)"#)
        .await?;

    // disk cache for product version names
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS product_version (
                         guild_id               INTEGER NOT NULL,
                         product_id             TEXT NOT NULL,
                         version_id             TEXT NOT NULL,
                         product_version_name   TEXT NOT NULL,
                         PRIMARY KEY            (guild_id, product_id, version_id)
                     ) STRICT"#,
        )
        .await?;
    connection
        .execute(r#"CREATE INDEX IF NOT EXISTS version_lookup_by_guild ON product_version (guild_id)"#)
        .await?;

    // this is the "blanket" roles for any version in a product
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS product_role (
                         guild_id               INTEGER NOT NULL,
                         product_id             TEXT NOT NULL,
                         role_id                INTEGER NOT NULL,
                         PRIMARY KEY            (guild_id, product_id, role_id)
                     ) STRICT"#,
        )
        .await?;
    connection
        .execute(r#"CREATE INDEX IF NOT EXISTS role_lookup ON product_role (guild_id, product_id)"#)
        .await?;

    // this is product-version specific role grants
    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS product_version_role (
                         guild_id               INTEGER NOT NULL,
                         product_id             TEXT NOT NULL,
                         version_id             TEXT NOT NULL,
                         role_id                INTEGER NOT NULL,
                         PRIMARY KEY            (guild_id, product_id, version_id, role_id)
                     ) STRICT"#,
        )
        .await?;
    connection.execute(r#"CREATE INDEX IF NOT EXISTS version_role_lookup ON product_version_role (guild_id, product_id, version_id)"#).await?;

    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS license_activation (
                         guild_id               INTEGER NOT NULL,
                         license_id             TEXT NOT NULL,
                         license_activation_id  TEXT NOT NULL,
                         user_id                INTEGER NOT NULL,
                         product_id             TEXT,
                         version_id             TEXT,
                         PRIMARY KEY            (guild_id, license_id, license_activation_id, user_id)
                     ) STRICT"#,
        )
        .await?;
    connection.execute(r#"CREATE INDEX IF NOT EXISTS license_activation_lookup ON license_activation (guild_id, license_id, user_id)"#).await?;

    connection
        .execute(
            r#"CREATE TABLE IF NOT EXISTS "owner" (
                         owner_id               INTEGER PRIMARY KEY
                     ) STRICT"#,
        )
        .await?;

    let schema_version: i32 = helper::get_setting(&mut connection, SCHEMA_VERSION_KEY)
        .await?
        .unwrap_or(DB_V1_SCHEMA_VERSION_VALUE);

    // handle schema downgrade (or rather, DON'T handle it and throw an error)
    if schema_version > DB_V1_SCHEMA_VERSION_VALUE {
        let message = format!(
            "db schema version is v{schema_version}, which is newer than v{DB_V1_SCHEMA_VERSION_VALUE} which is the latest schema this Jinx build supports."
        );
        return Err(JinxError::new(message));
    }

    // handle schema v1 -> v2 migration
    if schema_version < 2 {
        // "log_channel_id" column needs to be added to "guild"
        connection
            .execute(r#"ALTER TABLE guild ADD COLUMN log_channel_id INTEGER"#)
            .await?;
        // "test" column needs to be added to "guild"
        connection
            .execute(r#"ALTER TABLE guild ADD COLUMN test INTEGER NOT NULL DEFAULT 0"#)
            .await?;
    }

    // handle schema v2 -> v3 migration
    if schema_version < 3 {
        // "owner" column needs to be added to "guild"
        connection
            .execute(r#"ALTER TABLE guild ADD COLUMN owner INTEGER NOT NULL DEFAULT 0"#)
            .await?;
    }

    // handle schema v3 -> v4 migration
    if schema_version < 4 {
        // "guild.id" column needs to be renamed to "guild_id"
        connection
            .execute(r#"ALTER TABLE guild RENAME COLUMN id TO guild_id"#)
            .await?;
    }

    // handle schema v4 -> v5 migration
    if schema_version < 5 {
        // "gumroad_failure_count" and "gumroad_nag_count" columns need to be added to "guild"
        connection
            .execute(r#"ALTER TABLE guild ADD COLUMN gumroad_failure_count INTEGER NOT NULL DEFAULT 0"#)
            .await?;
        connection
            .execute(r#"ALTER TABLE guild ADD COLUMN gumroad_nag_count INTEGER NOT NULL DEFAULT 0"#)
            .await?;
    }

    // handle schema v5 -> v6 migration
    if schema_version < 6 {
        // "blanket_role_id" needs to be added to "guild"
        connection
            .execute(r#"ALTER TABLE guild ADD COLUMN blanket_role_id INTEGER"#)
            .await?;
    }

    // handle schema v6 -> v7 migration
    if schema_version < 7 {
        // "cache_time_unix_ms" needs to be added to "guild"
        connection
            .execute(r#"ALTER TABLE guild ADD COLUMN cache_time_unix_ms INTEGER NOT NULL DEFAULT 0"#)
            .await?;
    }

    // handle schema v7 -> v8 migration
    if schema_version < 8 {
        // "product_id" and "version_id" need to be added to "license_activation"
        connection
            .execute(r#"ALTER TABLE license_activation ADD COLUMN product_id TEXT"#)
            .await?;
        connection
            .execute(r#"ALTER TABLE license_activation ADD COLUMN version_id TEXT"#)
            .await?;
    }

    // handle schema v8 -> v9 migration
    if schema_version < 9 {
        // "etag"  needs to be added to "product"
        connection
            .execute(r#"ALTER TABLE product ADD COLUMN etag BLOB"#)
            .await?;
    }

    // update the schema version value persisted to the DB
    helper::set_setting(&mut connection, SCHEMA_VERSION_KEY, DB_V1_SCHEMA_VERSION_VALUE).await?;

    let elapsed = start.elapsed();
    debug!(
        "initialized v1.{} db in {}ms",
        DB_V1_SCHEMA_VERSION_VALUE,
        elapsed.as_millis()
    );

    Ok(())
}
