// This file is part of jinx. Copyright © 2024 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

use crate::error::JinxError;
use crate::http::jinxxy::ProductVersionId;
use dashmap::DashMap;
use poise::serenity_prelude::{ChannelId, GuildId, RoleId};
use std::collections::HashMap;
use std::path::Path;
use tokio::time::Instant;
use tokio_rusqlite::{named_params, Connection, OptionalExtension, Result};
use tracing::debug;

const SCHEMA_VERSION_KEY: &str = "schema_version";
const SCHEMA_VERSION_VALUE: i32 = 6;
const DISCORD_TOKEN_KEY: &str = "discord_token";

pub struct JinxDb {
    connection: Connection,
    api_key_cache: DashMap<GuildId, Option<String>, ahash::RandomState>,
}

impl Drop for JinxDb {
    fn drop(&mut self) {
        debug!("Closing sqlite db…");
    }
}

impl JinxDb {
    /// Open a new database
    pub async fn open() -> Result<Self> {
        Self::open_path("jinx.sqlite").await
    }

    /// Open a new database
    async fn open_path<P: AsRef<Path>>(path: P) -> Result<Self> {
        let connection = Connection::open(path).await?;
        JinxDb::init(&connection).await?;
        let db = JinxDb {
            connection,
            api_key_cache: Default::default(),
        };
        Ok(db)
    }

    /// Set up the database
    async fn init(connection: &Connection) -> Result<()> {
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
                             blanket_role_id        INTEGER\
                         ) STRICT",
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
                             version_id             TEXT, \
                             role_id                INTEGER NOT NULL, \
                             PRIMARY KEY            (guild_id, product_id, version_id, role_id) \
                         ) STRICT",
                    (),
                )?;
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
                             PRIMARY KEY            (guild_id, license_id, license_activation_id, user_id) \
                         ) STRICT",
                    (),
                )?;

                connection.execute(
                    "CREATE TABLE IF NOT EXISTS \"owner\" ( \
                             owner_id               INTEGER PRIMARY KEY \
                         ) STRICT",
                    (),
                )?;

                let mut settings_read =
                    connection.prepare("SELECT value FROM settings where key = :key")?;
                let schema_version: i32 = settings_read
                    .query_row(named_params! {":key": SCHEMA_VERSION_KEY}, |a| a.get(0))
                    .optional()?
                    .unwrap_or(SCHEMA_VERSION_VALUE);

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

                // Applications that use long-lived database connections should run "PRAGMA optimize=0x10002;" when the connection is first opened.
                // All applications should run "PRAGMA optimize;" after a schema change.
                connection.execute("PRAGMA optimize = 0x10002", ())?;

                // update the schema version value persisted to the DB
                let mut settings_insert = connection.prepare(
                    "INSERT OR REPLACE INTO settings (key, value) VALUES (:key, :value)",
                )?;
                settings_insert.execute(
                    named_params! {":key": SCHEMA_VERSION_KEY, ":value": SCHEMA_VERSION_VALUE},
                )?;

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
    pub async fn optimize(&self) -> Result<()> {
        self.connection
            .call(move |connection| {
                connection.execute("PRAGMA optimize", ())?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn add_owner(&self, owner_id: u64) -> Result<()> {
        self.connection
            .call(move |connection| {
                let mut statement = connection
                    .prepare_cached("INSERT OR IGNORE INTO owner (owner_id) VALUES (:owner)")?;
                statement.execute(named_params! {":owner": owner_id})?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn delete_owner(&self, owner_id: u64) -> Result<()> {
        self.connection
            .call(move |connection| {
                let mut statement =
                    connection.prepare_cached("DELETE FROM owner WHERE owner_id = :owner")?;
                statement.execute(named_params! {":owner": owner_id})?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn set_discord_token(&self, discord_token: String) -> Result<()> {
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached(
                    "INSERT OR REPLACE INTO settings (key, value) VALUES (:key, :value)",
                )?;
                statement
                    .execute(named_params! {":key": DISCORD_TOKEN_KEY, ":value": discord_token})?;
                Ok(())
            })
            .await?;
        Ok(())
    }

    pub async fn get_owners(&self) -> Result<Vec<u64>> {
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached("SELECT owner_id FROM owner")?;
                let result = statement.query_map((), |row| {
                    let owner_id: u64 = row.get(0)?;
                    Ok(owner_id)
                })?;
                let mut vec = Vec::with_capacity(result.size_hint().0);
                for row in result {
                    vec.push(row?);
                }
                Ok(vec)
            })
            .await
    }

    pub async fn is_user_owner(&self, owner_id: u64) -> Result<bool> {
        self.connection
            .call(move |connection| {
                let mut statement = connection
                    .prepare_cached("SELECT EXISTS(SELECT * FROM owner WHERE owner_id = :owner)")?;
                let owner_exists =
                    statement.query_row(named_params! {":owner": owner_id}, |row| {
                        let exists: bool = row.get(0)?;
                        Ok(exists)
                    })?;
                Ok(owner_exists)
            })
            .await
    }

    pub async fn get_discord_token(&self) -> Result<Option<String>> {
        let discord_token = self
            .connection
            .call(move |connection| {
                let result: Option<String> = connection
                    .query_row(
                        format!(r#"SELECT value FROM settings WHERE key = "{DISCORD_TOKEN_KEY}""#)
                            .as_str(),
                        [],
                        |row| row.get(0),
                    )
                    .optional()?;
                Ok(result)
            })
            .await?;
        Ok(discord_token)
    }

    /// Locally record that we've activated a license for a user
    pub async fn activate_license(
        &self,
        guild: GuildId,
        license_id: String,
        license_activation_id: String,
        user_id: u64,
    ) -> Result<()> {
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT OR IGNORE INTO license_activation (guild_id, license_id, license_activation_id, user_id) VALUES (:guild, :license, :activation, :user)")?;
            statement.execute(named_params! {":guild": guild.get(), ":license": license_id, ":activation": license_activation_id, ":user": user_id})?;
            Ok(())
        }).await
    }

    /// Locally record that we've deactivated a license for a user. Returns `true` if a row was found and deleted, or `false` if no row was found to delete.
    pub async fn deactivate_license(
        &self,
        guild: GuildId,
        license_id: String,
        license_activation_id: String,
        user_id: u64,
    ) -> Result<bool> {
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("DELETE FROM license_activation WHERE guild_id = :guild AND license_id = :license AND license_activation_id = :activation AND user_id = :user")?;
            let delete_count = statement.execute(named_params! {":guild": guild.get(), ":license": license_id, ":activation": license_activation_id, ":user": user_id})?;
            Ok(delete_count != 0)
        }).await
    }

    /// Locally check if a license is locked. This may be out of sync with Jinxxy!
    pub async fn is_license_locked(&self, guild: GuildId, license_id: String) -> Result<bool> {
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached("SELECT EXISTS(SELECT * FROM license_activation WHERE guild_id = :guild AND license_id = :license AND user_id = 0)")?; //TODO: could use an index
                let lock_exists = statement.query_row(
                    named_params! {":guild": guild.get(), ":license": license_id},
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
    pub async fn set_jinxxy_api_key(&self, guild: GuildId, api_key: String) -> Result<()> {
        let api_key_clone = api_key.clone();
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT INTO guild (guild_id, jinxxy_api_key) VALUES (:guild, :api_key) ON CONFLICT (guild_id) DO UPDATE SET jinxxy_api_key = excluded.jinxxy_api_key")?;
            statement.execute(named_params! {":guild": guild.get(), ":api_key": api_key_clone})?;
            Ok(())
        }).await?;
        self.api_key_cache.insert(guild, Some(api_key));
        Ok(())
    }

    /// Get Jinxxy API key for this guild
    pub async fn get_jinxxy_api_key(&self, guild: GuildId) -> Result<Option<String>> {
        if let Some(api_key) = self.api_key_cache.get(&guild) {
            // cached read
            Ok(api_key.value().clone())
        } else {
            // cache miss
            let api_key = self
                .connection
                .call(move |connection| {
                    let mut statement = connection
                        .prepare_cached("SELECT jinxxy_api_key FROM guild WHERE guild_id = ?")?;
                    let result: Option<String> = statement
                        .query_row([guild.get()], |row| row.get(0))
                        .optional()?;
                    Ok(result)
                })
                .await?;
            self.api_key_cache.insert(guild, api_key.clone());
            Ok(api_key)
        }
    }

    /// Set or unset blanket role
    pub async fn set_blanket_role_id(&self, guild: GuildId, role_id: Option<RoleId>) -> Result<()> {
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT INTO guild (guild_id, blanket_role_id) VALUES (:guild, :role_id) ON CONFLICT (guild_id) DO UPDATE SET blanket_role_id = excluded.blanket_role_id")?;
            statement.execute(named_params! {":guild": guild.get(), ":role_id": role_id.map(RoleId::get)})?;
            Ok(())
        }).await?;
        Ok(())
    }

    /// blanket link a Jinxxy product and a role
    pub async fn link_product(
        &self,
        guild: GuildId,
        product_id: String,
        role: RoleId,
    ) -> Result<()> {
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT OR IGNORE INTO product_role (guild_id, product_id, role_id) VALUES (:guild, :product, :role)")?;
            statement.execute(named_params! {":guild": guild.get(), ":product": product_id, ":role": role.get()})?;
            Ok(())
        }).await
    }

    /// blanket unlink a Jinxxy product and a role. Returns `true` if a row was found and deleted, or `false` if no row was found to delete.
    pub async fn unlink_product(
        &self,
        guild: GuildId,
        product_id: String,
        role: RoleId,
    ) -> Result<bool> {
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("DELETE FROM product_role WHERE guild_id = :guild AND product_id = :product AND role_id = :role")?;
            let delete_count = statement.execute(named_params! {":guild": guild.get(), ":product": product_id, ":role": role.get()})?;
            Ok(delete_count != 0)
        }).await
    }

    /// link a Jinxxy product-version and a role
    pub async fn link_product_version(
        &self,
        guild: GuildId,
        product_version_id: ProductVersionId,
        role: RoleId,
    ) -> Result<()> {
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT OR IGNORE INTO product_version_role (guild_id, product_id, version_id, role_id) VALUES (:guild, :product, :version, :role)")?;
            statement.execute(named_params! {":guild": guild.get(), ":product": product_version_id.product_id, ":version": product_version_id.product_version_id, ":role": role.get()})?;
            Ok(())
        }).await
    }

    /// unlink a Jinxxy product-version and a role. Returns `true` if a row was found and deleted, or `false` if no row was found to delete.
    pub async fn unlink_product_version(
        &self,
        guild: GuildId,
        product_version_id: ProductVersionId,
        role: RoleId,
    ) -> Result<bool> {
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("DELETE FROM product_version_role WHERE guild_id = :guild AND product_id = :product AND version_id IS :version AND role_id = :role")?;
            let delete_count = statement.execute(named_params! {":guild": guild.get(), ":product": product_version_id.product_id, ":version": product_version_id.product_version_id, ":role": role.get()})?;
            Ok(delete_count != 0)
        }).await
    }

    /// Get role grants for a product ID. This includes blanket grants.
    pub async fn get_role_grants(
        &self,
        guild: GuildId,
        product_id: ProductVersionId,
    ) -> Result<Vec<RoleId>> {
        self.connection
            .call(move |connection| {
                // uses `role_lookup` and `version_role_lookup` indices
                let mut statement = connection.prepare_cached("(SELECT blanket_role_id as role_id from guild WHERE guild_id = :guild AND blanket_role_id IS NOT NULL) UNION (SELECT role_id FROM product_role WHERE guild_id = :guild AND product_id = :product) UNION (SELECT role_id FROM product_version_role WHERE guild_id = :guild AND product_id = :product AND version_id IS :version)")?;
                let result = statement.query_map(
                    named_params! {":guild": guild.get(), ":product": product_id.product_id, ":version": product_id.product_version_id},
                    |row| {
                        let role_id: u64 = row.get(0)?;
                        Ok(RoleId::new(role_id))
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
    pub async fn get_linked_roles_for_product(
        &self,
        guild: GuildId,
        product_id: String,
    ) -> Result<Vec<RoleId>> {
        self.connection
            .call(move |connection| {
                // uses `role_lookup` index
                let mut statement = connection.prepare_cached("SELECT role_id FROM product_role WHERE guild_id = :guild AND product_id = :product")?;
                let result = statement.query_map(
                    named_params! {":guild": guild.get(), ":product": product_id},
                    |row| {
                        let role_id: u64 = row.get(0)?;
                        Ok(RoleId::new(role_id))
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

    /// Get roles for a product version. This does not include blanket grants.
    pub async fn get_linked_roles_for_product_version(
        &self,
        guild: GuildId,
        product_id: ProductVersionId,
    ) -> Result<Vec<RoleId>> {
        self.connection
            .call(move |connection| {
                // uses `version_role_lookup` index
                let mut statement = connection.prepare_cached("SELECT role_id FROM product_version_role WHERE guild_id = :guild AND product_id = :product AND version_id IS :version")?;
                let result = statement.query_map(
                    named_params! {":guild": guild.get(), ":product": product_id.product_id, ":version": product_id.product_version_id},
                    |row| {
                        let role_id: u64 = row.get(0)?;
                        Ok(RoleId::new(role_id))
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

    /// get all links
    pub async fn get_links(
        &self,
        guild: GuildId,
    ) -> Result<HashMap<RoleId, Vec<LinkSource>, ahash::RandomState>> {
        self.connection
            .call(move |connection| {
                let mut map: HashMap<RoleId, Vec<LinkSource>, ahash::RandomState> = Default::default();

                // deal with global blanket
                let mut blanket_statement = connection.prepare_cached(
                    "SELECT blanket_role_id from guild where guild_id = ?",
                )?;
                let blanket_result: Option<RoleId> = blanket_statement.query_row([guild.get()], |row| {
                    row.get(0)
                }).optional()?
                    .map(|role_id| RoleId::new(role_id));
                if let Some(blanket_role) = blanket_result {
                    map.entry(blanket_role).or_default().push(LinkSource::GlobalBlanket);
                }

                // deal with product blankets
                let mut product_statement = connection.prepare_cached(
                    "SELECT product_id, role_id FROM product_role WHERE guild_id = ?",
                )?; //TODO: could use an index
                let product_result = product_statement.query_map([guild.get()], |row| {
                    let product_id: String = row.get(0)?;
                    let role_id: u64 = row.get(1)?;
                    Ok((RoleId::new(role_id), product_id))
                })?;
                for row in product_result {
                    let (role, product_id) = row?;
                    map.entry(role).or_default().push(LinkSource::ProductBlanket {product_id});
                }

                // deal with specific links
                let mut product_version_statement = connection.prepare_cached(
                    "SELECT product_id, version_id, role_id FROM product_version_role WHERE guild_id = ?",
                )?; //TODO: could use an index
                let product_version_result = product_version_statement.query_map([guild.get()], |row| {
                    let product_id: String = row.get(0)?;
                    let product_version_id: Option<String> = row.get(1)?;
                    let role_id: u64 = row.get(2)?;
                    let product_version_id = ProductVersionId {
                        product_id,
                        product_version_id,
                    };
                    Ok((RoleId::new(role_id), product_version_id))
                })?;
                for row in product_version_result {
                    let (role, product_version_id) = row?;
                    map.entry(role).or_default().push(LinkSource::ProductVersion(product_version_id));
                }
                Ok(map)
            })
            .await
    }

    /// Locally get all licences a users has been recorded to activate. This may be out of sync with Jinxxy!
    pub async fn get_user_licenses(&self, guild: GuildId, user_id: u64) -> Result<Vec<String>> {
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached("SELECT license_id FROM license_activation WHERE guild_id = :guild AND user_id = :user")?; //TODO: could use an index
                let result = statement.query_map(
                    named_params! {":guild": guild.get(), ":user": user_id},
                    |row| {
                        let license_id: String = row.get(0)?;
                        Ok(license_id)
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

    /// Locally get all activations for a user and license has been recorded to activate. This may be out of sync with Jinxxy!
    pub async fn get_user_license_activations(
        &self,
        guild: GuildId,
        user_id: u64,
        license_id: String,
    ) -> Result<Vec<String>> {
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached("SELECT license_activation_id FROM license_activation WHERE guild_id = :guild AND user_id = :user AND license_id = :license")?; //TODO: could use an index
                let result = statement.query_map(
                    named_params! {":guild": guild.get(), ":user": user_id, ":license": license_id},
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
    pub async fn get_license_users(&self, guild: GuildId, license_id: String) -> Result<Vec<u64>> {
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached("SELECT user_id FROM license_activation WHERE guild_id = :guild AND license_id = :license")?; //TODO: could use an index
                let result = statement.query_map(
                    named_params! {":guild": guild.get(), ":license": license_id},
                    |row| {
                        let user_id: u64 = row.get(0)?;
                        Ok(user_id)
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

    /// Get DB size in bytes
    pub async fn size(&self) -> Result<u64> {
        self.connection.call(move |connection| {
            let result: u64 = connection.query_row("SELECT page_count * page_size as size FROM pragma_page_count(), pragma_page_size()", [], |row| row.get(0))?;
            Ok(result)
        }).await
    }

    /// Get count of license activations
    pub async fn license_activation_count(&self) -> Result<u64> {
        self.connection.call(move |connection| {
            let result: u64 = connection.query_row("SELECT count(*) FROM license_activation LEFT JOIN guild USING (guild_id) WHERE guild.test = 0", [], |row| row.get(0))?;
            Ok(result)
        }).await
    }

    /// Get count of distinct users who have activated licenses
    pub async fn distinct_user_count(&self) -> Result<u64> {
        self.connection.call(move |connection| {
            let result: u64 = connection.query_row("SELECT count(DISTINCT user_id) FROM license_activation LEFT JOIN guild USING (guild_id) WHERE guild.test = 0", [], |row| row.get(0))?;
            Ok(result)
        }).await
    }

    /// Get count of configured guilds
    pub async fn guild_count(&self) -> Result<u64> {
        self.connection
            .call(move |connection| {
                let result: u64 = connection.query_row(
                    "SELECT count(*) FROM guild WHERE test = 0",
                    [],
                    |row| row.get(0),
                )?;
                Ok(result)
            })
            .await
    }

    /// Get count of distinct bot log channels
    pub async fn log_channel_count(&self) -> Result<u64> {
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

    /// Get count of product->role mappings
    pub async fn product_role_count(&self) -> Result<u64> {
        self.connection.call(move |connection| {
            let result: u64 = connection.query_row("SELECT count(*) FROM product_role LEFT JOIN guild USING (guild_id) WHERE guild.test = 0", [], |row| row.get(0))?;
            Ok(result)
        }).await
    }

    /// Get count of license activations in a guild
    pub async fn guild_license_activation_count(&self, guild: GuildId) -> Result<u64> {
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("SELECT count(*) FROM license_activation LEFT JOIN guild USING (guild_id) WHERE guild.guild_id = :guild")?;
            let result: u64 = statement.query_row(named_params! {":guild": guild.get()}, |row| row.get(0))?;
            Ok(result)
        }).await
    }

    /// Get count of product->role mappings in a guild
    pub async fn guild_product_role_count(&self, guild: GuildId) -> Result<u64> {
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("SELECT count(*) FROM product_role LEFT JOIN guild USING (guild_id) WHERE guild.guild_id = :guild")?;
            let result: u64 = statement.query_row(named_params! {":guild": guild.get()}, |row| row.get(0))?;
            Ok(result)
        }).await
    }

    /// Get bot log channel
    pub async fn get_log_channel(&self, guild: GuildId) -> Result<Option<ChannelId>> {
        let channel_id = self
            .connection
            .call(move |connection| {
                let mut statement = connection
                    .prepare_cached("SELECT log_channel_id FROM guild WHERE guild_id = ?")?;
                let result: Option<Option<u64>> = statement
                    .query_row([guild.get()], |row| row.get(0))
                    .optional()?;
                // inner optional is for if the guild has no log channel set
                // outer optional is for if the guild does not exist in our DB
                Ok(result.flatten())
            })
            .await?;
        Ok(channel_id.map(ChannelId::new))
    }

    /// Get all bot log channels.
    /// If `TEST_ONLY` is true, then only returns non-production servers. Otherwise, returns all servers.
    pub async fn get_log_channels<const TEST_ONLY: bool>(&self) -> Result<Vec<ChannelId>> {
        self.connection.call(move |connection| {
            let mut statement = if TEST_ONLY {
                // only non-production servers
                connection.prepare_cached("SELECT DISTINCT log_channel_id FROM guild WHERE log_channel_id IS NOT NULL AND guild.test != 0")
            } else {
                // all servers, including production servers
                connection.prepare_cached("SELECT DISTINCT log_channel_id FROM guild WHERE log_channel_id IS NOT NULL")
            }?;
            let mapped_rows = statement.query_and_then((), |row| row.get(0).map(|id| ChannelId::new(id)))?;
            let mut vec = Vec::with_capacity(mapped_rows.size_hint().0);
            for row in mapped_rows {
                vec.push(row?);
            }
            Ok(vec)
        }).await
    }

    /// Set or unset bot log channel
    pub async fn set_log_channel(&self, guild: GuildId, channel: Option<ChannelId>) -> Result<()> {
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT INTO guild (guild_id, log_channel_id) VALUES (:guild, :channel) ON CONFLICT (guild_id) DO UPDATE SET log_channel_id = excluded.log_channel_id")?;
            statement.execute(named_params! {":guild": guild.get(), ":channel": channel.map(ChannelId::get)})?;
            Ok(())
        }).await?;
        Ok(())
    }

    /// Set or unset this guild as a test guild
    pub async fn set_test(&self, guild: GuildId, test: bool) -> Result<()> {
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT INTO guild (guild_id, test) VALUES (:guild, :test) ON CONFLICT (guild_id) DO UPDATE SET test = excluded.test")?;
            statement.execute(named_params! {":guild": guild.get(), ":test": test})?;
            Ok(())
        }).await?;
        Ok(())
    }

    /// Check if a guild is a test guild
    pub async fn is_test_guild(&self, guild: GuildId) -> Result<bool> {
        self.connection
            .call(move |connection| {
                let mut statement =
                    connection.prepare_cached("SELECT test FROM guild WHERE guild_id = :guild")?;
                let test = statement
                    .query_row(named_params! {":guild": guild.get()}, |row| {
                        let test: bool = row.get(0)?;
                        Ok(test)
                    })
                    .optional()?;
                Ok(test.unwrap_or(false))
            })
            .await
    }

    /// Set or unset this guild as an owner guild (gets extra slash commands)
    pub async fn set_owner_guild(&self, guild: GuildId, owner: bool) -> Result<()> {
        self.connection.call(move |connection| {
            let mut statement = connection.prepare_cached("INSERT INTO guild (guild_id, owner) VALUES (:guild, :owner) ON CONFLICT (guild_id) DO UPDATE SET owner = excluded.owner")?;
            statement.execute(named_params! {":guild": guild.get(), ":owner": owner})?;
            Ok(())
        }).await?;
        Ok(())
    }

    /// Check if a guild is an owner guild (gets extra slash commands)
    pub async fn is_owner_guild(&self, guild: GuildId) -> Result<bool> {
        self.connection
            .call(move |connection| {
                let mut statement =
                    connection.prepare_cached("SELECT owner FROM guild WHERE guild_id = :guild")?;
                let owner = statement
                    .query_row(named_params! {":guild": guild.get()}, |row| {
                        let owner: bool = row.get(0)?;
                        Ok(owner)
                    })
                    .optional()?;
                Ok(owner.unwrap_or(false))
            })
            .await
    }

    /// Check gumroad failure count for a guild
    pub async fn get_gumroad_failure_count(&self, guild: GuildId) -> Result<Option<u64>> {
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached(
                    "SELECT gumroad_failure_count FROM guild WHERE guild_id = :guild",
                )?;
                let gumroad_failure_count = statement
                    .query_row(named_params! {":guild": guild.get()}, |row| {
                        let gumroad_failure_count: u64 = row.get(0)?;
                        Ok(gumroad_failure_count)
                    })
                    .optional()?;
                Ok(gumroad_failure_count)
            })
            .await
    }

    /// Increment gumroad failure count for a guild
    pub async fn increment_gumroad_failure_count(&self, guild: GuildId) -> Result<()> {
        self.connection
            .call(move |connection| {
                let mut statement =
                    connection.prepare_cached("UPDATE guild SET gumroad_failure_count = gumroad_failure_count + 1 WHERE guild_id = :guild")?;
                statement.execute(named_params! {":guild": guild.get()})?;
                Ok(())
            })
            .await
    }

    /// Get tuples of `(guild_id, log_channel_id)` with pending gumroad nag
    pub async fn get_guilds_pending_gumroad_nag(&self) -> Result<Vec<GuildGumroadInfo>> {
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached(
                    "SELECT guild_id, log_channel_id, gumroad_failure_count FROM guild WHERE log_channel_id IS NOT NULL AND gumroad_nag_count < 1 AND gumroad_failure_count >= 10 AND (gumroad_failure_count * 5) > (SELECT count(*) FROM license_activation WHERE license_activation.guild_id = guild.guild_id)",
                )?;
                let mapped_rows = statement
                    .query_map((), |row| Ok(GuildGumroadInfo {
                        guild_id: GuildId::new(row.get(0)?),
                        log_channel_id: ChannelId::new(row.get(1)?),
                        gumroad_failure_count: row.get(2)?,
                    }))?;
                let mut vec = Vec::with_capacity(mapped_rows.size_hint().0);
                for row in mapped_rows {
                    vec.push(row?);
                }
                Ok(vec)
            })
            .await
    }

    /// Check gumroad nag count for a guild
    pub async fn get_gumroad_nag_count(&self, guild: GuildId) -> Result<Option<u64>> {
        self.connection
            .call(move |connection| {
                let mut statement = connection.prepare_cached(
                    "SELECT gumroad_nag_count FROM guild WHERE guild_id = :guild",
                )?;
                let gumroad_nag_count = statement
                    .query_row(named_params! {":guild": guild.get()}, |row| {
                        let gumroad_nag_count: u64 = row.get(0)?;
                        Ok(gumroad_nag_count)
                    })
                    .optional()?;
                Ok(gumroad_nag_count)
            })
            .await
    }

    /// Increment gumroad nag count for a guild
    pub async fn increment_gumroad_nag_count(&self, guild: GuildId) -> Result<()> {
        self.connection
            .call(move |connection| {
                let mut statement =
                    connection.prepare_cached("UPDATE guild SET gumroad_nag_count = gumroad_nag_count + 1 WHERE guild_id = :guild")?;
                statement.execute(named_params! {":guild": guild.get()})?;
                Ok(())
            })
            .await
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
