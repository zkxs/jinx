// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

mod schema_v1;
mod schema_v2;

use crate::error::JinxError;
use crate::http::jinxxy;
use crate::http::jinxxy::{ProductNameInfo, ProductNameInfoValue, ProductVersionId, ProductVersionNameInfo};
use crate::time::SimpleTime;
use poise::futures_util::TryStreamExt;
use poise::serenity_prelude::{ChannelId, GuildId, RoleId, UserId};
use sqlx::pool::PoolConnection;
use sqlx::sqlite::{
    SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqliteLockingMode, SqlitePoolOptions, SqliteRow,
    SqliteSynchronous,
};
use sqlx::{
    ConnectOptions, Encode, Executor, FromRow, Pool, Sqlite, SqliteConnection, Type, error::Error as SqlxError,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tracing::debug;

const DB_V1_FILENAME: &str = "jinx.sqlite";
const DB_V2_FILENAME: &str = "jinx2.sqlite";
const SCHEMA_VERSION_KEY: &str = "schema_version";
const DISCORD_TOKEN_KEY: &str = "discord_token";
const LOW_PRIORITY_CACHE_EXPIRY_SECONDS: &str = "low_priority_cache_expiry_seconds";

type SqliteResult<T> = Result<T, SqlxError>;
type BoxedError = Box<dyn std::error::Error + Send + Sync>;

/// Cloning is by-reference.
#[derive(Clone)]
pub struct JinxDb {
    read_pool: Pool<Sqlite>,
    write_pool: Pool<Sqlite>,
    api_key_cache: Arc<papaya::HashMap<GuildId, Option<String>, ahash::RandomState>>,
}

impl Drop for JinxDb {
    fn drop(&mut self) {
        debug!("Closing sqlite db…");
    }
}

impl JinxDb {
    /// Open a new database
    pub async fn open() -> Result<Self, JinxError> {
        let pool_options_write = SqlitePoolOptions::new().min_connections(1).max_connections(1);
        let pool_options_read = SqlitePoolOptions::new().min_connections(1).max_connections(4);
        let connect_options_write = SqliteConnectOptions::new()
            .filename(DB_V2_FILENAME)
            .foreign_keys(true)
            .in_memory(false)
            .shared_cache(false) // superseded by WAL mode
            .journal_mode(SqliteJournalMode::Wal)
            .locking_mode(SqliteLockingMode::Normal) // must be Normal to have multiple connections
            .read_only(false)
            .create_if_missing(true)
            .statement_cache_capacity(100)
            .busy_timeout(Duration::from_secs(5))
            .synchronous(SqliteSynchronous::Full)
            .auto_vacuum(SqliteAutoVacuum::None)
            .page_size(4096)
            .pragma("trusted_schema", "OFF"); // all applications are encouraged to switch this setting off on every database connection as soon as that connection is opened
        let connect_options_read = connect_options_write.clone().read_only(true).create_if_missing(false);

        let v2_db_exists = Path::new(DB_V2_FILENAME).is_file();
        let write_pool = pool_options_write.connect_with(connect_options_write).await?;
        let read_pool = pool_options_read.connect_with(connect_options_read).await?;
        let mut write_connection = write_pool.acquire().await?;
        schema_v2::init(&mut write_connection).await?;

        {
            // sanity check to see if we can have multiple connections open
            let mut read_connection = read_pool.acquire().await?;
            read_connection.execute("SELECT name FROM sqlite_schema").await?;
        }

        if !v2_db_exists && Path::new(DB_V1_FILENAME).is_file() {
            // the v2 DB does not exist, so we must initialize the v1 db and migrate it to v2
            let connect_options_v1 = SqliteConnectOptions::new()
                .filename(DB_V1_FILENAME)
                .foreign_keys(false)
                .in_memory(false)
                .shared_cache(false)
                .journal_mode(SqliteJournalMode::Delete)
                .locking_mode(SqliteLockingMode::Normal)
                .read_only(false)
                .create_if_missing(false)
                .synchronous(SqliteSynchronous::Full)
                .auto_vacuum(SqliteAutoVacuum::None)
                .page_size(4096)
                .pragma("trusted_schema", "OFF");
            let mut v1_connection = connect_options_v1.connect().await?;
            // handle any pending migrations on the v1 db
            schema_v1::init(&mut v1_connection).await?;
            // perform the big migration
            schema_v2::copy_from_v1(&mut v1_connection, &mut write_connection).await?;
        }

        let db = JinxDb {
            read_pool,
            write_pool,
            api_key_cache: Default::default(),
        };
        Ok(db)
    }

    /// Get something that we can DerefMut as SqliteConnection
    async fn write_connection(&self) -> SqliteResult<PoolConnection<Sqlite>> {
        self.write_pool.acquire().await
    }

    /// Attempt to optimize the database.
    ///
    /// Applications that use long-lived database connections should run "PRAGMA optimize;" periodically, perhaps once per day or once per hour.
    pub async fn optimize(&self) -> SqliteResult<()> {
        let mut connection = self.write_connection().await?;
        connection.execute(r#"PRAGMA optimize"#).await?;
        Ok(())
    }

    async fn get_setting<'e, T>(&self, key: &str) -> SqliteResult<Option<T>>
    where
        T: Type<Sqlite> + Send + Unpin + 'e,
        (T,): for<'r> FromRow<'r, SqliteRow>, // what the fuck is this
    {
        let mut connection = self.write_connection().await?;
        helper::get_setting(&mut connection, key).await
    }

    async fn set_setting<'q, T>(&self, key: &'q str, value: T) -> SqliteResult<bool>
    where
        T: Encode<'q, Sqlite> + Type<Sqlite> + 'q,
    {
        let mut connection = self.write_connection().await?;
        helper::set_setting(&mut connection, key, value).await
    }

    pub async fn add_owner(&self, owner_id: u64) -> SqliteResult<()> {
        let owner_id = owner_id as i64;
        let mut connection = self.write_connection().await?;
        sqlx::query!(r#"INSERT OR IGNORE INTO owner (owner_id) VALUES (?)"#, owner_id)
            .execute(&mut *connection)
            .await?;
        Ok(())
    }

    pub async fn delete_owner(&self, owner_id: u64) -> SqliteResult<()> {
        let owner_id = owner_id as i64;
        let mut connection = self.write_connection().await?;
        sqlx::query!(r#"DELETE FROM owner WHERE owner_id = ?"#, owner_id)
            .execute(&mut *connection)
            .await?;
        Ok(())
    }

    pub async fn set_discord_token(&self, discord_token: String) -> SqliteResult<()> {
        self.set_setting(DISCORD_TOKEN_KEY, discord_token).await?;
        Ok(())
    }

    pub async fn get_owners(&self) -> SqliteResult<Vec<u64>> {
        sqlx::query!(r#"SELECT owner_id FROM owner"#)
            .map(|row| row.owner_id as u64)
            .fetch_all(&self.read_pool)
            .await
    }

    pub async fn is_user_owner(&self, owner_id: u64) -> SqliteResult<bool> {
        let owner_id = owner_id as i64;
        sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT * FROM owner WHERE owner_id = ?) AS "is_owner: bool""#,
            owner_id
        )
        .fetch_one(&self.read_pool)
        .await
    }

    pub async fn get_discord_token(&self) -> SqliteResult<Option<String>> {
        self.get_setting(DISCORD_TOKEN_KEY).await
    }

    pub async fn set_low_priority_cache_expiry_time(
        &self,
        low_priority_cache_expiry_time: Duration,
    ) -> SqliteResult<()> {
        self.set_setting(
            LOW_PRIORITY_CACHE_EXPIRY_SECONDS,
            low_priority_cache_expiry_time.as_secs() as i64,
        )
        .await?;
        Ok(())
    }

    pub async fn get_low_priority_cache_expiry_time(&self) -> SqliteResult<Option<Duration>> {
        let low_priority_cache_expiry_time = self
            .get_setting::<i64>(LOW_PRIORITY_CACHE_EXPIRY_SECONDS)
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
        let mut connection = self.write_connection().await?;
        sqlx::query!(
            r#"INSERT OR IGNORE INTO license_activation (guild_id, license_id, license_activation_id, user_id, product_id, version_id) VALUES (?, ?, ?, ?, ?, ?)"#,
            guild_id,
            license_id,
            license_activation_id,
            user_id,
            product_id,
            version_id
        )
        .execute(&mut *connection)
        .await?;
        Ok(())
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
        let mut connection = self.write_connection().await?;
        let update_count = sqlx::query!(
            r#"UPDATE license_activation SET product_id = :product_id, version_id = :version_id WHERE guild_id = :guild AND license_id = :license AND license_activation_id = :activation AND user_id = :user"#,
            guild_id,
            license_id,
            license_activation_id,
            user_id,
            product_id,
            version_id
        )
        .execute(&mut *connection)
        .await?
        .rows_affected();
        Ok(update_count != 0)
    }

    pub async fn backfill_license_info(&self) -> Result<usize, BoxedError> {
        let mut connection = self.write_connection().await?;
        let license_records = sqlx::query!(r#"SELECT guild_id, license_id, license_activation_id, user_id FROM license_activation WHERE (product_id IS NULL OR version_id IS NULL) and user_id != 0"#)
            .map(|row| LicenseRecord {
                guild_id: GuildId::new(row.guild_id as u64),
                license_id: row.license_id,
                license_activation_id: row.license_activation_id,
                user_id: row.user_id as u64,
            })
            .fetch_all(&mut *connection)
            .await?;

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
        let mut connection = self.write_connection().await?;
        let delete_count = sqlx::query!(
            r#"DELETE FROM license_activation WHERE guild_id = ? AND license_id = ? AND license_activation_id = ? AND user_id = ?"#,
            guild_id,
            license_id,
            license_activation_id,
            user_id
        )
        .execute(&mut *connection)
        .await?
        .rows_affected();
        Ok(delete_count != 0)
    }

    /// Locally check if a license is locked. This may be out of sync with Jinxxy!
    pub async fn is_license_locked(&self, guild: GuildId, license_id: String) -> SqliteResult<bool> {
        let guild_id = guild.get() as i64;
        sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT * FROM license_activation WHERE guild_id = ? AND license_id = ? AND user_id = 0) AS "is_locked: bool""#,
            guild_id,
            license_id
        )
        .fetch_one(&self.read_pool)
        .await
    }

    /// Set Jinxxy API key for this guild
    pub async fn set_jinxxy_api_key(&self, guild: GuildId, api_key: String) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let api_key_str = api_key.as_str();
        let mut connection = self.write_connection().await?;
        sqlx::query!(
            r#"INSERT INTO guild (guild_id, jinxxy_api_key) VALUES (?, ?)
               ON CONFLICT (guild_id) DO UPDATE SET jinxxy_api_key = excluded.jinxxy_api_key"#,
            guild_id,
            api_key_str
        )
        .execute(&mut *connection)
        .await?;
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
            let guild_id = guild.get() as i64;
            let api_key = sqlx::query_scalar!(r#"SELECT jinxxy_api_key FROM guild WHERE guild_id = ?"#, guild_id)
                .fetch_optional(&self.read_pool)
                .await?
                .flatten();
            self.api_key_cache.pin().insert(guild, api_key.clone());
            Ok(api_key)
        }
    }

    /// Set or unset blanket role
    pub async fn set_blanket_role_id(&self, guild: GuildId, role_id: Option<RoleId>) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let role_id = role_id.map(|role_id| role_id.get() as i64);
        let mut connection = self.write_connection().await?;
        sqlx::query!(
            r#"INSERT INTO guild (guild_id, blanket_role_id) VALUES (?, ?)
               ON CONFLICT (guild_id) DO UPDATE SET blanket_role_id = excluded.blanket_role_id"#,
            guild_id,
            role_id
        )
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    /// blanket link a Jinxxy product and a role
    pub async fn link_product(&self, guild: GuildId, product_id: String, role: RoleId) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        let mut connection = self.write_connection().await?;
        sqlx::query!(
            r#"INSERT OR IGNORE INTO product_role (guild_id, product_id, role_id) VALUES (?, ?, ?)"#,
            guild_id,
            product_id,
            role_id
        )
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    /// blanket unlink a Jinxxy product and a role. Returns `true` if a row was found and deleted, or `false` if no row was found to delete.
    pub async fn unlink_product(&self, guild: GuildId, product_id: String, role: RoleId) -> SqliteResult<bool> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        let mut connection = self.write_connection().await?;
        let delete_count = sqlx::query!(
            r#"DELETE FROM product_role WHERE guild_id = ? AND product_id = ? AND role_id = ?"#,
            guild_id,
            product_id,
            role_id
        )
        .execute(&mut *connection)
        .await?
        .rows_affected();
        Ok(delete_count != 0)
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
        let (product_id, version_id) = product_version_id.into_db_values();
        let mut connection = self.write_connection().await?;
        sqlx::query!(r#"INSERT OR IGNORE INTO product_version_role (guild_id, product_id, version_id, role_id) VALUES (?, ?, ?, ?)"#, guild_id, product_id, version_id, role_id)
            .execute(&mut *connection)
            .await?;
        Ok(())
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
        let (product_id, version_id) = product_version_id.into_db_values();
        let mut connection = self.write_connection().await?;
        let delete_count = sqlx::query!(r#"DELETE FROM product_version_role WHERE guild_id = ? AND product_id = ? AND version_id = ? AND role_id = ?"#, guild_id, product_id, version_id, role_id)
            .execute(&mut *connection)
            .await?
            .rows_affected();
        Ok(delete_count != 0)
    }

    /// Delete all references to a role id for the given guild
    pub async fn delete_role(&self, guild: GuildId, role: RoleId) -> SqliteResult<u64> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        let mut deleted = 0;
        let mut connection = self.write_connection().await?;

        // handle blanket role
        deleted += sqlx::query!(
            r#"UPDATE guild SET blanket_role_id = NULL WHERE guild_id = ? AND blanket_role_id = ?"#,
            guild_id,
            role_id
        )
        .execute(&mut *connection)
        .await?
        .rows_affected();
        // handle product links
        deleted += sqlx::query!(
            r#"DELETE FROM product_role WHERE guild_id = ? AND role_id = ?"#,
            guild_id,
            role_id
        )
        .execute(&mut *connection)
        .await?
        .rows_affected();
        // handle product-version links
        deleted += sqlx::query!(
            r#"DELETE FROM product_version_role WHERE guild_id = ? AND role_id = ?"#,
            guild_id,
            role_id
        )
        .execute(&mut *connection)
        .await?
        .rows_affected();

        Ok(deleted)
    }

    /// Get role grants for a product ID. This includes blanket grants.
    pub async fn get_role_grants(
        &self,
        guild: GuildId,
        product_version_id: ProductVersionId,
    ) -> SqliteResult<Vec<RoleId>> {
        let guild_id = guild.get() as i64;
        let (product_id, version_id) = product_version_id.into_db_values();
        sqlx::query!(
        r#"SELECT blanket_role_id AS "role_id!" from guild WHERE guild_id = ? AND blanket_role_id IS NOT NULL
           UNION SELECT role_id AS "role_id!" FROM product_role WHERE guild_id = ? AND product_id = ?
           UNION SELECT role_id AS "role_id!" FROM product_version_role WHERE guild_id = ? AND product_id = ? AND version_id = ?"#,
        guild_id,
        guild_id,
        product_id,
        guild_id,
        product_id,
        version_id)
            .map(|row| RoleId::new(row.role_id as u64))
            .fetch_all(&self.read_pool)
            .await
    }

    /// Get roles for a product. This is ONLY product-level blanket grants.
    pub async fn get_linked_roles_for_product(&self, guild: GuildId, product_id: String) -> SqliteResult<Vec<RoleId>> {
        let guild_id = guild.get() as i64;
        // uses `role_lookup` index
        sqlx::query!(
            r#"SELECT role_id AS "role_id!" FROM product_role WHERE guild_id = ? AND product_id = ?"#,
            guild_id,
            product_id
        )
        .map(|row| RoleId::new(row.role_id as u64))
        .fetch_all(&self.read_pool)
        .await
    }

    /// Get roles for a product version. This does not include blanket grants.
    pub async fn get_linked_roles_for_product_version(
        &self,
        guild: GuildId,
        product_version_id: ProductVersionId,
    ) -> SqliteResult<Vec<RoleId>> {
        let guild_id = guild.get() as i64;
        let (product_id, version_id) = product_version_id.into_db_values();
        sqlx::query!(
            r#"SELECT role_id FROM product_version_role WHERE guild_id = ? AND product_id = ? AND version_id = ?"#,
            guild_id,
            product_id,
            version_id
        )
        .map(|row| RoleId::new(row.role_id as u64))
        .fetch_all(&self.read_pool)
        .await
    }

    pub async fn get_users_for_role(&self, guild: GuildId, role: RoleId) -> SqliteResult<Vec<UserId>> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        sqlx::query!(
            r#"SELECT user_id AS "user_id!" FROM license_activation LEFT JOIN guild USING (guild_id) WHERE guild_id = ? AND blanket_role_id = ?
               UNION SELECT user_id AS "user_id!" FROM license_activation LEFT JOIN product_role USING (guild_id, product_id) WHERE guild_id = ? AND role_id = ?
               UNION SELECT user_id AS "user_id!" FROM license_activation LEFT JOIN product_version_role USING (guild_id, product_id, version_id) WHERE guild_id = ? AND role_id = ?"#,
            guild_id,
            role_id,
            guild_id,
            role_id,
            guild_id,
            role_id
        )
        .map(|row| UserId::new(row.user_id as u64))
        .fetch_all(&self.read_pool)
        .await
    }

    /// get distinct roles from all links
    pub async fn get_linked_roles(&self, guild: GuildId) -> SqliteResult<Vec<RoleId>> {
        let guild_id = guild.get() as i64;
        sqlx::query!(
            r#"SELECT blanket_role_id AS "role_id!" FROM guild WHERE guild_id = ? AND blanket_role_id IS NOT NULL
               UNION SELECT role_id AS "role_id!" FROM product_role WHERE guild_id = ?
               UNION SELECT role_id AS "role_id!" FROM product_version_role WHERE guild_id = ?"#,
            guild_id,
            guild_id,
            guild_id,
        )
        .map(|row| RoleId::new(row.role_id as u64))
        .fetch_all(&self.read_pool)
        .await
    }

    /// get all links
    pub async fn get_links(
        &self,
        guild: GuildId,
    ) -> SqliteResult<HashMap<RoleId, Vec<LinkSource>, ahash::RandomState>> {
        let guild_id = guild.get() as i64;
        let mut connection = self.read_pool.acquire().await?;

        let mut map: HashMap<RoleId, Vec<LinkSource>, ahash::RandomState> = Default::default();

        // deal with global blanket
        {
            let blanket_result =
                sqlx::query_scalar!(r#"SELECT blanket_role_id from guild where guild_id = ?"#, guild_id)
                    .fetch_optional(&mut *connection)
                    .await?
                    .flatten()
                    .map(|role_id| RoleId::new(role_id as u64));
            if let Some(blanket_role) = blanket_result {
                map.entry(blanket_role).or_default().push(LinkSource::GlobalBlanket);
            }
        }

        // deal with product blankets
        {
            let mut product_result = sqlx::query!(
                r#"SELECT product_id, role_id FROM product_role WHERE guild_id = ?"#,
                guild_id
            )
            .map(|row| (RoleId::new(row.role_id as u64), row.product_id))
            .fetch(&mut *connection);
            while let Some((role, product_id)) = product_result.try_next().await? {
                map.entry(role)
                    .or_default()
                    .push(LinkSource::ProductBlanket { product_id });
            }
        }

        // deal with specific links
        {
            let mut product_version_result = sqlx::query!(
                r#"SELECT product_id, version_id, role_id FROM product_version_role WHERE guild_id = ?"#,
                guild_id
            )
            .map(|row| {
                (
                    RoleId::new(row.role_id as u64),
                    ProductVersionId::from_db_values(row.product_id, row.version_id),
                )
            })
            .fetch(&mut *connection);
            while let Some((role, product_version_id)) = product_version_result.try_next().await? {
                map.entry(role)
                    .or_default()
                    .push(LinkSource::ProductVersion(product_version_id));
            }
        }

        Ok(map)
    }

    /// Locally get all licences a users has been recorded to activate. This may be out of sync with Jinxxy!
    pub async fn get_user_licenses(&self, guild: GuildId, user_id: u64) -> SqliteResult<Vec<String>> {
        let guild_id = guild.get() as i64;
        let user_id = user_id as i64;
        sqlx::query_scalar!(
            r#"SELECT license_id FROM license_activation WHERE guild_id = ? AND user_id = ?"#,
            guild_id,
            user_id
        )
        .fetch_all(&self.read_pool)
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
        sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT * FROM license_activation WHERE guild_id = ? AND user_id = ? AND license_id = ?) AS "has_activations: bool""#,
            guild_id,
            user_id,
            license_id
        )
        .fetch_one(&self.read_pool)
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
        sqlx::query_scalar!(
            r#"SELECT license_activation_id FROM license_activation WHERE guild_id = ? AND user_id = ? AND license_id = ?"#,
            guild_id,
            user_id,
            license_id
        )
        .fetch_all(&self.read_pool)
        .await
    }

    /// Locally get all users that have activated the given license. This may be out of sync with Jinxxy!
    pub async fn get_license_users(&self, guild: GuildId, license_id: String) -> SqliteResult<Vec<u64>> {
        let guild_id = guild.get() as i64;
        //TODO: could use an index
        sqlx::query!(
            r#"SELECT user_id FROM license_activation WHERE guild_id = ? AND license_id = ?"#,
            guild_id,
            license_id
        )
        .map(|row| row.user_id as u64)
        .fetch_all(&self.read_pool)
        .await
    }

    /// Get DB size in bytes
    pub async fn size(&self) -> SqliteResult<u64> {
        sqlx::query!(r#"SELECT page_count * page_size AS "size!" FROM pragma_page_count(), pragma_page_size()"#)
            .map(|row| row.size as u64)
            .fetch_one(&self.read_pool)
            .await
    }

    /// Get count of license activations
    pub async fn license_activation_count(&self) -> SqliteResult<u64> {
        sqlx::query!(r#"SELECT count(*) AS "count!" FROM license_activation LEFT JOIN guild USING (guild_id) WHERE NOT guild.test"#)
            .map(|row| row.count as u64)
            .fetch_one(&self.read_pool)
            .await
    }

    /// Get count of distinct users who have activated licenses
    pub async fn distinct_user_count(&self) -> SqliteResult<u64> {
        sqlx::query!(r#"SELECT count(DISTINCT user_id) AS "count!" FROM license_activation LEFT JOIN guild USING (guild_id) WHERE NOT guild.test"#)
            .map(|row| row.count as u64)
            .fetch_one(&self.read_pool)
            .await
    }

    /// Get count of configured guilds
    pub async fn guild_count(&self) -> SqliteResult<u64> {
        sqlx::query!(r#"SELECT count(*) AS "count!" FROM guild WHERE NOT test"#)
            .map(|row| row.count as u64)
            .fetch_one(&self.read_pool)
            .await
    }

    /// Get count of distinct bot log channels
    pub async fn log_channel_count(&self) -> SqliteResult<u64> {
        sqlx::query!(r#"SELECT count(DISTINCT log_channel_id) AS "count!" FROM guild WHERE NOT test"#)
            .map(|row| row.count as u64)
            .fetch_one(&self.read_pool)
            .await
    }

    /// Get count of guilds with blanket role set
    pub async fn blanket_role_count(&self) -> SqliteResult<u64> {
        sqlx::query!(r#"SELECT count(*) AS "count!" FROM guild WHERE NOT guild.test AND blanket_role_id IS NOT NULL"#)
            .map(|row| row.count as u64)
            .fetch_one(&self.read_pool)
            .await
    }

    /// Get count of product->role mappings
    pub async fn product_role_count(&self) -> SqliteResult<u64> {
        sqlx::query!(
            r#"SELECT count(*) AS "count!" FROM product_role LEFT JOIN guild USING (guild_id) WHERE NOT guild.test"#
        )
        .map(|row| row.count as u64)
        .fetch_one(&self.read_pool)
        .await
    }

    /// Get count of product+version->role mappings
    pub async fn product_version_role_count(&self) -> SqliteResult<u64> {
        sqlx::query!(r#"SELECT count(*) AS "count!" FROM product_version_role LEFT JOIN guild USING (guild_id) WHERE NOT guild.test"#)
            .map(|row| row.count as u64)
            .fetch_one(&self.read_pool)
            .await
    }

    /// Get count of license activations in a guild
    pub async fn guild_license_activation_count(&self, guild: GuildId) -> SqliteResult<u64> {
        let guild_id = guild.get() as i64;
        sqlx::query!(r#"SELECT count(*) AS "count!" FROM license_activation LEFT JOIN guild USING (guild_id) WHERE guild.guild_id = ?"#, guild_id)
            .map(|row| row.count as u64)
            .fetch_one(&self.read_pool)
            .await
    }

    /// Get bot log channel
    pub async fn get_log_channel(&self, guild: GuildId) -> SqliteResult<Option<ChannelId>> {
        let guild_id = guild.get() as i64;

        // inner optional is for if the guild has no log channel set
        // outer optional is for if the guild does not exist in our DB
        let channel_id = sqlx::query_scalar!(r#"SELECT log_channel_id FROM guild WHERE guild_id = ?"#, guild_id)
            .fetch_optional(&self.read_pool)
            .await?;
        let channel_id = channel_id.flatten().map(|channel_id| ChannelId::new(channel_id as u64));
        Ok(channel_id)
    }

    /// Get all bot log channels.
    /// If `TEST_ONLY` is true, then only returns non-production servers. Otherwise, returns all servers.
    pub async fn get_log_channels<const TEST_ONLY: bool>(&self) -> SqliteResult<Vec<ChannelId>> {
        if TEST_ONLY {
            // only non-production servers
            sqlx::query!(
                r#"SELECT DISTINCT log_channel_id AS "channel_id!" FROM guild WHERE log_channel_id IS NOT NULL AND guild.test"#
            )
            .map(|row| ChannelId::new(row.channel_id as u64))
            .fetch_all(&self.read_pool)
            .await
        } else {
            // all servers, including production servers
            sqlx::query!(
                r#"SELECT DISTINCT log_channel_id AS "channel_id!" FROM guild WHERE log_channel_id IS NOT NULL"#
            )
            .map(|row| ChannelId::new(row.channel_id as u64))
            .fetch_all(&self.read_pool)
            .await
        }
    }

    /// Set or unset bot log channel
    pub async fn set_log_channel(&self, guild: GuildId, channel: Option<ChannelId>) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let channel_id = channel.map(|channel| channel.get() as i64);
        let mut connection = self.write_connection().await?;
        sqlx::query!(
            r#"INSERT INTO guild (guild_id, log_channel_id) VALUES (?, ?)
               ON CONFLICT (guild_id) DO UPDATE SET log_channel_id = excluded.log_channel_id"#,
            guild_id,
            channel_id,
        )
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    /// Set or unset this guild as a test guild
    pub async fn set_test(&self, guild: GuildId, test: bool) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let mut connection = self.write_connection().await?;
        sqlx::query!(
            r#"INSERT INTO guild (guild_id, test) VALUES (?, ?)
               ON CONFLICT (guild_id) DO UPDATE SET test = excluded.test"#,
            guild_id,
            test
        )
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    /// Check if a guild is a test guild
    pub async fn is_test_guild(&self, guild: GuildId) -> SqliteResult<bool> {
        let guild_id = guild.get() as i64;
        sqlx::query_scalar!(
            r#"SELECT test AS "is_test: bool" FROM guild WHERE guild_id = ?"#,
            guild_id
        )
        .fetch_one(&self.read_pool)
        .await
    }

    /// Set or unset this guild as an owner guild (gets extra slash commands)
    pub async fn set_owner_guild(&self, guild: GuildId, owner: bool) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let mut connection = self.write_connection().await?;
        sqlx::query!(
            r#"INSERT INTO guild (guild_id, owner) VALUES (?, ?)
               ON CONFLICT (guild_id) DO UPDATE SET owner = excluded.owner"#,
            guild_id,
            owner
        )
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    /// Check if a guild is an owner guild (gets extra slash commands)
    pub async fn is_owner_guild(&self, guild: GuildId) -> SqliteResult<bool> {
        let guild_id = guild.get() as i64;

        let is_owner_guild = sqlx::query_scalar!(
            r#"SELECT owner AS "is_owner: bool" FROM guild WHERE guild_id = ?"#,
            guild_id
        )
        .fetch_optional(&self.read_pool)
        .await?
        .unwrap_or(false);
        Ok(is_owner_guild)
    }

    /// Check gumroad failure count for a guild
    pub async fn get_gumroad_failure_count(&self, guild: GuildId) -> SqliteResult<Option<u64>> {
        let guild_id = guild.get() as i64;
        sqlx::query!(
            r#"SELECT gumroad_failure_count FROM guild WHERE guild_id = ?"#,
            guild_id
        )
        .map(|row| row.gumroad_failure_count as u64)
        .fetch_optional(&self.read_pool)
        .await
    }

    /// Increment gumroad failure count for a guild
    pub async fn increment_gumroad_failure_count(&self, guild: GuildId) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let mut connection = self.write_connection().await?;
        sqlx::query!(
            r#"UPDATE guild SET gumroad_failure_count = gumroad_failure_count + 1 WHERE guild_id = ?"#,
            guild_id
        )
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    /// Get tuples of `(guild_id, log_channel_id)` with pending gumroad nag
    pub async fn get_guilds_pending_gumroad_nag(&self) -> SqliteResult<Vec<GuildGumroadInfo>> {
        sqlx::query!(
            r#"SELECT guild_id, log_channel_id AS "log_channel_id!", gumroad_failure_count FROM guild
               WHERE log_channel_id IS NOT NULL AND gumroad_nag_count < 1 AND gumroad_failure_count >= 10
               AND (gumroad_failure_count * 5) > (SELECT count(*) FROM license_activation WHERE license_activation.guild_id = guild.guild_id)"#
        )
        .map(|row| GuildGumroadInfo {
            guild_id: GuildId::new(row.guild_id as u64),
            log_channel_id: ChannelId::new(row.log_channel_id as u64),
            gumroad_failure_count: row.gumroad_failure_count as u64,
        })
        .fetch_all(&self.read_pool)
        .await
    }

    /// Check gumroad nag count for a guild
    pub async fn get_gumroad_nag_count(&self, guild: GuildId) -> SqliteResult<Option<u64>> {
        let guild_id = guild.get() as i64;
        sqlx::query!(r#"SELECT gumroad_nag_count FROM guild WHERE guild_id = ?"#, guild_id)
            .map(|row| row.gumroad_nag_count as u64)
            .fetch_optional(&self.read_pool)
            .await
    }

    /// Increment gumroad nag count for a guild
    pub async fn increment_gumroad_nag_count(&self, guild: GuildId) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let mut connection = self.write_connection().await?;
        sqlx::query!(
            r#"UPDATE guild SET gumroad_nag_count = gumroad_nag_count + 1 WHERE guild_id = ?"#,
            guild_id
        )
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    pub async fn get_guild_cache(&self, guild: GuildId) -> SqliteResult<GuildCache> {
        let mut connection = self.read_pool.acquire().await?;
        let cache_time = helper::get_cache_time(&mut connection, guild).await?;
        let product_name_info = helper::product_names_in_guild(&mut connection, guild).await?;
        let product_version_name_info = helper::product_version_names_in_guild(&mut connection, guild).await?;
        Ok(GuildCache {
            product_name_info,
            product_version_name_info,
            cache_time,
        })
    }

    pub async fn persist_guild_cache(&self, guild: GuildId, cache_entry: GuildCache) -> SqliteResult<()> {
        let mut connection = self.write_connection().await?;
        helper::persist_product_names(&mut connection, guild, cache_entry.product_name_info).await?;
        helper::persist_product_version_names(&mut connection, guild, cache_entry.product_version_name_info).await?;
        helper::set_cache_time(&mut connection, guild, cache_entry.cache_time).await?;
        Ok(())
    }

    /// Delete all cache entries for all guilds
    pub async fn clear_cache(&self) -> SqliteResult<()> {
        let mut connection = self.write_connection().await?;
        sqlx::query!(r#"DELETE FROM product"#).execute(&mut *connection).await?;
        sqlx::query!(r#"DELETE FROM product_version"#)
            .execute(&mut *connection)
            .await?;
        sqlx::query!(r#"UPDATE guild SET cache_time_unix_ms = 0"#)
            .execute(&mut *connection)
            .await?;
        Ok(())
    }

    /// Get cached name info for products in a guild
    pub async fn product_names_in_guild(&self, guild: GuildId) -> SqliteResult<Vec<ProductNameInfo>> {
        let mut connection = self.read_pool.acquire().await?;
        helper::product_names_in_guild(&mut connection, guild).await
    }

    /// Get versions for a product
    pub async fn product_versions(
        &self,
        guild: GuildId,
        product_id: String,
    ) -> SqliteResult<Vec<ProductVersionNameInfo>> {
        let guild_id = guild.get() as i64;
        sqlx::query!(
            r#"SELECT version_id, product_version_name FROM product_version WHERE guild_id = ? AND product_id = ?"#,
            guild_id,
            product_id
        )
        .map(|row| ProductVersionNameInfo {
            id: ProductVersionId {
                product_id: product_id.clone(),
                product_version_id: Some(row.version_id),
            },
            product_version_name: row.product_version_name,
        })
        .fetch_all(&self.read_pool)
        .await
    }
}

/// Helper functions that don't access a whole pool
mod helper {
    use super::*;

    /// Get a single setting from the `settings` table. Note that if your setting is nullable you MUST read it as an
    /// Option<T> instead of a T. This function returns None only if the entire row is absent.
    pub(crate) async fn get_setting<'e, T>(connection: &'e mut SqliteConnection, key: &str) -> SqliteResult<Option<T>>
    where
        T: Type<Sqlite> + Send + Unpin + 'e,
        (T,): for<'r> FromRow<'r, SqliteRow>, // what the fuck is this
    {
        let result: Option<T> = sqlx::query_scalar(r#"SELECT value FROM settings WHERE key = ?"#)
            .bind(key)
            .fetch_optional(connection)
            .await?;
        Ok(result)
    }

    pub(super) async fn set_setting<'q, T>(
        connection: &mut SqliteConnection,
        key: &'q str,
        value: T,
    ) -> SqliteResult<bool>
    where
        T: Encode<'q, Sqlite> + Type<Sqlite> + 'q,
    {
        let update_count = sqlx::query(r#"INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)"#)
            .bind(key)
            .bind(value)
            .execute(connection)
            .await?
            .rows_affected();
        Ok(update_count != 0)
    }

    pub(crate) async fn get_cache_time(connection: &mut SqliteConnection, guild: GuildId) -> SqliteResult<SimpleTime> {
        let guild_id = guild.get() as i64;
        let cache_time_unix_ms =
            sqlx::query_scalar!(r#"SELECT cache_time_unix_ms FROM guild WHERE guild_id = ?"#, guild_id)
                .fetch_one(connection)
                .await?;
        Ok(SimpleTime::from_unix_millis(cache_time_unix_ms as u64))
    }

    pub(super) async fn set_cache_time(
        connection: &mut SqliteConnection,
        guild: GuildId,
        time: SimpleTime,
    ) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let cache_time_unix_ms = time.as_epoch_millis() as i64;
        sqlx::query!(
            r#"INSERT INTO guild (guild_id, cache_time_unix_ms) VALUES (?, ?)
               ON CONFLICT (guild_id) DO UPDATE SET cache_time_unix_ms = excluded.cache_time_unix_ms"#,
            guild_id,
            cache_time_unix_ms
        )
        .execute(connection)
        .await?;
        Ok(())
    }

    /// Get cached name info for products in a guild
    pub(super) async fn product_names_in_guild(
        connection: &mut SqliteConnection,
        guild: GuildId,
    ) -> SqliteResult<Vec<ProductNameInfo>> {
        let guild_id = guild.get() as i64;
        sqlx::query!(
            r#"SELECT product_id, product_name, etag FROM product WHERE guild_id = ?"#,
            guild_id
        )
        .map(|row| ProductNameInfo {
            id: row.product_id,
            value: ProductNameInfoValue {
                product_name: row.product_name,
                etag: row.etag,
            },
        })
        .fetch_all(connection)
        .await
    }

    /// Get name info for products versions in a guild
    pub(super) async fn product_version_names_in_guild(
        connection: &mut SqliteConnection,
        guild: GuildId,
    ) -> SqliteResult<Vec<ProductVersionNameInfo>> {
        let guild_id = guild.get() as i64;
        sqlx::query!(
            r#"SELECT product_id, version_id, product_version_name FROM product_version WHERE guild_id = ?"#,
            guild_id
        )
        .map(|row| ProductVersionNameInfo {
            id: ProductVersionId {
                product_id: row.product_id,
                product_version_id: Some(row.version_id),
            },
            product_version_name: row.product_version_name,
        })
        .fetch_all(connection)
        .await
    }

    pub(super) async fn persist_product_names(
        connection: &mut SqliteConnection,
        guild: GuildId,
        product_name_info: Vec<ProductNameInfo>,
    ) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let mut new_key_set = HashSet::with_capacity_and_hasher(product_name_info.len(), ahash::RandomState::default());
        let mut unexpected_keys = Vec::new();

        // step 1: insert all entries and keep track of their keys in a set for later
        for info in product_name_info {
            let product_id = info.id;
            let product_name = info.value.product_name;
            let etag = info.value.etag;
            sqlx::query!(
                r#"INSERT INTO product (guild_id, product_id, product_name, etag) VALUES (?, ?, ?, ?)
                   ON CONFLICT (guild_id, product_id) DO UPDATE SET product_name = excluded.product_name, etag = excluded.etag"#,
                guild_id,
                product_id,
                product_name,
                etag
            )
                .execute(&mut *connection)
                .await?;
            new_key_set.insert(product_id);
        }

        // step 2: query all existing keys, and record any keys that we did NOT just insert
        {
            let mut rows = sqlx::query_scalar!(r#"SELECT product_id FROM product WHERE guild_id = ?"#, guild_id)
                .fetch(&mut *connection);
            while let Some(product_id) = rows.try_next().await? {
                if !new_key_set.contains(&product_id) {
                    unexpected_keys.push(product_id);
                }
            }
        }

        // step 3: delete any rows with keys that we did NOT just insert
        for product_id in unexpected_keys {
            sqlx::query!(
                r#"DELETE FROM product WHERE guild_id = ? AND product_id = ?"#,
                guild_id,
                product_id
            )
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    pub(super) async fn persist_product_version_names(
        connection: &mut SqliteConnection,
        guild: GuildId,
        product_version_name_info: Vec<ProductVersionNameInfo>,
    ) -> SqliteResult<()> {
        let guild_id = guild.get() as i64;
        let mut new_key_set =
            HashSet::with_capacity_and_hasher(product_version_name_info.len(), ahash::RandomState::default());
        let mut unexpected_keys = Vec::new();
        // step 1: insert all entries and keep track of their keys in a set for later
        for info in product_version_name_info {
            let (product_id, version_id) = info.id.into_db_values();
            let product_version_name = info.product_version_name;
            sqlx::query!(
            r#"INSERT INTO product_version (guild_id, product_id, version_id, product_version_name) VALUES (?, ?, ?, ?)
               ON CONFLICT (guild_id, product_id, version_id) DO UPDATE SET product_version_name = excluded.product_version_name"#,
                guild_id,
                product_id,
                version_id,
                product_version_name
            )
                .execute(&mut *connection)
                .await?;
            new_key_set.insert((product_id, version_id));
        }

        // step 2: query all existing keys, and record any keys that we did NOT just insert
        {
            let mut rows = sqlx::query!(
                r#"SELECT product_id, version_id FROM product_version WHERE guild_id = ?"#,
                guild_id
            )
            .fetch(&mut *connection);
            while let Some(row) = rows.try_next().await? {
                let product_id: String = row.product_id;
                let version_id: String = row.version_id;
                let key = (product_id, version_id);
                if !new_key_set.contains(&key) {
                    unexpected_keys.push(key);
                }
            }
        }

        // step 3: delete any rows with keys that we did NOT just insert
        for (product_id, version_id) in unexpected_keys {
            sqlx::query!(
                r#"DELETE FROM product_version WHERE guild_id = ? AND product_id = ? AND version_id = ?"#,
                guild_id,
                product_id,
                version_id
            )
            .execute(&mut *connection)
            .await?;
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
