//! PostgreSQL configuration and the original migration protocol.
mod connection;
mod migrations;
pub use connection::{ConnectionError, DatabaseOptions};
pub use migrations::{
    DEFAULT_LOCK_TIMEOUT, Migration, MigrationError, load_migrations, migrate, migrate_connection,
    migrate_with_timeout, migrations_from_sources,
};
