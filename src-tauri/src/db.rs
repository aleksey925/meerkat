use tauri_plugin_sql::{Migration, MigrationKind};

const DB_URL: &str = "sqlite:mr_notifier.db";

pub fn migrations() -> Vec<Migration> {
    vec![Migration {
        version: 1,
        description: "create initial tables",
        sql: "
            CREATE TABLE IF NOT EXISTS mr_state (
                mr_id       INTEGER PRIMARY KEY,
                unread      BOOLEAN NOT NULL DEFAULT 1,
                reminder_at TEXT,
                resolved_at TEXT,
                last_seen   TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS activity_log (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                mr_id       INTEGER NOT NULL,
                event_type  TEXT NOT NULL,
                actor_name  TEXT NOT NULL,
                description TEXT,
                created_at  TEXT NOT NULL,
                FOREIGN KEY (mr_id) REFERENCES mr_state(mr_id)
            );
        ",
        kind: MigrationKind::Up,
    }]
}

pub fn db_url() -> &'static str {
    DB_URL
}
