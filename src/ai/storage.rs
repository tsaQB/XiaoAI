use chrono::Local;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

#[derive(Debug, Clone)]
pub(crate) struct TelegramInboxRecord {
    pub update_id: i64,
    pub payload_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub endpoint: String,
    pub api_key: String,
    pub models: Vec<String>,
    pub active_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderStore {
    pub active_id: Option<String>,
    pub providers: Vec<ProviderConfig>,
    #[serde(default)]
    pub telegram_models: Vec<String>,
}

pub fn get_providers_store_path() -> std::path::PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return std::path::Path::new(&home).join(".xiao_providers.json");
    }
    std::path::Path::new(".xiao_providers.json").to_path_buf()
}

pub fn load_provider_store() -> ProviderStore {
    if let Ok(conn) = open_session_db() {
        if let Ok(value) = conn.query_row(
            "SELECT value FROM settings WHERE key='provider_store'",
            [],
            |row| row.get::<_, String>(0),
        ) {
            if let Ok(store) = serde_json::from_str(&value) {
                return store;
            }
        }
    }
    let p = get_providers_store_path();
    if p.exists() {
        if let Ok(content) = std::fs::read_to_string(&p) {
            if let Ok(store) = serde_json::from_str::<ProviderStore>(&content) {
                if save_provider_store(&store).is_ok() {
                    // The legacy JSON contains provider API keys in plaintext.
                    // Remove it only after the SQLite migration committed.
                    if let Err(err) = std::fs::remove_file(&p) {
                        warn!("Failed to remove migrated legacy provider file: {err}");
                    }
                }
                return store;
            }
        }
    }
    ProviderStore::default()
}

pub fn save_provider_store(store: &ProviderStore) -> std::io::Result<()> {
    save_provider_state_db(store)
}

fn save_provider_state_db(store: &ProviderStore) -> std::io::Result<()> {
    let json_str = serde_json::to_string_pretty(store).map_err(std::io::Error::other)?;
    let mut conn = open_session_db().map_err(|e| std::io::Error::other(e.to_string()))?;
    let tx = conn
        .transaction()
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    tx.execute(
        "INSERT INTO settings(key,value) VALUES('provider_store',?1)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![json_str],
    )
    .map_err(|e| std::io::Error::other(e.to_string()))?;

    let active = store
        .active_id
        .as_deref()
        .and_then(|id| store.providers.iter().find(|provider| provider.id == id))
        .or_else(|| store.providers.first());

    for (key, value) in [
        (
            "AI_ENDPOINT",
            active.map(|p| p.endpoint.as_str()).unwrap_or(""),
        ),
        (
            "AI_API_KEY",
            active.map(|p| p.api_key.as_str()).unwrap_or(""),
        ),
        (
            "AI_MODEL",
            active.map(|p| p.active_model.as_str()).unwrap_or(""),
        ),
    ] {
        tx.execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![format!("app:{key}"), value],
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    }

    tx.commit()
        .map_err(|e| std::io::Error::other(e.to_string()))
}

pub(super) async fn persist_provider_state(store: ProviderStore) -> bool {
    match tokio::task::spawn_blocking(move || save_provider_state_db(&store)).await {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            warn!("Failed to persist provider state: {err}");
            false
        }
        Err(err) => {
            warn!("Provider persistence task failed: {err}");
            false
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub id: usize,
    pub name: String,
    pub messages: Vec<ChatMessage>,
    pub created_at: String,
}

#[cfg(unix)]
fn harden_dir_mode(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = std::fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o700);
        if let Err(err) = std::fs::set_permissions(path, permissions) {
            warn!("Failed to harden XiaoAI data directory permissions: {err}");
        }
    }
}

#[cfg(not(unix))]
fn harden_dir_mode(_path: &std::path::Path) {}

#[cfg(unix)]
fn harden_file_mode(path: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = std::fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        if let Err(err) = std::fs::set_permissions(path, permissions) {
            warn!("Failed to harden XiaoAI data file permissions: {err}");
        }
    }
}

#[cfg(not(unix))]
fn harden_file_mode(_path: &std::path::Path) {}

fn session_db_path() -> std::path::PathBuf {
    let base = std::env::var("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    base.join(".local/share/xiaoai/xiaoai.db")
}

fn open_session_db() -> rusqlite::Result<Connection> {
    let path = session_db_path();
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            warn!("Failed to create XiaoAI data directory: {err}");
        }
        harden_dir_mode(parent);
    }
    let conn = Connection::open(&path)?;
    harden_file_mode(&path);
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA busy_timeout=5000;
        CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS sessions (
            user_id INTEGER NOT NULL, session_id INTEGER NOT NULL, name TEXT NOT NULL,
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL, PRIMARY KEY(user_id, session_id)
        );
        CREATE TABLE IF NOT EXISTS messages (
            user_id INTEGER NOT NULL, session_id INTEGER NOT NULL, role TEXT NOT NULL,
            content TEXT NOT NULL, created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS active_sessions (
            user_id INTEGER PRIMARY KEY, session_id INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS session_counters (
            user_id INTEGER PRIMARY KEY, next_session_id INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS telegram_state (
            key TEXT PRIMARY KEY, value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS telegram_inbox (
            update_id INTEGER PRIMARY KEY,
            payload_json TEXT NOT NULL,
            status TEXT NOT NULL,
            attempts INTEGER NOT NULL DEFAULT 0,
            received_at TEXT NOT NULL,
            last_error TEXT
        );",
    )?;
    // WAL/SHM files may be created lazily. The private 0700 parent directory
    // is the primary boundary; harden sidecars whenever they already exist.
    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        let parent = path.parent().unwrap_or_else(|| std::path::Path::new("."));
        harden_file_mode(&parent.join(format!("{name}-wal")));
        harden_file_mode(&parent.join(format!("{name}-shm")));
    }
    Ok(conn)
}

fn load_sessions_db(user_id: i64) -> rusqlite::Result<Vec<ChatSession>> {
    let conn = open_session_db()?;
    let mut stmt = conn.prepare(
        "SELECT session_id,name,created_at FROM sessions WHERE user_id=?1 ORDER BY session_id",
    )?;
    let mut rows = stmt.query(params![user_id])?;
    let mut sessions = Vec::new();
    while let Some(row) = rows.next()? {
        let id: usize = row.get(0)?;
        let mut session = ChatSession {
            id,
            name: row.get(1)?,
            messages: Vec::new(),
            created_at: row.get(2)?,
        };
        let mut msg_stmt = conn.prepare(
            "SELECT role,content FROM messages WHERE user_id=?1 AND session_id=?2 ORDER BY rowid",
        )?;
        let mut msg_rows = msg_stmt.query(params![user_id, id])?;
        while let Some(msg) = msg_rows.next()? {
            let content: String = msg.get(1)?;
            session.messages.push(ChatMessage {
                role: msg.get(0)?,
                content: serde_json::from_str(&content).unwrap_or(Value::String(content)),
            });
        }
        sessions.push(session);
    }
    Ok(sessions)
}

pub(super) fn compute_next_session_id(stored_next: Option<usize>, max_existing: usize) -> usize {
    stored_next
        .unwrap_or_else(|| max_existing.saturating_add(1))
        .max(max_existing.saturating_add(1))
        .max(1)
}

pub(super) fn legacy_active_session_id(
    legacy_index: Option<usize>,
    sessions: &[ChatSession],
) -> Option<usize> {
    if sessions.is_empty() {
        return None;
    }
    Some(
        legacy_index
            .and_then(|index| sessions.get(index).map(|session| session.id))
            .unwrap_or(sessions[0].id),
    )
}

fn allocate_session_id_db(user_id: i64) -> rusqlite::Result<usize> {
    let mut conn = open_session_db()?;
    let tx = conn.transaction()?;
    let max_existing: usize = tx.query_row(
        "SELECT COALESCE(MAX(session_id),0) FROM sessions WHERE user_id=?1",
        params![user_id],
        |row| row.get(0),
    )?;
    let stored_next = tx
        .query_row(
            "SELECT next_session_id FROM session_counters WHERE user_id=?1",
            params![user_id],
            |row| row.get::<_, usize>(0),
        )
        .ok();
    let next_id = compute_next_session_id(stored_next, max_existing);
    tx.execute(
        "INSERT INTO session_counters(user_id,next_session_id) VALUES(?1,?2)
         ON CONFLICT(user_id) DO UPDATE SET next_session_id=excluded.next_session_id",
        params![user_id, next_id.saturating_add(1)],
    )?;
    tx.commit()?;
    Ok(next_id)
}

fn save_session_metadata_db(user_id: i64, session: &ChatSession) -> rusqlite::Result<()> {
    let conn = open_session_db()?;
    let now = Local::now().to_rfc3339();
    conn.execute(
        "INSERT INTO sessions(user_id,session_id,name,created_at,updated_at) VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(user_id,session_id) DO UPDATE SET name=excluded.name,updated_at=excluded.updated_at",
        params![user_id, session.id, session.name, session.created_at, now],
    )?;
    Ok(())
}

fn replace_session_messages_db(user_id: i64, session: &ChatSession) -> rusqlite::Result<()> {
    let mut conn = open_session_db()?;
    let tx = conn.transaction()?;
    let now = Local::now().to_rfc3339();
    tx.execute(
        "INSERT INTO sessions(user_id,session_id,name,created_at,updated_at) VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(user_id,session_id) DO UPDATE SET name=excluded.name,updated_at=excluded.updated_at",
        params![user_id, session.id, session.name, session.created_at, now],
    )?;
    tx.execute(
        "DELETE FROM messages WHERE user_id=?1 AND session_id=?2",
        params![user_id, session.id],
    )?;
    for message in &session.messages {
        let content = serde_json::to_string(&message.content).unwrap_or_default();
        tx.execute(
            "INSERT INTO messages(user_id,session_id,role,content,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![user_id, session.id, message.role, content, now],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn append_session_messages_db(
    user_id: i64,
    session: &ChatSession,
    messages: &[ChatMessage],
) -> rusqlite::Result<()> {
    let mut conn = open_session_db()?;
    let tx = conn.transaction()?;
    let now = Local::now().to_rfc3339();
    tx.execute(
        "UPDATE sessions SET name=?3,updated_at=?4 WHERE user_id=?1 AND session_id=?2",
        params![user_id, session.id, session.name, now],
    )?;
    for message in messages {
        let content = serde_json::to_string(&message.content).unwrap_or_default();
        tx.execute(
            "INSERT INTO messages(user_id,session_id,role,content,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![user_id, session.id, message.role, content, now],
        )?;
    }
    tx.commit()?;
    Ok(())
}

fn delete_session_db(user_id: i64, session_id: usize) -> rusqlite::Result<()> {
    let mut conn = open_session_db()?;
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM messages WHERE user_id=?1 AND session_id=?2",
        params![user_id, session_id],
    )?;
    tx.execute(
        "DELETE FROM sessions WHERE user_id=?1 AND session_id=?2",
        params![user_id, session_id],
    )?;
    tx.commit()?;
    Ok(())
}

fn save_active_session_db(user_id: i64, session_id: usize) -> rusqlite::Result<()> {
    let conn = open_session_db()?;
    conn.execute(
        "INSERT INTO active_sessions(user_id,session_id) VALUES(?1,?2)
         ON CONFLICT(user_id) DO UPDATE SET session_id=excluded.session_id",
        params![user_id, session_id],
    )?;
    Ok(())
}

fn ensure_session_identity_v2_db(user_id: i64, sessions: &[ChatSession]) -> rusqlite::Result<()> {
    if sessions.is_empty() {
        return Ok(());
    }
    let conn = open_session_db()?;
    let marker = format!("session_identity_v2:{user_id}");
    let migrated = conn
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            params![&marker],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .is_some();

    if !migrated {
        let legacy_value = conn
            .query_row(
                "SELECT session_id FROM active_sessions WHERE user_id=?1",
                params![user_id],
                |row| row.get::<_, usize>(0),
            )
            .ok();
        let stable_id = legacy_active_session_id(legacy_value, sessions).unwrap_or(sessions[0].id);
        save_active_session_db(user_id, stable_id)?;
        conn.execute(
            "INSERT INTO settings(key,value) VALUES(?1,'1') ON CONFLICT(key) DO UPDATE SET value='1'",
            params![&marker],
        )?;
    }

    let next_id = sessions
        .iter()
        .map(|session| session.id)
        .max()
        .unwrap_or(0)
        .saturating_add(1)
        .max(1);
    conn.execute(
        "INSERT INTO session_counters(user_id,next_session_id) VALUES(?1,?2)
         ON CONFLICT(user_id) DO UPDATE SET next_session_id=MAX(next_session_id,excluded.next_session_id)",
        params![user_id, next_id],
    )?;
    Ok(())
}

async fn run_db<T, F>(operation: &'static str, task: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> rusqlite::Result<T> + Send + 'static,
{
    match tokio::task::spawn_blocking(task).await {
        Ok(Ok(value)) => Some(value),
        Ok(Err(err)) => {
            warn!("SQLite operation {operation} failed: {err}");
            None
        }
        Err(err) => {
            warn!("SQLite task {operation} failed: {err}");
            None
        }
    }
}

fn load_telegram_offset_db() -> rusqlite::Result<Option<i64>> {
    let conn = open_session_db()?;
    let value = conn
        .query_row(
            "SELECT value FROM telegram_state WHERE key='offset'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok();
    Ok(value.and_then(|value| value.parse::<i64>().ok()))
}

fn enqueue_telegram_update_db(update_id: i64, payload_json: &str) -> rusqlite::Result<bool> {
    let mut conn = open_session_db()?;
    let tx = conn.transaction()?;
    let inserted = tx.execute(
        "INSERT OR IGNORE INTO telegram_inbox(update_id,payload_json,status,attempts,received_at)
         VALUES(?1,?2,'pending',0,?3)",
        params![update_id, payload_json, Local::now().to_rfc3339()],
    )? == 1;
    tx.execute(
        "INSERT INTO telegram_state(key,value) VALUES('offset',?1)
         ON CONFLICT(key) DO UPDATE SET value=
           CASE
             WHEN CAST(excluded.value AS INTEGER) > CAST(value AS INTEGER)
             THEN excluded.value ELSE value
           END",
        params![update_id.saturating_add(1).to_string()],
    )?;
    tx.commit()?;
    Ok(inserted)
}

fn pending_telegram_updates_db(limit: usize) -> rusqlite::Result<Vec<TelegramInboxRecord>> {
    let conn = open_session_db()?;
    let mut stmt = conn.prepare(
        "SELECT update_id,payload_json
         FROM telegram_inbox
         WHERE status='pending'
         ORDER BY update_id
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit as i64], |row| {
            Ok(TelegramInboxRecord {
                update_id: row.get(0)?,
                payload_json: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn quarantine_telegram_processing_db() -> rusqlite::Result<usize> {
    let conn = open_session_db()?;
    conn.execute(
        "UPDATE telegram_inbox
         SET status='interrupted',last_error='daemon stopped while update was processing'
         WHERE status='processing'",
        [],
    )
}

fn mark_telegram_processing_db(update_id: i64) -> rusqlite::Result<bool> {
    let conn = open_session_db()?;
    // Once an update is claimed, XiaoAI never automatically replays it after
    // a crash because the handler may already have committed side effects.
    // Scrub the raw payload at this boundary so API keys or other sensitive
    // command text do not remain in the durable inbox longer than necessary.
    let scrubbed = serde_json::json!({
        "update_id": update_id,
        "payload": "redacted_after_claim"
    })
    .to_string();
    Ok(conn.execute(
        "UPDATE telegram_inbox
         SET status='processing',attempts=attempts+1,payload_json=?2,last_error=NULL
         WHERE update_id=?1 AND status='pending'",
        params![update_id, scrubbed],
    )? == 1)
}

fn mark_telegram_processed_db(update_id: i64) -> rusqlite::Result<()> {
    let conn = open_session_db()?;
    conn.execute(
        "DELETE FROM telegram_inbox WHERE update_id=?1 AND status='processing'",
        params![update_id],
    )?;
    Ok(())
}

pub(crate) async fn load_telegram_offset_async() -> Option<i64> {
    run_db("load_telegram_offset", load_telegram_offset_db)
        .await
        .flatten()
}

pub(crate) async fn enqueue_telegram_update_async(
    update_id: i64,
    payload_json: String,
) -> Option<bool> {
    run_db("enqueue_telegram_update", move || {
        enqueue_telegram_update_db(update_id, &payload_json)
    })
    .await
}

pub(crate) async fn pending_telegram_updates_async(
    limit: usize,
) -> Vec<TelegramInboxRecord> {
    run_db("pending_telegram_updates", move || {
        pending_telegram_updates_db(limit)
    })
    .await
    .unwrap_or_default()
}

pub(crate) async fn quarantine_telegram_processing_async() -> usize {
    run_db("quarantine_telegram_processing", quarantine_telegram_processing_db)
        .await
        .unwrap_or_default()
}

pub(crate) async fn mark_telegram_processing_async(update_id: i64) -> bool {
    run_db("mark_telegram_processing", move || {
        mark_telegram_processing_db(update_id)
    })
    .await
    .unwrap_or(false)
}

pub(crate) async fn mark_telegram_processed_async(update_id: i64) -> bool {
    run_db("mark_telegram_processed", move || {
        mark_telegram_processed_db(update_id)
    })
    .await
    .is_some()
}

pub(super) async fn load_sessions_db_async(user_id: i64) -> Vec<ChatSession> {
    run_db("load_sessions", move || load_sessions_db(user_id))
        .await
        .unwrap_or_default()
}

pub(super) async fn allocate_session_id_db_async(user_id: i64) -> Option<usize> {
    run_db("allocate_session_id", move || {
        allocate_session_id_db(user_id)
    })
    .await
}

pub(super) async fn save_session_metadata_db_async(user_id: i64, session: ChatSession) -> bool {
    run_db("save_session_metadata", move || {
        save_session_metadata_db(user_id, &session)
    })
    .await
    .is_some()
}

pub(super) async fn replace_session_messages_db_async(user_id: i64, session: ChatSession) -> bool {
    run_db("replace_session_messages", move || {
        replace_session_messages_db(user_id, &session)
    })
    .await
    .is_some()
}

pub(super) async fn append_session_messages_db_async(
    user_id: i64,
    session: ChatSession,
    messages: Vec<ChatMessage>,
) -> bool {
    run_db("append_session_messages", move || {
        append_session_messages_db(user_id, &session, &messages)
    })
    .await
    .is_some()
}

pub(super) async fn delete_session_db_async(user_id: i64, session_id: usize) -> bool {
    run_db("delete_session", move || {
        delete_session_db(user_id, session_id)
    })
    .await
    .is_some()
}

pub(super) async fn save_active_session_db_async(user_id: i64, session_id: usize) -> bool {
    run_db("save_active_session", move || {
        save_active_session_db(user_id, session_id)
    })
    .await
    .is_some()
}

pub(super) async fn ensure_session_identity_v2_db_async(
    user_id: i64,
    sessions: Vec<ChatSession>,
) -> bool {
    run_db("ensure_session_identity_v2", move || {
        ensure_session_identity_v2_db(user_id, &sessions)
    })
    .await
    .is_some()
}

pub(super) async fn load_active_session_id_db_async(user_id: i64) -> Option<usize> {
    run_db("load_active_session", move || {
        let conn = open_session_db()?;
        conn.query_row(
            "SELECT session_id FROM active_sessions WHERE user_id=?1",
            params![user_id],
            |row| row.get::<_, usize>(0),
        )
    })
    .await
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapabilityRecord {
    pub provider_id: String,
    pub provider_name: String,
    pub model: String,
    pub context_window: Option<usize>,
    #[serde(default)]
    pub supports_text: Option<bool>,
    pub supports_image: Option<bool>,
    pub supports_audio: Option<bool>,
    pub supports_video: Option<bool>,
    pub supports_reasoning: Option<bool>,
    #[serde(default)]
    pub supports_tools: Option<bool>,
    #[serde(default)]
    pub supports_structured_output: Option<bool>,
    #[serde(default)]
    pub supports_file_input: Option<bool>,
    pub source: String,
    pub details: Vec<String>,
    pub checked_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CapabilityRegistry {
    pub models: Vec<CapabilityRecord>,
}

pub fn get_capability_registry_path() -> std::path::PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        return std::path::Path::new(&home).join(".xiao_model_capabilities.json");
    }
    std::path::Path::new(".xiao_model_capabilities.json").to_path_buf()
}

pub fn load_capability_registry() -> CapabilityRegistry {
    if let Ok(conn) = open_session_db() {
        if let Ok(value) = conn.query_row(
            "SELECT value FROM settings WHERE key='capability_registry'",
            [],
            |row| row.get::<_, String>(0),
        ) {
            if let Ok(registry) = serde_json::from_str(&value) {
                return registry;
            }
        }
    }
    let path = get_capability_registry_path();
    let registry = std::fs::read_to_string(path)
        .ok()
        .and_then(|value| serde_json::from_str(&value).ok())
        .unwrap_or_default();
    let _ = save_capability_registry(&registry);
    registry
}

pub fn save_capability_registry(registry: &CapabilityRegistry) -> std::io::Result<()> {
    let value =
        serde_json::to_string_pretty(registry).map_err(|e| std::io::Error::other(e.to_string()))?;
    let conn = open_session_db().map_err(|e| std::io::Error::other(e.to_string()))?;
    conn.execute("INSERT INTO settings(key,value) VALUES('capability_registry',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![value])
        .map(|_| ())
        .map_err(|e| std::io::Error::other(e.to_string()))
}

pub(super) async fn persist_capability_registry(registry: CapabilityRegistry) -> bool {
    match tokio::task::spawn_blocking(move || save_capability_registry(&registry)).await {
        Ok(Ok(())) => true,
        Ok(Err(err)) => {
            warn!("Failed to persist capability registry: {err}");
            false
        }
        Err(err) => {
            warn!("Capability persistence task failed: {err}");
            false
        }
    }
}

pub(super) async fn load_app_setting_async(key: &'static str) -> Option<String> {
    tokio::task::spawn_blocking(move || load_app_setting(key))
        .await
        .ok()
        .flatten()
}

pub fn load_app_setting(key: &str) -> Option<String> {
    open_session_db()
        .ok()?
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            params![format!("app:{key}")],
            |row| row.get(0),
        )
        .ok()
}

pub fn save_app_setting(key: &str, value: &str) -> std::io::Result<()> {
    let conn = open_session_db().map_err(|e| std::io::Error::other(e.to_string()))?;
    conn.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![format!("app:{key}"), value])
        .map(|_| ())
        .map_err(|e| std::io::Error::other(e.to_string()))
}
