// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! Tests certain sqlite edge cases to make sure the sqlite library acts as expected even under edge cases

use semver::Version;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Executor, SqlitePool};

async fn get_test_connection() -> SqlitePool {
    let pool_options = SqlitePoolOptions::new().max_connections(1);
    let connect_options = SqliteConnectOptions::new()
        .foreign_keys(true)
        .in_memory(true)
        .shared_cache(false) // superseded by WAL mode
        .pragma("trusted_schema", "OFF");
    let pool = pool_options
        .connect_with(connect_options)
        .await
        .expect("Can't open in-memory database");

    pool.execute(
        r#"CREATE TABLE IF NOT EXISTS "test" (
                     key    TEXT PRIMARY KEY,
                     value  INTEGER NOT NULL
                 ) STRICT"#,
    )
    .await
    .expect("Can't create table");
    pool
}

/// Store a u64 by casting it to an i64 first. This is necessary, as sqlx does NOT support storing u64 at all
async fn store_u64_hack(pool: &SqlitePool, value: u64) {
    let value = value as i64;
    let result = sqlx::query(r#"INSERT INTO "test" ( "key", "value" ) VALUES ('key', ?)"#)
        .bind(value)
        .execute(pool)
        .await
        .expect("unable to insert into test table");
    assert!(result.rows_affected() > 0);
}

/// Load a u64 by casting it from an i64. This is necessary, as sqlx does NOT support loading values greater than
/// i64::MAX and will fail
async fn load_u64_hack(pool: &SqlitePool) -> u64 {
    let value: i64 = sqlx::query_scalar(r#"SELECT "value" FROM "test" WHERE "key" = 'key'"#)
        .fetch_one(pool)
        .await
        .expect("unable to read test table");
    value as u64
}

/// Test a u64 store/load
async fn test_u64(value: u64) {
    let connection = get_test_connection().await;
    let input_value = value;
    store_u64_hack(&connection, input_value).await;
    let output_value = load_u64_hack(&connection).await;
    assert_eq!(output_value, input_value);
}

/// sqlite can store at most a 64 bit signed integer. This test checks if a u64 well within the i64 range can be stored
#[tokio::test(flavor = "current_thread")]
async fn test_u64_normal_persistence() {
    test_u64(0).await;
}

/// sqlite can store at most a 64 bit signed integer. This test checks if a u64 at the i64 limit can be stored
#[tokio::test(flavor = "current_thread")]
async fn test_u64_normal_limit_persistence() {
    let value: u64 = 0x7FFF_FFFF_FFFF_FFFF;
    assert_eq!(value, 2u64.pow(63) - 1);
    test_u64(value).await;
}

/// sqlite can store at most a 64 bit signed integer. This test checks if a u64 right past the i64 limit can be stored.
/// This particular edge case is tricky because it ONLY has a negative representation for an i64.
#[tokio::test(flavor = "current_thread")]
async fn test_u64_edge_case_persistence() {
    let value: u64 = 0x8000_0000_0000_0000;
    assert_eq!(value, 2u64.pow(63));
    test_u64(value).await;
}

/// sqlite can store at most a 64 bit signed integer. This test checks if a u64 well past the i64 limit can be stored
#[tokio::test(flavor = "current_thread")]
async fn test_u64_past_limit_persistence() {
    let value: u64 = 0x8000_0000_0000_0001;
    test_u64(value).await;
}

/// sqlite can store at most a 64 bit signed integer. This test checks if a u64 at the limit can be stored
#[tokio::test(flavor = "current_thread")]
async fn test_u64_limit_persistence() {
    let value: u64 = 0xFFFF_FFFF_FFFF_FFFF;
    assert_eq!(value, u64::MAX);
    test_u64(value).await;
}

/// Test a smaller number to make sure the i64/u64 conversion doesn't screw up
#[tokio::test(flavor = "current_thread")]
async fn test_u64_127_persistence() {
    let value: u64 = 0x7F;
    test_u64(value).await;
}

/// Test a smaller number to make sure the i64/u64 conversion doesn't screw up
#[tokio::test(flavor = "current_thread")]
async fn test_u64_128_persistence() {
    let value: u64 = 0x80;
    test_u64(value).await;
}

/// Test a smaller number to make sure the i64/u64 conversion doesn't screw up
#[tokio::test(flavor = "current_thread")]
async fn test_u64_129_persistence() {
    let value: u64 = 0x81;
    test_u64(value).await;
}

/// Assert that the bundled sqlite version is at least as new as some expected version
#[tokio::test(flavor = "current_thread")]
async fn assert_sqlite_version() {
    let connection = get_test_connection().await;
    let row: String = sqlx::query_scalar(r#"SELECT sqlite_version()"#)
        .fetch_one(&connection)
        .await
        .expect("can't get version row");
    let version = Version::parse(&row).expect("can't parse version");
    let expected = Version::new(3, 51, 1);
    assert!(version >= expected, "Expected {version} >= {expected}");
}
