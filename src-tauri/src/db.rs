use crate::error::Res;
use rusqlite::Connection;

const MIGRATIONS: &[&str] = &[
    // 1 — first schema
    r#"
    CREATE TABLE projects(id TEXT PRIMARY KEY, name TEXT NOT NULL, instructions TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
    CREATE TABLE threads(id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        kind TEXT NOT NULL DEFAULT 'chat', title TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
    CREATE TABLE messages(id TEXT PRIMARY KEY, thread_id TEXT NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
        role TEXT NOT NULL, content TEXT NOT NULL DEFAULT '', status TEXT NOT NULL DEFAULT 'complete',
        model_id TEXT, provider TEXT, pinned INTEGER NOT NULL DEFAULT 0, meta_json TEXT NOT NULL DEFAULT '{}',
        created_at INTEGER NOT NULL);
    CREATE INDEX messages_thread ON messages(thread_id, created_at);
    CREATE TABLE canvases(project_id TEXT PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
        revision INTEGER NOT NULL, schema_version INTEGER NOT NULL, graph_json TEXT NOT NULL,
        viewport_json TEXT NOT NULL, updated_at INTEGER NOT NULL);
    CREATE TABLE canvas_history(id INTEGER PRIMARY KEY AUTOINCREMENT, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        stack TEXT NOT NULL, graph_json TEXT NOT NULL, label TEXT NOT NULL, actor TEXT NOT NULL, group_id TEXT);
    CREATE TABLE assets(id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        kind TEXT NOT NULL, rel_path TEXT NOT NULL, mime TEXT NOT NULL, width INTEGER NOT NULL DEFAULT 0,
        height INTEGER NOT NULL DEFAULT 0, sha256 TEXT NOT NULL, title TEXT NOT NULL DEFAULT '',
        provider TEXT, region TEXT, model_id TEXT, prompt TEXT, settings_json TEXT, node_id TEXT, node_run_id TEXT,
        created_at INTEGER NOT NULL);
    CREATE TABLE asset_inputs(asset_id TEXT NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
        input_asset_id TEXT NOT NULL, ord INTEGER NOT NULL, PRIMARY KEY(asset_id, ord));
    CREATE TABLE runs(id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        status TEXT NOT NULL, plan_json TEXT NOT NULL, created_at INTEGER NOT NULL);
    CREATE TABLE node_runs(id TEXT PRIMARY KEY, run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
        project_id TEXT NOT NULL, node_id TEXT NOT NULL, kind TEXT NOT NULL, status TEXT NOT NULL,
        fingerprint TEXT NOT NULL, provider TEXT NOT NULL, region TEXT NOT NULL, model_id TEXT NOT NULL,
        final_prompt TEXT NOT NULL, settings_json TEXT NOT NULL, asset_id TEXT, text_output TEXT, error TEXT,
        usage_json TEXT, started_at INTEGER NOT NULL, finished_at INTEGER);
    CREATE INDEX node_runs_lookup ON node_runs(project_id, node_id, fingerprint);
    CREATE TABLE agent_actions(id TEXT PRIMARY KEY, project_id TEXT NOT NULL, thread_id TEXT, turn_id TEXT NOT NULL,
        tool_call_id TEXT NOT NULL, tool TEXT NOT NULL, rev_before INTEGER NOT NULL, rev_after INTEGER NOT NULL,
        summary TEXT NOT NULL, created_at INTEGER NOT NULL, UNIQUE(turn_id, tool_call_id));
    CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE model_status(region TEXT NOT NULL, model_id TEXT NOT NULL, status TEXT NOT NULL,
        checked_at INTEGER NOT NULL, PRIMARY KEY(region, model_id));
    "#,
    // 2 — dropped documents (text extracted locally) and English default titles
    r#"
    CREATE TABLE documents(id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        name TEXT NOT NULL, kind TEXT NOT NULL, text TEXT NOT NULL, chars INTEGER NOT NULL, truncated INTEGER NOT NULL DEFAULT 0,
        created_at INTEGER NOT NULL);
    UPDATE threads SET title='New chat' WHERE title='محادثة جديدة';
    UPDATE threads SET title='Canvas assistant' WHERE kind='agent';
    "#,
    // 3 — video generation tasks (Seedance): the task id is stored the moment it is known
    r#"
    CREATE TABLE videos(id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        thread_id TEXT, kind TEXT NOT NULL, draft_of TEXT, status TEXT NOT NULL, task_id TEXT,
        provider TEXT NOT NULL, region TEXT NOT NULL, model_key TEXT NOT NULL, model_id TEXT NOT NULL,
        prompt TEXT NOT NULL, params_json TEXT NOT NULL, inputs_json TEXT NOT NULL,
        rel_path TEXT, error TEXT, usage_json TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
    "#,
    // 4 — a video can belong to a canvas node
    r#"
    ALTER TABLE videos ADD COLUMN node_id TEXT;
    "#,
    // 5 — a project voice track; a video can use a clip of it (shot_id is reserved)
    r#"
    CREATE TABLE audio(
        id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        name TEXT NOT NULL, rel_path TEXT NOT NULL, sample_rate INTEGER NOT NULL, duration_ms INTEGER NOT NULL,
        peaks_json TEXT NOT NULL, created_at INTEGER NOT NULL);
    ALTER TABLE videos ADD COLUMN shot_id TEXT;
    "#,
    // 6 — project elements: characters, environments and props called into prompts as @Name
    r#"
    CREATE TABLE elements(
        id TEXT PRIMARY KEY, project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
        kind TEXT NOT NULL, name TEXT NOT NULL, asset_id TEXT, description TEXT NOT NULL DEFAULT '',
        created_at INTEGER NOT NULL);
    "#,
    // 7 — every HTTP request sent to the provider or its gateway (no key, no content)
    r#"
    CREATE TABLE api_log(id INTEGER PRIMARY KEY AUTOINCREMENT, started_at INTEGER NOT NULL, finished_at INTEGER,
        purpose TEXT NOT NULL, method TEXT NOT NULL, host TEXT NOT NULL, path TEXT NOT NULL, connection TEXT NOT NULL,
        model TEXT, params_json TEXT NOT NULL DEFAULT '{}', bytes_out INTEGER NOT NULL DEFAULT 0, outcome TEXT NOT NULL,
        http_status INTEGER, error_code TEXT, error_message TEXT, request_id TEXT, task_id TEXT, usage_json TEXT, info TEXT);
    CREATE INDEX api_log_time ON api_log(started_at);
    "#,
];

pub fn migrate(conn: &Connection) -> Res<()> {
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let v = i as i64 + 1;
        if v > current {
            conn.execute_batch(&format!("BEGIN; {sql} PRAGMA user_version={v}; COMMIT;"))?;
        }
    }
    Ok(())
}
