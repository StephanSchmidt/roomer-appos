//! The platform database. Only the shell touches it; apps go through the
//! platform APIs.

use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    team TEXT NOT NULL,
    is_admin INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS pods (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL UNIQUE
);

-- One booking per pod and hour, enforced here no matter which app version runs.
CREATE TABLE IF NOT EXISTS bookings (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    pod_id INTEGER NOT NULL REFERENCES pods(id) ON DELETE CASCADE,
    date TEXT NOT NULL,
    hour INTEGER NOT NULL,
    subject TEXT NOT NULL,
    booker_id TEXT NOT NULL REFERENCES users(id),
    created_at TEXT NOT NULL,
    UNIQUE (pod_id, date, hour)
);

-- Every deploy attempt, including refused ones. At most one row per app and
-- version is ever pending (only while it loads), active or retired.
CREATE TABLE IF NOT EXISTS app_versions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    app_id TEXT NOT NULL,
    version TEXT NOT NULL,
    name TEXT NOT NULL,
    author TEXT NOT NULL,
    manifest TEXT NOT NULL,
    wasm BLOB,
    wasm_hash TEXT NOT NULL,
    imports TEXT NOT NULL,
    status TEXT NOT NULL,
    reason TEXT,
    -- The appfuzz review as JSON; NULL for versions installed before review.
    review TEXT,
    deployed_by TEXT NOT NULL,
    deployed_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS deploy_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at TEXT NOT NULL,
    app_id TEXT NOT NULL,
    version TEXT NOT NULL,
    kind TEXT NOT NULL,
    message TEXT NOT NULL,
    actor TEXT NOT NULL
);

-- Small shell state, such as which events the platform team has seen.
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- What each app request cost. Only the most recent requests per app are kept.
CREATE TABLE IF NOT EXISTS request_stats (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    at INTEGER NOT NULL,
    app_id TEXT NOT NULL,
    version TEXT NOT NULL,
    slot TEXT NOT NULL,
    outcome TEXT NOT NULL,
    io_units INTEGER NOT NULL,
    platform_us INTEGER NOT NULL,
    fuel INTEGER NOT NULL,
    total_us INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS request_stats_app ON request_stats (app_id, id);
"#;

const SEED_USERS: &[(&str, &str, &str, bool)] = &[
    ("alice", "Alice", "Marketing", false),
    ("bob", "Bob", "Product", false),
    ("dana", "Dana", "Platform team", true),
];

const SEED_PODS: &[&str] = &["Focus Pod A", "Focus Pod B", "Quiet Room", "Phone Booth"];

pub fn open(path: &str) -> anyhow::Result<Connection> {
    let conn = Connection::open(path)?;
    // A single connection, so this applies to every query.
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;
    conn.execute_batch(SCHEMA)?;

    for (id, name, team, admin) in SEED_USERS {
        conn.execute(
            "INSERT OR IGNORE INTO users (id, name, team, is_admin) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, team, admin],
        )?;
    }
    let pods: i64 = conn.query_row("SELECT COUNT(*) FROM pods", [], |r| r.get(0))?;
    if pods == 0 {
        for name in SEED_PODS {
            conn.execute("INSERT INTO pods (name) VALUES (?1)", [name])?;
        }
    }
    Ok(conn)
}

#[derive(Debug, Clone)]
pub struct User {
    pub id: String,
    pub name: String,
    pub team: String,
    pub is_admin: bool,
}

pub fn users(conn: &Connection) -> rusqlite::Result<Vec<User>> {
    let mut stmt = conn.prepare("SELECT id, name, team, is_admin FROM users ORDER BY is_admin, name")?;
    stmt.query_map([], row_to_user)?.collect()
}

pub fn user(conn: &Connection, id: &str) -> rusqlite::Result<Option<User>> {
    conn.query_row("SELECT id, name, team, is_admin FROM users WHERE id = ?1", [id], row_to_user).optional()
}

fn row_to_user(r: &rusqlite::Row) -> rusqlite::Result<User> {
    Ok(User { id: r.get(0)?, name: r.get(1)?, team: r.get(2)?, is_admin: r.get(3)? })
}

pub fn now() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

pub fn event(conn: &Connection, app_id: &str, version: &str, kind: &str, message: &str, actor: &str) {
    let _ = conn.execute(
        "INSERT INTO deploy_events (at, app_id, version, kind, message, actor) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![now(), app_id, version, kind, message, actor],
    );
}
