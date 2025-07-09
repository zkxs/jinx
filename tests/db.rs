// This file is part of jinx. Copyright © 2025 jinx contributors.
// jinx is licensed under the GNU AGPL v3.0 or any later version. See LICENSE file for full text.

//! Tests certain sqlite edge cases to make sure the sqlite library acts as expected even under edge cases

use rusqlite::Connection;

fn get_test_connection() -> Connection {
    let connection = Connection::open_in_memory().expect("Can't open in-memory database");
    connection
        .execute("PRAGMA trusted_schema = OFF;", ())
        .expect("Can't execute PRAGMA trusted_schema");
    connection
        .execute(
            "CREATE TABLE IF NOT EXISTS \"test\" ( \
                             key                    TEXT PRIMARY KEY, \
                             value                  INTEGER NOT NULL \
                         ) STRICT",
            (),
        )
        .expect("Can't create table");
    connection
}

/// Store a u64 by casting it to an i64 first. This is necessary, as rusqlite does NOT support storing values greater than
/// i64::MAX.
fn store_u64(connection: &Connection, value: u64) {
    let value = value as i64;
    let result = connection
        .execute(r#"INSERT INTO "test" ( "key", "value" ) VALUES ('key', ?)"#, [value])
        .expect("unable to insert into test table");
    assert!(result > 0);
}

/// Load a u64 by casting it from an i64. This is necessary, as rusqlite does NOT support loading values greater than
/// i64::MAX.
fn load_u64(connection: &Connection) -> u64 {
    let value: i64 = connection
        .query_row(r#"SELECT "value" FROM "test" WHERE "key" = 'key'"#, [], |row| {
            row.get(0)
        })
        .expect("unable to read test table");
    value as u64
}

/// Test a u64 store/load
fn test_u64(value: u64) {
    let connection = get_test_connection();
    let input_value = value;
    store_u64(&connection, input_value);
    let output_value = load_u64(&connection);
    assert_eq!(output_value, input_value);
}

/// sqlite can store at most a 64 bit signed integer. This test checks if a u64 well within the i64 range can be stored
#[test]
fn test_u64_normal_persistence() {
    test_u64(0);
}

/// sqlite can store at most a 64 bit signed integer. This test checks if a u64 at the i64 limit can be stored
#[test]
fn test_u64_normal_limit_persistence() {
    let value: u64 = 0x7FFF_FFFF_FFFF_FFFF;
    assert_eq!(value, 2u64.pow(63) - 1);
    test_u64(value);
}

/// sqlite can store at most a 64 bit signed integer. This test checks if a u64 right past the i64 limit can be stored.
/// This particular edge case is tricky because it ONLY has a negative representation for an i64.
#[test]
fn test_u64_edge_case_persistence() {
    let value: u64 = 0x8000_0000_0000_0000;
    assert_eq!(value, 2u64.pow(63));
    test_u64(value);
}

/// sqlite can store at most a 64 bit signed integer. This test checks if a u64 well past the i64 limit can be stored
#[test]
fn test_u64_past_limit_persistence() {
    let value: u64 = 0x8000_0000_0000_0001;
    test_u64(value);
}

/// sqlite can store at most a 64 bit signed integer. This test checks if a u64 at the limit can be stored
#[test]
fn test_u64_limit_persistence() {
    let value: u64 = 0xFFFF_FFFF_FFFF_FFFF;
    assert_eq!(value, u64::MAX);
    test_u64(value);
}

/// Test a smaller number to make sure the i64/u64 conversion doesn't screw up
#[test]
fn test_u64_127_persistence() {
    let value: u64 = 0x7F;
    test_u64(value);
}

/// Test a smaller number to make sure the i64/u64 conversion doesn't screw up
#[test]
fn test_u64_128_persistence() {
    let value: u64 = 0x80;
    test_u64(value);
}

/// Test a smaller number to make sure the i64/u64 conversion doesn't screw up
#[test]
fn test_u64_129_persistence() {
    let value: u64 = 0x81;
    test_u64(value);
}
