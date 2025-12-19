// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

mod schema_v1;
mod schema_v2;

use crate::error::JinxError;
use crate::http::jinxxy::{ProductNameInfo, ProductNameInfoValue, ProductVersionId, ProductVersionNameInfo};
use crate::time::SimpleTime;
use poise::futures_util::TryStreamExt;
use poise::serenity_prelude as serenity;
use serenity::{ChannelId, GuildId, RoleId, UserId};
use sqlx::{
    ConnectOptions, Connection, Encode, Executor, FromRow, Pool, Sqlite, SqliteConnection,
    error::Error as SqlxError,
    pool::PoolConnection,
    sqlite::{
        SqliteAutoVacuum, SqliteConnectOptions, SqliteJournalMode, SqliteLockingMode, SqlitePoolOptions, SqliteRow,
        SqliteSynchronous,
    },
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

const DB_V1_FILENAME: &str = "jinx.sqlite";
const DB_V2_FILENAME: &str = "jinx2.sqlite";
const DISCORD_TOKEN_KEY: &str = "discord_token";
const LOW_PRIORITY_CACHE_EXPIRY_SECONDS: &str = "low_priority_cache_expiry_seconds";

type JinxResult<T> = Result<T, JinxError>;
type SqliteResult<T> = Result<T, SqlxError>;

/// Cloning is by-reference.
#[derive(Clone)]
pub struct JinxDb {
    read_pool: Pool<Sqlite>,
    write_pool: Pool<Sqlite>,
    new_db: bool,
}

impl JinxDb {
    /// Open a new database
    pub async fn open() -> JinxResult<Self> {
        let pool_options_write = SqlitePoolOptions::new()
            .min_connections(1) // always keep at least one connection open
            .max_connections(1) // allow only 1 write connection
            .max_lifetime(None) // don't close connections for no reason, as we assume sqlite doesn't leak resources
            .test_before_acquire(false) // we assume sqlite is extremely reliable, as it's in-process
            .acquire_slow_threshold(Duration::from_millis(100)) // we expect sqlite to be fast
            .idle_timeout(Some(Duration::from_secs(90))); // idle extra connections may be closed after a while
        let pool_options_read = pool_options_write.clone().max_connections(4); // allow up to 4 read connections
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
            .synchronous(SqliteSynchronous::Normal) // small possibility a transaction may be rolled back on OS crash or power-off
            .auto_vacuum(SqliteAutoVacuum::None)
            .page_size(4096)
            .pragma("trusted_schema", "OFF"); // all applications are encouraged to switch this setting off on every database connection as soon as that connection is opened
        let connect_options_read = connect_options_write.clone().read_only(true).create_if_missing(false);

        let new_db = !Path::new(DB_V2_FILENAME).is_file();
        let write_pool = pool_options_write.connect_with(connect_options_write).await?;
        let mut write_connection = write_pool.acquire().await?;
        schema_v2::init(&mut write_connection).await?;

        let read_pool = pool_options_read.connect_with(connect_options_read).await?;

        let db = JinxDb {
            read_pool,
            write_pool,
            new_db,
        };
        Ok(db)
    }

    pub async fn migrate(&self) -> JinxResult<()> {
        if Path::new(DB_V1_FILENAME).is_file() {
            if self.new_db {
                // the v2 DB does not exist, so we must initialize the v1 db and migrate it to v2
                let connect_options_v1 = SqliteConnectOptions::new()
                    .filename(DB_V1_FILENAME)
                    .foreign_keys(false) // note that this actually IS required as sqlx overrides the normal sqlite default here
                    .in_memory(false)
                    .shared_cache(false)
                    .journal_mode(SqliteJournalMode::Delete)
                    .locking_mode(SqliteLockingMode::Normal)
                    .read_only(false) // we might have to perform version migrations
                    .create_if_missing(false)
                    .synchronous(SqliteSynchronous::Full)
                    .auto_vacuum(SqliteAutoVacuum::None)
                    .page_size(4096)
                    .pragma("trusted_schema", "OFF");
                let mut v1_connection = connect_options_v1.connect().await?;
                // handle any pending migrations on the v1 db
                schema_v1::init(&mut v1_connection).await?;

                // perform the big migration
                {
                    let mut write_connection = self.write_pool.acquire().await?;
                    let (read_transaction, write_transaction) =
                        tokio::join!(v1_connection.begin(), write_connection.begin());
                    let mut read_transaction = read_transaction?;
                    let mut write_transaction = write_transaction?;
                    schema_v2::copy_from_v1(&mut read_transaction, &mut write_transaction).await?;
                    let (read_commit, write_commit) =
                        tokio::join!(read_transaction.commit(), write_transaction.commit());
                    let () = read_commit?;
                    let () = write_commit?;
                }
                v1_connection.close().await?;

                Ok(())
            } else {
                Err(JinxError::new(
                    "attempted to perform v1->v2 DB migration into an existing v2 DB",
                ))
            }
        } else {
            Err(JinxError::new(
                "attempted to perform v1->v2 DB migration when v1 DB does not exist",
            ))
        }
    }

    /// Gracefully the database connections and wait for the close to complete
    pub async fn close(&self) {
        self.read_pool.close().await;
        self.write_pool.close().await;
    }

    /// Get something that we can DerefMut as SqliteConnection
    async fn write_connection(&self) -> SqliteResult<PoolConnection<Sqlite>> {
        self.write_pool.acquire().await
    }

    /// Attempt to optimize the database.
    ///
    /// Applications that use long-lived database connections should run "PRAGMA optimize;" periodically, perhaps once per day or once per hour.
    pub async fn optimize(&self) -> JinxResult<()> {
        let mut connection = self.write_connection().await?;
        connection.execute(r#"PRAGMA optimize"#).await?;
        Ok(())
    }

    async fn get_setting<'e, T>(&self, key: &str) -> SqliteResult<Option<T>>
    where
        T: sqlx::Type<Sqlite> + Send + Unpin + 'e,
        (T,): for<'r> FromRow<'r, SqliteRow>, // what the fuck is this
    {
        let mut connection = self.write_connection().await?;
        helper::get_setting(&mut connection, key).await
    }

    async fn set_setting<'q, T>(&self, key: &'q str, value: T) -> SqliteResult<bool>
    where
        T: Encode<'q, Sqlite> + sqlx::Type<Sqlite> + 'q,
    {
        let mut connection = self.write_connection().await?;
        helper::set_setting(&mut connection, key, value).await
    }

    pub async fn add_owner(&self, owner_id: u64) -> JinxResult<()> {
        let owner_id = owner_id as i64;
        let mut connection = self.write_connection().await?;
        sqlx::query!(r#"INSERT OR IGNORE INTO owner (owner_id) VALUES (?)"#, owner_id)
            .execute(&mut *connection)
            .await?;
        Ok(())
    }

    pub async fn delete_owner(&self, owner_id: u64) -> JinxResult<()> {
        let owner_id = owner_id as i64;
        let mut connection = self.write_connection().await?;
        sqlx::query!(r#"DELETE FROM owner WHERE owner_id = ?"#, owner_id)
            .execute(&mut *connection)
            .await?;
        Ok(())
    }

    pub async fn set_discord_token(&self, discord_token: &str) -> JinxResult<()> {
        self.set_setting(DISCORD_TOKEN_KEY, discord_token).await?;
        Ok(())
    }

    pub async fn get_owners(&self) -> JinxResult<Vec<u64>> {
        let result = sqlx::query!(r#"SELECT owner_id FROM owner"#)
            .map(|row| row.owner_id as u64)
            .fetch_all(&self.read_pool)
            .await?;
        Ok(result)
    }

    pub async fn is_user_owner(&self, owner_id: u64) -> JinxResult<bool> {
        let owner_id = owner_id as i64;
        let result = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT * FROM owner WHERE owner_id = ?) AS "is_owner: bool""#,
            owner_id
        )
        .fetch_one(&self.read_pool)
        .await?;
        Ok(result)
    }

    pub async fn get_discord_token(&self) -> JinxResult<Option<String>> {
        let result = self.get_setting(DISCORD_TOKEN_KEY).await?;
        Ok(result)
    }

    pub async fn set_low_priority_cache_expiry_time(&self, low_priority_cache_expiry_time: Duration) -> JinxResult<()> {
        self.set_setting(
            LOW_PRIORITY_CACHE_EXPIRY_SECONDS,
            low_priority_cache_expiry_time.as_secs() as i64,
        )
        .await?;
        Ok(())
    }

    pub async fn get_low_priority_cache_expiry_time(&self) -> JinxResult<Option<Duration>> {
        let low_priority_cache_expiry_time = self
            .get_setting::<i64>(LOW_PRIORITY_CACHE_EXPIRY_SECONDS)
            .await?
            .map(|secs| Duration::from_secs(secs as u64));
        Ok(low_priority_cache_expiry_time)
    }

    /// Locally record that we've activated a license for a user
    pub async fn activate_license(
        &self,
        jinxxy_user_id: &str,
        license_id: &str,
        license_activation_id: &str,
        activator_user_id: u64,
        product_id: Option<&str>,
        version_id: Option<&str>,
    ) -> JinxResult<()> {
        let activator_user_id = activator_user_id as i64;
        let mut connection = self.write_connection().await?;
        sqlx::query!(
            r#"INSERT OR IGNORE INTO license_activation (jinxxy_user_id, license_id, license_activation_id, activator_user_id, product_id, version_id) VALUES (?, ?, ?, ?, ?, ?)"#,
            jinxxy_user_id,
            license_id,
            license_activation_id,
            activator_user_id,
            product_id,
            version_id
        )
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    /// Locally record that we've deactivated a license for a user. Returns `true` if a row was found and deleted, or `false` if no row was found to delete.
    pub async fn deactivate_license(
        &self,
        jinxxy_user_id: &str,
        license_id: &str,
        license_activation_id: &str,
        activator_user_id: u64,
    ) -> JinxResult<bool> {
        let activator_user_id = activator_user_id as i64;
        let mut connection = self.write_connection().await?;
        let delete_count = sqlx::query!(
            r#"DELETE FROM license_activation WHERE jinxxy_user_id = ? AND license_id = ? AND license_activation_id = ? AND activator_user_id = ?"#,
            jinxxy_user_id,
            license_id,
            license_activation_id,
            activator_user_id
        )
        .execute(&mut *connection)
        .await?
        .rows_affected();
        Ok(delete_count != 0)
    }

    /// Locally check if a license is locked. This may be out of sync with Jinxxy!
    pub async fn is_license_locked(&self, jinxxy_user_id: &str, license_id: &str) -> JinxResult<bool> {
        let result = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT * FROM license_activation WHERE jinxxy_user_id = ? AND license_id = ? AND activator_user_id = 0) AS "is_locked: bool""#,
            jinxxy_user_id,
            license_id
        )
        .fetch_one(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// this returns a `jinxxy_user_id: String` for all stores
    pub async fn get_all_stores(&self) -> JinxResult<Vec<String>> {
        let result = sqlx::query_scalar!(r#"SELECT jinxxy_user_id FROM jinxxy_user"#)
            .fetch_all(&self.read_pool)
            .await?;
        Ok(result)
    }

    /// Set Jinxxy API key for this guild. This creates a guild link, and if the guild does not exist it creates the guild.
    pub async fn set_jinxxy_api_key(
        &self,
        guild: GuildId,
        jinxxy_user_id: &str,
        jinxxy_username: Option<&str>,
        api_key: &str,
    ) -> JinxResult<()> {
        let guild_id = guild.get() as i64;
        let mut connection = self.write_connection().await?;
        let mut transaction = connection.begin().await?;

        // ensure the guild exists
        sqlx::query!(
            r#"INSERT INTO guild (guild_id)
               VALUES (?)
               ON CONFLICT (guild_id) DO NOTHING"#,
            guild_id
        )
        .execute(&mut *transaction)
        .await?;

        // ensure the jinxxy user exists, and also make sure the stored username is fully up to date
        sqlx::query!(
            r#"INSERT INTO jinxxy_user (jinxxy_user_id, jinxxy_username)
               VALUES (?, ?)
               ON CONFLICT (jinxxy_user_id) DO UPDATE
               SET jinxxy_username = excluded.jinxxy_username"#,
            jinxxy_user_id,
            jinxxy_username
        )
        .execute(&mut *transaction)
        .await?;

        // set the api key
        sqlx::query!(
            r#"INSERT INTO jinxxy_user_guild (guild_id, jinxxy_user_id, jinxxy_api_key, jinxxy_api_key_valid)
               VALUES (?, ?, ?, TRUE)
               ON CONFLICT (guild_id, jinxxy_user_id) DO UPDATE
               SET jinxxy_api_key = excluded.jinxxy_api_key, jinxxy_api_key_valid = excluded.jinxxy_api_key_valid"#,
            guild_id,
            jinxxy_user_id,
            api_key
        )
        .execute(&mut *transaction)
        .await?;

        transaction.commit().await?;
        Ok(())
    }

    /// This deletes MOST of a guild's data:
    ///
    /// - all guild:store links for this guild, represented as rows in the `jinxxy_user_guild` table
    /// - most guild settings in the `guild` table *except* certain values that should persist forever in case the guild
    ///   is restored later, including:
    ///   - gumroad_failure_count and gumroad_nag_count, as your sins must not be forgotten
    ///   - default_jinxxy_user, as we cannot guarantee you've destroyed all your legacy register buttons
    /// - all `product_role` and `product_version_role` entries referencing the removed `jinxxy_user_guild` entries
    ///   (handled automatically by ON DELETE CASCADE)
    /// - all `jinxxy_user` entries that are no longer linked to any guild via `jinxxy_user_guild`
    ///
    /// Finally, this returns `jinxxy_user_id: String` for all stores that have been deleted. These store IDs must be
    /// subsequently unregistered from the cache background job.
    pub async fn delete_guild(&self, guild: GuildId) -> JinxResult<Vec<String>> {
        let guild_id = guild.get() as i64;
        let mut connection = self.write_connection().await?;
        let mut transaction = connection.begin().await?;

        // delete guild:store links. This cascades to role setups.
        sqlx::query!(r#"DELETE FROM jinxxy_user_guild WHERE guild_id = ?"#, guild_id)
            .execute(&mut *transaction)
            .await?;

        // note that if the guild's row is somehow not present, this is a no-op, which is totally fine.
        sqlx::query!(
            r#"UPDATE guild SET log_channel_id = NULL, test = FALSE, owner = FALSE, blanket_role_id = NULL
               WHERE guild_id = ?"#,
            guild_id
        )
        .execute(&mut *transaction)
        .await?;

        let deleted_users = sqlx::query_scalar!(
            r#"DELETE from jinxxy_user WHERE jinxxy_user_id NOT IN (SELECT jinxxy_user_id FROM jinxxy_user_guild) RETURNING jinxxy_user_id"#
        )
        .fetch_all(&mut *transaction)
        .await?;

        transaction.commit().await?;

        Ok(deleted_users)
    }

    /// Get a specific Jinxxy API key for this guild
    pub async fn get_jinxxy_api_key(&self, guild: GuildId, jinxxy_user_id: &str) -> JinxResult<Option<String>> {
        let guild_id = guild.get() as i64;
        let api_key = sqlx::query_scalar!(
            r#"SELECT jinxxy_api_key FROM jinxxy_user_guild WHERE guild_id = ? AND jinxxy_user_id = ?"#,
            guild_id,
            jinxxy_user_id
        )
        .fetch_optional(&self.read_pool)
        .await?;
        Ok(api_key)
    }

    /// Get an arbitrary Jinxxy API key for this store.
    pub async fn get_arbitrary_jinxxy_api_key(&self, jinxxy_user_id: &str) -> JinxResult<Option<GuildApiKey>> {
        let api_key = sqlx::query!(
            r#"SELECT guild_id, jinxxy_api_key FROM jinxxy_user_guild WHERE jinxxy_user_id = ? AND jinxxy_api_key_valid LIMIT 1"#,
            jinxxy_user_id
        )
            .map(|row| GuildApiKey {
                guild_id: GuildId::new(row.guild_id as u64),
                jinxxy_api_key: row.jinxxy_api_key,
            })
            .fetch_optional(&self.read_pool)
            .await?;
        Ok(api_key)
    }

    /// Get an all linked stores for this guild
    pub async fn get_store_links(&self, guild: GuildId) -> JinxResult<Vec<LinkedStore>> {
        let guild_id = guild.get() as i64;
        let result = sqlx::query_as!(
            LinkedStore,
            r#"SELECT jinxxy_user_id, jinxxy_username, jinxxy_api_key FROM jinxxy_user_guild
               LEFT JOIN jinxxy_user USING (jinxxy_user_id)
               WHERE guild_id = ?"#,
            guild_id
        )
        .fetch_all(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Get an all linked store's user ids for this guild
    pub async fn get_store_link_user_ids(&self, guild: GuildId) -> JinxResult<Vec<String>> {
        let guild_id = guild.get() as i64;
        let result = sqlx::query_scalar!(
            r#"SELECT jinxxy_user_id FROM jinxxy_user_guild
               LEFT JOIN jinxxy_user USING (jinxxy_user_id)
               WHERE guild_id = ?"#,
            guild_id
        )
        .fetch_all(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Get a specific store link for this guild using the provided username. Returns `None` if no link
    /// with that username exists.
    pub async fn get_store_link(&self, guild: GuildId, jinxxy_username: &str) -> JinxResult<Option<LinkedStore>> {
        let guild_id = guild.get() as i64;
        let result = sqlx::query_as!(
            LinkedStore,
            r#"SELECT jinxxy_user_id, jinxxy_username, jinxxy_api_key FROM jinxxy_user_guild
               LEFT JOIN jinxxy_user USING (jinxxy_user_id)
               WHERE guild_id = ?
               AND jinxxy_username = ?
               LIMIT 1"#,
            guild_id,
            jinxxy_username,
        )
        .fetch_optional(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Pure sqlite autocompletion of Jinxxy usernames.
    ///
    /// Caveats:
    /// - returned strings are truncated to at most 100 characters
    /// - returned strings are deduplicated *after* the above truncation step
    /// - usernames may not be fully up to date if they were changed in Jinxxy
    /// - it is possible for anonymous Jinxxy accounts to have no username set. I do not believe this is possible for sellers.
    ///
    /// This should generally only return a single username in the normal case: only weirdos who have multiple stores per guild
    /// will actually see multiple results listed here.
    pub async fn autocomplete_jinxxy_username(
        &self,
        guild: GuildId,
        jinxxy_username_prefix: &str,
    ) -> JinxResult<Vec<String>> {
        let guild_id = guild.get() as i64;

        // turn the prefix into a sqlite LIKE pattern
        let mut like_pattern = helper::escape_like(jinxxy_username_prefix).into_owned();
        like_pattern.push('%');

        let result = sqlx::query_scalar!(
            r#"SELECT DISTINCT substr(jinxxy_username, 1, 100) AS "username!: String" FROM jinxxy_user_guild
               LEFT JOIN jinxxy_user USING (jinxxy_user_id)
               WHERE guild_id = ?
               AND jinxxy_username IS NOT NULL
               AND jinxxy_username LIKE ? ESCAPE '?'"#,
            guild_id,
            like_pattern,
        )
        .fetch_all(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Mark an API key as valid, or invalid and excluded from use in the background cache job
    pub async fn set_jinxxy_api_key_validity(&self, jinxxy_api_key: &str, valid: bool) -> JinxResult<()> {
        let mut connection = self.write_connection().await?;
        sqlx::query!(
            r#"UPDATE jinxxy_user_guild SET jinxxy_api_key_valid = ? WHERE jinxxy_api_key = ?"#,
            valid,
            jinxxy_api_key
        )
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    /// Check if this guild has any Jinxxy stores linked
    pub async fn has_jinxxy_linked(&self, guild: GuildId) -> JinxResult<bool> {
        let guild_id = guild.get() as i64;
        let result = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT * FROM jinxxy_user_guild WHERE guild_id = ?) AS "has_key: bool""#,
            guild_id
        )
        .fetch_one(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Get the default Jinxxy user id for a guild. For legacy guilds this **should** be set, and is used for legacy
    /// buttons without Jinxxy user id embedded.
    pub async fn get_default_jinxxy_user_id(&self, guild: GuildId) -> JinxResult<Option<String>> {
        let guild_id = guild.get() as i64;
        let jinxxy_user_id =
            sqlx::query_scalar!(r#"SELECT default_jinxxy_user FROM guild WHERE guild_id = ?"#, guild_id)
                .fetch_optional(&self.read_pool)
                .await?;
        Ok(jinxxy_user_id.flatten())
    }

    /// Set or unset blanket role
    pub async fn set_blanket_role_id(&self, guild: GuildId, role_id: Option<RoleId>) -> JinxResult<()> {
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
    pub async fn link_product(
        &self,
        jinxxy_user_id: &str,
        guild: GuildId,
        product_id: &str,
        role: RoleId,
    ) -> JinxResult<()> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        let mut connection = self.write_connection().await?;
        sqlx::query!(
            r#"INSERT OR IGNORE INTO product_role (jinxxy_user_id, guild_id, product_id, role_id) VALUES (?, ?, ?, ?)"#,
            jinxxy_user_id,
            guild_id,
            product_id,
            role_id
        )
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    /// blanket unlink a Jinxxy product and a role. Returns `true` if a row was found and deleted, or `false` if no row was found to delete.
    pub async fn unlink_product(
        &self,
        jinxxy_user_id: &str,
        guild: GuildId,
        product_id: &str,
        role: RoleId,
    ) -> JinxResult<bool> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        let mut connection = self.write_connection().await?;
        let delete_count = sqlx::query!(
            r#"DELETE FROM product_role WHERE jinxxy_user_id = ? AND guild_id = ? AND product_id = ? AND role_id = ?"#,
            jinxxy_user_id,
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
        jinxxy_user_id: &str,
        guild: GuildId,
        product_version_id: &ProductVersionId,
        role: RoleId,
    ) -> JinxResult<()> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        let (product_id, version_id) = product_version_id.as_db_values();
        let mut connection = self.write_connection().await?;
        sqlx::query!(
            r#"INSERT OR IGNORE INTO product_version_role (jinxxy_user_id, guild_id, product_id, version_id, role_id) VALUES (?, ?, ?, ?, ?)"#,
            jinxxy_user_id,
            guild_id,
            product_id,
            version_id,
            role_id
        )
        .execute(&mut *connection)
        .await?;
        Ok(())
    }

    /// unlink a Jinxxy product-version and a role. Returns `true` if a row was found and deleted, or `false` if no row was found to delete.
    pub async fn unlink_product_version(
        &self,
        jinxxy_user_id: &str,
        guild: GuildId,
        product_version_id: &ProductVersionId,
        role: RoleId,
    ) -> JinxResult<bool> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        let (product_id, version_id) = product_version_id.as_db_values();
        let mut connection = self.write_connection().await?;
        let delete_count = sqlx::query!(
            r#"DELETE FROM product_version_role WHERE jinxxy_user_id = ? AND guild_id = ? AND product_id = ? AND version_id = ? AND role_id = ?"#,
            jinxxy_user_id,
            guild_id,
            product_id,
            version_id,
            role_id
        )
        .execute(&mut *connection)
        .await?
        .rows_affected();
        Ok(delete_count != 0)
    }

    /// Delete all references to a role id for the given guild
    pub async fn delete_role(&self, guild: GuildId, role: RoleId) -> JinxResult<u64> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        let mut deleted = 0;
        let mut connection = self.write_connection().await?;
        let mut transaction = connection.begin().await?;

        // handle blanket role
        deleted += sqlx::query!(
            r#"UPDATE guild SET blanket_role_id = NULL WHERE guild_id = ? AND blanket_role_id = ?"#,
            guild_id,
            role_id
        )
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        // handle product links
        deleted += sqlx::query!(
            r#"DELETE FROM product_role WHERE guild_id = ? AND role_id = ?"#,
            guild_id,
            role_id
        )
        .execute(&mut *transaction)
        .await?
        .rows_affected();
        // handle product-version links
        deleted += sqlx::query!(
            r#"DELETE FROM product_version_role WHERE guild_id = ? AND role_id = ?"#,
            guild_id,
            role_id
        )
        .execute(&mut *transaction)
        .await?
        .rows_affected();

        transaction.commit().await?;

        Ok(deleted)
    }

    /// Get role grants for a product ID. This includes blanket grants.
    pub async fn get_role_grants(
        &self,
        guild: GuildId,
        product_version_id: &ProductVersionId,
    ) -> JinxResult<Vec<RoleId>> {
        let guild_id = guild.get() as i64;
        let (product_id, version_id) = product_version_id.as_db_values();
        let result = sqlx::query!(
            r#"SELECT blanket_role_id AS "role_id!" from guild WHERE guild_id = ? AND blanket_role_id IS NOT NULL
               UNION SELECT role_id AS "role_id!" FROM product_role WHERE guild_id = ? AND product_id = ?
               UNION SELECT role_id AS "role_id!" FROM product_version_role WHERE guild_id = ? AND product_id = ? AND version_id = ?"#,
            guild_id,
            guild_id,
            product_id,
            guild_id,
            product_id,
            version_id
        )
        .map(|row| RoleId::new(row.role_id as u64))
        .fetch_all(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Get roles for a product. This is ONLY product-level blanket grants.
    pub async fn get_linked_roles_for_product(
        &self,
        jinxxy_user_id: &str,
        guild: GuildId,
        product_id: &str,
    ) -> JinxResult<Vec<RoleId>> {
        let guild_id = guild.get() as i64;
        // uses `role_lookup` index
        let result = sqlx::query!(
            r#"SELECT role_id AS "role_id!" FROM product_role WHERE jinxxy_user_id = ? AND guild_id = ? AND product_id = ?"#,
            jinxxy_user_id,
            guild_id,
            product_id
        )
        .map(|row| RoleId::new(row.role_id as u64))
        .fetch_all(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Get roles for a product version. This does not include blanket grants.
    pub async fn get_linked_roles_for_product_version(
        &self,
        jinxxy_user_id: &str,
        guild: GuildId,
        product_version_id: &ProductVersionId,
    ) -> JinxResult<Vec<RoleId>> {
        let guild_id = guild.get() as i64;
        let (product_id, version_id) = product_version_id.as_db_values();
        let result = sqlx::query!(
            r#"SELECT role_id FROM product_version_role WHERE jinxxy_user_id = ? AND guild_id = ? AND product_id = ? AND version_id = ?"#,
            jinxxy_user_id,
            guild_id,
            product_id,
            version_id
        )
        .map(|row| RoleId::new(row.role_id as u64))
        .fetch_all(&self.read_pool)
        .await?;
        Ok(result)
    }

    pub async fn get_users_for_role(&self, guild: GuildId, role: RoleId) -> JinxResult<Vec<UserId>> {
        let guild_id = guild.get() as i64;
        let role_id = role.get() as i64;
        let result = sqlx::query!(
            r#"SELECT DISTINCT activator_user_id AS "user_id!" FROM license_activation INNER JOIN jinxxy_user_guild USING (jinxxy_user_id) INNER JOIN guild USING (guild_id) WHERE guild_id = ? AND blanket_role_id = ?
               UNION SELECT DISTINCT activator_user_id AS "user_id!" FROM license_activation INNER JOIN jinxxy_user_guild USING (jinxxy_user_id) INNER JOIN product_role USING (guild_id, jinxxy_user_id, product_id) WHERE guild_id = ? AND role_id = ?
               UNION SELECT DISTINCT activator_user_id AS "user_id!" FROM license_activation INNER JOIN jinxxy_user_guild USING (jinxxy_user_id) INNER JOIN product_version_role USING (guild_id, jinxxy_user_id, product_id, version_id) WHERE guild_id = ? AND role_id = ?"#,
            guild_id,
            role_id,
            guild_id,
            role_id,
            guild_id,
            role_id
        )
        .map(|row| UserId::new(row.user_id as u64))
        .fetch_all(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// get distinct roles from all links
    pub async fn get_linked_roles(&self, guild: GuildId) -> JinxResult<Vec<RoleId>> {
        let guild_id = guild.get() as i64;
        let result = sqlx::query!(
            r#"SELECT blanket_role_id AS "role_id!" FROM guild WHERE guild_id = ? AND blanket_role_id IS NOT NULL
               UNION SELECT role_id AS "role_id!" FROM product_role WHERE guild_id = ?
               UNION SELECT role_id AS "role_id!" FROM product_version_role WHERE guild_id = ?"#,
            guild_id,
            guild_id,
            guild_id,
        )
        .map(|row| RoleId::new(row.role_id as u64))
        .fetch_all(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// get an aggregate all role links for a guild
    pub async fn get_links(&self, guild: GuildId) -> JinxResult<Links> {
        let guild_id = guild.get() as i64;
        let mut connection = self.read_pool.acquire().await?;
        let mut transaction = connection.begin().await?;

        let mut links: HashMap<RoleId, Vec<LinkSource>, ahash::RandomState> = Default::default();

        // enumerate linked stores
        let stores = sqlx::query_as!(
            LinkedDisplayStore,
            r#"SELECT jinxxy_user_id, jinxxy_username FROM jinxxy_user_guild
               LEFT JOIN jinxxy_user USING (jinxxy_user_id)
               WHERE guild_id = ?"#,
            guild_id
        )
        .fetch_all(&mut *transaction)
        .await?;

        // deal with global blanket
        {
            let blanket_result =
                sqlx::query_scalar!(r#"SELECT blanket_role_id from guild where guild_id = ?"#, guild_id)
                    .fetch_optional(&mut *transaction)
                    .await?
                    .flatten()
                    .map(|role_id| RoleId::new(role_id as u64));
            if let Some(blanket_role) = blanket_result {
                links.entry(blanket_role).or_default().push(LinkSource::GlobalBlanket);
            }
        }

        // deal with product blankets
        {
            let mut product_result = sqlx::query!(
                r#"SELECT product_id, role_id FROM product_role WHERE guild_id = ?"#,
                guild_id
            )
            .map(|row| (RoleId::new(row.role_id as u64), row.product_id))
            .fetch(&mut *transaction);
            while let Some((role, product_id)) = product_result.try_next().await? {
                links
                    .entry(role)
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
            .fetch(&mut *transaction);
            while let Some((role, product_version_id)) = product_version_result.try_next().await? {
                links
                    .entry(role)
                    .or_default()
                    .push(LinkSource::ProductVersion { product_version_id });
            }
        }

        transaction.commit().await?;

        Ok(Links { stores, links })
    }

    /// Locally get all licences a users has been recorded to activate. This may be out of sync with Jinxxy!
    pub async fn get_user_licenses(&self, guild: GuildId, activator_user_id: u64) -> JinxResult<Vec<UserLicense>> {
        let guild_id = guild.get() as i64;
        let activator_user_id = activator_user_id as i64;
        let result = sqlx::query_as!(
            UserLicense,
            r#"SELECT jinxxy_user_id, jinxxy_api_key, license_id, jinxxy_username FROM license_activation
               INNER JOIN jinxxy_user_guild USING (jinxxy_user_id)
               INNER JOIN jinxxy_user USING (jinxxy_user_id)
               WHERE guild_id = ? AND activator_user_id = ?"#,
            guild_id,
            activator_user_id
        )
        .fetch_all(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Get all guilds a user has activated licenses in
    pub async fn get_user_guilds(&self, activator_user_id: u64) -> JinxResult<Vec<GuildId>> {
        let activator_user_id = activator_user_id as i64;
        let result = sqlx::query!(r#"SELECT DISTINCT guild_id FROM license_activation INNER JOIN jinxxy_user_guild USING (jinxxy_user_id) WHERE activator_user_id = ?"#, activator_user_id)
            .map(|row| GuildId::new(row.guild_id as u64))
            .fetch_all(&self.read_pool)
            .await?;
        Ok(result)
    }

    /// Locally check if any activations exist for this user/license combo. This may be out of sync with Jinxxy!
    pub async fn has_user_license_activations(
        &self,
        jinxxy_user_id: &str,
        activator_user_id: u64,
        license_id: &str,
    ) -> JinxResult<bool> {
        let activator_user_id = activator_user_id as i64;
        let result = sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT * FROM license_activation WHERE jinxxy_user_id = ? AND activator_user_id = ? AND license_id = ?) AS "has_activations: bool""#,
            jinxxy_user_id,
            activator_user_id,
            license_id
        )
        .fetch_one(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Locally get all activations for a user:license combo. This may be out of sync with Jinxxy!
    pub async fn get_user_license_activations(
        &self,
        jinxxy_user_id: &str,
        activator_user_id: u64,
        license_id: &str,
    ) -> JinxResult<Vec<String>> {
        let activator_user_id = activator_user_id as i64;
        let result = sqlx::query_scalar!(
            r#"SELECT DISTINCT license_activation_id FROM license_activation WHERE jinxxy_user_id = ? AND activator_user_id = ? AND license_id = ?"#,
            jinxxy_user_id,
            activator_user_id,
            license_id
        )
        .fetch_all(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Locally get all users that have activated the given license. This may be out of sync with Jinxxy!
    pub async fn get_license_users(&self, jinxxy_user_id: &str, license_id: &str) -> SqliteResult<Vec<u64>> {
        let result = sqlx::query!(
            r#"SELECT DISTINCT activator_user_id FROM license_activation WHERE jinxxy_user_id = ? AND license_id = ?"#,
            jinxxy_user_id,
            license_id
        )
        .map(|row| row.activator_user_id as u64)
        .fetch_all(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Get DB size in bytes
    pub async fn size(&self) -> JinxResult<u64> {
        let result =
            sqlx::query!(r#"SELECT page_count * page_size AS "size!" FROM pragma_page_count(), pragma_page_size()"#)
                .map(|row| row.size as u64)
                .fetch_one(&self.read_pool)
                .await?;
        Ok(result)
    }

    /// Get count of license activations
    pub async fn license_activation_count(&self) -> JinxResult<u64> {
        let result = sqlx::query!(r#"SELECT count(*) AS "count!" FROM (
                            SELECT DISTINCT jinxxy_user_id, license_id, activator_user_id, license_activation_id FROM license_activation
                            INNER JOIN jinxxy_user_guild USING (jinxxy_user_id)
                            INNER JOIN guild USING (guild_id)
                            WHERE NOT guild.test
                        )"#)
            .map(|row| row.count as u64)
            .fetch_one(&self.read_pool)
            .await?;
        Ok(result)
    }

    /// Get count of distinct users who have activated licenses
    pub async fn distinct_user_count(&self) -> JinxResult<u64> {
        let result = sqlx::query!(r#"SELECT count(DISTINCT activator_user_id) AS "count!" FROM license_activation INNER JOIN jinxxy_user_guild USING (jinxxy_user_id) INNER JOIN guild USING (guild_id) WHERE NOT guild.test"#)
            .map(|row| row.count as u64)
            .fetch_one(&self.read_pool)
            .await?;
        Ok(result)
    }

    /// Get count of configured guilds
    pub async fn guild_count(&self) -> JinxResult<u64> {
        let result = sqlx::query!(r#"SELECT count(*) AS "count!" FROM guild WHERE NOT guild.test"#)
            .map(|row| row.count as u64)
            .fetch_one(&self.read_pool)
            .await?;
        Ok(result)
    }

    /// Get count of distinct bot log channels
    pub async fn log_channel_count(&self) -> JinxResult<u64> {
        let result =
            sqlx::query!(r#"SELECT count(DISTINCT log_channel_id) AS "count!" FROM guild WHERE NOT guild.test"#)
                .map(|row| row.count as u64)
                .fetch_one(&self.read_pool)
                .await?;
        Ok(result)
    }

    /// Get count of guilds with blanket role set
    pub async fn blanket_role_count(&self) -> JinxResult<u64> {
        let result = sqlx::query!(
            r#"SELECT count(*) AS "count!" FROM guild WHERE NOT guild.test AND blanket_role_id IS NOT NULL"#
        )
        .map(|row| row.count as u64)
        .fetch_one(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Get count of product->role mappings
    pub async fn product_role_count(&self) -> JinxResult<u64> {
        let result = sqlx::query!(
            r#"SELECT count(*) AS "count!" FROM product_role INNER JOIN guild USING (guild_id) WHERE NOT guild.test"#
        )
        .map(|row| row.count as u64)
        .fetch_one(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Get count of product+version->role mappings
    pub async fn product_version_role_count(&self) -> JinxResult<u64> {
        let result = sqlx::query!(r#"SELECT count(*) AS "count!" FROM product_version_role INNER JOIN guild USING (guild_id) WHERE NOT guild.test"#)
            .map(|row| row.count as u64)
            .fetch_one(&self.read_pool)
            .await?;
        Ok(result)
    }

    /// Get count of license activations in a guild
    pub async fn guild_license_activation_count(&self, guild: GuildId) -> JinxResult<u64> {
        let guild_id = guild.get() as i64;
        let result = sqlx::query!(r#"SELECT count(*) AS "count!" FROM (
                            SELECT DISTINCT jinxxy_user_id, license_id, activator_user_id, license_activation_id FROM license_activation
                            INNER JOIN jinxxy_user_guild USING (jinxxy_user_id)
                            INNER JOIN guild USING (guild_id)
                            WHERE guild.guild_id = ?
                        )"#, guild_id)
            .map(|row| row.count as u64)
            .fetch_one(&self.read_pool)
            .await?;
        Ok(result)
    }

    /// Get bot log channel
    pub async fn get_log_channel(&self, guild: GuildId) -> JinxResult<Option<ChannelId>> {
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
    pub async fn get_log_channels<const TEST_ONLY: bool>(&self) -> JinxResult<Vec<ChannelId>> {
        let result = if TEST_ONLY {
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
        }?;
        Ok(result)
    }

    /// Set or unset bot log channel
    pub async fn set_log_channel(&self, guild: GuildId, channel: Option<ChannelId>) -> JinxResult<()> {
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
    pub async fn set_test(&self, guild: GuildId, test: bool) -> JinxResult<()> {
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
    pub async fn is_test_guild(&self, guild: GuildId) -> JinxResult<bool> {
        let guild_id = guild.get() as i64;
        let result = sqlx::query_scalar!(
            r#"SELECT test AS "is_test: bool" FROM guild WHERE guild_id = ?"#,
            guild_id
        )
        .fetch_one(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Set or unset this guild as an owner guild (gets extra slash commands)
    pub async fn set_owner_guild(&self, guild: GuildId, owner: bool) -> JinxResult<()> {
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
    pub async fn is_owner_guild(&self, guild: GuildId) -> JinxResult<bool> {
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
    pub async fn get_gumroad_failure_count(&self, guild: GuildId) -> JinxResult<Option<u64>> {
        let guild_id = guild.get() as i64;
        let result = sqlx::query!(
            r#"SELECT gumroad_failure_count FROM guild WHERE guild_id = ?"#,
            guild_id
        )
        .map(|row| row.gumroad_failure_count as u64)
        .fetch_optional(&self.read_pool)
        .await?;
        Ok(result)
    }

    /// Increment gumroad failure count for a guild
    pub async fn increment_gumroad_failure_count(&self, guild: GuildId) -> JinxResult<()> {
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
    pub async fn get_guilds_pending_gumroad_nag(&self) -> JinxResult<Vec<GuildGumroadInfo>> {
        // at least 10 gumroad failures AND gumroad failure count exceeds 20% of successful activation count
        let result = sqlx::query!(
            r#"SELECT guild_id, log_channel_id AS "log_channel_id!", gumroad_failure_count FROM guild AS "outer"
               WHERE log_channel_id IS NOT NULL AND gumroad_nag_count < 1 AND gumroad_failure_count >= 10
               AND (gumroad_failure_count * 5) > (
                   SELECT count(*) FROM (
                       SELECT DISTINCT jinxxy_user_id, license_id, activator_user_id, license_activation_id FROM license_activation
                       INNER JOIN jinxxy_user_guild USING (jinxxy_user_id)
                       WHERE jinxxy_user_guild.guild_id = "outer".guild_id
                   )
               )"#
        )
            .map(|row| GuildGumroadInfo {
                guild_id: GuildId::new(row.guild_id as u64),
                log_channel_id: ChannelId::new(row.log_channel_id as u64),
                gumroad_failure_count: row.gumroad_failure_count as u64,
            })
            .fetch_all(&self.read_pool)
            .await?;
        Ok(result)
    }

    /// Check gumroad nag count for a guild
    pub async fn get_gumroad_nag_count(&self, guild: GuildId) -> JinxResult<Option<u64>> {
        let guild_id = guild.get() as i64;
        let result = sqlx::query!(r#"SELECT gumroad_nag_count FROM guild WHERE guild_id = ?"#, guild_id)
            .map(|row| row.gumroad_nag_count as u64)
            .fetch_optional(&self.read_pool)
            .await?;
        Ok(result)
    }

    /// Increment gumroad nag count for a guild
    pub async fn increment_gumroad_nag_count(&self, guild: GuildId) -> JinxResult<()> {
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

    pub async fn get_store_cache(&self, jinxxy_user_id: &str) -> JinxResult<StoreCache> {
        let mut connection = self.read_pool.acquire().await?;
        let mut transaction = connection.begin().await?;
        let cache_time = helper::get_cache_time(&mut transaction, jinxxy_user_id).await?;
        let product_name_info = helper::product_names_in_store(&mut transaction, jinxxy_user_id).await?;
        let product_version_name_info =
            helper::product_version_names_in_store(&mut transaction, jinxxy_user_id).await?;
        transaction.commit().await?;
        Ok(StoreCache {
            product_name_info,
            product_version_name_info,
            cache_time,
        })
    }

    pub async fn persist_store_cache(&self, jinxxy_user_id: &str, cache_entry: StoreCache) -> JinxResult<()> {
        let mut connection = self.write_connection().await?;
        let mut transaction = connection.begin().await?;
        helper::persist_product_names(&mut transaction, jinxxy_user_id, cache_entry.product_name_info).await?;
        helper::persist_product_version_names(&mut transaction, jinxxy_user_id, cache_entry.product_version_name_info)
            .await?;
        helper::set_cache_time(&mut transaction, jinxxy_user_id, cache_entry.cache_time).await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Delete all cache entries for all guilds
    pub async fn clear_cache(&self) -> JinxResult<()> {
        let mut connection = self.write_connection().await?;
        let mut transaction = connection.begin().await?;
        sqlx::query!(r#"DELETE FROM product"#)
            .execute(&mut *transaction)
            .await?;
        sqlx::query!(r#"DELETE FROM product_version"#)
            .execute(&mut *transaction)
            .await?;
        sqlx::query!(r#"UPDATE jinxxy_user SET cache_time_unix_ms = 0"#)
            .execute(&mut *transaction)
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    /// Get cached name info for products in a guild
    pub async fn product_names_in_store(&self, jinxxy_user_id: &str) -> JinxResult<Vec<ProductNameInfo>> {
        let mut connection = self.read_pool.acquire().await?;
        let result = helper::product_names_in_store(&mut connection, jinxxy_user_id).await?;
        Ok(result)
    }

    /// Get versions for a product
    pub async fn product_versions(
        &self,
        jinxxy_user_id: &str,
        product_id: &str,
    ) -> JinxResult<Vec<ProductVersionNameInfo>> {
        let result = sqlx::query!(
            r#"SELECT version_id, product_version_name FROM product_version WHERE jinxxy_user_id = ? AND product_id = ?"#,
            jinxxy_user_id,
            product_id
        )
            .map(|row| ProductVersionNameInfo {
                id: ProductVersionId {
                    product_id: product_id.to_string(),
                    product_version_id: Some(row.version_id),
                },
                product_version_name: row.product_version_name,
            })
            .fetch_all(&self.read_pool)
            .await?;
        Ok(result)
    }
}

/// Helper functions that don't access a whole pool
mod helper {
    use super::*;
    use regex::Regex;
    use sqlx::SqliteTransaction;
    use std::borrow::Cow;
    use std::sync::LazyLock;

    /// Get a single setting from the `settings` table. Note that if your setting is nullable you MUST read it as an
    /// Option<T> instead of a T. This function returns None only if the entire row is absent.
    pub(crate) async fn get_setting<'e, T>(connection: &'e mut SqliteConnection, key: &str) -> SqliteResult<Option<T>>
    where
        T: sqlx::Type<Sqlite> + Send + Unpin + 'e,
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
        T: Encode<'q, Sqlite> + sqlx::Type<Sqlite> + 'q,
    {
        let update_count = sqlx::query(r#"INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)"#)
            .bind(key)
            .bind(value)
            .execute(connection)
            .await?
            .rows_affected();
        Ok(update_count != 0)
    }

    pub(crate) async fn get_cache_time(
        connection: &mut SqliteConnection,
        jinxxy_user_id: &str,
    ) -> SqliteResult<SimpleTime> {
        let cache_time_unix_ms = sqlx::query_scalar!(
            r#"SELECT cache_time_unix_ms FROM jinxxy_user WHERE jinxxy_user_id = ?"#,
            jinxxy_user_id
        )
        .fetch_one(connection)
        .await?;
        Ok(SimpleTime::from_unix_millis(cache_time_unix_ms as u64))
    }

    pub(super) async fn set_cache_time(
        connection: &mut SqliteConnection,
        jinxxy_user_id: &str,
        time: SimpleTime,
    ) -> SqliteResult<()> {
        let cache_time_unix_ms = time.as_epoch_millis() as i64;
        sqlx::query!(
            r#"INSERT INTO jinxxy_user (jinxxy_user_id, cache_time_unix_ms) VALUES (?, ?)
               ON CONFLICT (jinxxy_user_id) DO UPDATE SET cache_time_unix_ms = excluded.cache_time_unix_ms"#,
            jinxxy_user_id,
            cache_time_unix_ms
        )
        .execute(connection)
        .await?;
        Ok(())
    }

    /// Get cached name info for products in a guild
    pub(super) async fn product_names_in_store(
        connection: &mut SqliteConnection,
        jinxxy_user_id: &str,
    ) -> SqliteResult<Vec<ProductNameInfo>> {
        sqlx::query!(
            r#"SELECT product_id, product_name, etag FROM product WHERE jinxxy_user_id = ?"#,
            jinxxy_user_id
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
    pub(super) async fn product_version_names_in_store(
        transaction: &mut SqliteTransaction<'_>,
        jinxxy_user_id: &str,
    ) -> SqliteResult<Vec<ProductVersionNameInfo>> {
        sqlx::query!(
            r#"SELECT product_id, version_id, product_version_name FROM product_version WHERE jinxxy_user_id = ?"#,
            jinxxy_user_id
        )
        .map(|row| ProductVersionNameInfo {
            id: ProductVersionId {
                product_id: row.product_id,
                product_version_id: Some(row.version_id),
            },
            product_version_name: row.product_version_name,
        })
        .fetch_all(&mut **transaction)
        .await
    }

    pub(super) async fn persist_product_names(
        connection: &mut SqliteConnection,
        jinxxy_user_id: &str,
        product_name_info: Vec<ProductNameInfo>,
    ) -> SqliteResult<()> {
        let mut new_key_set = HashSet::with_capacity_and_hasher(product_name_info.len(), ahash::RandomState::default());
        let mut unexpected_keys = Vec::new();

        // step 1: insert all entries and keep track of their keys in a set for later
        for info in product_name_info {
            let product_id = info.id;
            let product_name = info.value.product_name;
            let etag = info.value.etag;
            sqlx::query!(
                r#"INSERT INTO product (jinxxy_user_id, product_id, product_name, etag) VALUES (?, ?, ?, ?)
                   ON CONFLICT (jinxxy_user_id, product_id) DO UPDATE SET product_name = excluded.product_name, etag = excluded.etag"#,
                jinxxy_user_id,
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
            let mut rows = sqlx::query_scalar!(
                r#"SELECT product_id FROM product WHERE jinxxy_user_id = ?"#,
                jinxxy_user_id
            )
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
                r#"DELETE FROM product WHERE jinxxy_user_id = ? AND product_id = ?"#,
                jinxxy_user_id,
                product_id
            )
            .execute(&mut *connection)
            .await?;
        }
        Ok(())
    }

    pub(super) async fn persist_product_version_names(
        transaction: &mut SqliteTransaction<'_>,
        jinxxy_user_id: &str,
        product_version_name_info: Vec<ProductVersionNameInfo>,
    ) -> SqliteResult<()> {
        let mut new_key_set =
            HashSet::with_capacity_and_hasher(product_version_name_info.len(), ahash::RandomState::default());
        let mut unexpected_keys = Vec::new();
        // step 1: insert all entries and keep track of their keys in a set for later
        for info in product_version_name_info {
            let (product_id, version_id) = info.id.into_db_values();
            let product_version_name = info.product_version_name;
            sqlx::query!(
            r#"INSERT INTO product_version (jinxxy_user_id, product_id, version_id, product_version_name) VALUES (?, ?, ?, ?)
               ON CONFLICT (jinxxy_user_id, product_id, version_id) DO UPDATE SET product_version_name = excluded.product_version_name"#,
                jinxxy_user_id,
                product_id,
                version_id,
                product_version_name
            )
                .execute(&mut **transaction)
                .await?;
            new_key_set.insert((product_id, version_id));
        }

        // step 2: query all existing keys, and record any keys that we did NOT just insert
        {
            let mut rows = sqlx::query!(
                r#"SELECT product_id, version_id FROM product_version WHERE jinxxy_user_id = ?"#,
                jinxxy_user_id
            )
            .fetch(&mut **transaction);
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
                r#"DELETE FROM product_version WHERE jinxxy_user_id = ? AND product_id = ? AND version_id = ?"#,
                jinxxy_user_id,
                product_id,
                version_id
            )
            .execute(&mut **transaction)
            .await?;
        }
        Ok(())
    }

    static GLOBAL_LIKE_ESCAPE_REGEX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"([_%?])").expect("Failed to compile GLOBAL_LIKE_ESCAPE_REGEX"));

    thread_local! {
        // trick to avoid a subtle performance edge case: https://docs.rs/regex/latest/regex/index.html#sharing-a-regex-across-threads-can-result-in-contention
        static LIKE_ESCAPE_REGEX: Regex = GLOBAL_LIKE_ESCAPE_REGEX.clone();
    }

    /// Escapes LIKE strings using the escape character '?', which was specifically chosen due to its lack of URL safety
    pub(super) fn escape_like(s: &str) -> Cow<'_, str> {
        LIKE_ESCAPE_REGEX.with(|regex| regex.replace_all(s, "?${1}"))
    }

    #[cfg(test)]
    mod test {
        use super::*;

        #[test]
        fn test_escape_like_none() {
            assert_eq!(escape_like("hello"), "hello");
        }

        #[test]
        fn test_escape_like_empty() {
            assert_eq!(escape_like(""), "");
        }

        #[test]
        fn test_escape_like_percent() {
            assert_eq!(escape_like("foo%bar"), "foo?%bar");
        }

        #[test]
        fn test_escape_like_underscore() {
            assert_eq!(escape_like("foo_bar"), "foo?_bar");
        }

        #[test]
        fn test_escape_like_question() {
            assert_eq!(escape_like("foo?bar"), "foo??bar");
        }

        #[test]
        fn test_escape_like_multiple() {
            assert_eq!(escape_like("%_?"), "?%?_??");
        }

        #[test]
        fn test_escape_like_multiple_same() {
            assert_eq!(escape_like("???"), "??????");
        }
    }
}

/// Helper struct returned by [`JinxDb::get_guilds_pending_gumroad_nag`]
pub struct GuildGumroadInfo {
    pub guild_id: GuildId,
    pub log_channel_id: ChannelId,
    pub gumroad_failure_count: u64,
}

/// Helper struct returned by [`JinxDb::get_links`].
pub struct Links {
    pub stores: Vec<LinkedDisplayStore>,
    pub links: HashMap<RoleId, Vec<LinkSource>, ahash::RandomState>,
}

/// Helper struct used by [`Links`]. This is the parts of a store needed for display only.
#[derive(sqlx::FromRow)]
pub struct LinkedDisplayStore {
    pub jinxxy_user_id: String,
    pub jinxxy_username: Option<String>,
}

/// Helper enum used by [`Links`]. This is any source for a product->role link.
pub enum LinkSource {
    GlobalBlanket,
    ProductBlanket { product_id: String },
    ProductVersion { product_version_id: ProductVersionId },
}

/// Helper struct returned by [`JinxDb::get_store_cache`] and taken by [`JinxDb::persist_store_cache`].
pub struct StoreCache {
    pub product_name_info: Vec<ProductNameInfo>,
    pub product_version_name_info: Vec<ProductVersionNameInfo>,
    pub cache_time: SimpleTime,
}

/// Extra functions specifically for using this with the DB
impl ProductVersionId {
    fn as_db_values(&self) -> (&str, &str) {
        (
            self.product_id.as_str(),
            self.product_version_id.as_deref().unwrap_or_default(),
        )
    }

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

/// Helper struct returned by [`JinxDb::get_user_licenses`]
#[derive(sqlx::FromRow)]
pub struct UserLicense {
    pub jinxxy_user_id: String,
    pub jinxxy_api_key: String,
    pub license_id: String,
    pub jinxxy_username: Option<String>,
}

/// Helper struct returned by [`JinxDb::get_arbitrary_jinxxy_api_key`]
pub struct GuildApiKey {
    pub guild_id: GuildId,
    pub jinxxy_api_key: String,
}

/// Helper struct returned by [`JinxDb::get_store_links`]
#[derive(sqlx::FromRow)]
pub struct LinkedStore {
    pub jinxxy_user_id: String,
    pub jinxxy_username: Option<String>,
    pub jinxxy_api_key: String,
}
