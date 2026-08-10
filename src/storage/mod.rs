//! Embedded `SQLite` state, ordered migrations, and profile-scoped repositories.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc
)]

use std::{
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_MEMORY_DB: AtomicU64 = AtomicU64::new(1);

use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;

const MIGRATIONS: &[(&str, &str)] = &[(
    "0001_initial.sql",
    include_str!("../../migrations/0001_initial.sql"),
)];

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database schema version {found} is newer than supported version {supported}")]
    NewerSchema { found: i64, supported: i64 },
}

pub type Result<T> = std::result::Result<T, StorageError>;

pub struct Store {
    connection: Connection,
    database_path: Option<std::path::PathBuf>,
    shared_memory_uri: Option<String>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        Self::from_connection(Connection::open(&path)?, Some(path), None)
    }

    pub fn open_in_memory() -> Result<Self> {
        let uri = format!(
            "file:websift-{}?mode=memory&cache=shared",
            NEXT_MEMORY_DB.fetch_add(1, Ordering::Relaxed)
        );
        let connection = Connection::open_with_flags(
            &uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )?;
        Self::from_connection(connection, None, Some(uri))
    }

    pub fn database_path(&self) -> Option<&Path> {
        self.database_path.as_deref()
    }

    pub fn open_worker_store(&self) -> Result<Self> {
        if let Some(path) = &self.database_path {
            return Self::open(path);
        }
        let uri = self.shared_memory_uri.as_ref().ok_or_else(|| {
            rusqlite::Error::InvalidParameterName("missing shared memory URI".into())
        })?;
        let connection = Connection::open_with_flags(
            uri,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
                | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
                | rusqlite::OpenFlags::SQLITE_OPEN_URI,
        )?;
        Ok(Self {
            connection,
            database_path: None,
            shared_memory_uri: Some(uri.clone()),
        })
    }

    fn from_connection(
        connection: Connection,
        database_path: Option<std::path::PathBuf>,
        shared_memory_uri: Option<String>,
    ) -> Result<Self> {
        connection.execute_batch(
            "PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL; PRAGMA busy_timeout = 5000;",
        )?;
        apply_migrations(&connection)?;
        Ok(Self {
            connection,
            database_path,
            shared_memory_uri,
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }
    pub fn runtime_instances(&self, profile: &str) -> RuntimeInstances<'_> {
        RuntimeInstances {
            connection: &self.connection,
            profile: profile.to_owned(),
        }
    }
    pub fn crawl_jobs(&self, profile: &str) -> CrawlJobs<'_> {
        CrawlJobs {
            connection: &self.connection,
            profile: profile.to_owned(),
        }
    }
    pub fn crawl_urls(&self, profile: &str) -> CrawlUrls<'_> {
        CrawlUrls {
            connection: &self.connection,
            profile: profile.to_owned(),
        }
    }
    pub fn documents(&self, profile: &str) -> Documents<'_> {
        Documents {
            connection: &self.connection,
            profile: profile.to_owned(),
        }
    }
    pub fn artifacts(&self, profile: &str) -> Artifacts<'_> {
        Artifacts {
            connection: &self.connection,
            profile: profile.to_owned(),
        }
    }
}

fn apply_migrations(connection: &Connection) -> Result<()> {
    connection.execute_batch("CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);")?;
    let applied = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let supported = MIGRATIONS.len() as i64;
    if applied > supported {
        return Err(StorageError::NewerSchema {
            found: applied,
            supported,
        });
    }
    for (index, (name, sql)) in MIGRATIONS.iter().enumerate().skip(applied as usize) {
        let transaction = connection.unchecked_transaction()?;
        transaction.execute_batch(sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations (version, name) VALUES (?1, ?2)",
            params![index as i64 + 1, name],
        )?;
        transaction.commit()?;
    }
    Ok(())
}

pub struct RuntimeInstances<'a> {
    connection: &'a Connection,
    profile: String,
}
impl RuntimeInstances<'_> {
    pub fn register(&self, instance_id: &str, started_at: &str, expires_at: &str) -> Result<()> {
        self.connection.execute("INSERT INTO runtime_instances (id, profile, started_at, heartbeat_at, expires_at) VALUES (?1, ?2, ?3, ?3, ?4)", params![instance_id, self.profile, started_at, expires_at])?;
        Ok(())
    }
    pub fn heartbeat(
        &self,
        instance_id: &str,
        heartbeat_at: &str,
        expires_at: &str,
    ) -> Result<bool> {
        Ok(self.connection.execute("UPDATE runtime_instances SET heartbeat_at = ?1, expires_at = ?2 WHERE id = ?3 AND profile = ?4", params![heartbeat_at, expires_at, instance_id, self.profile])? == 1)
    }
    pub fn exists(&self, instance_id: &str) -> Result<bool> {
        Ok(self
            .connection
            .query_row(
                "SELECT 1 FROM runtime_instances WHERE id = ?1 AND profile = ?2",
                params![instance_id, self.profile],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
}

pub struct CrawlJobs<'a> {
    connection: &'a Connection,
    profile: String,
}
impl CrawlJobs<'_> {
    pub fn create(
        &self,
        id: &str,
        request: &str,
        idempotency_key: Option<&str>,
        created_at: &str,
    ) -> Result<bool> {
        Ok(self.connection.execute("INSERT OR IGNORE INTO crawl_jobs (id, profile, request, idempotency_key, state, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?5)", params![id, self.profile, request, idempotency_key, created_at])? == 1)
    }
    pub fn get_state(&self, id: &str) -> Result<Option<String>> {
        Ok(self
            .connection
            .query_row(
                "SELECT state FROM crawl_jobs WHERE id = ?1 AND profile = ?2",
                params![id, self.profile],
                |row| row.get(0),
            )
            .optional()?)
    }
    pub fn set_state(
        &self,
        id: &str,
        state: &str,
        reason: Option<&str>,
        updated_at: &str,
    ) -> Result<bool> {
        Ok(self.connection.execute(
            "UPDATE crawl_jobs SET state = ?1, terminal_reason = ?2, updated_at = ?3 WHERE id = ?4 AND profile = ?5 AND (state NOT IN ('completed', 'failed', 'cancelled') OR state = ?1)",
            params![state, reason, updated_at, id, self.profile],
        )? == 1)
    }
    pub fn list(&self) -> Result<Vec<(String, String)>> {
        let mut statement = self.connection.prepare(
            "SELECT id, state FROM crawl_jobs WHERE profile = ?1 ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([&self.profile], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
    pub fn count(&self) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM crawl_jobs WHERE profile = ?1",
            [&self.profile],
            |row| row.get(0),
        )?)
    }
}

pub struct CrawlUrls<'a> {
    connection: &'a Connection,
    profile: String,
}
impl CrawlUrls<'_> {
    pub fn add(&self, id: &str, job_id: &str, normalized_url: &str, depth: i64) -> Result<bool> {
        Ok(self.connection.execute("INSERT OR IGNORE INTO crawl_urls (id, profile, job_id, normalized_url, depth, state) SELECT ?1, ?2, ?3, ?4, ?5, 'pending' WHERE EXISTS (SELECT 1 FROM crawl_jobs WHERE id = ?3 AND profile = ?2)", params![id, self.profile, job_id, normalized_url, depth])? == 1)
    }
    pub fn count_for_job(&self, job_id: &str) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM crawl_urls WHERE job_id = ?1 AND profile = ?2",
            params![job_id, self.profile],
            |row| row.get(0),
        )?)
    }
    pub fn pending(&self, job_id: &str, limit: usize) -> Result<Vec<(String, String, i64)>> {
        let mut statement = self.connection.prepare("SELECT id, normalized_url, depth FROM crawl_urls WHERE job_id = ?1 AND profile = ?2 AND state = 'pending' ORDER BY depth, id LIMIT ?3")?;
        let rows = statement.query_map(params![job_id, self.profile, limit as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
    pub fn set_state(&self, id: &str, state: &str, error: Option<&str>) -> Result<bool> {
        Ok(self.connection.execute("UPDATE crawl_urls SET state = ?1, error = ?2, attempts = attempts + 1 WHERE id = ?3 AND profile = ?4", params![state, error, id, self.profile])? == 1)
    }
}

pub struct Documents<'a> {
    connection: &'a Connection,
    profile: String,
}
impl Documents<'_> {
    pub fn add(
        &self,
        id: &str,
        job_id: &str,
        canonical_url: &str,
        content_hash: Option<&str>,
    ) -> Result<bool> {
        Ok(self.connection.execute("INSERT OR IGNORE INTO documents (id, profile, job_id, canonical_url, content_hash) SELECT ?1, ?2, ?3, ?4, ?5 WHERE EXISTS (SELECT 1 FROM crawl_jobs WHERE id = ?3 AND profile = ?2)", params![id, self.profile, job_id, canonical_url, content_hash])? == 1)
    }
    pub fn count_for_job(&self, job_id: &str) -> Result<i64> {
        Ok(self.connection.query_row(
            "SELECT COUNT(*) FROM documents WHERE job_id = ?1 AND profile = ?2",
            params![job_id, self.profile],
            |row| row.get(0),
        )?)
    }
    pub fn list_for_job(&self, job_id: &str) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT canonical_url FROM documents WHERE job_id = ?1 AND profile = ?2 ORDER BY id",
        )?;
        let rows = statement.query_map(params![job_id, self.profile], |row| row.get(0))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}

pub struct Artifacts<'a> {
    connection: &'a Connection,
    profile: String,
}
impl Artifacts<'_> {
    pub fn add(
        &self,
        id: &str,
        owner_id: &str,
        relative_path: &str,
        media_type: &str,
        size: i64,
        hash: Option<&str>,
    ) -> Result<bool> {
        Ok(self.connection.execute("INSERT OR IGNORE INTO artifacts (id, profile, owner_id, relative_path, media_type, size, hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)", params![id, self.profile, owner_id, relative_path, media_type, size, hash])? == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::Store;

    #[test]
    fn configures_sqlite_and_applies_schema() {
        let store = Store::open_in_memory().unwrap();
        assert_eq!(
            store
                .connection()
                .pragma_query_value(None, "foreign_keys", |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(
            store
                .connection()
                .query_row(
                    "SELECT 1 FROM schema_migrations WHERE version = 1",
                    [],
                    |_| Ok(())
                )
                .is_ok()
        );
    }

    #[test]
    fn profile_scopes_all_repository_operations() {
        let store = Store::open_in_memory().unwrap();
        store
            .runtime_instances("alpha")
            .register("instance", "2026-01-01", "2026-01-02")
            .unwrap();
        store
            .crawl_jobs("alpha")
            .create("job", "{}", None, "2026-01-01")
            .unwrap();
        assert_eq!(store.crawl_jobs("alpha").count().unwrap(), 1);
        assert_eq!(store.crawl_jobs("beta").count().unwrap(), 0);
        assert!(
            !store
                .crawl_urls("beta")
                .add("url", "job", "https://example.test", 0)
                .unwrap()
        );
        assert!(
            store
                .crawl_urls("alpha")
                .add("url", "job", "https://example.test", 0)
                .unwrap()
        );
        assert_eq!(store.crawl_urls("alpha").count_for_job("job").unwrap(), 1);
        assert_eq!(store.documents("beta").count_for_job("job").unwrap(), 0);
    }

    #[test]
    fn uniqueness_is_idempotent() {
        let store = Store::open_in_memory().unwrap();
        assert!(
            store
                .crawl_jobs("p")
                .create("j", "{}", Some("key"), "now")
                .unwrap()
        );
        assert!(
            !store
                .crawl_jobs("p")
                .create("j2", "{}", Some("key"), "now")
                .unwrap()
        );
        assert!(
            store
                .crawl_urls("p")
                .add("u", "j", "https://example.test", 0)
                .unwrap()
        );
        assert!(
            !store
                .crawl_urls("p")
                .add("u2", "j", "https://example.test", 0)
                .unwrap()
        );
    }
}
