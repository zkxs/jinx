// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::error::JinxError;
use crate::http::jinxxy;
use crate::http::jinxxy::{ProductNameInfo, ProductNameInfoValue, ProductVersionId, ProductVersionNameInfo};
use crate::time::SimpleTime;
use poise::serenity_prelude::{ChannelId, GuildId, RoleId, UserId};
use rusqlite::types::{FromSql, ToSql};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;
use tokio::time::Instant;
use tokio_rusqlite::{Connection, OptionalExtension, Result as SqliteResult, named_params};
use tracing::debug;

const SCHEMA_VERSION_KEY: &str = "schema_version";
const SCHEMA_VERSION_VALUE: i32 = 9;
const DISCORD_TOKEN_KEY: &str = "discord_token";
const LOW_PRIORITY_CACHE_EXPIRY_SECONDS: &str = "low_priority_cache_expiry_seconds";

type Error = Box<dyn std::error::Error + Send + Sync>;

pub struct JinxDb {
    connection: Connection,
    api_key_cache: papaya::HashMap<GuildId, Option<String>, ahash::RandomState>,
}

impl Drop for JinxDb {
    fn drop(&mut self) {
        debug!("Closing sqlite db…");
    }
}

impl JinxDb {
    /// Open a new database
    pub async fn open() -> SqliteResult<Self> {
        Self::open_path("jinx.sqlite").await
    }

    /// Open a new database
    async fn open_path<P: AsRef<Path>>(path: P) -> SqliteResult<Self> {
        let connection = Connection::open(path).await?;
        JinxDb::init(&connection).await?;
        let db = JinxDb {
            connection,
            api_key_cache: Default::default(),
        };
        Ok(db)
    }

    /// Set up the database
    async fn init(connection: &Connection) -> SqliteResult<()> {
        let start = Instant::now();
        connection
            .call(|connection| {
                // all applications are encouraged to switch this setting off on every database connection as soon as that connection is opened
                connection.execute("PRAGMA trusted_schema = OFF;", ())?;

                connection.execute(
                    "CREATE TABLE IF NOT EXISTS \"settings\" ( \
                             key                    TEXT PRIMARY KEY, \
                             value                  ANY \
                         ) STRICT",
                    (),
                )?;

                connection.execute(
                    "CREATE TABLE IF NOT EXISTS guild ( \
                             guild_id               INTEGER PRIMARY KEY, \
                             jinxxy_api_key         TEXT, \
                             log_channel_id         INTEGER, \
                             test                   INTEGER NOT NULL DEFAULT 0, \
                             owner                  INTEGER NOT NULL DEFAULT 0, \
                             gumroad_failure_count  INTEGER NOT NULL DEFAULT 0, \
                             gumroad_nag_count      INTEGER NOT NULL DEFAULT 0, \
                             cache_time_unix_ms     INTEGER NOT NULL DEFAULT 0, \
                             blanket_role_id        INTEGER\
                         ) STRICT",
                    (),
                )?;

                // disk cache for product names
                connection.execute(
                    "CREATE TABLE IF NOT EXISTS product ( \
                             guild_id               INTEGER NOT NULL, \
                             product_id             TEXT NOT NULL, \
                             product_name           TEXT NOT NULL, \
                             etag                   BLOB, \
                             PRIMARY KEY            (guild_id, product_id) \
                         ) STRICT",
                    (),
                )?;
                // index used for initial cache load
                connection.execute(
                    "CREATE INDEX IF NOT EXISTS product_lookup_by_guild ON product (guild_id)",
                    (),
                )?;

                // disk cache for product version names
                connection.execute(
                    "CREATE TABLE IF NOT EXISTS product_version ( \
                             guild_id               INTEGER NOT NULL, \
                             product_id             TEXT NOT NULL, \
                             version_id             TEXT NOT NULL, \
                             product_version_name   TEXT NOT NULL, \
                             PRIMARY KEY            (guild_id, product_id, version_id) \
                         ) STRICT",
                    (),
                )?;
                // index used for initial cache load
                connection.execute(
                    "CREATE INDEX IF NOT EXISTS version_lookup_by_guild ON product_version (guild_id)",
                    (),
                )?;

                // this is the "blanket" roles for any version in a product
                connection.execute(
                    "CREATE TABLE IF NOT EXISTS product_role ( \
                             guild_id               INTEGER NOT NULL, \
                             product_id             TEXT NOT NULL, \
                             role_id                INTEGER NOT NULL, \
                             PRIMARY KEY            (guild_id, product_id, role_id) \
                         ) STRICT",
                    (),
                )?;
                connection.execute(
                    "CREATE INDEX IF NOT EXISTS role_lookup ON product_role (guild_id, product_id)",
                    (),
                )?;

                // this is product-version specific role grants
                connection.execute(
                    "CREATE TABLE IF NOT EXISTS product_version_role ( \
                             guild_id               INTEGER NOT NULL, \
                             product_id             TEXT NOT NULL, \
                             version_id             TEXT NOT NULL, \
                             role_id                INTEGER NOT NULL, \
                             PRIMARY KEY            (guild_id, product_id, version_id, role_id) \
                         ) STRICT",
                    (),
                )?;
                // index used for initial cache load
                connection.execute(
                    "CREATE INDEX IF NOT EXISTS version_role_lookup ON product_version_role (guild_id, product_id, version_id)",
                    (),
                )?;

                connection.execute(
                    "CREATE TABLE IF NOT EXISTS license_activation ( \
                             guild_id               INTEGER NOT NULL, \
                             license_id             TEXT NOT NULL, \
                             license_activation_id  TEXT NOT NULL, \
                             user_id                INTEGER NOT NULL, \
                             product_id             TEXT, \
                             version_id             TEXT, \
                             PRIMARY KEY            (guild_id, license_id, license_activation_id, user_id) \
                         ) STRICT",
                    (),
                )?;
                connection.execute(
                    "CREATE INDEX IF NOT EXISTS license_activation_lookup ON license_activation (guild_id, license_id, user_id)",
                    (),
                )?;

                connection.execute(
                    "CREATE TABLE IF NOT EXISTS \"owner\" ( \
                             owner_id               INTEGER PRIMARY KEY \
                         ) STRICT",
                    (),
                )?;

                let schema_version: i32 = Self::get_setting_sync(connection, SCHEMA_VERSION_KEY)?.unwrap_or(SCHEMA_VERSION_VALUE);

                // handle schema downgrade (or rather, DON'T handle it and throw an error)
                if schema_version > SCHEMA_VERSION_VALUE {
                    let message = format!("db schema version is v{schema_version}, which is newer than v{SCHEMA_VERSION_VALUE} which is the latest schema this Jinx build supports.");
                    return Err(tokio_rusqlite::Error::Other(JinxError::boxed(message)));
                }

                // handle schema v1 -> v2 migration
                if schema_version < 2 {
                    // "log_channel_id" column needs to be added to "guild"
                    connection
                        .execute("ALTER TABLE guild ADD COLUMN log_channel_id INTEGER", ())?;
                    // "test" column needs to be added to "guild"
                    connection.execute(
                        "ALTER TABLE guild ADD COLUMN test INTEGER NOT NULL DEFAULT 0",
                        (),
                    )?;
                }

                // handle schema v2 -> v3 migration
                if schema_version < 3 {
                    // "owner" column needs to be added to "guild"
                    connection.execute(
                        "ALTER TABLE guild ADD COLUMN owner INTEGER NOT NULL DEFAULT 0",
                        (),
                    )?;
                }

                // handle schema v3 -> v4 migration
                if schema_version < 4 {
                    // "guild.id" column needs to be renamed to "guild_id"
                    connection.execute("ALTER TABLE guild RENAME COLUMN id TO guild_id", ())?;
                }

                // handle schema v4 -> v5 migration
                if schema_version < 5 {
                    // "gumroad_failure_count" and "gumroad_nag_count" columns need to be added to "guild"
                    connection.execute(
                        "ALTER TABLE guild ADD COLUMN gumroad_failure_count INTEGER NOT NULL DEFAULT 0",
                        (),
                    )?;
                    connection.execute(
                        "ALTER TABLE guild ADD COLUMN gumroad_nag_count INTEGER NOT NULL DEFAULT 0",
                        (),
                    )?;
                }

                // handle schema v5 -> v6 migration
                if schema_version < 6 {
                    // "blanket_role_id" needs to be added to "guild"
                    connection.execute(
                        "ALTER TABLE guild ADD COLUMN blanket_role_id INTEGER",
                        (),
                    )?;
                }

                // handle schema v6 -> v7 migration
                if schema_version < 7 {
                    // "cache_time_unix_ms" needs to be added to "guild"
                    connection.execute(
                        "ALTER TABLE guild ADD COLUMN cache_time_unix_ms INTEGER NOT NULL DEFAULT 0",
                        (),
                    )?;
                }

                // handle schema v7 -> v8 migration
                if schema_version < 8 {
                    // "product_id" and "version_id" need to be added to "license_activation"
                    connection.execute(
                        "ALTER TABLE license_activation ADD COLUMN product_id TEXT",
                        (),
                    )?;
                    connection.execute(
                        "ALTER TABLE license_activation ADD COLUMN version_id TEXT",
                        (),
                    )?;
                }

                // handle schema v8 -> v9 migration
                if schema_version < 9 {
                    // "etag"  needs to be added to "product"
                    connection.execute(
                        "ALTER TABLE product ADD COLUMN etag BLOB",
                        (),
                    )?;
                }

                // Applications that use long-lived database connections should run "PRAGMA optimize=0x10002;" when the connection is first opened.
                // All applications should run "PRAGMA optimize;" after a schema change.
                connection.execute("PRAGMA optimize = 0x10002", ())?;

                // update the schema version value persisted to the DB
                Self::set_setting_sync(connection, SCHEMA_VERSION_KEY, SCHEMA_VERSION_VALUE)?;

                Ok(())
            })
            .await?;

        let elapsed = start.elapsed();
        debug!("initialized db in {}ms", elapsed.as_millis());

        Ok(())
    }

    /// Attempt to optimize the database.
    ///
    /// Applications that use long-lived database connections should run "PRAGMA optimize;" periodically, perhaps once per day or once per hour.
    pub async fn optimize(&self) -> SqliteResult<()> {
        self.connection
            .call(move |connection| {
                connection.execute("PRAGMA optimize", ())?;
                Ok(())
            })
            .await
    }

    async fn get_setting<T>(&self, key: String) -> SqliteResult<Option<T>>
    where
        T: FromSql + Send + 'static,
    {
        let value = self
            .connection
            .call(move |connection| Self::get_setting_sync(connection, key.as_str()))
            .await?;
        Ok(value)
    }

    fn get_setting_sync<T>(connection: &rusqlite::Connection, key: &str) -> SqliteResult<Option<T>>
    where
        T: FromSql,
    {
        let mut statement = connection.prepare_cached("SELECT value FROM settings WHERE key = :key")?;
        let result: Option<T> = statement
            .query_row(named_params! {":key": key}, |row| row.get(0))
            .optional()?;
        Ok(result)
    }

    async fn set_setting<T>(&self, key: String, value: T) -> SqliteResult<bool>
    where
        T: ToSql + Send + 'static,
    {
        let result = self
            .connection
            .call(move |connection| Self::set_setting_sync(connection, key.as_str(), value))
            .await?;
        Ok(result)
    }

    fn set_setting_sync<T>(connection: &rusqlite::Connection, key: &str, value: T) -> SqliteResult<bool>
    where
        T: ToSql,
    {
        let mut statement =
            connection.prepare_cached("INSERT OR REPLACE INTO settings (key, value) VALUES (:key, :value)")?;
        let update_count = statement.execute(named_params! {":key": key, ":value": value})?;
        Ok(update_count != 0)
    }

    pub async fn add_owner(&self, owner_id: u64) -> SqliteResult<()> {
        let owner_id = owner_id as i64;
        self.connection
            .call(move |connection| {
                let mut statement =
                    connection.prepare_cached("INSERT OR IGNORE INTO owner (owner_id) VALUES (:owner)")?;
                statement.execute(named_params! {":owner": owner_id})?;
                Ok(())
            })
            .await
    }

    pub async fn delete_owner(&self, owner_id: u64) -> SqliteResult<()> {
        let owner_id = owner_id as i64;
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached("DELETE FROM owner WHERE owner_id = :owner")?;
                statement.execute(named_params! {":owner": owner_id})?;
                Ok(())
            })
            .await
    }

    pub async fn set_discord_token(&self, discord_token: String) -> SqliteResult<()> {
        self.set_setting(DISCORD_TOKEN_KEY.to_owned(), discord_token).await?;
        Ok(())
    }

    pub async fn get_owners(&self) -> SqliteResult<Vec<u64>> {
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached("SELECT owner_id FROM owner")?;
                let result = statement.query_map((), |row| {
                    let owner_id: i64 = row.get(0)?;
                    Ok(owner_id as u64)
                })?;
                let mut vec = Vec::with_capacity(result.size_hint().0);
                for row in result {
                    vec.push(row?);
                }
                Ok(vec)
            })
            .await
    }

    pub async fn is_user_owner(&self, owner_id: u64) -> SqliteResult<bool> {
        let owner_id = owner_id as i64;
        self.connection
            .call(move |connection| {
                let mut statement =
                    connection.prepare_cached("SELECT EXISTS(SELECT * FROM owner WHERE owner_id = :owner)")?;
                let owner_exists = statement.query_row(named_params! {":owner": owner_id}, |row| {
                    let exists: bool = row.get(0)?;
                    Ok(exists)
                })?;
                Ok(owner_exists)
            })
            .await
    }

    pub async fn get_discord_token(&self) -> SqliteResult<Option<String>> {
        self.get_setting(DISCORD_TOKEN_KEY.to_owned()).await
    }

    pub async fn set_low_priority_cache_expiry_time(
        &self,
        low_priority_cache_expiry_time: Duration,
    ) -> SqliteResult<()> {
        self.set_setting(
            LOW_PRIORITY_CACHE_EXPIRY_SECONDS.to_owned(),
            low_priority_cache_expiry_time.as_secs() as i64,
        )
        .await?;
        Ok(())
    }

    pub async fn get_low_priority_cache_expiry_time(&self) -> SqliteResult<Option<Duration>> {
        let low_priority_cache_expiry_time = self
            .get_setting::<i64>(LOW_PRIORITY_CACHE_EXPIRY_SECONDS.to_owned())
            .await?
            .map(|secs| Duration::from_secs(secs as u64));
        Ok(low_priority_cache_expiry_time)
    }

    /// Locally record that we've activated a license for a user
    pub async fn activate_license(
        &self,
        guild: GuildId,
        license_id: String,
        license_activation_id: String,
        user_id: u64,
        product_id: Option<String>,
        version_id: Option<String>,
    ) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let user_id = user_id as i64;
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT OR IGNORE INTO license_activation (guild_id, license_id, license_activation_id, user_id, product_id, version_id) VALUES (:guild, :license, :activation, :user, :product_id, :version_id)")?;
            statement.execute(named_params! {":guild": guild_id, ":license": license_id, ":activation": license_activation_id, ":user": user_id, ":product_id": product_id, ":version_id": version_id })?;
            Ok(())
        }).await
    }

    /// Update product_id and version_id for an existing license. Returns `true` if a row was updated, or `false` if no matching row was found.
    async fn update_license(
        &self,
        guild: GuildId,
        license_id: String,
        license_activation_id: String,
        user_id: u64,
        product_id: Option<String>,
        version_id: Option<String>,
    ) -> SqliteResult<bool> {
        let guild_id = guild.get() as i64;
        let user_id = user_id as i64;
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("UPDATE license_activation SET product_id = :product_id, version_id = :version_id WHERE guild_id = :guild AND license_id = :license AND license_activation_id = :activation AND user_id = :user")?;
            let update_count = statement.execute(named_params! {":guild": guild_id, ":license": license_id, ":activation": license_activation_id, ":user": user_id, ":product_id": product_id, ":version_id": version_id })?;
            Ok(update_count != 0)
        }).await
    }

    pub async fn backfill_license_info(&self) -> Result<usize, Error> {
        let license_records = self.connection.call(move |connection| {
            let mut license_query = connection.prepare("SELECT guild_id, license_id, license_activation_id, user_id FROM license_activation WHERE (product_id IS NULL OR version_id IS NULL) and user_id != 0")?;
            let license_rows = license_query.query_map((), |row| {
                let guild_id: i64 = row.get(0)?;
                let guild_id: GuildId = GuildId::new(guild_id as u64);
                let license_id: String = row.get(1)?;
                let license_activation_id: String = row.get(2)?;
                let user_id: i64 = row.get(3)?;
                let user_id = user_id as u64;
                Ok(LicenseRecord { guild_id, license_id, license_activation_id, user_id })
            })?;
            let mut vec = Vec::with_capacity(license_rows.size_hint().0);
            for row in license_rows {
                vec.push(row?);
            }
            Ok(vec)
        }).await?;

        let mut updated: usize = 0;
        for license_record in license_records {
            if let Some(api_key) = self.get_jinxxy_api_key(license_record.guild_id).await? {
                if let Some(license_info) =
                    jinxxy::check_license_id(&api_key, &license_record.license_id, false).await?
                {
                    let version_id = license_info.version_id().map(|str| str.to_string()).unwrap_or_default();
                    if self
                        .update_license(
                            license_record.guild_id,
                            license_record.license_id,
                            license_record.license_activation_id,
                            license_record.user_id,
                            Some(license_info.product_id),
                            Some(version_id),
                        )
                        .await?
                    {
                        updated += 1;
                    }
                }

                // delay a little bit before hitting Jinxxy again to avoid just completely spamming the hell out of it
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        Ok(updated)
    }

    /// Locally record that we've deactivated a license for a user. Returns `true` if a row was found and deleted, or `false` if no row was found to delete.
    pub async fn deactivate_license(
        &self,
        guild: GuildId,
        license_id: String,
        license_activation_id: String,
        user_id: u64,
    ) -> SqliteResult<bool> {
        let guild_id = guild.get() as i64;
        let user_id = user_id as i64;
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("DELETE FROM license_activation WHERE guild_id = :guild AND license_id = :license AND license_activation_id = :activation AND user_id = :user")?;
            let delete_count = statement.execute(named_params! {":guild": guild_id, ":license": license_id, ":activation": license_activation_id, ":user": user_id})?;
            Ok(delete_count != 0)
        }).await
    }

    /// Locally check if a license is locked. This may be out of sync with Jinxxy!
    pub async fn is_license_locked(&self, guild: GuildId, license_id: String) -> SqliteResult<bool> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                //TODO: could use an index
                let mut statement = connection.prepare_cached("SELECT EXISTS(SELECT * FROM license_activation WHERE guild_id = :guild AND license_id = :license AND user_id = 0)")?;
                let lock_exists = statement.query_row(
                    named_params! {":guild": guild_id, ":license": license_id},
                    |row| {
                        let exists: bool = row.get(0)?;
                        Ok(exists)
                    },
                )?;
                Ok(lock_exists)
            })
            .await
    }

    /// Set Jinxxy API key for this guild
    pub async fn set_jinxxy_api_key(&self, guild: GuildId, api_key: String) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let api_key_clone = api_key.clone();
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT INTO guild (guild_id, jinxxy_api_key) VALUES (:guild, :api_key) ON CONFLICT (guild_id) DO UPDATE SET jinxxy_api_key = excluded.jinxxy_api_key")?;
            statement.execute(named_params! {":guild": guild_id, ":api_key": api_key_clone})?;
            Ok(())
        }).await?;
        let api_key_cache = self.api_key_cache.pin();
        api_key_cache.insert(guild, Some(api_key));
        Ok(())
    }

    /// Get Jinxxy API key for this guild
    pub async fn get_jinxxy_api_key(&self, guild: GuildId) -> SqliteResult<Option<String>> {
        if let Some(api_key) = self.api_key_cache.pin().get(&guild) {
            // cache hit
            Ok(api_key.clone())
        } else {
            // cache miss
            let api_key = self
                .connection
                .call(move |connection| {
                    let mut statement =
                        connection.prepare_cached("SELECT jinxxy_api_key FROM guild WHERE guild_id = ?")?;
                    let result: Option<String> =
                        statement.query_row([guild.get() as i64], |row| row.get(0)).optional()?;
                    Ok(result)
                })
                .await?;
            self.api_key_cache.pin().insert(guild, api_key.clone());
            Ok(api_key)
        }
    }

    /// Set or unset blanket role
    pub async fn set_blanket_role_id(&self, guild: GuildId, role_id: Option<RoleId>) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let role_id = role_id.map(|role_id| role_id.get() as i64);
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT INTO guild (guild_id, blanket_role_id) VALUES (:guild, :role_id) ON CONFLICT (guild_id) DO UPDATE SET blanket_role_id = excluded.blanket_role_id")?;
            statement.execute(named_params! {":guild": guild_id, ":role_id": role_id})?;
            Ok(())
        }).await
    }

    /// blanket link a Jinxxy product and a role
    pub async fn link_product(&self, guild: GuildId, product_id: String, role: RoleId) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT OR IGNORE INTO product_role (guild_id, product_id, role_id) VALUES (:guild, :product, :role)")?;
            statement.execute(named_params! {":guild": guild_id, ":product": product_id, ":role": role_id})?;
            Ok(())
        }).await
    }

    /// blanket unlink a Jinxxy product and a role. Returns `true` if a row was found and deleted, or `false` if no row was found to delete.
    pub async fn unlink_product(&self, guild: GuildId, product_id: String, role: RoleId) -> SqliteResult<bool> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached(
                    "DELETE FROM product_role WHERE guild_id = :guild AND product_id = :product AND role_id = :role",
                )?;
                let delete_count =
                    statement.execute(named_params! {":guild": guild_id, ":product": product_id, ":role": role_id})?;
                Ok(delete_count != 0)
            })
            .await
    }

    /// link a Jinxxy product-version and a role
    pub async fn link_product_version(
        &self,
        guild: GuildId,
        product_version_id: ProductVersionId,
        role: RoleId,
    ) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT OR IGNORE INTO product_version_role (guild_id, product_id, version_id, role_id) VALUES (:guild, :product, :version, :role)")?;
            let (product_id, version_id) = product_version_id.into_db_values();
            statement.execute(named_params! {":guild": guild_id, ":product": product_id, ":version": version_id, ":role": role_id})?;
            Ok(())
        }).await
    }

    /// unlink a Jinxxy product-version and a role. Returns `true` if a row was found and deleted, or `false` if no row was found to delete.
    pub async fn unlink_product_version(
        &self,
        guild: GuildId,
        product_version_id: ProductVersionId,
        role: RoleId,
    ) -> SqliteResult<bool> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("DELETE FROM product_version_role WHERE guild_id = :guild AND product_id = :product AND version_id = :version AND role_id = :role")?;
            let (product_id, version_id) = product_version_id.into_db_values();
            let delete_count = statement.execute(named_params! {":guild": guild_id, ":product": product_id, ":version": version_id, ":role": role_id})?;
            Ok(delete_count != 0)
        }).await
    }

    /// Get role grants for a product ID. This includes blanket grants.
    pub async fn get_role_grants(
        &self,
        guild: GuildId,
        product_version_id: ProductVersionId,
    ) -> SqliteResult<Vec<RoleId>> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                // uses `role_lookup` and `version_role_lookup` indices
                let mut statement = connection.prepare_cached(
                    "SELECT blanket_role_id as role_id from guild WHERE guild_id = :guild AND blanket_role_id IS NOT NULL \
                    UNION SELECT role_id FROM product_role WHERE guild_id = :guild AND product_id = :product \
                    UNION SELECT role_id FROM product_version_role WHERE guild_id = :guild AND product_id = :product AND version_id = :version")?;
                let (product_id, version_id) = product_version_id.into_db_values();
                let result = statement.query_map(
                    named_params! {":guild": guild_id, ":product": product_id, ":version": version_id},
                    |row| {
                        let role_id: i64 = row.get(0)?;
                        Ok(RoleId::new(role_id as u64))
                    },
                )?;
                let mut vec = Vec::with_capacity(result.size_hint().0);
                for row in result {
                    vec.push(row?);
                }
                Ok(vec)
            })
            .await
    }

    /// Get roles for a product. This is ONLY product-level blanket grants.
    pub async fn get_linked_roles_for_product(&self, guild: GuildId, product_id: String) -> SqliteResult<Vec<RoleId>> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                // uses `role_lookup` index
                let mut statement = connection.prepare_cached(
                    "SELECT role_id FROM product_role WHERE guild_id = :guild AND product_id = :product",
                )?;
                let result =
                    statement.query_map(named_params! {":guild": guild_id, ":product": product_id}, |row| {
                        let role_id: i64 = row.get(0)?;
                        Ok(RoleId::new(role_id as u64))
                    })?;
                let mut vec = Vec::with_capacity(result.size_hint().0);
                for row in result {
                    vec.push(row?);
                }
                Ok(vec)
            })
            .await
    }

    /// Get roles for a product version. This does not include blanket grants.
    pub async fn get_linked_roles_for_product_version(
        &self,
        guild: GuildId,
        product_version_id: ProductVersionId,
    ) -> SqliteResult<Vec<RoleId>> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                // uses `version_role_lookup` index
                let mut statement = connection.prepare_cached("SELECT role_id FROM product_version_role WHERE guild_id = :guild AND product_id = :product AND version_id = :version")?;
                let (product_id, version_id) = product_version_id.into_db_values();
                let result = statement.query_map(
                    named_params! {":guild": guild_id, ":product": product_id, ":version": version_id},
                    |row| {
                        let role_id: i64 = row.get(0)?;
                        Ok(RoleId::new(role_id as u64))
                    },
                )?;
                let mut vec = Vec::with_capacity(result.size_hint().0);
                for row in result {
                    vec.push(row?);
                }
                Ok(vec)
            })
            .await
    }

    pub async fn get_users_for_role(&self, guild: GuildId, role: RoleId) -> SqliteResult<Vec<UserId>> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        self.connection
            .call(move |connection| {
                //TODO: this could use an index, or even several indices
                let mut statement = connection.prepare(
                    "SELECT user_id FROM license_activation LEFT JOIN guild USING (guild_id) WHERE guild_id = :guild AND blanket_role_id = :role \
                     UNION SELECT user_id FROM license_activation LEFT JOIN product_role USING (guild_id, product_id) WHERE guild_id = :guild AND role_id = :role \
                     UNION SELECT user_id FROM license_activation LEFT JOIN product_version_role USING (guild_id, product_id, version_id) WHERE guild_id = :guild AND role_id = :role")?;
                let result =
                    statement.query_map(named_params! {":guild": guild_id, ":role": role_id }, |row| {
                        let user_id: i64 = row.get(0)?;
                        let user_id: UserId = UserId::new(user_id as u64);
                        Ok(user_id)
                    })?;
                let mut vec = Vec::with_capacity(result.size_hint().0);
                for row in result {
                    vec.push(row?);
                }
                Ok(vec)
            })
            .await
    }

    /// get distinct roles from all links
    pub async fn get_linked_roles(&self, guild: GuildId) -> SqliteResult<Vec<RoleId>> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare(
                    "SELECT blanket_role_id AS role_id FROM guild WHERE guild_id = :guild AND blanket_role_id IS NOT NULL \
                    UNION SELECT role_id FROM product_role WHERE guild_id = :guild \
                    UNION SELECT role_id FROM product_version_role WHERE guild_id = :guild",
                )?;
                let result = statement.query_map(named_params! {":guild": guild_id}, |row| {
                    let role_id: i64 = row.get(0)?;
                    let role_id: RoleId = RoleId::new(role_id as u64);
                    Ok(role_id)
                })?;
                let mut vec = Vec::with_capacity(result.size_hint().0);
                for row in result {
                    vec.push(row?);
                }
                Ok(vec)
            })
            .await
    }

    /// get all links
    pub async fn get_links(
        &self,
        guild: GuildId,
    ) -> SqliteResult<HashMap<RoleId, Vec<LinkSource>, ahash::RandomState>> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                let mut map: HashMap<RoleId, Vec<LinkSource>, ahash::RandomState> = Default::default();

                // deal with global blanket
                let mut blanket_statement =
                    connection.prepare_cached("SELECT blanket_role_id from guild where guild_id = ?")?;
                let blanket_result: Option<Option<i64>> =
                    blanket_statement.query_row([guild_id], |row| row.get(0)).optional()?;
                let blanket_result = blanket_result.flatten().map(|role_id| RoleId::new(role_id as u64));
                if let Some(blanket_role) = blanket_result {
                    map.entry(blanket_role).or_default().push(LinkSource::GlobalBlanket);
                }

                // deal with product blankets
                //TODO: could use an index
                let mut product_statement =
                    connection.prepare_cached("SELECT product_id, role_id FROM product_role WHERE guild_id = ?")?;
                let product_result = product_statement.query_map([guild_id], |row| {
                    let product_id: String = row.get(0)?;
                    let role_id: i64 = row.get(1)?;
                    Ok((RoleId::new(role_id as u64), product_id))
                })?;
                for row in product_result {
                    let (role, product_id) = row?;
                    map.entry(role)
                        .or_default()
                        .push(LinkSource::ProductBlanket { product_id });
                }

                // deal with specific links
                //TODO: could use an index
                let mut product_version_statement = connection.prepare_cached(
                    "SELECT product_id, version_id, role_id FROM product_version_role WHERE guild_id = ?",
                )?;
                let product_version_result = product_version_statement.query_map([guild_id], |row| {
                    let product_id: String = row.get(0)?;
                    let product_version_id: String = row.get(1)?;
                    let role_id: i64 = row.get(2)?;
                    let product_version_id = ProductVersionId::from_db_values(product_id, product_version_id);
                    Ok((RoleId::new(role_id as u64), product_version_id))
                })?;
                for row in product_version_result {
                    let (role, product_version_id) = row?;
                    map.entry(role)
                        .or_default()
                        .push(LinkSource::ProductVersion(product_version_id));
                }
                Ok(map)
            })
            .await
    }

    /// Locally get all licences a users has been recorded to activate. This may be out of sync with Jinxxy!
    pub async fn get_user_licenses(&self, guild: GuildId, user_id: u64) -> SqliteResult<Vec<String>> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached(
                    "SELECT license_id FROM license_activation WHERE guild_id = :guild AND user_id = :user",
                )?;
                let result = statement.query_map(named_params! {":guild": guild_id, ":user": user_id}, |row| {
                    let license_id: String = row.get(0)?;
                    Ok(license_id)
                })?;
                let mut vec = Vec::with_capacity(result.size_hint().0);
                for row in result {
                    vec.push(row?);
                }
                Ok(vec)
            })
            .await
    }

    /// Locally check if any activations exist for this user/license combo. This may be out of sync with Jinxxy!
    pub async fn has_user_license_activations(
        &self,
        guild: GuildId,
        user_id: u64,
        license_id: String,
    ) -> SqliteResult<bool> {
        let guild_id = guild.get() as i64;
        let user_id = user_id as i64;
        self.connection
            .call(move |connection| {
                let mut statement = connection
                    .prepare_cached("SELECT EXISTS(SELECT * FROM license_activation WHERE guild_id = :guild AND user_id = :user AND license_id = :license)")?;
                let activation_exists =
                    statement.query_row(named_params! {":guild": guild_id, ":user": user_id, ":license": license_id}, |row| {
                        let exists: bool = row.get(0)?;
                        Ok(exists)
                    })?;
                Ok(activation_exists)
            })
            .await
    }

    /// Locally get all activations for a user and license has been recorded to activate. This may be out of sync with Jinxxy!
    pub async fn get_user_license_activations(
        &self,
        guild: GuildId,
        user_id: u64,
        license_id: String,
    ) -> SqliteResult<Vec<String>> {
        let guild_id = guild.get() as i64;
        let user_id = user_id as i64;
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached("SELECT license_activation_id FROM license_activation WHERE guild_id = :guild AND user_id = :user AND license_id = :license")?;
                let result = statement.query_map(
                    named_params! {":guild": guild_id, ":user": user_id, ":license": license_id},
                    |row| {
                        let activation_id: String = row.get(0)?;
                        Ok(activation_id)
                    },
                )?;
                let mut vec = Vec::with_capacity(result.size_hint().0);
                for row in result {
                    vec.push(row?);
                }
                Ok(vec)
            })
            .await
    }

    /// Locally get all users that have activated the given license. This may be out of sync with Jinxxy!
    pub async fn get_license_users(&self, guild: GuildId, license_id: String) -> SqliteResult<Vec<u64>> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                //TODO: could use an index
                let mut statement = connection.prepare_cached(
                    "SELECT user_id FROM license_activation WHERE guild_id = :guild AND license_id = :license",
                )?;
                let result =
                    statement.query_map(named_params! {":guild": guild_id, ":license": license_id}, |row| {
                        let user_id: i64 = row.get(0)?;
                        Ok(user_id as u64)
                    })?;
                let mut vec = Vec::with_capacity(result.size_hint().0);
                for row in result {
                    vec.push(row?);
                }
                Ok(vec)
            })
            .await
    }

    /// Get DB size in bytes
    pub async fn size(&self) -> SqliteResult<u64> {
        self.connection
            .call(move |connection| {
                let result: u64 = connection.query_row(
                    "SELECT page_count * page_size as size FROM pragma_page_count(), pragma_page_size()",
                    [],
                    |row| row.get(0),
                )?;
                Ok(result)
            })
            .await
    }

    /// Get count of license activations
    pub async fn license_activation_count(&self) -> SqliteResult<u64> {
        self.connection
            .call(move |connection| {
                let result: u64 = connection.query_row(
                    "SELECT count(*) FROM license_activation LEFT JOIN guild USING (guild_id) WHERE guild.test = 0",
                    [],
                    |row| row.get(0),
                )?;
                Ok(result)
            })
            .await
    }

    /// Get count of distinct users who have activated licenses
    pub async fn distinct_user_count(&self) -> SqliteResult<u64> {
        self.connection.call(move |connection| {
            let result: u64 = connection.query_row("SELECT count(DISTINCT user_id) FROM license_activation LEFT JOIN guild USING (guild_id) WHERE guild.test = 0", [], |row| row.get(0))?;
            Ok(result)
        }).await
    }

    /// Get count of configured guilds
    pub async fn guild_count(&self) -> SqliteResult<u64> {
        self.connection
            .call(move |connection| {
                let result: u64 =
                    connection.query_row("SELECT count(*) FROM guild WHERE test = 0", [], |row| row.get(0))?;
                Ok(result)
            })
            .await
    }

    /// Get count of distinct bot log channels
    pub async fn log_channel_count(&self) -> SqliteResult<u64> {
        self.connection
            .call(move |connection| {
                let result: u64 = connection.query_row(
                    "SELECT count(DISTINCT log_channel_id) FROM guild WHERE test = 0",
                    [],
                    |row| row.get(0),
                )?;
                Ok(result)
            })
            .await
    }

    /// Get count of guilds with blanket role set
    pub async fn blanket_role_count(&self) -> SqliteResult<u64> {
        self.connection
            .call(move |connection| {
                let result: u64 = connection.query_row(
                    "SELECT count(*) FROM guild WHERE guild.test = 0 AND blanket_role_id IS NOT NULL",
                    [],
                    |row| row.get(0),
                )?;
                Ok(result)
            })
            .await
    }

    /// Get count of product->role mappings
    pub async fn product_role_count(&self) -> SqliteResult<u64> {
        self.connection
            .call(move |connection| {
                let result: u64 = connection.query_row(
                    "SELECT count(*) FROM product_role LEFT JOIN guild USING (guild_id) WHERE guild.test = 0",
                    [],
                    |row| row.get(0),
                )?;
                Ok(result)
            })
            .await
    }

    /// Get count of product+version->role mappings
    pub async fn product_version_role_count(&self) -> SqliteResult<u64> {
        self.connection
            .call(move |connection| {
                let result: u64 = connection.query_row(
                    "SELECT count(*) FROM product_version_role LEFT JOIN guild USING (guild_id) WHERE guild.test = 0",
                    [],
                    |row| row.get(0),
                )?;
                Ok(result)
            })
            .await
    }

    /// Get count of license activations in a guild
    pub async fn guild_license_activation_count(&self, guild: GuildId) -> SqliteResult<u64> {
        let guild_id = guild.get() as i64;
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("SELECT count(*) FROM license_activation LEFT JOIN guild USING (guild_id) WHERE guild.guild_id = :guild")?;
            let result: u64 = statement.query_row(named_params! {":guild": guild_id}, |row| row.get(0))?;
            Ok(result)
        }).await
    }

    /// Get bot log channel
    pub async fn get_log_channel(&self, guild: GuildId) -> SqliteResult<Option<ChannelId>> {
        let guild_id = guild.get() as i64;
        let channel_id = self
            .connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached("SELECT log_channel_id FROM guild WHERE guild_id = ?")?;
                let result: Option<Option<i64>> = statement.query_row([guild_id], |row| row.get(0)).optional()?;
                // inner optional is for if the guild has no log channel set
                // outer optional is for if the guild does not exist in our DB
                Ok(result.flatten())
            })
            .await?;
        Ok(channel_id.map(|channel_id| ChannelId::new(channel_id as u64)))
    }

    /// Get all bot log channels.
    /// If `TEST_ONLY` is true, then only returns non-production servers. Otherwise, returns all servers.
    pub async fn get_log_channels<const TEST_ONLY: bool>(&self) -> SqliteResult<Vec<ChannelId>> {
        self.connection.call(move |connection| {
            let mut statement = if TEST_ONLY {
                // only non-production servers
                connection.prepare_cached("SELECT DISTINCT log_channel_id FROM guild WHERE log_channel_id IS NOT NULL AND guild.test != 0")
            } else {
                // all servers, including production servers
                connection.prepare_cached("SELECT DISTINCT log_channel_id FROM guild WHERE log_channel_id IS NOT NULL")
            }?;
            let mapped_rows = statement.query_map((), |row| row.get(0).map(|id: i64| ChannelId::new(id as u64)))?;
            let mut vec = Vec::with_capacity(mapped_rows.size_hint().0);
            for row in mapped_rows {
                vec.push(row?);
            }
            Ok(vec)
        }).await
    }

    /// Set or unset bot log channel
    pub async fn set_log_channel(&self, guild: GuildId, channel: Option<ChannelId>) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let channel_id = channel.map(|channel| channel.get() as i64);
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT INTO guild (guild_id, log_channel_id) VALUES (:guild, :channel) ON CONFLICT (guild_id) DO UPDATE SET log_channel_id = excluded.log_channel_id")?;
            statement.execute(named_params! {":guild": guild_id, ":channel": channel_id})?;
            Ok(())
        }).await
    }

    /// Set or unset this guild as a test guild
    pub async fn set_test(&self, guild: GuildId, test: bool) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT INTO guild (guild_id, test) VALUES (:guild, :test) ON CONFLICT (guild_id) DO UPDATE SET test = excluded.test")?;
            statement.execute(named_params! {":guild": guild_id, ":test": test})?;
            Ok(())
        }).await
    }

    /// Check if a guild is a test guild
    pub async fn is_test_guild(&self, guild: GuildId) -> SqliteResult<bool> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached("SELECT test FROM guild WHERE guild_id = :guild")?;
                let test = statement
                    .query_row(named_params! {":guild": guild_id}, |row| {
                        let test: bool = row.get(0)?;
                        Ok(test)
                    })
                    .optional()?;
                Ok(test.unwrap_or(false))
            })
            .await
    }

    /// Set or unset this guild as an owner guild (gets extra slash commands)
    pub async fn set_owner_guild(&self, guild: GuildId, owner: bool) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT INTO guild (guild_id, owner) VALUES (:guild, :owner) ON CONFLICT (guild_id) DO UPDATE SET owner = excluded.owner")?;
            statement.execute(named_params! {":guild": guild_id, ":owner": owner})?;
            Ok(())
        }).await
    }

    /// Check if a guild is an owner guild (gets extra slash commands)
    pub async fn is_owner_guild(&self, guild: GuildId) -> SqliteResult<bool> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached("SELECT owner FROM guild WHERE guild_id = :guild")?;
                let owner = statement
                    .query_row(named_params! {":guild": guild_id}, |row| {
                        let owner: bool = row.get(0)?;
                        Ok(owner)
                    })
                    .optional()?;
                Ok(owner.unwrap_or(false))
            })
            .await
    }

    /// Check gumroad failure count for a guild
    pub async fn get_gumroad_failure_count(&self, guild: GuildId) -> SqliteResult<Option<u64>> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                let mut statement =
                    connection.prepare_cached("SELECT gumroad_failure_count FROM guild WHERE guild_id = :guild")?;
                let gumroad_failure_count = statement
                    .query_row(named_params! {":guild": guild_id}, |row| {
                        let gumroad_failure_count: i64 = row.get(0)?;
                        Ok(gumroad_failure_count as u64)
                    })
                    .optional()?;
                Ok(gumroad_failure_count)
            })
            .await
    }

    /// Increment gumroad failure count for a guild
    pub async fn increment_gumroad_failure_count(&self, guild: GuildId) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached(
                    "UPDATE guild SET gumroad_failure_count = gumroad_failure_count + 1 WHERE guild_id = :guild",
                )?;
                statement.execute(named_params! {":guild": guild_id})?;
                Ok(())
            })
            .await
    }

    /// Get tuples of `(guild_id, log_channel_id)` with pending gumroad nag
    pub async fn get_guilds_pending_gumroad_nag(&self) -> SqliteResult<Vec<GuildGumroadInfo>> {
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached(
                    "SELECT guild_id, log_channel_id, gumroad_failure_count FROM guild WHERE log_channel_id IS NOT NULL AND gumroad_nag_count < 1 AND gumroad_failure_count >= 10 AND (gumroad_failure_count * 5) > (SELECT count(*) FROM license_activation WHERE license_activation.guild_id = guild.guild_id)",
                )?;
                let mapped_rows = statement
                    .query_map((), |row| {
                        let guild_id: i64 = row.get(0)?;
                        let log_channel_id: i64 = row.get(1)?;
                        let gumroad_failure_count: i64 = row.get(2)?;
                        Ok(GuildGumroadInfo {
                            guild_id: GuildId::new(guild_id as u64),
                            log_channel_id: ChannelId::new(log_channel_id as u64),
                            gumroad_failure_count: gumroad_failure_count as u64,
                        })
                    })?;
                let mut vec = Vec::with_capacity(mapped_rows.size_hint().0);
                for row in mapped_rows {
                    vec.push(row?);
                }
                Ok(vec)
            })
            .await
    }

    /// Check gumroad nag count for a guild
    pub async fn get_gumroad_nag_count(&self, guild: GuildId) -> SqliteResult<Option<u64>> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                let mut statement =
                    connection.prepare_cached("SELECT gumroad_nag_count FROM guild WHERE guild_id = :guild")?;
                let gumroad_nag_count = statement
                    .query_row(named_params! {":guild": guild_id}, |row| {
                        let gumroad_nag_count: i64 = row.get(0)?;
                        Ok(gumroad_nag_count as u64)
                    })
                    .optional()?;
                Ok(gumroad_nag_count)
            })
            .await
    }

    /// Increment gumroad nag count for a guild
    pub async fn increment_gumroad_nag_count(&self, guild: GuildId) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached(
                    "UPDATE guild SET gumroad_nag_count = gumroad_nag_count + 1 WHERE guild_id = :guild",
                )?;
                statement.execute(named_params! {":guild": guild_id})?;
                Ok(())
            })
            .await
    }

    pub async fn get_guild_cache(&self, guild: GuildId) -> SqliteResult<GuildCache> {
        self.connection
            .call(move |connection| {
                let cache_time = JinxDb::get_cache_time(connection, guild)?;
                let product_name_info = JinxDb::product_names_in_guild_sync(connection, guild)?;
                let product_version_name_info = JinxDb::product_version_names_in_guild(connection, guild)?;
                Ok(GuildCache {
                    product_name_info,
                    product_version_name_info,
                    cache_time,
                })
            })
            .await
    }

    pub async fn persist_guild_cache(&self, guild: GuildId, cache_entry: GuildCache) -> SqliteResult<()> {
        self.connection
            .call(move |connection| {
                JinxDb::persist_product_names(connection, guild, cache_entry.product_name_info)?;
                JinxDb::persist_product_version_names(connection, guild, cache_entry.product_version_name_info)?;
                JinxDb::set_cache_time(connection, guild, cache_entry.cache_time)?;
                Ok(())
            })
            .await
    }

    /// Delete all cache entries for all guilds
    pub async fn clear_cache(&self) -> SqliteResult<()> {
        self.connection
            .call(move |connection| {
                connection.execute("DELETE FROM product", ())?;
                connection.execute("DELETE FROM product_version", ())?;
                connection.execute("UPDATE guild SET cache_time_unix_ms = 0", ())?;
                Ok(())
            })
            .await
    }

    fn get_cache_time(connection: &rusqlite::Connection, guild: GuildId) -> SqliteResult<SimpleTime> {
        let guild_id = guild.get() as i64;
        let mut statement =
            connection.prepare_cached("SELECT cache_time_unix_ms FROM guild WHERE guild_id = :guild")?;
        let cache_time_unix_ms = statement.query_row(named_params! {":guild": guild_id}, |row| {
            let cache_time_unix_ms: u64 = row.get(0)?;
            Ok(cache_time_unix_ms)
        })?;
        Ok(SimpleTime::from_unix_millis(cache_time_unix_ms))
    }

    fn set_cache_time(connection: &rusqlite::Connection, guild: GuildId, time: SimpleTime) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let mut statement = connection.prepare_cached("INSERT INTO guild (guild_id, cache_time_unix_ms) VALUES (:guild, :time) ON CONFLICT (guild_id) DO UPDATE SET cache_time_unix_ms = excluded.cache_time_unix_ms")?;
        statement.execute(named_params! {":guild": guild_id, ":time": time.as_epoch_millis()})?;
        Ok(())
    }

    /// Get cached name info for products in a guild
    pub async fn product_names_in_guild(&self, guild: GuildId) -> SqliteResult<Vec<ProductNameInfo>> {
        self.connection
            .call(move |connection| Self::product_names_in_guild_sync(connection, guild))
            .await
    }

    /// Get cached name info for products in a guild
    fn product_names_in_guild_sync(
        connection: &rusqlite::Connection,
        guild: GuildId,
    ) -> SqliteResult<Vec<ProductNameInfo>> {
        let guild_id = guild.get() as i64;
        let mut statement =
            connection.prepare_cached("SELECT product_id, product_name, etag FROM product WHERE guild_id = :guild")?;
        let mapped_rows = statement.query_map(named_params! {":guild": guild_id}, |row| {
            let product_id: String = row.get(0)?;
            let product_name: String = row.get(1)?;
            let etag: Option<Vec<u8>> = row.get(2)?;
            let info = ProductNameInfo {
                id: product_id,
                value: ProductNameInfoValue { product_name, etag },
            };
            Ok(info)
        })?;
        let mut vec = Vec::with_capacity(mapped_rows.size_hint().0);
        for row in mapped_rows {
            vec.push(row?);
        }
        Ok(vec)
    }

    /// Get versions for a product
    pub async fn product_versions(
        &self,
        guild: GuildId,
        product_id: String,
    ) -> SqliteResult<Vec<ProductVersionNameInfo>> {
        self.connection.call(move |connection| {
            let guild_id = guild.get() as i64;
            //TODO: could use an index
            let mut statement = connection.prepare_cached(
                "SELECT version_id, product_version_name FROM product_version WHERE guild_id = :guild AND product_id = :product_id",
            )?;
            let mapped_rows = statement.query_map(named_params! {":guild": guild_id, ":product_id": product_id}, |row| {
                let id = row.get(0)?;
                let name = row.get(1)?;
                let info = ProductVersionNameInfo {
                    id: ProductVersionId { product_id: product_id.clone(), product_version_id: id },
                    product_version_name: name,
                };
                Ok(info)
            })?;
            let mut vec = Vec::with_capacity(mapped_rows.size_hint().0);
            for row in mapped_rows {
                vec.push(row?);
            }
            Ok(vec)
        }).await
    }

    /// Get name info for products versions in a guild
    fn product_version_names_in_guild(
        connection: &rusqlite::Connection,
        guild: GuildId,
    ) -> SqliteResult<Vec<ProductVersionNameInfo>> {
        let guild_id = guild.get() as i64;
        let mut statement = connection.prepare_cached(
            "SELECT product_id, version_id, product_version_name FROM product_version WHERE guild_id = :guild",
        )?;
        let mapped_rows = statement.query_map(named_params! {":guild": guild_id}, |row| {
            let product_id: String = row.get(0)?;
            let version_id: String = row.get(1)?;
            let product_version_name: String = row.get(2)?;
            let id = ProductVersionId::from_db_values(product_id, version_id);
            let info = ProductVersionNameInfo {
                id,
                product_version_name,
            };
            Ok(info)
        })?;
        let mut vec = Vec::with_capacity(mapped_rows.size_hint().0);
        for row in mapped_rows {
            vec.push(row?);
        }
        Ok(vec)
    }

    fn persist_product_names(
        connection: &rusqlite::Connection,
        guild: GuildId,
        product_name_info: Vec<ProductNameInfo>,
    ) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let mut new_key_set = HashSet::with_capacity_and_hasher(product_name_info.len(), ahash::RandomState::default());
        let mut unexpected_keys = Vec::new();

        // step 1: insert all entries and keep track of their keys in a set for later
        let mut statement = connection.prepare_cached("INSERT INTO product (guild_id, product_id, product_name, etag) VALUES (:guild, :product_id, :product_name, :etag) ON CONFLICT (guild_id, product_id) DO UPDATE SET product_name = excluded.product_name, etag = excluded.etag")?;
        for info in product_name_info {
            let product_id = info.id;
            let product_name = info.value.product_name;
            let etag = info.value.etag;
            statement.execute(
                named_params! {":guild": guild_id, ":product_id": product_id, ":product_name": product_name, ":etag": etag},
            )?;
            new_key_set.insert(product_id);
        }

        // step 2: query all existing keys, and record any keys that we did NOT just insert
        let mut statement = connection.prepare_cached("SELECT product_id FROM product WHERE guild_id = :guild")?;
        let mut rows = statement.query(named_params! {":guild": guild_id})?;
        while let Some(row) = rows.next()? {
            let product_id: String = row.get(0)?;
            if !new_key_set.contains(&product_id) {
                unexpected_keys.push(product_id);
            }
        }

        // step 3: delete any rows with keys that we did NOT just insert
        let mut statement =
            connection.prepare_cached("DELETE FROM product WHERE guild_id = :guild AND product_id = :product_id")?;
        for product_id in unexpected_keys {
            statement.execute(named_params! {":guild": guild_id, ":product_id": product_id})?;
        }
        Ok(())
    }

    fn persist_product_version_names(
        connection: &rusqlite::Connection,
        guild: GuildId,
        product_version_name_info: Vec<ProductVersionNameInfo>,
    ) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let mut new_key_set =
            HashSet::with_capacity_and_hasher(product_version_name_info.len(), ahash::RandomState::default());
        let mut unexpected_keys = Vec::new();
        // step 1: insert all entries and keep track of their keys in a set for later
        let mut statement = connection.prepare_cached("INSERT INTO product_version (guild_id, product_id, version_id, product_version_name) VALUES (:guild, :product_id, :version_id, :product_version_name) ON CONFLICT (guild_id, product_id, version_id) DO UPDATE SET product_version_name = excluded.product_version_name")?;
        for info in product_version_name_info {
            let (product_id, version_id) = info.id.into_db_values();
            let product_version_name = info.product_version_name;
            statement.execute(named_params! {":guild": guild_id, ":product_id": product_id, ":version_id": version_id, ":product_version_name": product_version_name})?;
            new_key_set.insert((product_id, version_id));
        }

        // step 2: query all existing keys, and record any keys that we did NOT just insert
        let mut statement =
            connection.prepare_cached("SELECT product_id, version_id FROM product_version WHERE guild_id = :guild")?;
        let mut rows = statement.query(named_params! {":guild": guild_id})?;
        while let Some(row) = rows.next()? {
            let product_id: String = row.get(0)?;
            let version_id: String = row.get(1)?;
            let key = (product_id, version_id);
            if !new_key_set.contains(&key) {
                unexpected_keys.push(key);
            }
        }

        // step 3: delete any rows with keys that we did NOT just insert
        let mut statement = connection.prepare_cached("DELETE FROM product_version WHERE guild_id = :guild AND product_id = :product_id AND version_id = :version_id")?;
        for (product_id, version_id) in unexpected_keys {
            statement
                .execute(named_params! {":guild": guild_id, ":product_id": product_id, ":version_id": version_id})?;
        }
        Ok(())
    }
}

/// Helper struct returned by [`JinxDb::get_guilds_pending_gumroad_nag`]
pub struct GuildGumroadInfo {
    pub guild_id: GuildId,
    pub log_channel_id: ChannelId,
    pub gumroad_failure_count: u64,
}

/// Helper enum returned by [`JinxDb::get_links`]. This is any source for a product->role link.
pub enum LinkSource {
    GlobalBlanket,
    ProductBlanket { product_id: String },
    ProductVersion(ProductVersionId),
}

/// Helper struct returned by [`JinxDb::get_guild_cache`] and taken by [`JinxDb::persist_guild_cache`].
pub struct GuildCache {
    pub product_name_info: Vec<ProductNameInfo>,
    pub product_version_name_info: Vec<ProductVersionNameInfo>,
    pub cache_time: SimpleTime,
}

/// Extra functions specifically for using this with the DB
impl ProductVersionId {
    fn into_db_values(self) -> (String, String) {
        (self.product_id, self.product_version_id.unwrap_or_default())
    }

    fn from_db_values(product_id: String, version_id: String) -> Self {
        let product_version_id = if version_id.is_empty() { None } else { Some(version_id) };
        Self {
            product_id,
            product_version_id,
        }
    }
}

/// Used internally in [`JinxDb::backfill_license_info`]
struct LicenseRecord {
    guild_id: GuildId,
    license_id: String,
    license_activation_id: String,
    user_id: u64,
}
