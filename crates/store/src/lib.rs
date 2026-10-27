//! PostgreSQL storage for accounts, events and tiers.
//!
//! `dx-auth` and `dx-admin` keep their data in memory and know nothing about databases. They
//! each define a small "save it" interface ([`dx_auth::UserRepo`], [`dx_admin::AdminRepo`]).
//! [`Db`] implements both, so the gateway can attach one connection to both of them. Without a
//! database the two crates simply keep working in memory.
//!
//! The tables are created by `migrations/0001_accounts_and_events.sql`, run by [`Db::migrate`].

use std::sync::Arc;
use std::time::Duration;

use dx_admin::{AdminRepo, StoredEvent, StoredTier};
use dx_auth::{RepoError as AuthRepoError, RepoFuture as AuthFuture, Role, StoredUser, UserRepo};
use dx_types::MarketConfig;
use sqlx::postgres::PgPoolOptions;
use sqlx::types::Json;
use sqlx::{Error, PgPool};

const MIGRATION: &str = include_str!("../migrations/0001_accounts_and_events.sql");
/// Any number, as long as only this migration uses it for its lock.
const MIGRATION_LOCK: i64 = 7_340_001;

/// A connection pool to the database. Cheap to clone.
#[derive(Clone)]
pub struct Db {
    pool: PgPool,
}

/// What the database already holds for the admin console.
pub struct SavedAdmin {
    pub events: Vec<StoredEvent>,
    pub tiers: Vec<StoredTier>,
}

fn is_unique_violation(error: &Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|database| database.is_unique_violation())
}

fn failed(error: Error) -> String {
    error.to_string()
}

impl Db {
    /// Connects, for example to `postgres://darkhold:darkhold@localhost:5432/darkhold`.
    /// Fails (instead of waiting forever) if the database does not answer in a few seconds.
    pub async fn connect(url: &str) -> Result<Db, String> {
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(Duration::from_secs(5))
            .connect(url)
            .await
            .map_err(failed)?;
        Ok(Db { pool })
    }

    /// Creates the tables if they do not exist yet. Safe to call on every start, and safe if
    /// two servers start at the same moment: a database lock makes them take turns (without
    /// it Postgres can fail with a "duplicate key" error while both create the same table).
    pub async fn migrate(&self) -> Result<(), String> {
        let mut transaction = self.pool.begin().await.map_err(failed)?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(MIGRATION_LOCK)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        sqlx::raw_sql(MIGRATION)
            .execute(&mut *transaction)
            .await
            .map_err(failed)?;
        transaction.commit().await.map_err(failed)
    }

    /// Every saved account.
    pub async fn load_users(&self) -> Result<Vec<StoredUser>, String> {
        let rows: Vec<(i64, String, String, String)> =
            sqlx::query_as("SELECT user_id, username, role, password_hash FROM app_users")
                .fetch_all(&self.pool)
                .await
                .map_err(failed)?;
        rows.into_iter()
            .map(|(user_id, username, role, password_hash)| {
                Ok(StoredUser {
                    user_id: u64::try_from(user_id).map_err(|_| "negative user id".to_string())?,
                    role: Role::parse(&role).ok_or_else(|| format!("unknown role {role:?}"))?,
                    username,
                    password_hash,
                })
            })
            .collect()
    }

    /// Every saved event and tier.
    pub async fn load_admin(&self) -> Result<SavedAdmin, String> {
        let events: Vec<(i64, String, Option<String>)> =
            sqlx::query_as("SELECT event_id, name, venue FROM admin_events ORDER BY event_id")
                .fetch_all(&self.pool)
                .await
                .map_err(failed)?;
        let tiers: Vec<(i64, i64, String, Json<MarketConfig>, bool)> = sqlx::query_as(
            "SELECT market_id, event_id, tier_name, config, published \
             FROM admin_tiers ORDER BY market_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(failed)?;
        let id32 = |value: i64| u32::try_from(value).map_err(|_| format!("bad id {value}"));
        Ok(SavedAdmin {
            events: events
                .into_iter()
                .map(|(event_id, name, venue)| {
                    Ok(StoredEvent {
                        event_id: id32(event_id)?,
                        name,
                        venue,
                    })
                })
                .collect::<Result<_, String>>()?,
            tiers: tiers
                .into_iter()
                .map(|(market_id, event_id, tier_name, config, published)| {
                    Ok(StoredTier {
                        market_id: id32(market_id)?,
                        event_id: id32(event_id)?,
                        tier_name,
                        config: config.0,
                        published,
                    })
                })
                .collect::<Result<_, String>>()?,
        })
    }
}

impl UserRepo for Db {
    fn insert(&self, user: StoredUser) -> AuthFuture<()> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let user_id = i64::try_from(user.user_id)
                .map_err(|_| AuthRepoError::Failed("user id too large".into()))?;
            sqlx::query(
                "INSERT INTO app_users (user_id, username, role, password_hash) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind(user_id)
            .bind(&user.username)
            .bind(user.role.as_str())
            .bind(&user.password_hash)
            .execute(&pool)
            .await
            .map(|_| ())
            .map_err(|error| {
                if is_unique_violation(&error) {
                    AuthRepoError::UsernameTaken
                } else {
                    AuthRepoError::Failed(failed(error))
                }
            })
        })
    }

    fn upsert(&self, user: StoredUser) -> AuthFuture<()> {
        let pool = self.pool.clone();
        Box::pin(async move {
            let user_id = i64::try_from(user.user_id)
                .map_err(|_| AuthRepoError::Failed("user id too large".into()))?;
            sqlx::query(
                "INSERT INTO app_users (user_id, username, role, password_hash) \
                 VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (username) DO UPDATE \
                 SET role = EXCLUDED.role, password_hash = EXCLUDED.password_hash",
            )
            .bind(user_id)
            .bind(&user.username)
            .bind(user.role.as_str())
            .bind(&user.password_hash)
            .execute(&pool)
            .await
            .map(|_| ())
            .map_err(|error| AuthRepoError::Failed(failed(error)))
        })
    }
}

impl AdminRepo for Db {
    fn save_event(&self, event: StoredEvent) -> dx_admin::RepoFuture<()> {
        let pool = self.pool.clone();
        Box::pin(async move {
            sqlx::query("INSERT INTO admin_events (event_id, name, venue) VALUES ($1, $2, $3)")
                .bind(i64::from(event.event_id))
                .bind(&event.name)
                .bind(&event.venue)
                .execute(&pool)
                .await
                .map(|_| ())
                .map_err(|error| dx_admin::RepoError::Failed(failed(error)))
        })
    }

    fn save_tier(&self, tier: StoredTier) -> dx_admin::RepoFuture<()> {
        let pool = self.pool.clone();
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO admin_tiers (market_id, event_id, tier_name, config, published) \
                 VALUES ($1, $2, $3, $4, $5) \
                 ON CONFLICT (market_id) DO UPDATE \
                 SET config = EXCLUDED.config, published = EXCLUDED.published",
            )
            .bind(i64::from(tier.market_id))
            .bind(i64::from(tier.event_id))
            .bind(&tier.tier_name)
            .bind(Json(&tier.config))
            .bind(tier.published)
            .execute(&pool)
            .await
            .map(|_| ())
            .map_err(|error| {
                if is_unique_violation(&error) {
                    dx_admin::RepoError::Duplicate
                } else {
                    dx_admin::RepoError::Failed(failed(error))
                }
            })
        })
    }
}

/// Connects, creates the tables and loads everything, then attaches the database to the login
/// and admin state. This is the one call the gateway makes when `DATABASE_URL` is set.
pub async fn attach(
    url: &str,
    auth: &dx_auth::AuthState,
    admin: &dx_admin::AdminState,
) -> Result<(), String> {
    let db = Db::connect(url).await?;
    db.migrate().await?;
    let users = db.load_users().await?;
    let saved = db.load_admin().await?;
    let repo = Arc::new(db);
    auth.attach_repo(repo.clone(), users).await?;
    admin.attach_repo(repo, saved.events, saved.tiers).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use dx_types::seed_markets;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// These tests need a real PostgreSQL. They run only when `DX_TEST_DATABASE_URL` is set, for
    /// example `postgres://darkhold:darkhold@localhost:5432/darkhold_test`. Point it at a
    /// database of its own, never the one the gateway uses: new sign-ups continue from the
    /// highest saved id, so the very large ids these tests write would be inherited by real
    /// accounts. The tests never delete anything: every row they write has an id and a name
    /// made from the clock, so runs do not disturb each other.
    async fn test_db() -> Option<Db> {
        let Ok(url) = std::env::var("DX_TEST_DATABASE_URL") else {
            eprintln!("skipped: DX_TEST_DATABASE_URL is not set");
            return None;
        };
        let db = Db::connect(&url).await.expect("connect");
        db.migrate().await.expect("migrate");
        db.migrate().await.expect("migrating twice is fine");
        Some(db)
    }

    /// A number that is different on every run.
    fn unique() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
            % 1_000_000_000
    }

    fn user(user_id: u64, username: &str, hash: &str) -> StoredUser {
        StoredUser {
            user_id,
            username: username.to_string(),
            role: Role::User,
            password_hash: hash.to_string(),
        }
    }

    #[tokio::test]
    async fn accounts_are_saved_loaded_and_never_duplicated() {
        let Some(db) = test_db().await else { return };
        let n = unique();
        let id = 9_000_000_000 + n;
        let name = format!("t_user_{n}");

        let alice = user(id, &name, "hash-1");
        db.insert(alice.clone()).await.unwrap();
        assert!(db.load_users().await.unwrap().contains(&alice));

        // The same username again, or the same id again, is refused.
        let same_name = user(id + 1, &name, "hash-2");
        assert_eq!(
            db.insert(same_name).await,
            Err(AuthRepoError::UsernameTaken)
        );
        let same_id = user(id, &format!("{name}_b"), "hash-3");
        assert_eq!(db.insert(same_id).await, Err(AuthRepoError::UsernameTaken));

        // An upsert replaces the password hash and role, and keeps the id.
        let mut changed = alice.clone();
        changed.role = Role::Admin;
        changed.password_hash = "hash-new".into();
        changed.user_id = id + 2;
        db.upsert(changed).await.unwrap();
        let saved = db.load_users().await.unwrap();
        let row = saved.iter().find(|u| u.username == name).unwrap();
        assert_eq!(
            (row.user_id, row.role, row.password_hash.as_str()),
            (id, Role::Admin, "hash-new")
        );
    }

    #[tokio::test]
    async fn events_and_tiers_are_saved_loaded_and_checked() {
        let Some(db) = test_db().await else { return };
        let n = unique();
        let event_id = 3_000_000_000u32 + n as u32;
        let market_id = 3_000_000_000u32 + n as u32;

        let event = StoredEvent {
            event_id,
            name: format!("Event {n}"),
            venue: Some("Main hall".into()),
        };
        db.save_event(event.clone()).await.unwrap();

        let mut config = seed_markets()[0].clone();
        config.id = market_id;
        config.event_id = event_id;
        let draft = StoredTier {
            market_id,
            event_id,
            tier_name: "General".into(),
            config: config.clone(),
            published: false,
        };
        db.save_tier(draft.clone()).await.unwrap();

        let saved = db.load_admin().await.unwrap();
        assert!(saved.events.contains(&event));
        assert!(saved.tiers.contains(&draft));

        // Saving the same market again updates it: that is how publishing is saved.
        let published = StoredTier {
            published: true,
            ..draft.clone()
        };
        db.save_tier(published.clone()).await.unwrap();
        let saved = db.load_admin().await.unwrap();
        assert!(saved.tiers.contains(&published));
        assert!(!saved.tiers.contains(&draft));

        // Another tier of the same event with the same name (any capitals) is a duplicate.
        let clash = StoredTier {
            market_id: market_id + 1,
            tier_name: "GENERAL".into(),
            ..draft.clone()
        };
        assert_eq!(
            db.save_tier(clash).await,
            Err(dx_admin::RepoError::Duplicate)
        );

        // A tier of an event that does not exist is refused.
        let orphan = StoredTier {
            market_id: market_id + 2,
            event_id: event_id + 1_000_000,
            tier_name: "Orphan".into(),
            ..draft
        };
        assert!(matches!(
            db.save_tier(orphan).await,
            Err(dx_admin::RepoError::Failed(_))
        ));
    }
}
