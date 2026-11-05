//! 存储层。持久化与并发原语；快照导出/恢复随表清单一起留在这里。
//!
//! 提供者：SQLite（WAL）。所有写入走单事务，CAS 用行版本。

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::json;

use std::collections::HashSet;

use crate::domain::{self, Issue, LinkKind, Predicate, Status};
use crate::error::{Result, ZenError};

pub struct Store {
    conn: Connection,
}

impl Store {
    /// 打开数据库并设定并发参数。库不存在则创建。
    pub fn open(db_path: &Path) -> Result<Self> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(db_path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Store { conn })
    }

    /// 单一时间源：所有 `now` 都从这里取，不信任调用方传入。
    pub fn now(&self) -> String {
        now_iso8601()
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

        /// 单事务。闭包正常返回即提交，返回错误即回滚——半写不会落库。
    ///
    /// 用 IMMEDIATE 而不是默认的 DEFERRED：延迟事务先读后写时，若期间别人提交了，
    /// SQLite 立刻返回 database is locked，busy_timeout 管不到这种快照冲突。
    /// 开事务就抢写锁，五个进程并发建任务才会排队而不是有人失败（R21）。
    pub fn tx<T>(&mut self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }
}

/// UTC ISO8601，毫秒精度，固定宽度，可直接做字符串比较。
fn now_iso8601() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs() as i64;
    let millis = d.subsec_millis();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, day) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// 纪元日 → 公历年月日（Howard Hinnant 的 civil_from_days）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 建库。当前只建到能支撑首个纵切的最小集合。
pub fn init_schema(store: &Store) -> Result<()> {
    store.conn().execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS plan (
            id          TEXT PRIMARY KEY,
            title       TEXT NOT NULL UNIQUE,
            background  TEXT NOT NULL DEFAULT '',
            context     TEXT NOT NULL DEFAULT '',
            created_at  TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS issue (
            id            TEXT PRIMARY KEY,
            plan_id       TEXT REFERENCES plan(id),
            type          TEXT NOT NULL,
            title         TEXT NOT NULL,
            description   TEXT NOT NULL DEFAULT '',
            status        TEXT NOT NULL,
            priority      TEXT NOT NULL,
            parent_id     TEXT REFERENCES issue(id),
            block_reason  TEXT,
            fields        TEXT NOT NULL DEFAULT '{}',
            external_key  TEXT,
            claim_actor   TEXT,
            claim_until   TEXT,
            reviewed_at   TEXT,
            rev           INTEGER NOT NULL DEFAULT 0,
            created_at    TEXT NOT NULL,
            updated_at    TEXT NOT NULL
        );

        CREATE UNIQUE INDEX IF NOT EXISTS issue_external_key ON issue(external_key)
            WHERE external_key IS NOT NULL;
        CREATE INDEX IF NOT EXISTS issue_status ON issue(status);
        CREATE INDEX IF NOT EXISTS issue_plan ON issue(plan_id);

        CREATE TABLE IF NOT EXISTS link (
            from_id  TEXT NOT NULL REFERENCES issue(id),
            to_id    TEXT NOT NULL REFERENCES issue(id),
            kind     TEXT NOT NULL,
            PRIMARY KEY (from_id, to_id, kind)
        );

        CREATE TABLE IF NOT EXISTS acceptance (
            issue_id   TEXT NOT NULL REFERENCES issue(id),
            ordinal    INTEGER NOT NULL,
            text       TEXT NOT NULL,
            checked_at TEXT,
            checked_by TEXT,
            PRIMARY KEY (issue_id, ordinal)
        );

        CREATE TABLE IF NOT EXISTS comment (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            issue_id TEXT NOT NULL REFERENCES issue(id),
            actor    TEXT NOT NULL,
            body     TEXT NOT NULL,
            at       TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS artifact (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            issue_id TEXT NOT NULL REFERENCES issue(id),
            kind     TEXT NOT NULL,
            "ref"    TEXT NOT NULL,
            sha256   TEXT,
            at       TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS event (
            seq           INTEGER PRIMARY KEY AUTOINCREMENT,
            subject_type  TEXT NOT NULL,
            subject_id    TEXT NOT NULL,
            kind          TEXT NOT NULL,
            from_state    TEXT,
            to_state      TEXT,
            actor         TEXT NOT NULL,
            role          TEXT NOT NULL,
            payload       TEXT NOT NULL DEFAULT '{}',
            at            TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS event_subject ON event(subject_type, subject_id, seq);
        "#,
    )?;
    Ok(())
}

/// issue 表的列顺序。读回与写入共用同一份，避免两处漂移。
const ISSUE_COLUMNS: &str = "id, plan_id, type, title, description, status, priority, parent_id, \
     block_reason, fields, external_key, claim_actor, claim_until, reviewed_at, rev, created_at, updated_at";

/// 事件行的写入形态。subject_type 取 issue / plan / config。
#[derive(Debug, Clone)]
pub struct Event {
    pub subject_type: &'static str,
    pub subject_id: String,
    pub kind: String,
    pub from_state: Option<String>,
    pub to_state: Option<String>,
    pub actor: String,
    pub role: String,
    pub payload: serde_json::Value,
}

/// 插入一条任务。事务与 ID 分配由调用方负责。
pub fn insert_issue(conn: &Connection, issue: &Issue) -> Result<()> {
    let sql = format!("INSERT INTO issue ({ISSUE_COLUMNS}) VALUES ({})", issue_placeholders());
    conn.execute(
        &sql,
        params![
            &issue.id,
            issue.plan.as_deref(),
            &issue.kind,
            &issue.title,
            &issue.description,
            issue.status.as_str(),
            &issue.priority,
            issue.parent.as_deref(),
            issue.block_reason.as_deref(),
            serde_json::Value::Object(issue.fields.clone()).to_string(),
            issue.external_key.as_deref(),
            issue.claim_actor.as_deref(),
            issue.claim_until.as_deref(),
            issue.reviewed_at.as_deref(),
            issue.rev,
            &issue.created_at,
            &issue.updated_at,
        ],
    )
    .map_err(|e| write_err(conn, e, issue.external_key.as_deref()))?;
    Ok(())
}

/// 读一条任务。不存在报 `NotFound`。
pub fn load_issue(conn: &Connection, id: &str) -> Result<Issue> {
    let sql = format!("SELECT {ISSUE_COLUMNS} FROM issue WHERE id = ?1");
    conn.query_row(&sql, [id], row_to_issue)
        .optional()?
        .ok_or_else(|| ZenError::NotFound { what: format!("任务 {id}") })
}

/// 带版本检查的写回：`rev` 对不上就报 `Conflict` 并给出当前版本，调用方重读后重试。
/// 成功返回新版本号。
pub fn update_issue_cas(conn: &Connection, issue: &Issue) -> Result<i64> {
    let n = conn.execute(
        "UPDATE issue SET plan_id=?1, type=?2, title=?3, description=?4, status=?5, priority=?6,
             parent_id=?7, block_reason=?8, fields=?9, external_key=?10, claim_actor=?11,
             claim_until=?12, reviewed_at=?13, updated_at=?14, rev=rev+1
         WHERE id=?15 AND rev=?16",
        params![
            issue.plan.as_deref(),
            &issue.kind,
            &issue.title,
            &issue.description,
            issue.status.as_str(),
            &issue.priority,
            issue.parent.as_deref(),
            issue.block_reason.as_deref(),
            serde_json::Value::Object(issue.fields.clone()).to_string(),
            issue.external_key.as_deref(),
            issue.claim_actor.as_deref(),
            issue.claim_until.as_deref(),
            issue.reviewed_at.as_deref(),
            &issue.updated_at,
            &issue.id,
            issue.rev,
        ],
    )?;
    if n == 0 {
        return Err(match current_rev(conn, &issue.id)? {
            None => ZenError::NotFound { what: format!("任务 {}", issue.id) },
            Some(actual) => ZenError::Conflict { expected: issue.rev.to_string(), actual: actual.to_string() },
        });
    }
    Ok(issue.rev + 1)
}

/// 追加一条事件，返回自增 `seq`。同一毫秒内的两次写入靠它定序。
pub fn append_event(conn: &Connection, event: &Event, at: &str) -> Result<i64> {
    conn.execute(
        "INSERT INTO event (subject_type, subject_id, kind, from_state, to_state, actor, role, payload, at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            event.subject_type,
            &event.subject_id,
            &event.kind,
            event.from_state.as_deref(),
            event.to_state.as_deref(),
            &event.actor,
            &event.role,
            event.payload.to_string(),
            at,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// 现有同前缀的任务 ID。ID 分配用。
pub fn issue_ids(conn: &Connection, prefix: &str) -> Result<Vec<String>> {
    ids_like(conn, "SELECT id FROM issue WHERE id LIKE ?1", prefix)
}

/// 现有同前缀的计划 ID。
pub fn plan_ids(conn: &Connection, prefix: &str) -> Result<Vec<String>> {
    ids_like(conn, "SELECT id FROM plan WHERE id LIKE ?1", prefix)
}

fn ids_like(conn: &Connection, sql: &str, prefix: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map([format!("{prefix}-%")], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// 计划按标题唯一：同名即同一个容器。也接受现成的计划 ID。
pub fn find_plan(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn
        .query_row("SELECT id FROM plan WHERE title = ?1 OR id = ?1", [key], |r| r.get(0))
        .optional()?)
}

/// 读一个计划，按 ID 或标题都可。
pub fn load_plan(conn: &Connection, key: &str) -> Result<domain::Plan> {
    conn.query_row(
        "SELECT id, title, background, context, created_at FROM plan WHERE id = ?1 OR title = ?1",
        [key],
        |r| {
            Ok(domain::Plan {
                id: r.get(0)?,
                title: r.get(1)?,
                background: r.get(2)?,
                context: r.get(3)?,
                created_at: r.get(4)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| ZenError::NotFound { what: format!("计划 {key}") })
}

pub fn insert_plan(conn: &Connection, id: &str, title: &str, at: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO plan (id, title, background, context, created_at) VALUES (?1, ?2, '', '', ?3)",
        params![id, title, at],
    )?;
    Ok(())
}

/// 写计划的正文两节；None 表示保留原值。
pub fn update_plan_sections(conn: &Connection, id: &str, title: Option<&str>, background: Option<&str>, context: Option<&str>) -> Result<()> {
    let plan = load_plan(conn, id)?;
    conn.execute(
        "UPDATE plan SET title = ?1, background = ?2, context = ?3 WHERE id = ?4",
        params![
            title.unwrap_or(&plan.title),
            background.unwrap_or(&plan.background),
            context.unwrap_or(&plan.context),
            plan.id
        ],
    )?;
    Ok(())
}

/// 视图求值要用的上下文：谁在看、现在几点、两个时间阈值。
pub struct ViewCtx<'a> {
    pub actor: &'a str,
    pub now: &'a str,
    pub stale_after_days: u32,
}

/// 一个视图的行集。ready 走领域谓词（含成环检测与父链传染），其余是列条件。
pub fn view(conn: &Connection, pred: Predicate, ctx: &ViewCtx<'_>) -> Result<Vec<Issue>> {
    if pred == Predicate::Ready {
        return ready_view(conn);
    }
    let sql = format!("SELECT {ISSUE_COLUMNS} FROM issue WHERE {} ORDER BY priority, id", view_clause(pred));
    let args: Vec<&dyn rusqlite::ToSql> = match pred {
        Predicate::Mine => vec![&ctx.actor, &ctx.now],
        Predicate::Stale => vec![&ctx.now, &ctx.stale_after_days],
        _ => vec![],
    };
    query_issues(conn, &sql, &args)
}

fn ready_view(conn: &Connection) -> Result<Vec<Issue>> {
    let issues = load_issues(conn)?;
    let keep: HashSet<String> = domain::ready(&issues, &load_links(conn)?)?.into_iter().collect();
    Ok(issues.into_iter().filter(|i| keep.contains(&i.id)).collect())
}

/// 每个视图的列条件。只有名字是配置的，谓词本身留在这一处。
fn view_clause(pred: Predicate) -> &'static str {
    match pred {
        Predicate::Ready => "0",
        Predicate::Mine => "status NOT IN ('done','dropped') AND claim_actor = ?1 AND (claim_until IS NULL OR claim_until >= ?2)",
        Predicate::Blocked => "status = 'blocked'",
        Predicate::Backlog => "plan_id IS NULL AND status NOT IN ('done','dropped')",
        Predicate::Stale => "status NOT IN ('done','dropped') AND julianday(?1) - julianday(updated_at) >= ?2",
        Predicate::Review => "status = 'done' AND reviewed_at IS NULL",
    }
}

/// 全部任务，按优先级与编号排序。
pub fn load_issues(conn: &Connection) -> Result<Vec<Issue>> {
    query_issues(conn, &format!("SELECT {ISSUE_COLUMNS} FROM issue ORDER BY priority, id"), &[])
}

/// 插入一条关联。已经存在时返回 false，不报错——建关联是幂等的。
pub fn insert_link(conn: &Connection, from: &str, to: &str, kind: LinkKind) -> Result<bool> {
    let n = conn.execute(
        "INSERT OR IGNORE INTO link (from_id, to_id, kind) VALUES (?1, ?2, ?3)",
        params![from, to, kind.as_str()],
    )?;
    Ok(n == 1)
}

/// 全部关联。
pub fn load_links(conn: &Connection) -> Result<Vec<domain::Link>> {
    let mut stmt = conn.prepare("SELECT from_id, to_id, kind FROM link")?;
    let rows = stmt.query_map([], |row| {
        let kind: String = row.get(2)?;
        let kind = LinkKind::parse(&kind).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, format!("未知关联种类 {kind}").into())
        })?;
        Ok(domain::Link { from: row.get(0)?, to: row.get(1)?, kind })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn query_issues(conn: &Connection, sql: &str, args: &[&dyn rusqlite::ToSql]) -> Result<Vec<Issue>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(args, row_to_issue)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// 批量写入验收标准，序号从 1 起接在现有条目之后。
pub fn insert_acceptance(conn: &Connection, issue_id: &str, texts: &[String]) -> Result<()> {
    let 已有 = load_acceptance(conn, issue_id)?.len() as i64;
    for (i, text) in texts.iter().enumerate() {
        conn.execute(
            "INSERT INTO acceptance (issue_id, ordinal, text, checked_at, checked_by) VALUES (?1, ?2, ?3, NULL, NULL)",
            params![issue_id, 已有 + i as i64 + 1, text],
        )?;
    }
    Ok(())
}

/// 勾一条验收标准。CAS 在 `checked_at IS NULL` 上：同一条被勾两次报冲突，不静默覆盖。
pub fn check_acceptance(conn: &Connection, issue_id: &str, ordinal: u32, actor: &str, at: &str) -> Result<()> {
    let n = conn.execute(
        "UPDATE acceptance SET checked_at = ?3, checked_by = ?4
         WHERE issue_id = ?1 AND ordinal = ?2 AND checked_at IS NULL",
        params![issue_id, ordinal, at, actor],
    )?;
    if n == 1 {
        return Ok(());
    }
    let 那一行: Option<Option<String>> = conn
        .query_row(
            "SELECT checked_at FROM acceptance WHERE issue_id = ?1 AND ordinal = ?2",
            params![issue_id, ordinal],
            |r| r.get(0),
        )
        .optional()?;
    Err(match 那一行 {
        None => ZenError::NotFound { what: format!("{issue_id} 的第 {ordinal} 条验收标准") },
        Some(checked) => ZenError::Conflict { expected: "未勾选".into(), actual: format!("已于 {} 勾选", checked.unwrap_or_default()) },
    })
}

pub fn insert_comment(conn: &Connection, issue_id: &str, actor: &str, body: &str, at: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO comment (issue_id, actor, body, at) VALUES (?1, ?2, ?3, ?4)",
        params![issue_id, actor, body, at],
    )?;
    Ok(())
}

pub fn load_comments(conn: &Connection, issue_id: &str) -> Result<Vec<domain::Comment>> {
    let mut stmt = conn.prepare("SELECT actor, body, at FROM comment WHERE issue_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([issue_id], |r| {
        Ok(domain::Comment { issue: issue_id.to_string(), actor: r.get(0)?, body: r.get(1)?, at: r.get(2)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn insert_artifact(
    conn: &Connection,
    issue_id: &str,
    kind: domain::ArtifactKind,
    reference: &str,
    sha256: Option<&str>,
    at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO artifact (issue_id, kind, \"ref\", sha256, at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![issue_id, kind.as_str(), reference, sha256, at],
    )?;
    Ok(())
}

/// 事件回放：按 seq 升序返回最后 `limit` 条；`subject_id` 为空则跨主体。
pub fn load_events(conn: &Connection, subject_id: Option<&str>, limit: usize) -> Result<Vec<serde_json::Value>> {
    let mut stmt = conn.prepare(
        "SELECT seq, subject_type, subject_id, kind, from_state, to_state, actor, role, payload, at
         FROM event WHERE (?1 IS NULL OR subject_id = ?1) ORDER BY seq DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![subject_id, limit as i64], |r| {
        Ok(json!({
            "seq": r.get::<_, i64>(0)?,
            "subject_type": r.get::<_, String>(1)?,
            "subject_id": r.get::<_, String>(2)?,
            "kind": r.get::<_, String>(3)?,
            "from": r.get::<_, Option<String>>(4)?,
            "to": r.get::<_, Option<String>>(5)?,
            "actor": r.get::<_, String>(6)?,
            "role": r.get::<_, String>(7)?,
            "payload": serde_json::from_str::<serde_json::Value>(&r.get::<_, String>(8)?).unwrap_or(serde_json::Value::Null),
            "at": r.get::<_, String>(9)?,
        }))
    })?;
    let mut 倒序: Vec<serde_json::Value> = rows.collect::<rusqlite::Result<_>>()?;
    倒序.reverse();
    Ok(倒序)
}

/// 最近一条报告的类型。报告是事件，读时从 payload 里取。
pub fn last_report(conn: &Connection, issue_id: &str) -> Result<Option<domain::ReportKind>> {
    let 载荷: Option<String> = conn
        .query_row(
            "SELECT payload FROM event WHERE subject_type = 'issue' AND subject_id = ?1 AND kind = 'report'
             ORDER BY seq DESC LIMIT 1",
            [issue_id],
            |r| r.get(0),
        )
        .optional()?;
    let Some(文本) = 载荷 else { return Ok(None) };
    let 值: serde_json::Value = serde_json::from_str(&文本)
        .map_err(|e| ZenError::Io(format!("报告事件的 payload 不是 JSON：{e}")))?;
    let kind = 值
        .get("kind")
        .and_then(|v| v.as_str())
        .and_then(domain::ReportKind::parse)
        .ok_or_else(|| ZenError::Io(format!("报告事件缺类型：{文本}")))?;
    Ok(Some(kind))
}

/// 一条任务的验收项，按序号。
pub fn load_acceptance(conn: &Connection, issue_id: &str) -> Result<Vec<domain::Acceptance>> {
    let mut stmt = conn.prepare(
        "SELECT ordinal, text, checked_at, checked_by FROM acceptance WHERE issue_id = ?1 ORDER BY ordinal",
    )?;
    let rows = stmt.query_map([issue_id], |r| {
        Ok(domain::Acceptance {
            issue: issue_id.to_string(),
            ordinal: r.get::<_, i64>(0)? as u32,
            text: r.get(1)?,
            checked_at: r.get(2)?,
            checked_by: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// 一条任务的产物，按加入顺序。
pub fn load_artifacts(conn: &Connection, issue_id: &str) -> Result<Vec<domain::Artifact>> {
    let mut stmt = conn.prepare("SELECT kind, \"ref\", sha256, at FROM artifact WHERE issue_id = ?1 ORDER BY id")?;
    let rows = stmt.query_map([issue_id], |r| {
        let kind: String = r.get(0)?;
        let kind = domain::ArtifactKind::parse(&kind).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    format!("未知产物种类 {kind}").into(),
            )
        })?;
        Ok(domain::Artifact {
            issue: issue_id.to_string(),
            kind,
            reference: r.get(1)?,
            sha256: r.get(2)?,
            at: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// 当前租约：(持有者, 到期时刻)。没有租约返回 None。
pub fn lease_of(conn: &Connection, id: &str) -> Result<Option<(String, String)>> {
    let row = conn
        .query_row("SELECT claim_actor, claim_until FROM issue WHERE id = ?1", [id], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?))
        })
        .optional()?;
    match row {
        None => Ok(None),
        Some((None, _)) | Some((_, None)) => Ok(None),
        Some((Some(actor), Some(until))) => Ok(Some((actor, until))),
    }
}

/// 到期时刻 = 现在 + TTL 分钟。用 SQLite 做进位，固定宽度便于直接比字符串。
pub fn lease_until(conn: &Connection, now: &str, minutes: u32) -> Result<String> {
    let modifier = format!("+{minutes} minutes");
    Ok(conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', datetime(?1, ?2))",
        params![now, modifier],
        |r| r.get(0),
    )?)
}

/// 租约只动 claim_* 与 updated_at，不动 rev——状态与版本号由调用方的写回负责，
/// 两件事同处一个事务，原子性由事务保证。
///
/// 进入 wip 时抢占租约：只有空闲或已过期才拿得到。
/// 失败报 `Lease` 并带出当时的持有者与到期时间，不合并成通用失败。
pub fn claim_lease(conn: &Connection, id: &str, actor: &str, until: &str, now: &str) -> Result<()> {
    let n = conn.execute(
        "UPDATE issue SET claim_actor = ?2, claim_until = ?3, updated_at = ?4
         WHERE id = ?1 AND (claim_actor IS NULL OR claim_until IS NULL OR claim_until < ?5)",
        params![id, actor, until, now, now],
    )?;
    if n == 1 {
        return Ok(());
    }
    match lease_of(conn, id)? {
        Some((holder, until)) => Err(ZenError::Lease { holder, until }),
        None => Err(ZenError::NotFound { what: format!("任务 {id}") }),
    }
}

/// 续期只限持有者本人；租约已过期时原持有者也不再算持有者。
pub fn renew_lease(conn: &Connection, id: &str, actor: &str, until: &str, now: &str) -> Result<()> {
    let n = conn.execute(
        "UPDATE issue SET claim_until = ?3, updated_at = ?4
         WHERE id = ?1 AND claim_actor = ?2 AND claim_until >= ?5",
        params![id, actor, until, now, now],
    )?;
    if n == 1 {
        return Ok(());
    }
    match lease_of(conn, id)? {
        Some((holder, until)) => Err(ZenError::Lease { holder, until }),
        None if exists(conn, id)? => Err(ZenError::Lease { holder: "（空闲）".into(), until: "（无）".into() }),
        None => Err(ZenError::NotFound { what: format!("任务 {id}") }),
    }
}

/// 离开 wip 一律释放租约。已经空闲时什么也不做。
pub fn release_lease(conn: &Connection, id: &str, now: &str) -> Result<()> {
    conn.execute(
        "UPDATE issue SET claim_actor = NULL, claim_until = NULL, updated_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    Ok(())
}

fn exists(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn
        .query_row("SELECT 1 FROM issue WHERE id = ?1", [id], |_| Ok(()))
        .optional()?
        .is_some())
}

/// 快照里的一张表：JSON 键名与 SQL 列名分开，`ref` 这种要在 SQL 里加引号。
/// 快照的表清单。顺序即导出顺序，也是重灌时的插入顺序（父表在前）。

/// 占位符编号从列清单推导，增删列不必数数。
fn issue_placeholders() -> String {
    let n = ISSUE_COLUMNS.matches(',').count() + 1;
    (1..=n).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ")
}

fn current_rev(conn: &Connection, id: &str) -> Result<Option<i64>> {
    Ok(conn.query_row("SELECT rev FROM issue WHERE id = ?1", [id], |r| r.get(0)).optional()?)
}

fn row_to_issue(row: &Row<'_>) -> rusqlite::Result<Issue> {
    let status: String = row.get(5)?;
    let status = Status::parse(&status).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, format!("未知状态 {status}").into())
    })?;
    let raw_fields: String = row.get(9)?;
    let fields = serde_json::from_str(&raw_fields)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e)))?;
    Ok(Issue {
        id: row.get(0)?,
        plan: row.get(1)?,
        kind: row.get(2)?,
        title: row.get(3)?,
        description: row.get(4)?,
        status,
        priority: row.get(6)?,
        parent: row.get(7)?,
        block_reason: row.get(8)?,
        fields,
        external_key: row.get(10)?,
        claim_actor: row.get(11)?,
        claim_until: row.get(12)?,
        reviewed_at: row.get(13)?,
        rev: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
    })
}

/// 唯一键冲突翻译成 `Conflict` 并带上已占用的任务 ID——调用方据此幂等返回。
fn write_err(conn: &Connection, e: rusqlite::Error, external_key: Option<&str>) -> ZenError {
    if let (Some(key), Some(rusqlite::ErrorCode::ConstraintViolation)) = (external_key, e.sqlite_error_code()) {
        let holder = conn.query_row("SELECT id FROM issue WHERE external_key = ?1", [key], |r| r.get::<_, String>(0));
        if let Ok(id) = holder {
            return ZenError::Conflict { expected: format!("external_key {key} 未占用"), actual: id };
        }
    }
    e.into()
}

impl Store {
    /// 用法自检：库能开、能建表、能读时间。
    pub fn health(&self) -> Result<()> {
        self.conn
            .query_row("SELECT 1", [], |_| Ok(()))
            .map_err(|e| ZenError::Io(format!("sqlite 不可用：{e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ArtifactKind;

    #[test]
    fn 时间格式固定宽度且可比较() {
        let t = now_iso8601();
        assert_eq!(t.len(), 24, "长度应为 24：{t}");
        assert!(t.ends_with('Z'));
        assert!(t.as_str() > "2020-01-01T00:00:00.000Z");
    }

    #[test]
    fn 建库后可读写() {
        let dir = std::env::temp_dir().join(format!("dsh-zen-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir.join("zen.zendb")).unwrap();
        init_schema(&store).unwrap();
        store.health().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 独立临时库，tag 区分并行跑的测试。
    fn 临时库(tag: &str) -> (std::path::PathBuf, Store) {
        let dir = std::env::temp_dir().join(format!("dsh-zen-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::open(&dir.join("zen.zendb")).unwrap();
        init_schema(&store).unwrap();
        (dir, store)
    }

    fn 任务(id: &str) -> Issue {
        Issue {
            id: id.into(),
            plan: None,
            kind: "task".into(),
            title: "标题".into(),
            description: String::new(),
            status: Status::Todo,
            priority: "P2".into(),
            parent: None,
            block_reason: None,
            fields: serde_json::Map::new(),
            external_key: None,
            claim_actor: None,
            claim_until: None,
            reviewed_at: None,
            rev: 0,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    #[test]
    fn 插入后可原样读回() {
        let (dir, store) = 临时库("roundtrip");
        let mut a = 任务("ZEN-1");
        a.fields.insert("severity".into(), serde_json::Value::from("high"));
        insert_issue(store.conn(), &a).unwrap();

        let b = load_issue(store.conn(), "ZEN-1").unwrap();
        assert_eq!(b.id, a.id);
        assert_eq!(b.title, a.title);
        assert_eq!(b.status, Status::Todo);
        assert_eq!(b.rev, 0);
        assert_eq!(b.fields.get("severity").and_then(|v| v.as_str()), Some("high"));
        assert_eq!(load_issue(store.conn(), "ZEN-404").unwrap_err().code(), "not_found");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 版本不匹配时写回失败() {
        let (dir, store) = 临时库("cas");
        let a = 任务("ZEN-1");
        insert_issue(store.conn(), &a).unwrap();

        let mut b = load_issue(store.conn(), "ZEN-1").unwrap();
        b.title = "改过的标题".into();
        assert_eq!(update_issue_cas(store.conn(), &b).unwrap(), 1, "写成功一次 rev 加一");

        // 拿着旧版本再写一次：必须失败，不覆盖
        let mut stale = a.clone();
        stale.title = "另一个人".into();
        let e = update_issue_cas(store.conn(), &stale).unwrap_err();
        assert_eq!(e.code(), "conflict");
        assert!(e.to_string().contains("期望 0") && e.to_string().contains("实际 1"), "{e}");
        assert_eq!(load_issue(store.conn(), "ZEN-1").unwrap().title, "改过的标题");
        assert_eq!(update_issue_cas(store.conn(), &任务("ZEN-404")).unwrap_err().code(), "not_found");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 唯一键冲突报出已有任务() {
        let (dir, store) = 临时库("unique");
        let mut a = 任务("ZEN-1");
        a.external_key = Some("arch-dev:T1.1".into());
        insert_issue(store.conn(), &a).unwrap();

        let mut b = 任务("ZEN-2");
        b.external_key = Some("arch-dev:T1.1".into());
        let e = insert_issue(store.conn(), &b).unwrap_err();
        assert_eq!(e.code(), "conflict");
        assert!(e.to_string().contains("ZEN-1"), "{e}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 事务回滚不留半写() {
        let (dir, mut store) = 临时库("tx");
        let e = store
            .tx(|conn| {
                insert_issue(conn, &任务("ZEN-1"))?;
                Err::<(), _>(ZenError::Io("模拟中途失败".into()))
            })
            .unwrap_err();
        assert_eq!(e.code(), "io");
        assert_eq!(load_issue(store.conn(), "ZEN-1").unwrap_err().code(), "not_found");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 同毫秒事件按序号定序() {
        let (dir, store) = 临时库("event");
        let at = "2026-01-01T00:00:00.000Z";
        let 造事件 = |kind: &str| Event {
            subject_type: "issue",
            subject_id: "ZEN-1".into(),
            kind: kind.into(),
            from_state: None,
            to_state: None,
            actor: "a@b".into(),
            role: "implementer".into(),
            payload: serde_json::json!({}),
        };
        let first = append_event(store.conn(), &造事件("create"), at).unwrap();
        let second = append_event(store.conn(), &造事件("status_change"), at).unwrap();
        assert_eq!(second, first + 1, "同一时间戳靠 seq 定序");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 三视图各返回预期行数() {
        let (dir, store) = 临时库("views");
        insert_plan(store.conn(), "PLAN-1", "P1 数据通路", "2026-01-01T00:00:00.000Z").unwrap();
        // ZEN-1 计划内待办；ZEN-2 计划内待办但被 ZEN-1 阻塞；ZEN-3 积压；ZEN-4 由 a@b 持有；ZEN-5 主动受阻
        let mut 一号 = 任务("ZEN-1");
        一号.plan = Some("PLAN-1".into());
        let mut 二号 = 任务("ZEN-2");
        二号.plan = Some("PLAN-1".into());
        let 三号 = 任务("ZEN-3");
        let mut 四号 = 任务("ZEN-4");
        四号.plan = Some("PLAN-1".into());
        四号.status = Status::Wip;
        四号.claim_actor = Some("a@b".into());
        四号.claim_until = Some("2026-01-01T00:30:00.000Z".into());
        let mut 五号 = 任务("ZEN-5");
        五号.plan = Some("PLAN-1".into());
        五号.status = Status::Blocked;
        // ZEN-6 长期没动（陈旧）；ZEN-7 已完成待复盘
        let mut 六号 = 任务("ZEN-6");
        六号.plan = Some("PLAN-1".into());
        六号.status = Status::Wip;
        六号.updated_at = "2020-01-01T00:00:00.000Z".into();
        let mut 七号 = 任务("ZEN-7");
        七号.plan = Some("PLAN-1".into());
        七号.status = Status::Done;
        for issue in [&一号, &二号, &三号, &四号, &五号, &六号, &七号] {
            insert_issue(store.conn(), issue).unwrap();
        }
        store
            .conn()
            .execute("INSERT INTO link (from_id, to_id, kind) VALUES ('ZEN-1', 'ZEN-2', 'blocks')", [])
            .unwrap();

        let ctx = ViewCtx { actor: "a@b", now: "2026-01-01T00:00:00.000Z", stale_after_days: 14 };
        let ids = |pred| -> Vec<String> { view(store.conn(), pred, &ctx).unwrap().into_iter().map(|i| i.id).collect() };

        assert_eq!(ids(Predicate::Ready), vec!["ZEN-1".to_string()], "被阻塞者与积压都不进就绪");
        assert_eq!(ids(Predicate::Backlog), vec!["ZEN-3".to_string()]);
        assert_eq!(ids(Predicate::Mine), vec!["ZEN-4".to_string()], "mine 只看本人持有的租约");
        assert_eq!(ids(Predicate::Blocked), vec!["ZEN-5".to_string()], "只列主动受阻");
        assert_eq!(ids(Predicate::Stale), vec!["ZEN-6".to_string()], "超过阈值没动过");
        assert_eq!(ids(Predicate::Review), vec!["ZEN-7".to_string()], "已完成但没复盘");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 空闲可抢占且过期可接管() {
        let (dir, store) = 临时库("lease");
        insert_issue(store.conn(), &任务("ZEN-1")).unwrap();
        let 到期 = lease_until(store.conn(), "2026-01-01T00:00:00.000Z", 30).unwrap();

        claim_lease(store.conn(), "ZEN-1", "a@b", &到期, "2026-01-01T00:00:00.000Z").unwrap();
        assert_eq!(lease_of(store.conn(), "ZEN-1").unwrap(), Some(("a@b".into(), 到期.clone())));

        let e = claim_lease(store.conn(), "ZEN-1", "c@d", &到期, "2026-01-01T00:05:00.000Z").unwrap_err();
        assert_eq!(e.code(), "lease");
        assert!(e.to_string().contains("a@b"), "要报出持有者与到期时间：{e}");

        // 过了到期时刻就算空闲，可以被别人接管
        claim_lease(store.conn(), "ZEN-1", "c@d", &到期, "2026-01-01T00:31:00.000Z").unwrap();
        assert_eq!(lease_of(store.conn(), "ZEN-1").unwrap().unwrap().0, "c@d");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 非持有者不能续期() {
        let (dir, store) = 临时库("renew");
        insert_issue(store.conn(), &任务("ZEN-1")).unwrap();
        let 到期 = lease_until(store.conn(), "2026-01-01T00:00:00.000Z", 30).unwrap();
        claim_lease(store.conn(), "ZEN-1", "a@b", &到期, "2026-01-01T00:00:00.000Z").unwrap();

        let 新到期 = lease_until(store.conn(), "2026-01-01T00:10:00.000Z", 30).unwrap();
        renew_lease(store.conn(), "ZEN-1", "a@b", &新到期, "2026-01-01T00:10:00.000Z").unwrap();
        assert_eq!(lease_of(store.conn(), "ZEN-1").unwrap().unwrap().1, 新到期);

        let e = renew_lease(store.conn(), "ZEN-1", "c@d", &新到期, "2026-01-01T00:20:00.000Z").unwrap_err();
        assert_eq!(e.code(), "lease");

        // 过期之后连原持有者也不再算持有者
        let 过期后 = "2026-02-01T00:00:00.000Z";
        assert_eq!(renew_lease(store.conn(), "ZEN-1", "a@b", &新到期, 过期后).unwrap_err().code(), "lease");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 释放后谁都能拿() {
        let (dir, store) = 临时库("release");
        insert_issue(store.conn(), &任务("ZEN-1")).unwrap();
        let 到期 = lease_until(store.conn(), "2026-01-01T00:00:00.000Z", 30).unwrap();
        claim_lease(store.conn(), "ZEN-1", "a@b", &到期, "2026-01-01T00:00:00.000Z").unwrap();
        release_lease(store.conn(), "ZEN-1", "2026-01-01T00:00:10.000Z").unwrap();
        assert_eq!(lease_of(store.conn(), "ZEN-1").unwrap(), None);
        release_lease(store.conn(), "ZEN-1", "2026-01-01T00:00:20.000Z").unwrap();
        claim_lease(store.conn(), "ZEN-1", "c@d", &到期, "2026-01-01T00:00:30.000Z").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 租约操作不存在的任务报找不到() {
        let (dir, store) = 临时库("lease-missing");
        let 到期 = lease_until(store.conn(), "2026-01-01T00:00:00.000Z", 30).unwrap();
        let 现在 = "2026-01-01T00:00:00.000Z";
        assert_eq!(claim_lease(store.conn(), "ZEN-404", "a@b", &到期, 现在).unwrap_err().code(), "not_found");
        assert_eq!(renew_lease(store.conn(), "ZEN-404", "a@b", &到期, 现在).unwrap_err().code(), "not_found");
        assert_eq!(lease_of(store.conn(), "ZEN-404").unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 到期时刻按分钟推进且宽度不变() {
        let (dir, store) = 临时库("ttl");
        let 到期 = lease_until(store.conn(), "2026-01-01T23:45:00.000Z", 30).unwrap();
        assert_eq!(到期, "2026-01-02T00:15:00.000Z", "跨日要进位");
        assert_eq!(到期.len(), 24, "固定宽度才能直接比较字符串");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 验收逐条勾选不互相覆盖() {
        let (dir, store) = 临时库("accept");
        insert_issue(store.conn(), &任务("ZEN-1")).unwrap();
        let at = "2026-01-01T00:00:00.000Z";
        insert_acceptance(store.conn(), "ZEN-1", &["跑通甲".into(), "跑通乙".into()]).unwrap();

        let 初始 = load_acceptance(store.conn(), "ZEN-1").unwrap();
        assert_eq!(初始.len(), 2);
        assert_eq!(初始[0].ordinal, 1);
        assert!(初始.iter().all(|a| a.checked_at.is_none()));

        check_acceptance(store.conn(), "ZEN-1", 1, "a@b", at).unwrap();
        let 勾一条 = load_acceptance(store.conn(), "ZEN-1").unwrap();
        assert!(勾一条[0].checked_at.is_some(), "勾过的那条要留时间");
        assert_eq!(勾一条[0].checked_by.as_deref(), Some("a@b"));
        assert!(勾一条[1].checked_at.is_none(), "别的条目不受影响");

        assert_eq!(
            check_acceptance(store.conn(), "ZEN-1", 1, "c@d", at).unwrap_err().code(),
            "conflict",
            "同一条被勾两次是并发冲突，不静默覆盖"
        );
        assert_eq!(check_acceptance(store.conn(), "ZEN-1", 9, "a@b", at).unwrap_err().code(), "not_found");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 评论与产物可追加多次() {
        let (dir, store) = 临时库("topic");
        insert_issue(store.conn(), &任务("ZEN-1")).unwrap();
        let at = "2026-01-01T00:00:00.000Z";
        insert_comment(store.conn(), "ZEN-1", "a@b", "第一条评论", at).unwrap();
        insert_comment(store.conn(), "ZEN-1", "c@d", "第二条评论", at).unwrap();
        insert_artifact(store.conn(), "ZEN-1", ArtifactKind::Commit, "abc1234", None, at).unwrap();
        insert_artifact(store.conn(), "ZEN-1", ArtifactKind::Note, "把视图谓词挪回 domain，store 只留列条件", None, at).unwrap();

        let 评论 = load_comments(store.conn(), "ZEN-1").unwrap();
        assert_eq!(评论.len(), 2);
        assert_eq!(评论[0].body, "第一条评论");
        assert_eq!(评论[1].actor, "c@d");

        let 产物 = load_artifacts(store.conn(), "ZEN-1").unwrap();
        assert_eq!(产物.len(), 2);
        assert_eq!(产物[0].kind, ArtifactKind::Commit);
        assert_eq!(产物[0].reference, "abc1234");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
