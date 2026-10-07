//! Native PostgreSQL codecs with checked SQL and domain-typed fields.
#![forbid(unsafe_code)]

use cannery_core::{db::DatabaseOptions, ids::UserId, timestamps::Timestamp};
use sqlx::{Row, Type, ValueRef, postgres::PgValueFormat};
use std::{error::Error, str::FromStr, time::Duration};

#[derive(Debug, PartialEq)]
struct NativeRow {
    user_id: UserId,
    created_at: Timestamp,
}

async fn isolated_connection() -> Result<sqlx::PgConnection, Box<dyn Error>> {
    let dsn = std::env::var("CANNERY_SQL_TYPE_TEST_DATABASE_URL")
        .map_err(|_| "CANNERY_SQL_TYPE_TEST_DATABASE_URL is required")?;
    let mut connection = DatabaseOptions::parse(&dsn)
        .map_err(|_| "invalid test database configuration")?
        .connect(Some(Duration::from_secs(5)))
        .await
        .map_err(|_| "test database connection failed")?;
    let name: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&mut connection)
        .await
        .map_err(|_| "database isolation check failed")?;
    let suffix = name
        .strip_prefix("conformance_")
        .ok_or("isolated database required")?;
    if suffix.len() != 24
        || !suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("isolated database required".into());
    }
    sqlx::query("SET TIME ZONE 'UTC'")
        .execute(&mut connection)
        .await
        .map_err(|_| "test timezone setup failed")?;
    Ok(connection)
}

async fn select_native(
    connection: &mut sqlx::PgConnection,
    user_id: UserId,
    created_at: Timestamp,
) -> Result<Option<NativeRow>, sqlx::Error> {
    // Wildcards infer domain fields through normal PostgreSQL Type/Decode
    // implementations, retaining runtime type compatibility checks. Bind
    // overrides use the same native Type/Encode implementations.
    sqlx::query_as!(
        NativeRow,
        r#"SELECT '8f26d098-107e-48f2-a9bc-68c8f6232498'::uuid AS "user_id!: _",
                  '2026-10-03 08:12:34.123456+05:30'::timestamptz AS "created_at!: _"
           WHERE '8f26d098-107e-48f2-a9bc-68c8f6232498'::uuid = $1::uuid
             AND '2026-10-03 08:12:34.123456+05:30'::timestamptz = $2::timestamptz"#,
        user_id as _,
        created_at as _,
    )
    .fetch_optional(connection)
    .await
}

#[tokio::test]
#[ignore = "requires an isolated PostgreSQL database"]
async fn native_checked_query_binds_and_decodes_domain_types() -> Result<(), Box<dyn Error>> {
    let mut connection = isolated_connection().await?;
    let user_id = UserId::from_str("8f26d098-107e-48f2-a9bc-68c8f6232498")?;
    let created_at = Timestamp::from_str("2026-10-03T08:12:34.123456+05:30")?;
    let row = select_native(&mut connection, user_id, created_at)
        .await
        .map_err(|_| "native checked query failed")?
        .ok_or("native checked query returned no matching row")?;
    assert_eq!(row.user_id, user_id);
    assert_eq!(row.created_at, created_at);
    // SQLx's native DateTime<FixedOffset> decoder returns a UTC offset while
    // preserving the instant and PostgreSQL's microsecond precision.
    assert_eq!(
        row.created_at.isoformat(),
        "2026-10-03T02:42:34.123456+00:00"
    );
    assert_eq!(
        serde_json::to_value(row.created_at)?,
        serde_json::json!("2026-10-03T02:42:34.123456+00:00")
    );
    let wrong_user = UserId::from_str("8f26d098-107e-48f2-a9bc-68c8f6232499")?;
    assert!(
        select_native(&mut connection, wrong_user, created_at)
            .await
            .map_err(|_| "native UUID bind query failed")?
            .is_none()
    );
    let wrong_time = Timestamp::from_str("2026-10-03T08:12:34.123457+05:30")?;
    assert!(
        select_native(&mut connection, user_id, wrong_time)
            .await
            .map_err(|_| "native timestamp bind query failed")?
            .is_none()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated PostgreSQL database"]
async fn native_timestamp_python_calendar_bounds() -> Result<(), Box<dyn Error>> {
    let mut connection = isolated_connection().await?;
    for (literal, expected) in [
        ("0001-01-01 00:00:00+00", "0001-01-01T00:00:00+00:00"),
        (
            "9999-12-31 23:59:59.999999+00",
            "9999-12-31T23:59:59.999999+00:00",
        ),
        (
            "2026-10-03 08:12:34.123456+05:30",
            "2026-10-03T02:42:34.123456+00:00",
        ),
    ] {
        let row = sqlx::query("SELECT $1::text::timestamptz")
            .bind(literal)
            .fetch_one(&mut connection)
            .await
            .map_err(|_| "valid timestamp query failed")?;
        let raw = row
            .try_get_raw(0)
            .map_err(|_| "raw timestamp unavailable")?;
        assert_eq!(raw.format(), PgValueFormat::Binary);
        assert!(<Timestamp as Type<sqlx::Postgres>>::compatible(
            &raw.type_info()
        ));
        let value: Timestamp = row
            .try_get(0)
            .map_err(|_| "valid timestamp decode failed")?;
        assert_eq!(value.isoformat(), expected);
        let rebound: Timestamp = sqlx::query_scalar("SELECT $1::timestamptz")
            .bind(value)
            .fetch_one(&mut connection)
            .await
            .map_err(|_| "valid timestamp bind failed")?;
        assert_eq!(value, rebound);
    }
    for literal in [
        "0001-01-01 00:00:00+00 BC",
        "0002-01-01 00:00:00+00 BC",
        "10000-01-01 00:00:00+00",
        "infinity",
        "-infinity",
        "0001-01-01 00:00:00+05:30",
        "9999-12-31 23:59:59.999999-05:30",
        "294276-12-31 00:00:00+00",
    ] {
        let result = sqlx::query_scalar::<_, Timestamp>("SELECT $1::text::timestamptz")
            .bind(literal)
            .fetch_one(&mut connection)
            .await;
        match result {
            Err(sqlx::Error::ColumnDecode { source, .. }) => {
                assert_eq!(
                    source.to_string(),
                    "timestamp cannot be represented by Python datetime"
                );
            }
            _ => return Err("out-of-range timestamp did not fail during decode".into()),
        }
    }
    // PostgreSQL rejects literal year zero before a row can be decoded; BC is
    // the representable PostgreSQL case corresponding to astronomical zero.
    assert!(matches!(
        sqlx::query_scalar::<_, Timestamp>("SELECT '0000-01-01 00:00:00+00'::timestamptz")
            .fetch_one(&mut connection)
            .await,
        Err(sqlx::Error::Database(_))
    ));
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated PostgreSQL database"]
async fn native_timestamp_array_and_upstream_controls() -> Result<(), Box<dyn Error>> {
    let mut connection = isolated_connection().await?;
    // The upstream Chrono codec accepts these finite dates. The wrapper must
    // impose the measured source boundary rather than trusting that codec.
    for literal in ["0001-01-01 00:00:00+00 BC", "10000-01-01 00:00:00+00"] {
        let _: chrono::DateTime<chrono::FixedOffset> =
            sqlx::query_scalar("SELECT $1::text::timestamptz")
                .bind(literal)
                .fetch_one(&mut connection)
                .await
                .map_err(|_| "upstream finite timestamp control failed")?;
    }
    let values = vec![
        Timestamp::from_str("0001-01-01T00:00:00+00:00")?,
        Timestamp::from_str("9999-12-31T23:59:59.999999+00:00")?,
    ];
    let rebound: Vec<Timestamp> = sqlx::query_scalar("SELECT $1::timestamptz[]")
        .bind(&values)
        .fetch_one(&mut connection)
        .await
        .map_err(|_| "timestamp array roundtrip failed")?;
    assert_eq!(values, rebound);
    assert!(matches!(
        sqlx::query_scalar::<_, Vec<Timestamp>>(
            "SELECT ARRAY['2026-10-03'::timestamptz, 'infinity'::timestamptz]"
        )
        .fetch_one(&mut connection)
        .await,
        Err(sqlx::Error::ColumnDecode { .. })
    ));
    Ok(())
}

#[tokio::test]
#[ignore = "requires an isolated PostgreSQL database"]
async fn native_timestamp_text_protocol_bounds() -> Result<(), Box<dyn Error>> {
    let mut connection = isolated_connection().await?;
    for (statement, expected) in [
        (
            "SELECT '0001-01-01 00:00:00+00'::timestamptz",
            Some("0001-01-01T00:00:00+00:00"),
        ),
        (
            "SELECT '9999-12-31 23:59:59.999999+00'::timestamptz",
            Some("9999-12-31T23:59:59.999999+00:00"),
        ),
        ("SELECT '0001-01-01 00:00:00+00 BC'::timestamptz", None),
        ("SELECT '10000-01-01 00:00:00+00'::timestamptz", None),
        ("SELECT 'infinity'::timestamptz", None),
        ("SELECT '-infinity'::timestamptz", None),
    ] {
        let row = sqlx::raw_sql(statement)
            .fetch_one(&mut connection)
            .await
            .map_err(|_| "text protocol query failed")?;
        assert_eq!(
            row.try_get_raw(0)
                .map_err(|_| "raw text timestamp unavailable")?
                .format(),
            PgValueFormat::Text
        );
        match (expected, row.try_get::<Timestamp, _>(0)) {
            (Some(expected), Ok(value)) => assert_eq!(value.isoformat(), expected),
            (None, Err(sqlx::Error::ColumnDecode { source, .. })) => assert_eq!(
                source.to_string(),
                "timestamp cannot be represented by Python datetime"
            ),
            _ => return Err("text protocol timestamp verdict differs from source".into()),
        }
    }
    Ok(())
}
