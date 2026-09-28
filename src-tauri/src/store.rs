//! 任务持久化（SQLite）。
//!
//! ## 为什么用「文档表」而不是把字段摊平
//!
//! `tasks` 表的列只有 `id / state / updated_at / data`，任务本体整段存 JSON。
//! 理由：`Task` 的字段还在演进（探测结果、播放列表、aria2c 标记都是后加的），
//! 摊平的话每加一个字段就要写一次 `ALTER TABLE` 迁移；存 JSON 则天然向后兼容
//! （新字段用 `#[serde(default)]` 读旧行即可）。
//!
//! 需要按列查询的只有 `state` 和 `updated_at`，所以把它们冗余出来建索引，
//! 既拿到查询能力又不牺牲演进速度。
//!
//! ## 写入纪律（DESIGN §5.2）
//!
//! **进度绝不落库**。`AppState` 只记录「哪些任务变过」，后台每 3 秒
//! 把变过的那几条在**一个事务里**写下去。所以这里的接口是
//! `upsert_many` 而不是「每次变更写一次」。

use crate::state::Task;
use rusqlite::{params, Connection};
use std::path::Path;

/// 打开（必要时创建）数据库并建表。
pub fn open(path: &Path) -> Result<Connection, String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("无法创建数据目录：{e}"))?;
    }
    let conn = Connection::open(path).map_err(|e| format!("打开数据库失败：{e}"))?;

    // WAL：读写不互相阻塞；NORMAL：桌面应用里吞吐与安全的合理折中。
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| format!("启用 WAL 失败：{e}"))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| format!("设置 synchronous 失败：{e}"))?;

    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tasks (
             id         TEXT PRIMARY KEY,
             state      TEXT NOT NULL,
             updated_at INTEGER NOT NULL,
             data       TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_tasks_state   ON tasks(state);
         CREATE INDEX IF NOT EXISTS idx_tasks_updated ON tasks(updated_at DESC);",
    )
    .map_err(|e| format!("建表失败：{e}"))?;

    Ok(conn)
}

/// 读出全部任务，最近添加的在前。
///
/// 单行 JSON 解析失败时**跳过该行而不是整体失败**——一条坏数据
/// 不该让用户丢掉整个下载历史。
pub fn load_all(conn: &Connection) -> Result<Vec<Task>, String> {
    let mut stmt = conn
        .prepare("SELECT data FROM tasks ORDER BY updated_at DESC")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    for row in rows {
        let json = match row {
            Ok(j) => j,
            Err(_) => continue,
        };
        match serde_json::from_str::<Task>(&json) {
            Ok(t) => out.push(t),
            Err(e) => eprintln!("跳过一条无法解析的任务记录：{e}"),
        }
    }
    Ok(out)
}

/// 在**一个事务里**写入若干任务。任一条失败则整体回滚。
pub fn upsert_many(conn: &mut Connection, tasks: &[Task]) -> Result<(), String> {
    if tasks.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut stmt = tx
            .prepare_cached(
                "INSERT INTO tasks (id, state, updated_at, data) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(id) DO UPDATE SET
                     state = excluded.state,
                     updated_at = excluded.updated_at,
                     data = excluded.data",
            )
            .map_err(|e| e.to_string())?;

        for t in tasks {
            let data = serde_json::to_string(t).map_err(|e| e.to_string())?;
            let updated = t.finished_at.unwrap_or(t.added_at);
            stmt.execute(params![t.id, t.state, updated, data])
                .map_err(|e| format!("写入任务 {} 失败：{e}", t.id))?;
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

/// 删除若干任务。
pub fn delete_many(conn: &mut Connection, ids: &[String]) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let tx = conn.transaction().map_err(|e| e.to_string())?;
    {
        let mut stmt = tx
            .prepare_cached("DELETE FROM tasks WHERE id = ?1")
            .map_err(|e| e.to_string())?;
        for id in ids {
            stmt.execute(params![id]).map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())
}

/// 任务总数，供诊断与「清理历史」使用。
pub fn count(conn: &Connection) -> Result<u64, String> {
    conn.query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Progress;

    fn tmp_db(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("ytdlp-store-test-{tag}-{}.db", std::process::id()))
    }

    fn task(id: &str, state: &str) -> Task {
        let mut t = Task::new(
            id.to_string(),
            format!("https://example.com/{id}"),
            "C:/out".into(),
            "bv*+ba/b".into(),
        );
        t.state = state.to_string();
        t.title = format!("标题 {id}");
        t.progress = Progress {
            downloaded: Some(123),
            total: Some(456),
            speed: None,
            eta: None,
        };
        t
    }

    #[test]
    fn roundtrip_preserves_all_fields() {
        let path = tmp_db("roundtrip");
        let _ = std::fs::remove_file(&path);

        let mut conn = open(&path).unwrap();
        let t = task("a", "downloading");
        upsert_many(&mut conn, std::slice::from_ref(&t)).unwrap();

        let back = load_all(&conn).unwrap();
        assert_eq!(back.len(), 1);
        // 全字段往返：这是「存 JSON」方案的核心保证
        assert_eq!(back[0], t);
        assert_eq!(back[0].progress.downloaded, Some(123));
        assert_eq!(back[0].title, "标题 a");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn upsert_updates_in_place() {
        let path = tmp_db("upsert");
        let _ = std::fs::remove_file(&path);
        let mut conn = open(&path).unwrap();

        upsert_many(&mut conn, &[task("a", "downloading")]).unwrap();
        upsert_many(&mut conn, &[task("a", "completed")]).unwrap();

        let back = load_all(&conn).unwrap();
        assert_eq!(back.len(), 1, "同 id 应是更新而不是新增");
        assert_eq!(back[0].state, "completed");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn delete_and_count() {
        let path = tmp_db("delete");
        let _ = std::fs::remove_file(&path);
        let mut conn = open(&path).unwrap();

        upsert_many(&mut conn, &[task("a", "pending"), task("b", "pending")]).unwrap();
        assert_eq!(count(&conn).unwrap(), 2);

        delete_many(&mut conn, &["a".to_string()]).unwrap();
        assert_eq!(count(&conn).unwrap(), 1);
        assert_eq!(load_all(&conn).unwrap()[0].id, "b");

        std::fs::remove_file(&path).ok();
    }

    /// 空输入不该开事务、也不该报错。
    #[test]
    fn empty_inputs_are_noops() {
        let path = tmp_db("empty");
        let _ = std::fs::remove_file(&path);
        let mut conn = open(&path).unwrap();
        assert!(upsert_many(&mut conn, &[]).is_ok());
        assert!(delete_many(&mut conn, &[]).is_ok());
        assert_eq!(count(&conn).unwrap(), 0);
        std::fs::remove_file(&path).ok();
    }

    /// 损坏的行应当被跳过，而不是让整个历史读不出来。
    #[test]
    fn corrupt_row_is_skipped_not_fatal() {
        let path = tmp_db("corrupt");
        let _ = std::fs::remove_file(&path);
        let mut conn = open(&path).unwrap();

        upsert_many(&mut conn, &[task("good", "pending")]).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, state, updated_at, data) VALUES ('bad','pending',1,'{不是合法JSON')",
            [],
        )
        .unwrap();

        let back = load_all(&conn).unwrap();
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, "good");

        std::fs::remove_file(&path).ok();
    }

    /// 重复打开同一个库应幂等（建表用 IF NOT EXISTS）。
    #[test]
    fn open_is_idempotent() {
        let path = tmp_db("reopen");
        let _ = std::fs::remove_file(&path);
        {
            let mut conn = open(&path).unwrap();
            upsert_many(&mut conn, &[task("a", "pending")]).unwrap();
        }
        let conn2 = open(&path).unwrap();
        assert_eq!(count(&conn2).unwrap(), 1, "重开后数据应还在");
        std::fs::remove_file(&path).ok();
    }
}
