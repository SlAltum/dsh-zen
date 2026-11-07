//! 用例编排层。每条命令一个函数；判定顺序、事务边界与事件写入在这里组装。
//!
//! 数据目录由 `dispatch` 统一解析，各命令只接收 `data_dir`——测试因此可以指定临时目录，
//! 不必改进程工作目录。

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::config;
use crate::domain::{
    self, Artifact, ArtifactKind, Effect, FieldDef, FieldKind, GuardFacts, Issue, LinkKind, NOTE_MIN_CHARS, ReportKind,
    Role, Status,
    check_accept_permission, check_granted, check_link_permission, check_permission, check_reopen_permission, derive_stage,
    eval_guards,
    normalize_fields, normalize_link,
};
use crate::error::{Result, ZenError};
use crate::store::{self, Event, Store};

/// 一次命令的输入。CLI 与 NDJSON 两个入口都归一到它。
#[derive(Debug)]
pub enum Invocation {
    /// `root` 显式给出时必须是仓库根；缺省从当前目录向上找。
    Init { root: Option<PathBuf> },
    /// 其余命令都要先定位数据目录。
    Data(DataCommand),
}

#[derive(Debug)]
pub enum DataCommand {
    Create {
        title: String,
        plan: Option<String>,
        kind: String,
        role: Role,
        actor: String,
        acceptance: Vec<String>,
        description: Option<String>,
        external_key: Option<String>,
        fields: Option<Map<String, Value>>,
    },
    Move {
        id: String,
        to: Status,
        role: Role,
        actor: String,
        extend: bool,
        reason: Option<String>,
        clear_block: bool,
    },
    Comment { id: String, body: String, role: Role, actor: String },
    Artifact { id: String, kind: String, reference: String, role: Role, actor: String },
    Accept { id: String, ordinal: u32, role: Role, actor: String },
    Update { id: String, title: Option<String>, description: Option<String>, role: Role, actor: String },
    Plan { id: String, title: Option<String>, background: Option<String>, context: Option<String>, role: Role, actor: String },
    Report { id: String, kind: String, note: Option<String>, role: Role, actor: String },
    Context { id: String, role: Role, actor: String },
    Check,
    Migrate { dry_run: bool },
    Board,
    Stats,
    Import { path: String, dry_run: bool, actor: String },
    Link { from: String, to: String, kind: String, role: Role, actor: String },
    Review { role: Role, actor: String, dry_run: bool },
    Reopen { id: String, role: Role, actor: String },
    Log { id: Option<String>, limit: usize },
    Schema,
    Workflow,
    Roles { role: Role },
    List(ListArgs),
    Views,
    Show { id: String, section: Option<String> },
}

/// `create` 的参数。字段与 SRS §5 的 `--plan` / `--type` 一一对应。
pub struct CreateArgs<'a> {
    pub title: &'a str,
    /// 计划标题。同名计划复用，不重复建。
    pub plan: Option<&'a str>,
    pub kind: &'a str,
    pub role: Role,
    pub actor: &'a str,
    /// 建任务时一并写下的验收标准，序号从 1 起。
    pub acceptance: &'a [String],
    /// 正文（任务描述）。缺省空串。
    pub description: Option<&'a str>,
    /// 外部键：同键重复创建返回已有任务，不重复建（R24）。导入用它做幂等。
    pub external_key: Option<&'a str>,
    /// 自定义字段值。键集由类型方案约束，校验走 domain::validate_fields。
    pub fields: Option<&'a Map<String, Value>>,
}

/// 命令结果。结构化，供渲染与工具返回共用。
pub type Report = serde_json::Value;

pub fn dispatch(inv: Invocation) -> Result<Report> {
    match inv {
        Invocation::Init { root } => init(root.as_deref()),
        Invocation::Data(cmd) => {
            let data_dir = config::find_data_dir(&std::env::current_dir()?)?;
            run(&data_dir, cmd)
        }
    }
}

fn run(data_dir: &Path, cmd: DataCommand) -> Result<Report> {
    let 结果 = 执行(data_dir, cmd)?;
    Ok(结果)
}

fn 执行(data_dir: &Path, cmd: DataCommand) -> Result<Report> {
    match cmd {
        DataCommand::Create { title, plan, kind, role, actor, acceptance, description, external_key, fields } => {
            let args = CreateArgs {
                title: &title,
                plan: plan.as_deref(),
                kind: &kind,
                role,
                actor: &actor,
                acceptance: &acceptance,
                description: description.as_deref(),
                external_key: external_key.as_deref(),
                fields: fields.as_ref(),
            };
            create(data_dir, &args)
        }
        DataCommand::Comment { id, body, role, actor } => {
            comment(data_dir, &CommentArgs { id: &id, body: &body, role, actor: &actor })
        }
        DataCommand::Artifact { id, kind, reference, role, actor } => artifact(
            data_dir,
            &ArtifactArgs { id: &id, kind: &kind, reference: &reference, role, actor: &actor },
        ),
        DataCommand::Accept { id, ordinal, role, actor } => {
            accept(data_dir, &AcceptArgs { id: &id, ordinal, role, actor: &actor })
        }
        DataCommand::Update { id, title, description, role, actor } => {
            update_issue_meta(data_dir, &id, title.as_deref(), description.as_deref(), role, &actor)
        }
        DataCommand::Plan { id, title, background, context, role, actor } => {
            edit_plan(data_dir, &id, title.as_deref(), background.as_deref(), context.as_deref(), role, &actor)
        }
        DataCommand::Report { id, kind, note, role, actor } => {
            report(data_dir, &ReportArgs { id: &id, kind: &kind, note: note.as_deref(), role, actor: &actor })
        }
        DataCommand::Context { id, role, actor } => context(data_dir, &ContextArgs { id: &id, role, actor: &actor }),
        DataCommand::Check => check(data_dir),
        DataCommand::Migrate { dry_run } => migrate(data_dir, dry_run),
        DataCommand::Import { path, dry_run, actor } => import(data_dir, Path::new(&path), dry_run, &actor),
        DataCommand::Link { from, to, kind, role, actor } => {
            link_cmd(data_dir, &LinkArgs { from: &from, to: &to, kind: &kind, role, actor: &actor })
        }
        DataCommand::Review { role, actor, dry_run } => review(data_dir, &ReviewArgs { role, actor: &actor, dry_run }),
        DataCommand::Reopen { id, role, actor } => reopen(data_dir, &ReopenArgs { id: &id, role, actor: &actor }),
        DataCommand::Log { id, limit } => log(data_dir, &LogArgs { id: id.as_deref(), limit }),
        DataCommand::Board => board(data_dir),
        DataCommand::Stats => stats(data_dir),
        DataCommand::Schema => schema(data_dir),
        DataCommand::Workflow => workflow(data_dir),
        DataCommand::Roles { role } => roles(data_dir, role),
        DataCommand::List(args) => list(data_dir, &args),
        DataCommand::Views => views(data_dir),
        DataCommand::Show { id, section } => show(data_dir, &id, section.as_deref()),
        DataCommand::Move { id, to, role, actor, extend, reason, clear_block } => move_issue(
            data_dir,
            &MoveArgs { id: &id, to, role, actor: &actor, extend, reason: reason.as_deref(), clear_block },
        ),
    }
}

/// `comment` 的参数。
pub struct CommentArgs<'a> {
    pub id: &'a str,
    pub body: &'a str,
    pub role: Role,
    pub actor: &'a str,
}

/// `artifact` 的参数。
pub struct ArtifactArgs<'a> {
    pub id: &'a str,
    /// file / commit / url / note
    pub kind: &'a str,
    pub reference: &'a str,
    pub role: Role,
    pub actor: &'a str,
}

/// `import`：从 arch-dev 的计划文档导入任务。走 create / move，不直插库（R39）。
/// 每行任务的 external_key = 「文件名#第一格」，因此重复导入不会重复建（R24）。
fn import(data_dir: &Path, 来源: &Path, dry_run: bool, actor: &str) -> Result<Report> {
    let 文档: Vec<PathBuf> = if 来源.is_dir() {
        let mut 列表: Vec<PathBuf> = std::fs::read_dir(来源)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
            .collect();
        列表.sort();
        列表
    } else {
        vec![来源.to_path_buf()]
    };

    let mut items: Vec<serde_json::Value> = Vec::new();
    let mut imported = 0;
    let mut existing = 0;
    let mut dropped = 0;

    for 文件 in &文档 {
        let 文本 = std::fs::read_to_string(文件).map_err(|e| ZenError::Io(format!("读不到 {}：{e}", 文件.display())))?;
        let 文件名 = 文件.file_name().and_then(|n| n.to_str()).unwrap_or("导入");
        let 计划名 = 文本
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .map(str::trim)
            .unwrap_or(文件名);

        // 只认「检查表 / 看板」小节里的表：计划文档里还有需求矩阵等派生表，
        // 把它们也导成任务就重复了。整篇没有这样的小节时退回全表。
        let 有看板节 = 文本.lines().any(|l| l.starts_with("## ") && (l.contains("检查表") || l.contains("看板")));
        let mut 当前节 = String::new();

        for (行号, line) in 文本.lines().enumerate() {
            if line.starts_with("## ") {
                当前节 = line.trim_start_matches('#').trim().to_string();
                continue;
            }
            let 行 = line.trim();
            if !行.starts_with('|') {
                continue;
            }
            if 有看板节 && !(当前节.contains("检查表") || 当前节.contains("看板")) {
                continue;
            }
            let 格: Vec<&str> = 行.trim_matches('|').split('|').map(str::trim).collect();
            let Some((状态, _)) = 格.iter().enumerate().find_map(|(i, c)| 状态标记(c).map(|s| (s, i))) else {
                continue;
            };
            let 键 = 格.first().copied().unwrap_or("").to_string();
            let 标题 = 格.get(1).copied().filter(|t| !t.is_empty()).unwrap_or(&键).to_string();
            if 键.is_empty() || 标题.is_empty() || 标题.chars().all(|c| c == '-') {
                continue;
            }
            let external_key = format!("{文件名}#{键}");

            if dry_run {
                // 预演也要报「会导入几条」，否则调用方没法判断要不要真跑
                imported += 1;
                items.push(json!({ "external_key": external_key, "source_status": 状态.as_str(), "line": 行号 + 1 }));
                continue;
            }

            let args = CreateArgs {
                title: &标题,
                plan: Some(计划名),
                kind: "task",
                role: Role::Planner,
                actor,
                acceptance: &[],
                description: None,
                external_key: Some(&external_key),
                fields: None,
            };
            let 结果 = create(data_dir, &args)?;
            let id = 结果["id"].as_str().unwrap_or_default().to_string();
            if 结果.get("existing").and_then(|v| v.as_bool()).unwrap_or(false) {
                existing += 1;
            } else {
                imported += 1;
                if 状态 == Status::Dropped {
                    move_issue(
                        data_dir,
                        &MoveArgs { id: &id, to: Status::Dropped, role: Role::Planner, actor, extend: false, reason: None, clear_block: false },
                    )?;
                    dropped += 1;
                }
            }
            items.push(json!({ "external_key": external_key, "id": id, "source_status": 状态.as_str() }));
        }
    }

    Ok(json!({ "dry_run": dry_run, "imported": imported, "existing": existing, "dropped": dropped, "items": items }))
}

/// 计划文档里的状态标记 → 状态。空格占位（\`[ ]\`）算待办。
fn 状态标记(格: &str) -> Option<Status> {
    match 格 {
        "[TODO]" => Some(Status::Todo),
        "[WIP]" => Some(Status::Wip),
        "[DONE]" => Some(Status::Done),
        "[DEPRECATED]" => Some(Status::Dropped),
        _ => None,
    }
}

/// `review` 的参数。
pub struct ReviewArgs<'a> {
    pub role: Role,
    pub actor: &'a str,
    pub dry_run: bool,
}

/// 定期复盘：扫 done 且 reviewed_at 为空的任务，各建一条复盘任务并打标（R31）。
/// 打标之后第二次跑就没有可扫的了，因此连跑两次不会重复建。
fn review(data_dir: &Path, args: &ReviewArgs<'_>) -> Result<Report> {
    let cfg = config::load(data_dir)?;
    check_granted(&cfg.roles.role_grants(), args.role, args.actor)?;
    if !matches!(args.role, Role::Reviewer | Role::Lead) {
        return Err(ZenError::Permission { action: "定期 review".into(), role: args.role.as_str().to_string() });
    }

    let mut store = Store::open(&data_dir.join(config::DB_FILE))?;
    let now = store.now();
    store.tx(|conn| {
        let 待复盘: Vec<Issue> = store::load_issues(conn)?
            .into_iter()
            .filter(|i| i.status == Status::Done && i.reviewed_at.is_none())
            .collect();

        let mut created = Vec::new();
        for 原 in &待复盘 {
            let id = if args.dry_run {
                String::from("（预演）")
            } else {
                let id = domain::next_id(domain::ID_PREFIX, &store::issue_ids(conn, domain::ID_PREFIX)?);
                let 复盘任务 = Issue {
                    id: id.clone(),
                    plan: 原.plan.clone(),
                    kind: "task".into(),
                    title: format!("复盘 {}：{}", 原.id, 原.title),
                    description: String::new(),
                    status: Status::Todo,
                    priority: domain::DEFAULT_PRIORITY.to_string(),
                    parent: None,
                    block_reason: None,
                    fields: Map::new(),
                    external_key: Some(format!("review:{}", 原.id)),
                    claim_actor: None,
                    claim_until: None,
                    reviewed_at: None,
                    rev: 0,
                    created_at: now.clone(),
                    updated_at: now.clone(),
                };
                store::insert_issue(conn, &复盘任务)?;

                let mut 已复盘 = 原.clone();
                已复盘.reviewed_at = Some(now.clone());
                已复盘.updated_at = now.clone();
                store::update_issue_cas(conn, &已复盘)?;

                for (subject, kind, payload) in [
                    (id.clone(), "create", json!({ "title": 复盘任务.title, "source": 原.id })),
                    (原.id.clone(), "reviewed", json!({ "created": id })),
                ] {
                    store::append_event(
                        conn,
                        &Event {
                            subject_type: "issue",
                            subject_id: subject,
                            kind: kind.into(),
                            from_state: None,
                            to_state: None,
                            actor: args.actor.to_string(),
                            role: args.role.as_str().to_string(),
                            payload,
                        },
                        &now,
                    )?;
                }
                id
            };
            created.push(json!({ "id": id, "source": 原.id, "title": format!("复盘 {}：{}", 原.id, 原.title) }));
        }

        Ok(json!({ "dry_run": args.dry_run, "reviewed": 待复盘.len(), "created": created }))
    })
}

/// `link` 的参数。
pub struct LinkArgs<'a> {
    pub from: &'a str,
    pub to: &'a str,
    /// blocks / relates / duplicates
    pub kind: &'a str,
    pub role: Role,
    pub actor: &'a str,
}

/// 建关联。三类各有约束：blocks 只给 planner/lead 且不许成环；duplicates 先把两端归一化再存。
fn link_cmd(data_dir: &Path, args: &LinkArgs<'_>) -> Result<Report> {
    let kind = LinkKind::parse(args.kind).ok_or_else(|| ZenError::Validation {
        field: "kind".into(),
        why: format!("未知关联种类 {}，可用：blocks / relates / duplicates", args.kind),
    })?;
    if args.from == args.to {
        return Err(ZenError::Validation { field: "to".into(), why: "不能和自己建关联".into() });
    }
    let cfg = config::load(data_dir)?;
    check_granted(&cfg.roles.role_grants(), args.role, args.actor)?;
    check_link_permission(args.role, kind)?;

    let mut store = Store::open(&data_dir.join(config::DB_FILE))?;
    let now = store.now();
    store.tx(|conn| {
        store::load_issue(conn, args.from)?;
        store::load_issue(conn, args.to)?;
        let (from, to) = normalize_link(args.from, args.to, kind);

        if kind == LinkKind::Blocks {
            // 先按「加进去之后」的图判环，环上的编号由检测器给出
            let mut 候选 = store::load_links(conn)?;
            候选.push(domain::Link { from: from.clone(), to: to.clone(), kind });
            domain::check_blocks_acyclic(&候选)?;
        }

        let 新增 = store::insert_link(conn, &from, &to, kind)?;
        if 新增 {
            store::append_event(
                conn,
                &Event {
                    subject_type: "issue",
                    subject_id: from.clone(),
                    kind: "link".into(),
                    from_state: None,
                    to_state: None,
                    actor: args.actor.to_string(),
                    role: args.role.as_str().to_string(),
                    payload: json!({ "to": to, "kind": kind.as_str() }),
                },
                &now,
            )?;
        }
        Ok(json!({ "from": from, "to": to, "kind": kind.as_str(), "existing": !新增 }))
    })
}

/// `reopen` 的参数。
pub struct ReopenArgs<'a> {
    pub id: &'a str,
    pub role: Role,
    pub actor: &'a str,
}

/// 重开终态：回到 todo 并落事件（R14）。只 lead，非终态拒绝。
fn reopen(data_dir: &Path, args: &ReopenArgs<'_>) -> Result<Report> {
    let cfg = config::load(data_dir)?;
    check_granted(&cfg.roles.role_grants(), args.role, args.actor)?;
    check_reopen_permission(args.role)?;

    let mut store = Store::open(&data_dir.join(config::DB_FILE))?;
    let now = store.now();
    store.tx(|conn| {
        let mut issue = store::load_issue(conn, args.id)?;
        if !issue.status.is_terminal() {
            return Err(ZenError::Validation {
                field: "id".into(),
                why: format!("只有终态可以重开，{} 现在是 {}", args.id, issue.status.as_str()),
            });
        }
        let from = issue.status;
        issue.status = Status::Todo;
        issue.block_reason = None;
        issue.updated_at = now.clone();
        let rev = store::update_issue_cas(conn, &issue)?;

        store::append_event(
            conn,
            &Event {
                subject_type: "issue",
                subject_id: args.id.to_string(),
                kind: "reopen".into(),
                from_state: Some(from.as_str().to_string()),
                to_state: Some(Status::Todo.as_str().to_string()),
                actor: args.actor.to_string(),
                role: args.role.as_str().to_string(),
                payload: json!({ "rev": rev }),
            },
            &now,
        )?;
        Ok(json!({ "id": args.id, "from": from.as_str(), "status": Status::Todo.as_str(), "rev": rev }))
    })
}

/// `log` 的参数。
pub struct LogArgs<'a> {
    /// 只看某个任务或计划；省略则全局。
    pub id: Option<&'a str>,
    pub limit: usize,
}

/// 事件回放：只读，按 seq 升序返回最后 N 条（R32）。
fn log(data_dir: &Path, args: &LogArgs<'_>) -> Result<Report> {
    let store = Store::open(&data_dir.join(config::DB_FILE))?;
    let events = store::load_events(store.conn(), args.id, args.limit)?;
    Ok(json!({ "events": events }))
}

/// `board`：终端看板。人读用，不进 agent 主路径（R40）。
fn board(data_dir: &Path) -> Result<Report> {
    let store = Store::open(&data_dir.join(config::DB_FILE))?;
    let issues = store::load_issues(store.conn())?;

    let mut text = String::new();
    let mut counts = serde_json::Map::new();
    for status in Status::ALL {
        let 组: Vec<&Issue> = issues.iter().filter(|i| i.status == status).collect();
        counts.insert(status.as_str().to_string(), json!(组.len()));
        text.push_str(&format!("{}（{}）\n", status.as_str(), 组.len()));
        for issue in 组.iter().take(MAX_ROWS) {
            let 持有 = issue.claim_actor.as_deref().map(|a| format!(" @{a}")).unwrap_or_default();
            text.push_str(&format!("  {} {}{}\n", issue.id, issue.title, 持有));
        }
    }
    Ok(json!({ "text": text, "counts": counts }))
}

/// `stats`：结构化统计。人读用（R40）。
fn stats(data_dir: &Path) -> Result<Report> {
    let cfg = config::load(data_dir)?;
    let store = Store::open(&data_dir.join(config::DB_FILE))?;
    let now = store.now();
    let issues = store::load_issues(store.conn())?;

    let 计数 = |pick: &dyn Fn(&Issue) -> String| {
        let mut acc = serde_json::Map::new();
        for issue in &issues {
            let key = pick(issue);
            acc.insert(key.clone(), json!(acc.get(&key).and_then(|v| v.as_i64()).unwrap_or(0) + 1));
        }
        acc
    };

    Ok(json!({
        "total": issues.len(),
        "by_status": 计数(&|i| i.status.as_str().to_string()),
        "by_type": 计数(&|i| i.kind.clone()),
        "by_priority": 计数(&|i| i.priority.clone()),
        "unclaimed": issues.iter().filter(|i| domain::is_unclaimed(i, &now)).count(),
        "stale_after_days": cfg.workflow.tunables.stale_after_days,
        "review_after_days": cfg.workflow.tunables.review_after_days,
    }))
}

/// `migrate`：把存量任务的字段值迁到当前类型方案。`dry_run` 只报告不落库（R38）。
fn migrate(data_dir: &Path, dry_run: bool) -> Result<Report> {
    let cfg = config::load(data_dir)?;
    let mut store = Store::open(&data_dir.join(config::DB_FILE))?;
    let now = store.now();

    let mut changed = Vec::new();
    let mut 待补: Vec<serde_json::Value> = Vec::new();
    let mut counts = serde_json::Map::new();
    store.tx(|conn| {
        for issue in store::load_issues(conn)? {
            let specs = match cfg.field_specs(&issue.kind) {
                Ok(specs) => specs,
                Err(_) => continue, // 未知类型归 check 管
            };
            let 结果 = normalize_fields(&issue.kind, &specs, &issue.fields);
            // 必填但无缺省值的字段只能人补，不构成「可自动迁移的改动」，每轮都报一次。
            if !结果.unresolved.is_empty() {
                待补.push(json!({ "id": issue.id, "fields": 结果.unresolved.clone() }));
            }
            if 结果.dropped.is_empty() && 结果.fields == issue.fields {
                continue;
            }
            let mut 条目 = json!({
                "id": issue.id,
                "dropped": 结果.dropped.iter().map(|(k, why)| json!({ "field": k, "why": why })).collect::<Vec<_>>(),
                "fields": 结果.fields,
            });
            counts.insert("issues".into(), json!(counts.get("issues").and_then(|v| v.as_i64()).unwrap_or(0) + 1));
            条目["dry_run"] = json!(dry_run);
            changed.push(条目);

            if !dry_run {
                let mut 迁移后 = issue.clone();
                迁移后.fields = 结果.fields;
                迁移后.updated_at = now.clone();
                store::update_issue_cas(conn, &迁移后)?;
            }
        }
        Ok(())
    })?;

    Ok(json!({ "dry_run": dry_run, "counts": counts, "changed": changed, "unresolved": 待补 }))
}

/// `check`：六类校验，只报不修（R37）。
fn check(data_dir: &Path) -> Result<Report> {
    let mut 发现: Vec<serde_json::Value> = Vec::new();
    let mut 记 = |kind: &str, target: String, detail: String| {
        发现.push(json!({ "kind": kind, "target": target, "detail": detail }));
    };

    let cfg = match config::load(data_dir) {
        Ok(cfg) => Some(cfg),
        Err(e) => {
            记("config", "配置".into(), e.to_string());
            None
        }
    };

    let store = Store::open(&data_dir.join(config::DB_FILE))?;
    store::init_schema(&store)?;
    let conn = store.conn();
    let now = store.now();

    // 结构：状态、优先级、类型
    let mut stmt = conn.prepare("SELECT id, status, priority, type FROM issue ORDER BY id")?;
    let 行: Vec<(String, String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, status, priority, kind) in &行 {
        if Status::parse(status).is_none() {
            记("structure", id.clone(), format!("未知状态 {status}"));
        }
        if !domain::PRIORITIES.contains(&priority.as_str()) {
            记("structure", id.clone(), format!("未知优先级 {priority}"));
        }
        if let Some(cfg) = &cfg {
            if cfg.type_def(kind).is_err() {
                记("structure", id.clone(), format!("未声明的类型 {kind}"));
            }
        }
    }

    // 配置：类型绑定的字段都要有定义
    if let Some(cfg) = &cfg {
        for t in &cfg.types {
            for field in &t.fields {
                if cfg.field_def(field).is_err() {
                    记("config", t.name.clone(), format!("类型绑定未定义的字段 {field}"));
                }
            }
        }
    }

    // 断引用：外键在运行期拦得住，但被外部改过的库拦不住
    let 悬空 = |sql: &str| -> Result<Vec<(String, String)>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    };
    for (id, plan_id) in 悬空("SELECT i.id, i.plan_id FROM issue i LEFT JOIN plan p ON p.id = i.plan_id WHERE i.plan_id IS NOT NULL AND p.id IS NULL")? {
        记("dangling", id, format!("计划 {plan_id} 不存在"));
    }
    for (id, parent) in 悬空("SELECT i.id, i.parent_id FROM issue i LEFT JOIN issue p ON p.id = i.parent_id WHERE i.parent_id IS NOT NULL AND p.id IS NULL")? {
        记("dangling", id, format!("父任务 {parent} 不存在"));
    }
    for (id, issue_id) in 悬空("SELECT CAST(c.id AS TEXT), c.issue_id FROM comment c LEFT JOIN issue i ON i.id = c.issue_id WHERE i.id IS NULL")? {
        记("dangling", format!("comment#{id}"), format!("指向不存在的任务 {issue_id}"));
    }
    for (id, issue_id) in 悬空("SELECT CAST(a.id AS TEXT), a.issue_id FROM artifact a LEFT JOIN issue i ON i.id = a.issue_id WHERE i.id IS NULL")? {
        记("dangling", format!("artifact#{id}"), format!("指向不存在的任务 {issue_id}"));
    }
    for (id, issue_id) in 悬空("SELECT l.from_id || '→' || l.to_id, l.from_id FROM link l LEFT JOIN issue i ON i.id = l.from_id WHERE i.id IS NULL")? {
        记("dangling", format!("link#{id}"), format!("起点 {issue_id} 不存在"));
    }

    // 环
    match store::load_links(conn) {
        Ok(links) => {
            if let Err(e) = domain::check_blocks_acyclic(&links) {
                记("cycle", "blocks".into(), e.to_string());
            }
        }
        Err(e) => 记("structure", "link".into(), format!("关联读不出来：{e}")),
    }

    // 租约：状态与租约互相矛盾，或租约已过期
    for (id, status, actor, until) in 租约行(conn)? {
        match (Status::parse(&status), actor, until) {
            (Some(Status::Wip), None, _) => 记("lease", id, "在 wip 却没有租约".into()),
            (Some(s), Some(_), None) if s != Status::Wip => 记("lease", id, format!("状态 {} 却留着租约", s.as_str())),
            (Some(s), Some(actor), Some(until)) => {
                if s != Status::Wip {
                    记("lease", id.clone(), format!("状态 {} 却留着 {actor} 的租约", s.as_str()));
                }
                if until < now {
                    记("lease", id, format!("租约被 {actor} 持有至 {until}，已过期"));
                }
            }
            _ => {}
        }
    }

    // 产物可解析
    for (issue_id, kind, reference) in 产物行(conn)? {
        match ArtifactKind::parse(&kind) {
            None => 记("artifact", issue_id.clone(), format!("未知产物种类 {kind}")),
            Some(k) => {
                let 待查 = Artifact { issue: issue_id.clone(), kind: k, reference: reference.clone(), sha256: None, at: String::new() };
                if !待查.is_usable() {
                    记("artifact", issue_id.clone(), format!("产物不可用（note 需至少 {NOTE_MIN_CHARS} 字）：{reference}"));
                } else if k == ArtifactKind::File && !Path::new(&reference).exists() {
                    记("artifact", issue_id, format!("file 产物路径不存在：{reference}"));
                }
            }
        }
    }

    Ok(json!({
        "ok": 发现.is_empty(),
        "counts": 发现.iter().fold(serde_json::Map::new(), |mut acc, f| {
            let k = f["kind"].as_str().unwrap_or("?").to_string();
            let n = acc.get(&k).and_then(|v| v.as_i64()).unwrap_or(0) + 1;
            acc.insert(k, json!(n));
            acc
        }),
        "findings": 发现,
    }))
}

fn 租约行(conn: &rusqlite::Connection) -> Result<Vec<(String, String, Option<String>, Option<String>)>> {
    let mut stmt = conn.prepare("SELECT id, status, claim_actor, claim_until FROM issue ORDER BY id")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn 产物行(conn: &rusqlite::Connection) -> Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare("SELECT issue_id, kind, \"ref\" FROM artifact ORDER BY id")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// `schema`：类型与字段方案、状态集、优先级、计划与任务的字段面。
/// 版本化输出（R19）：同一 schema_version 内字段不增不减。
fn schema(data_dir: &Path) -> Result<Report> {
    let cfg = config::load(data_dir)?;
    let 类型: Vec<serde_json::Value> = cfg
        .types
        .iter()
        .map(|t| -> Result<serde_json::Value> {
            Ok(json!({ "name": t.name, "label": t.label, "fields": cfg.field_specs(&t.name)? }))
        })
        .collect::<Result<_>>()?;

    Ok(json!({
        "schema_version": 1,
        "statuses": Status::ALL.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        "priorities": domain::PRIORITIES,
        "types": 类型,
        "plan": { "fields": ["id", "title", "background", "context", "created_at"] },
        "issue": { "fields": [
            "id", "plan", "type", "title", "description", "status", "priority", "parent",
            "block_reason", "fields", "external_key", "claim_actor", "claim_until", "reviewed_at",
            "rev", "created_at", "updated_at"
        ] },
    }))
}

/// `workflow`：转移、守卫、效果、视图与时间阈值，全部来自配置。
fn workflow(data_dir: &Path) -> Result<Report> {
    let cfg = config::load(data_dir)?;
    let 转移: Vec<serde_json::Value> = cfg
        .workflow
        .transitions
        .iter()
        .map(|t| json!({ "from": t.from, "to": t.to, "guards": t.guard, "effects": t.effects }))
        .collect();
    let 视图: Vec<serde_json::Value> = cfg
        .workflow
        .views
        .iter()
        .map(|v| json!({ "name": v.name, "predicate": v.predicate, "columns": v.columns }))
        .collect();
    Ok(json!({ "transitions": 转移, "views": 视图, "tunables": cfg.workflow.tunables }))
}

/// `roles`：只输出当前角色这一行（R20），含它可发起的转移与授权方式。
fn roles(data_dir: &Path, role: Role) -> Result<Report> {
    let cfg = config::load(data_dir)?;
    let 缺 = || ZenError::Config { file: "roles.toml".into(), why: format!("没有角色 {}", role.as_str()) };
    let def = cfg.roles.role_def(role.as_str()).ok_or_else(缺)?;
    let sections = cfg.roles.sections_for(role.as_str()).ok_or_else(缺)?;

    let mut 可发起 = Vec::new();
    for grant in &cfg.roles.permissions {
        if grant.roles.iter().any(|r| r == role.as_str()) {
            可发起.push(json!({ "transition": grant.transition, "kind": "direct" }));
        } else if grant.holders.iter().any(|r| r == role.as_str()) {
            可发起.push(json!({ "transition": grant.transition, "kind": "holder" }));
        }
    }

    Ok(json!({
        "role": def,
        "context_sections": sections,
        "can_transitions": 可发起,
    }))
}

/// `context` 的参数。
pub struct ContextArgs<'a> {
    pub id: &'a str,
    pub role: Role,
    pub actor: &'a str,
}

/// 最小上下文包：该任务本身、计划摘要、直接依赖状态、验收标准、相关关联，
/// 外加该角色声明的额外字段集。不取同计划其它任务正文，不取评论（§6.2 / R25）。
fn context(data_dir: &Path, args: &ContextArgs<'_>) -> Result<Report> {
    let cfg = config::load(data_dir)?;
    let sections = cfg.roles.sections_for(args.role.as_str()).ok_or_else(|| ZenError::Validation {
        field: "role".into(),
        why: format!(
            "角色 {} 没有上下文包；已配置：{}",
            args.role.as_str(),
            cfg.roles.section_roles()
        ),
    })?;
    let _ = args.actor;

    let store = Store::open(&data_dir.join(config::DB_FILE))?;
    let issue = store::load_issue(store.conn(), args.id)?;
    let links = store::load_links(store.conn())?;

    let plan = match &issue.plan {
        None => serde_json::Value::Null,
        Some(plan_id) => {
            let plan = store::load_plan(store.conn(), plan_id)?;
            json!({ "id": plan.id, "title": plan.title })
        }
    };
    // 直接依赖：谁阻塞我；相关关联：进出两个方向。
    let deps: Vec<serde_json::Value> = links
        .iter()
        .filter(|l| l.kind == LinkKind::Blocks && l.to == args.id)
        .filter_map(|l| store::load_issue(store.conn(), &l.from).ok())
        .map(|dep| json!({ "id": dep.id, "title": dep.title, "status": dep.status.as_str() }))
        .collect();
    let 关联: Vec<serde_json::Value> = links
        .iter()
        .filter(|l| l.from == args.id || l.to == args.id)
        .map(|l| json!({ "from": l.from, "to": l.to, "kind": l.kind.as_str() }))
        .collect();

    let view = |name: &str| -> Result<serde_json::Value> {
        Ok(match name {
            "id" => json!(issue.id),
            "title" => json!(issue.title),
            // 工单必须有正文摘要，否则 agent 连任务描述都看不到；全文仍走 show（R25）
            "description_excerpt" => {
                let 全: Vec<char> = issue.description.chars().collect();
                let 摘要: String = 全.iter().take(200).collect();
                json!({
                    "text": 摘要,
                    "truncated": 全.len() > 200,
                    "full": format!("zen_show {}", issue.id),
                })
            }
            "status" => json!(issue.status.as_str()),
            "priority" => json!(issue.priority),
            "type" => json!(issue.kind),
            "block_reason" => json!(issue.block_reason),
            "plan" => plan.clone(),
            "deps" => json!(deps),
            "links" => json!(关联),
            "acceptance" => serde_json::to_value(store::load_acceptance(store.conn(), args.id)?)
                .map_err(|e| ZenError::Io(format!("序列化失败：{e}")))?,
            "artifacts" => serde_json::to_value(store::load_artifacts(store.conn(), args.id)?)
                .map_err(|e| ZenError::Io(format!("序列化失败：{e}")))?,
            other => {
                return Err(ZenError::Config {
                    file: "roles.toml".into(),
                    why: format!("上下文包字段 {other} 不认识"),
                });
            }
        })
    };

    let mut out = Map::new();
    for section in sections {
        out.insert(section.clone(), view(&section)?);
    }
    Ok(serde_json::Value::Object(out))
}

/// `report` 的参数。
pub struct ReportArgs<'a> {
    pub id: &'a str,
    /// progress / waiting / ready / blocked / failed / dropped
    pub kind: &'a str,
    pub note: Option<&'a str>,
    pub role: Role,
    pub actor: &'a str,
}

/// 提交结构化报告。不改状态，只落一条事件；阶段由报告与租约派生（R30）。
fn report(data_dir: &Path, args: &ReportArgs<'_>) -> Result<Report> {
    let kind = ReportKind::parse(args.kind).ok_or_else(|| ZenError::Validation {
        field: "kind".into(),
        why: format!(
            "未知报告类型 {}，可用：progress / waiting / ready / blocked / failed / dropped",
            args.kind
        ),
    })?;
    let mut store = Store::open(&data_dir.join(config::DB_FILE))?;
    let now = store.now();
    store.tx(|conn| {
        let issue = store::load_issue(conn, args.id)?;
        store::append_event(
            conn,
            &Event {
                subject_type: "issue",
                subject_id: args.id.to_string(),
                kind: "report".into(),
                from_state: Some(issue.status.as_str().to_string()),
                to_state: Some(issue.status.as_str().to_string()),
                actor: args.actor.to_string(),
                role: args.role.as_str().to_string(),
                payload: json!({ "kind": kind.as_str(), "note": args.note.unwrap_or_default() }),
            },
            &now,
        )?;
        let stage = derive_stage(issue.status, Some(kind), store::lease_of(conn, args.id)?.is_some());
        Ok(json!({ "id": args.id, "kind": kind.as_str(), "stage": stage.as_str() }))
    })
}

/// `accept` 的参数。
pub struct AcceptArgs<'a> {
    pub id: &'a str,
    pub ordinal: u32,
    pub role: Role,
    pub actor: &'a str,
}

/// 写一条评论。终态任务仍可评论（R14）。
fn comment(data_dir: &Path, args: &CommentArgs<'_>) -> Result<Report> {
    let body = args.body.trim();
    if body.is_empty() {
        return Err(ZenError::Validation { field: "body".into(), why: "不能为空".into() });
    }
    let mut store = Store::open(&data_dir.join(config::DB_FILE))?;
    let now = store.now();
    store.tx(|conn| {
        store::load_issue(conn, args.id)?;
        store::insert_comment(conn, args.id, args.actor, body, &now)?;
        store::append_event(
            conn,
            &Event {
                subject_type: "issue",
                subject_id: args.id.to_string(),
                kind: "comment".into(),
                from_state: None,
                to_state: None,
                actor: args.actor.to_string(),
                role: args.role.as_str().to_string(),
                payload: json!({ "body": body }),
            },
            &now,
        )?;
        Ok(json!({ "id": args.id, "at": now }))
    })
}

/// 挂一条产物。终态任务仍可挂（R14）；file 要求路径存在，note 要求够长。
fn artifact(data_dir: &Path, args: &ArtifactArgs<'_>) -> Result<Report> {
    let kind = ArtifactKind::parse(args.kind).ok_or_else(|| ZenError::Validation {
        field: "kind".into(),
        why: format!("未知产物种类 {}，可用：file / commit / url / note", args.kind),
    })?;
    let reference = args.reference.trim();
    let 待写 = Artifact { issue: args.id.to_string(), kind, reference: reference.to_string(), sha256: None, at: String::new() };
    if !待写.is_usable() {
        return Err(ZenError::Validation {
            field: "ref".into(),
            why: format!("note 至少要 {NOTE_MIN_CHARS} 字，其余种类不能为空"),
        });
    }
    if kind == ArtifactKind::File && !std::path::Path::new(reference).exists() {
        return Err(ZenError::Validation { field: "ref".into(), why: format!("路径不存在：{reference}") });
    }

    let mut store = Store::open(&data_dir.join(config::DB_FILE))?;
    let now = store.now();
    store.tx(|conn| {
        store::load_issue(conn, args.id)?;
        store::insert_artifact(conn, args.id, kind, reference, None, &now)?;
        store::append_event(
            conn,
            &Event {
                subject_type: "issue",
                subject_id: args.id.to_string(),
                kind: "artifact".into(),
                from_state: None,
                to_state: None,
                actor: args.actor.to_string(),
                role: args.role.as_str().to_string(),
                payload: json!({ "kind": kind.as_str(), "ref": reference }),
            },
            &now,
        )?;
        Ok(json!({ "id": args.id, "kind": kind.as_str(), "ref": reference }))
    })
}

/// 勾一条验收标准。逐条判定，不接受批量勾；终态任务的验收标准只读（§3.5）。
fn accept(data_dir: &Path, args: &AcceptArgs<'_>) -> Result<Report> {
    let cfg = config::load(data_dir)?;
    check_granted(&cfg.roles.role_grants(), args.role, args.actor)?;

    let mut store = Store::open(&data_dir.join(config::DB_FILE))?;
    let now = store.now();
    store.tx(|conn| {
        let issue = store::load_issue(conn, args.id)?;
        if issue.status.is_terminal() {
            return Err(ZenError::Guard {
                transition: format!("accept@{}", issue.status.as_str()),
                missing: "终态任务的验收标准只读".into(),
            });
        }
        let holder = store::lease_of(conn, args.id)?.map(|(actor, _)| actor);
        check_accept_permission(args.role, holder.as_deref(), args.actor)?;

        store::check_acceptance(conn, args.id, args.ordinal, args.actor, &now)?;
        store::append_event(
            conn,
            &Event {
                subject_type: "issue",
                subject_id: args.id.to_string(),
                kind: "acceptance_checked".into(),
                from_state: None,
                to_state: None,
                actor: args.actor.to_string(),
                role: args.role.as_str().to_string(),
                payload: json!({ "ordinal": args.ordinal }),
            },
            &now,
        )?;
        Ok(json!({ "id": args.id, "ordinal": args.ordinal, "checked_by": args.actor }))
    })
}

/// `move` 的参数。
pub struct MoveArgs<'a> {
    pub id: &'a str,
    pub to: Status,
    pub role: Role,
    pub actor: &'a str,
    /// 只续期，不改状态。
    pub extend: bool,
    /// 进入 blocked 时的原因，必填。
    pub reason: Option<&'a str>,
    /// 重新领取前显式清空 block_reason，§3.3 要求。
    pub clear_block: bool,
}

/// 状态流转。判定顺序是架构约定：授予 → 权限 → 转移合法性 → 守卫 → 租约 → 落库。
/// 任一步失败都不落状态；状态写回与事件在同一事务。
fn move_issue(data_dir: &Path, args: &MoveArgs<'_>) -> Result<Report> {
    let cfg = config::load(data_dir)?;
    check_granted(&cfg.roles.role_grants(), args.role, args.actor)?;

    let mut store = Store::open(&data_dir.join(config::DB_FILE))?;
    let now = store.now();
    let ttl = cfg.workflow.tunables.lease_ttl_minutes;

    store.tx(|conn| {
        let mut issue = store::load_issue(conn, args.id)?;
        if args.extend {
            return extend_lease(conn, &issue, args, ttl, &now);
        }

        if args.to == Status::Blocked && args.reason.map(|r| r.trim().is_empty()).unwrap_or(true) {
            return Err(ZenError::Validation {
                field: "reason".into(),
                why: "进入 blocked 必须说明原因".into(),
            });
        }
        if args.clear_block {
            issue.block_reason = None;
        }

        let holder = store::lease_of(conn, args.id)?.map(|(actor, _)| actor);
        check_permission(&cfg.roles.permissions, args.role, issue.status, args.to, holder.as_deref(), args.actor)?;

        let transition = cfg.transition(issue.status, args.to)?;
        let 键 = format!("{}→{}", issue.status.as_str(), args.to.as_str());
        let acceptance = store::load_acceptance(conn, args.id)?;
        let artifacts = store::load_artifacts(conn, args.id)?;
        let facts = GuardFacts { acceptance: &acceptance, artifacts: &artifacts, block_reason: issue.block_reason.as_deref() };
        eval_guards(&键, &transition.guard_kinds()?, &facts)?;

        if args.to == Status::Wip {
            let until = store::lease_until(conn, &now, ttl)?;
            store::claim_lease(conn, args.id, args.actor, &until, &now)?;
            issue.claim_actor = Some(args.actor.to_string());
            issue.claim_until = Some(until);
        }
        for effect in transition.effect_kinds()? {
            match effect {
                Effect::ReleaseLease => 释放(conn, &mut issue, &now)?,
            }
        }
        if issue.status == Status::Wip && args.to != Status::Wip {
            释放(conn, &mut issue, &now)?;
        }

        if args.to == Status::Blocked {
            issue.block_reason = args.reason.map(str::to_string);
        }

        let from = issue.status;
        issue.status = args.to;
        issue.updated_at = now.clone();
        let rev = store::update_issue_cas(conn, &issue)?;

        store::append_event(
            conn,
            &Event {
                subject_type: "issue",
                subject_id: args.id.to_string(),
                kind: "status_change".into(),
                from_state: Some(from.as_str().to_string()),
                to_state: Some(args.to.as_str().to_string()),
                actor: args.actor.to_string(),
                role: args.role.as_str().to_string(),
                payload: json!({ "rev": rev }),
            },
            &now,
        )?;

        Ok(json!({
            "id": args.id,
            "from": from.as_str(),
            "status": issue.status.as_str(),
            "rev": rev,
            "claim_actor": issue.claim_actor,
        }))
    })
}

/// 释放租约：库里清空，内存里也清空，免得写回时又把它写回去。
fn 释放(conn: &rusqlite::Connection, issue: &mut Issue, now: &str) -> Result<()> {
    store::release_lease(conn, &issue.id, now)?;
    issue.claim_actor = None;
    issue.claim_until = None;
    Ok(())
}

/// 续期：只动租约，不动状态与 rev。
fn extend_lease(conn: &rusqlite::Connection, issue: &Issue, args: &MoveArgs<'_>, ttl: u32, now: &str) -> Result<Report> {
    if issue.status != Status::Wip {
        return Err(ZenError::Validation {
            field: "extend".into(),
            why: format!("只有 wip 中的任务可以续期，当前是 {}", issue.status.as_str()),
        });
    }
    let until = store::lease_until(conn, now, ttl)?;
    store::renew_lease(conn, args.id, args.actor, &until, now)?;
    store::append_event(
        conn,
        &Event {
            subject_type: "issue",
            subject_id: args.id.to_string(),
            kind: "lease_renew".into(),
            from_state: Some(Status::Wip.as_str().to_string()),
            to_state: Some(Status::Wip.as_str().to_string()),
            actor: args.actor.to_string(),
            role: args.role.as_str().to_string(),
            payload: json!({ "claim_until": until }),
        },
        now,
    )?;
    Ok(json!({ "id": args.id, "status": Status::Wip.as_str(), "claim_actor": args.actor, "claim_until": until }))
}

/// 列表过滤条件，对应 SRS 的 flags（R18）。owned 便于直接来自 CLI 与工具参数。
#[derive(Debug, Default)]
pub struct ListArgs {
    pub view: Option<String>,
    pub status: Option<String>,
    pub kind: Option<String>,
    pub priority: Option<String>,
    pub plan: Option<String>,
    pub actor: Option<String>,
    pub unclaimed: bool,
}

/// 默认输出预算：一次列表最多这么多行（R28）。
const MAX_ROWS: usize = 40;

/// 不带视图名时的列集。带视图名时改用视图定义里的列集。
const DEFAULT_COLUMNS: [&str; 4] = ["id", "title", "status", "priority"];

/// 查询：视图名决定谓词与列集，flags 再收窄。
fn list(data_dir: &Path, args: &ListArgs) -> Result<Report> {
    let cfg = config::load(data_dir)?;
    let store = Store::open(&data_dir.join(config::DB_FILE))?;
    let now = store.now();
    let ctx = store::ViewCtx {
        actor: args.actor.as_deref().unwrap_or(""),
        now: &now,
        stale_after_days: cfg.workflow.tunables.stale_after_days,
    };

    let (columns, rows) = match &args.view {
        Some(name) => {
            let def = cfg.view(name)?;
            (def.columns.clone(), store::view(store.conn(), def.predicate_kind()?, &ctx)?)
        }
        None => (DEFAULT_COLUMNS.iter().map(|c| c.to_string()).collect(), store::load_issues(store.conn())?),
    };

    let picked: Vec<Issue> = rows.into_iter().filter(|issue| keep(issue, args, &now)).collect();
    let total = picked.len();
    let rows: Vec<serde_json::Value> =
        picked.iter().take(MAX_ROWS).map(|issue| row_json(issue, &columns)).collect::<Result<_>>()?;

    Ok(json!({
        "view": args.view,
        "columns": columns,
        "total": total,
        "truncated": total > MAX_ROWS,
        "rows": rows,
    }))
}

/// flags 是并列的收窄条件，全部满足才留下。
fn keep(issue: &Issue, args: &ListArgs, now: &str) -> bool {
    let same = |want: &Option<String>, got: Option<&str>| match want {
        None => true,
        Some(w) => got == Some(w.as_str()),
    };
    same(&args.status, Some(issue.status.as_str()))
        && same(&args.kind, Some(issue.kind.as_str()))
        && same(&args.priority, Some(issue.priority.as_str()))
        && same(&args.plan, issue.plan.as_deref())
        && same(&args.actor, issue.claim_actor.as_deref())
        && (!args.unclaimed || domain::is_unclaimed(issue, now))
}

/// 一行只输出视图声明的列，避免把正文带进列表。
fn row_json(issue: &Issue, columns: &[String]) -> Result<serde_json::Value> {
    let mut row = Map::new();
    for column in columns {
        let value = match column.as_str() {
            "id" => json!(issue.id),
            "title" => json!(issue.title),
            "status" => json!(issue.status.as_str()),
            "type" => json!(issue.kind),
            "priority" => json!(issue.priority),
            "plan" => json!(issue.plan),
            "parent" => json!(issue.parent),
            "block_reason" => json!(issue.block_reason),
            "updated_at" => json!(issue.updated_at),
            "reviewed_at" => json!(issue.reviewed_at),
            other => {
                return Err(ZenError::Validation {
                    field: "columns".into(),
                    why: format!("视图列不支持 {other}"),
                });
            }
        };
        row.insert(column.clone(), value);
    }
    Ok(serde_json::Value::Object(row))
}

/// 可用视图清单：名字、谓词、列集。只凭它就能调 list。
fn views(data_dir: &Path) -> Result<Report> {
    let cfg = config::load(data_dir)?;
    let list: Vec<serde_json::Value> = cfg
        .workflow
        .views
        .iter()
        .map(|v| json!({ "name": v.name, "predicate": v.predicate, "columns": v.columns }))
        .collect();
    Ok(json!({ "views": list }))
}

/// 单条详情。计划支持按节取正文，任务没有分节。
fn show(data_dir: &Path, id: &str, section: Option<&str>) -> Result<Report> {
    let store = Store::open(&data_dir.join(config::DB_FILE))?;
    if domain::parse_id(domain::PLAN_PREFIX, id).is_some() {
        return show_plan(store.conn(), id, section);
    }
    match store::load_issue(store.conn(), id) {
        Ok(issue) => {
            if section.is_some() {
                return Err(ZenError::Validation { field: "section".into(), why: "只有计划有分节正文".into() });
            }
            let stage =
                derive_stage(issue.status, store::last_report(store.conn(), id)?, store::lease_of(store.conn(), id)?.is_some());
            let mut report = to_report(&issue)?;
            report["stage"] = json!(stage.as_str());
            report["comments"] = serde_json::to_value(store::load_comments(store.conn(), id)?)
                .map_err(|e| ZenError::Io(format!("序列化失败：{e}")))?;
            Ok(report)
        }
        // 编号不是任务就按计划标题找：命令行声明了「任务编号或计划编号/标题」；
        // 两边都找不到时一起说，不要让调用方以为 id 一定是计划。
        Err(ZenError::NotFound { .. }) => match show_plan(store.conn(), id, section) {
            Err(ZenError::NotFound { .. }) => Err(ZenError::NotFound { what: format!("任务或计划 {id}") }),
            other => other,
        },
        Err(e) => Err(e),
    }
}

fn show_plan(conn: &rusqlite::Connection, key: &str, section: Option<&str>) -> Result<Report> {
    let plan = store::load_plan(conn, key)?;
    if let Some(section) = section {
        let content = match section {
            "background" => &plan.background,
            "context" => &plan.context,
            other => {
                return Err(ZenError::Validation {
                    field: "section".into(),
                    why: format!("计划只有 background / context，收到 {other}"),
                });
            }
        };
        return Ok(json!({ "id": plan.id, "title": plan.title, "section": section, "content": content }));
    }
    to_report(&plan)
}

fn to_report<T: serde::Serialize>(value: &T) -> Result<Report> {
    serde_json::to_value(value).map_err(|e| ZenError::Io(format!("序列化失败：{e}")))
}

/// 建立数据目录、默认配置与空库。已存在的文件跳过。
fn init(requested: Option<&Path>) -> Result<Report> {
    let cwd = std::env::current_dir()?;
    let root = config::init_root(requested, &cwd)?;
    let written = config::write_defaults(&root)?;
    let data_dir = root.join(config::DATA_DIR);
    let db_path = data_dir.join(config::DB_FILE);
    let store = Store::open(&db_path)?;
    store::init_schema(&store)?;
    store.health()?;
    // init root 与当前工作目录解析出的看板不一致时，后续调用会找不到这块看板——当场说明。
    let warning = match config::find_data_dir(&cwd) {
        Ok(found) if found == data_dir => None,
        _ => Some(format!(
            "当前工作目录 {} 解析不到这块看板；后续 zen_* 调用按会话工作目录向上找 .dsh-zen，请先将会话工作目录切到 {}",
            cwd.display(),
            root.display()
        )),
    };
    Ok(json!({
        "root": root.display().to_string(),
        "data_dir": data_dir.display().to_string(),
        "db": db_path.display().to_string(),
        "written": written
            .iter()
            .map(|p| p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default())
            .collect::<Vec<_>>(),
        "warning": warning,
    }))
}

/// 改任务标题或正文。只有 planner / lead 能改：这是需求方的措辞，不是执行者的。
fn update_issue_meta(data_dir: &Path, id: &str, title: Option<&str>, description: Option<&str>, role: Role, actor: &str) -> Result<Report> {
    if !matches!(role, Role::Planner | Role::Lead) {
        return Err(ZenError::Permission { action: "update".into(), role: format!("{role:?}") });
    }
    if title.is_none() && description.is_none() {
        return Err(ZenError::Validation { field: "title".into(), why: "title 与 description 至少给一个".into() });
    }
    let mut store = Store::open(&data_dir.join(config::DB_FILE))?;
    let at = store.now();
    store.tx(|conn| {
        let mut issue = store::load_issue(conn, id)?;
        if let Some(t) = title {
            let t = t.trim();
            if t.is_empty() {
                return Err(ZenError::Validation { field: "title".into(), why: "不能为空".into() });
            }
            issue.title = t.to_string();
        }
        if let Some(d) = description {
            issue.description = d.to_string();
        }
        issue.updated_at = at.clone();
        let rev = store::update_issue_cas(conn, &issue)?;
        store::append_event(conn, &store::Event {
            subject_type: "issue",
            subject_id: id.to_string(),
            kind: "updated".into(),
            from_state: None,
            to_state: None,
            actor: actor.to_string(),
            role: format!("{role:?}"),
            payload: json!({ "rev": rev }),
        }, &at)?;
        Ok(json!({ "id": issue.id, "rev": rev }))
    })
}

/// 写计划的标题与正文两节（background / context）。同样是 planner / lead 的事。
fn edit_plan(data_dir: &Path, id: &str, title: Option<&str>, background: Option<&str>, context: Option<&str>, role: Role, actor: &str) -> Result<Report> {
    if !matches!(role, Role::Planner | Role::Lead) {
        return Err(ZenError::Permission { action: "plan".into(), role: format!("{role:?}") });
    }
    if title.is_none() && background.is_none() && context.is_none() {
        return Err(ZenError::Validation { field: "background".into(), why: "title / background / context 至少给一个".into() });
    }
    let mut store = Store::open(&data_dir.join(config::DB_FILE))?;
    let at = store.now();
    store.tx(|conn| {
        let plan = store::load_plan(conn, id)?;
        store::update_plan_sections(conn, &plan.id, title, background, context)?;
        store::append_event(conn, &store::Event {
            subject_type: "plan",
            subject_id: plan.id.clone(),
            kind: "updated".into(),
            from_state: None,
            to_state: None,
            actor: actor.to_string(),
            role: format!("{role:?}"),
            payload: json!({}),
        }, &at)?;
        Ok(json!({ "id": plan.id, "title": title.unwrap_or(&plan.title) }))
    })
}

/// 建任务；带计划则建计划或复用它。任务与它的事件在同一事务里落库。
fn create(data_dir: &Path, args: &CreateArgs<'_>) -> Result<Report> {
    let cfg = config::load(data_dir)?;
    let specs = cfg.field_specs(args.kind)?;
    let 空 = Map::new();
    let fields = domain::validate_fields(args.kind, &specs, &coerce_fields(&specs, args.fields.unwrap_or(&空))?)?;

    let title = args.title.trim();
    if title.is_empty() {
        return Err(ZenError::Validation { field: "title".into(), why: "不能为空".into() });
    }

    let mut store = Store::open(&data_dir.join(config::DB_FILE))?;
    let at = store.now();
    store.tx(|conn| {
        let plan_id = match args.plan {
            None => None,
            Some(name) => Some(ensure_plan(conn, name, args.role, args.actor, &at)?),
        };

        let id = domain::next_id(domain::ID_PREFIX, &store::issue_ids(conn, domain::ID_PREFIX)?);
        let issue = Issue {
            id: id.clone(),
            plan: plan_id.clone(),
            kind: args.kind.to_string(),
            title: title.to_string(),
            description: args.description.unwrap_or("").to_string(),
            status: Status::Todo,
            priority: domain::DEFAULT_PRIORITY.to_string(),
            parent: None,
            block_reason: None,
            fields,
            external_key: args.external_key.map(str::to_string),
            claim_actor: None,
            claim_until: None,
            reviewed_at: None,
            rev: 0,
            created_at: at.clone(),
            updated_at: at.clone(),
        };
        match store::insert_issue(conn, &issue) {
            Ok(()) => {}
            // 同外部键已存在：幂等返回已有任务，不再建第二条（R24）
            Err(ZenError::Conflict { actual, .. }) => {
                return Ok(json!({ "id": actual, "plan": plan_id, "status": "existing", "title": title, "existing": true }));
            }
            Err(e) => return Err(e),
        }
        store::insert_acceptance(conn, &id, args.acceptance)?;

        let mut payload = Map::new();
        payload.insert("title".into(), json!(issue.title));
        payload.insert("type".into(), json!(issue.kind));
        store::append_event(
            conn,
            &Event {
                subject_type: "issue",
                subject_id: id.clone(),
                kind: "create".into(),
                from_state: None,
                to_state: Some(Status::Todo.as_str().to_string()),
                actor: args.actor.to_string(),
                role: args.role.as_str().to_string(),
                payload: serde_json::Value::Object(payload),
            },
            &at,
        )?;

        Ok(json!({
            "id": issue.id,
            "title": issue.title,
            "type": issue.kind,
            "status": issue.status.as_str(),
            "plan": plan_id,
        }))
    })
}

/// 命令行只能给字符串，int / bool 字段就地转换；不认识的值原样交给字段校验去报。
fn coerce_fields(specs: &[FieldDef], raw: &Map<String, Value>) -> Result<Map<String, Value>> {
    let mut out = Map::new();
    for (name, value) in raw {
        let coerced = match (specs.iter().find(|s| &s.name == name), value) {
            (Some(spec), Value::String(text)) => match spec.kind {
                FieldKind::Int => Value::from(text.parse::<i64>().map_err(|_| ZenError::Validation {
                    field: name.clone(),
                    why: format!("应为整数，收到 {text}"),
                })?),
                FieldKind::Bool => Value::Bool(match text.as_str() {
                    "true" | "1" => true,
                    "false" | "0" => false,
                    _ => {
                        return Err(ZenError::Validation {
                            field: name.clone(),
                            why: format!("应为布尔，收到 {text}"),
                        });
                    }
                }),
                FieldKind::Str | FieldKind::Enum => value.clone(),
            },
            _ => value.clone(),
        };
        out.insert(name.clone(), coerced);
    }
    Ok(out)
}

/// 同名计划复用，没有才建。
///
/// ponytail: 两个进程同时首建同名计划时，后到的会撞上 title 唯一约束并整体回滚；
///
/// 重试即可，暂不做冲突重读。
fn ensure_plan(conn: &rusqlite::Connection, name: &str, role: Role, actor: &str, at: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ZenError::Validation { field: "plan".into(), why: "不能为空".into() });
    }
    if let Some(id) = store::find_plan(conn, name)? {
        return Ok(id);
    }
    let id = domain::next_id(domain::PLAN_PREFIX, &store::plan_ids(conn, domain::PLAN_PREFIX)?);
    store::insert_plan(conn, &id, name, at)?;
    let mut payload = Map::new();
    payload.insert("title".into(), json!(name));
    store::append_event(
        conn,
        &Event {
            subject_type: "plan",
            subject_id: id.clone(),
            kind: "create".into(),
            from_state: None,
            to_state: None,
            actor: actor.to_string(),
            role: role.as_str().to_string(),
            payload: serde_json::Value::Object(payload),
        },
        at,
    )?;
    Ok(id)
}

/// actor 身份解析：环境变量优先，其次 git 邮箱，最后回退到用户名。
/// 同机多会话并行时必须显式设 `ZEN_ACTOR`，否则会被归为同一身份。
pub fn resolve_actor() -> String {
    if let Ok(v) = std::env::var("ZEN_ACTOR") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    if let Ok(out) = std::process::Command::new("git").args(["config", "user.email"]).output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
    }
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "agent".into());
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "localhost".into());
    format!("{user}@{host}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 工单（context）必须带正文摘要：否则 agent 连任务描述都看不到。
    #[test]
    fn 工单带正文摘要() {
        let dir = 临时仓库("ctx-excerpt");
        let 长文: String = "重".repeat(260);
        let 编号 = create(&dir, &CreateArgs {
            description: Some(&长文),
            ..任务参("甲", Some("P8"))
        })
        .unwrap()["id"].as_str().unwrap().to_string();

        let 包 = context(&dir, &ContextArgs { id: &编号, role: Role::Implementer, actor: "i@x" }).unwrap();
        let 摘要 = &包["description_excerpt"];
        assert_eq!(摘要["text"].as_str().unwrap().chars().count(), 200, "只取前 200 字");
        assert_eq!(摘要["truncated"], true, "超长要标截断");
        assert!(摘要["full"].as_str().unwrap().contains(&编号), "给出取全文的方式");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 正文与计划两节以前没有任何写入口（永远空串），这条盯住写路径。
    #[test]
    fn 正文与计划都能写() {
        let dir = 临时仓库("meta-write");
        let 编号 = create(&dir, &任务参("甲", Some("P7"))).unwrap()["id"].as_str().unwrap().to_string();

        update_issue_meta(&dir, &编号, None, Some("第一版正文"), Role::Planner, "p@x").unwrap();
        assert_eq!(show(&dir, &编号, None).unwrap()["description"], "第一版正文");

        // implementer 不得改需求措辞
        let 拒 = update_issue_meta(&dir, &编号, None, Some("改"), Role::Implementer, "i@x");
        assert_eq!(拒.unwrap_err().code(), "permission");

        // 计划两节
        edit_plan(&dir, "P7", None, Some("背景一"), Some("上下文一"), Role::Planner, "p@x").unwrap();
        let 计划 = show(&dir, "P7", None).unwrap();
        assert_eq!(计划["background"], "背景一");
        assert_eq!(计划["context"], "上下文一");

        // 两节都缺参数应被拒
        assert_eq!(edit_plan(&dir, "P7", None, None, None, Role::Planner, "p@x").unwrap_err().code(), "validation");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 在临时仓库根上跑真实的 init，返回数据目录。
    fn 临时仓库(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("dsh-zen-cmd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        init(Some(&root)).unwrap();
        root.join(config::DATA_DIR)
    }

    fn 任务参<'a>(title: &'a str, plan: Option<&'a str>) -> CreateArgs<'a> {
        CreateArgs {
            title,
            plan,
            kind: "task",
            role: Role::Implementer,
            actor: "a@b",
            acceptance: &[],
            description: None,
            external_key: None,
            fields: None,
        }
    }

    /// 事件条数：写路径必须留痕（R33）。
    fn 事件数(store: &Store, subject: &str, id: &str) -> i64 {
        store
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM event WHERE subject_type = ?1 AND subject_id = ?2",
                [subject, id],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn 不带计划的任务进积压并落事件() {
        let dir = 临时仓库("backlog");
        let report = create(&dir, &任务参("第一条任务", None)).unwrap();
        assert_eq!(report["id"], "ZEN-1");
        assert_eq!(report["plan"], serde_json::Value::Null);
        assert_eq!(report["status"], "todo");

        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        let got = store::load_issue(store.conn(), "ZEN-1").unwrap();
        assert_eq!(got.title, "第一条任务");
        assert_eq!(got.status, Status::Todo);
        assert_eq!(got.priority, "P2", "缺省优先级取自 SRS §9");
        assert!(got.plan.is_none(), "不带 --plan 即进积压");
        assert_eq!(事件数(&store, "issue", "ZEN-1"), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 同名计划只建一次() {
        let dir = 临时仓库("plan");
        let a = create(&dir, &任务参("甲", Some("P1 数据通路"))).unwrap();
        let b = create(&dir, &任务参("乙", Some("P1 数据通路"))).unwrap();
        assert_eq!(a["plan"], b["plan"], "同名计划复用同一条");
        assert_ne!(a["id"], b["id"], "任务 ID 不重复");

        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        let plan_id = a["plan"].as_str().unwrap().to_string();
        assert_eq!(事件数(&store, "plan", &plan_id), 1, "计划只在首次创建时落事件");
        assert_eq!(store::load_issue(store.conn(), a["id"].as_str().unwrap()).unwrap().plan.as_deref(), Some(plan_id.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 未知类型与空标题被拒() {
        let dir = 临时仓库("reject");
        let 未知 = CreateArgs { kind: "epic", ..任务参("甲", None) };
        assert_eq!(create(&dir, &未知).unwrap_err().code(), "validation");
        assert_eq!(create(&dir, &任务参("   ", None)).unwrap_err().code(), "validation");

        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        let n: i64 = store.conn().query_row("SELECT COUNT(*) FROM issue", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "被拒的创建不落任何行");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 连续创建按序号递增() {
        let dir = 临时仓库("seq");
        let ids: Vec<_> = ["甲", "乙", "丙"]
            .iter()
            .map(|t| create(&dir, &任务参(t, None)).unwrap()["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, vec!["ZEN-1", "ZEN-2", "ZEN-3"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn 列表参(view: Option<&str>) -> ListArgs {
        ListArgs { view: view.map(str::to_string), ..ListArgs::default() }
    }

    #[test]
    fn 列表按视图与过滤条件取行() {
        let dir = 临时仓库("list");
        create(&dir, &CreateArgs { kind: "task", ..任务参("甲", None) }).unwrap();
        let 缺陷字段: Map<String, Value> = serde_json::from_str(r#"{"severity":"low"}"#).unwrap();
        create(&dir, &CreateArgs { kind: "bug", fields: Some(&缺陷字段), ..任务参("乙", None) }).unwrap();
        create(&dir, &任务参("丙", Some("P2"))).unwrap();

        let 全部 = list(&dir, &列表参(None)).unwrap();
        assert_eq!(全部["total"], 3);
        assert_eq!(全部["truncated"], false);

        let 只看缺陷 = list(&dir, &ListArgs { kind: Some("bug".into()), ..列表参(None) }).unwrap();
        assert_eq!(只看缺陷["rows"][0]["id"], "ZEN-2", "flags 可组合");

        let 积压 = list(&dir, &列表参(Some("backlog"))).unwrap();
        assert_eq!(积压["total"], 2, "计划外的两条在积压里");

        let 就绪 = list(&dir, &列表参(Some("ready"))).unwrap();
        assert_eq!(就绪["rows"][0]["id"], "ZEN-3", "计划内无阻塞者才就绪");
        let 列: Vec<_> = 就绪["columns"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        assert_eq!(列, vec!["id", "title", "priority", "plan"], "默认列集取自视图定义");
        assert!(就绪["rows"][0].get("description").is_none(), "列表不含正文");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 视图清单与视图名一致() {
        let dir = 临时仓库("views");
        let report = views(&dir).unwrap();
        let names: Vec<_> = report["views"].as_array().unwrap().iter().map(|v| v["name"].as_str().unwrap().to_string()).collect();
        assert_eq!(names, vec!["ready", "mine", "blocked", "backlog", "stale", "review"]);
        for name in &names {
            assert!(list(&dir, &列表参(Some(name))).is_ok(), "清单里的视图必须能直接 list：{name}");
        }
        assert_eq!(list(&dir, &列表参(Some("sprint"))).unwrap_err().code(), "validation");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 单条详情与单节正文() {
        let dir = 临时仓库("show");
        let created = create(&dir, &任务参("甲", Some("P2 收尾"))).unwrap();
        let id = created["id"].as_str().unwrap();
        let plan_id = created["plan"].as_str().unwrap();

        let 详情 = show(&dir, id, None).unwrap();
        assert_eq!(详情["title"], "甲");
        assert_eq!(详情["status"], "todo");

        let 计划 = show(&dir, plan_id, None).unwrap();
        assert_eq!(计划["title"], "P2 收尾");
        assert_eq!(show(&dir, "P2 收尾", None).unwrap()["id"], plan_id, "按标题也能取到计划");
        let 背景 = show(&dir, plan_id, Some("background")).unwrap();
        assert_eq!(背景["section"], "background");
        assert!(背景.get("plan").is_none(), "按节取用时只返回那一节");

        assert_eq!(show(&dir, plan_id, Some("不存在")).unwrap_err().code(), "validation");
        assert_eq!(show(&dir, id, Some("background")).unwrap_err().code(), "validation", "任务没有正文分节");
        let 都没有 = show(&dir, "ZEN-99", None).unwrap_err();
        assert_eq!(都没有.code(), "not_found");
        assert!(都没有.to_string().contains("任务或计划"), "两边都没有时要说清楚：{都没有}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 列表默认预算四十行() {
        let dir = 临时仓库("budget");
        for n in 0..45 {
            create(&dir, &任务参(&format!("第{n}条"), None)).unwrap();
        }
        let report = list(&dir, &列表参(None)).unwrap();
        assert_eq!(report["rows"].as_array().unwrap().len(), 40, "R28：默认最多四十行");
        assert_eq!(report["total"], 45);
        assert_eq!(report["truncated"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn 流转参<'a>(id: &'a str, to: Status) -> MoveArgs<'a> {
        MoveArgs { id, to, role: Role::Implementer, actor: "a@b", extend: false, reason: None, clear_block: false }
    }

    /// 直接落一条验收项与产物：T4.1 的行操作命令尚未存在，这里造事实。
    fn 备齐完成凭据(store: &Store, id: &str) {
        let at = "2026-01-01T00:00:00.000Z";
        store
            .conn()
            .execute(
                "INSERT INTO acceptance (issue_id, ordinal, text, checked_at, checked_by) VALUES (?1, 1, '跑通', ?2, 'a@b')",
                [id, at],
            )
            .unwrap();
        store
            .conn()
            .execute("INSERT INTO artifact (issue_id, kind, \"ref\", at) VALUES (?1, 'commit', 'abc1234', ?2)", [id, at])
            .unwrap();
    }

    fn 状态(store: &Store, id: &str) -> (String, Option<String>) {
        let issue = store::load_issue(store.conn(), id).unwrap();
        (issue.status.as_str().to_string(), issue.claim_actor)
    }

    #[test]
    fn 状态流转全链路并留痕() {
        let dir = 临时仓库("move");
        let id = create(&dir, &任务参("走完一条", Some("P3"))).unwrap()["id"].as_str().unwrap().to_string();

        let 入列 = move_issue(&dir, &流转参(&id, Status::Wip)).unwrap();
        assert_eq!(入列["status"], "wip");
        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        let (当前状态, 当前持有者) = 状态(&store, &id);
        assert_eq!(当前状态, "wip");
        assert_eq!(当前持有者.as_deref(), Some("a@b"), "进入 wip 即获得租约");
        assert_eq!(事件数(&store, "issue", &id), 2);

        let e = move_issue(&dir, &流转参(&id, Status::Done)).unwrap_err();
        assert_eq!(e.code(), "guard", "缺验收与产物时不许完成");
        assert!(e.to_string().contains("验收标准"), "{e}");

        备齐完成凭据(&store, &id);
        let 完成 = move_issue(&dir, &流转参(&id, Status::Done)).unwrap();
        assert_eq!(完成["status"], "done");
        let (当前状态, 当前持有者) = 状态(&store, &id);
        assert_eq!(当前状态, "done");
        assert_eq!(当前持有者, None, "离开 wip 一律释放租约");
        assert_eq!(事件数(&store, "issue", &id), 3, "建、入列、完成各一条事件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 越权与终态被拒() {
        let dir = 临时仓库("move-deny");
        let id = create(&dir, &任务参("甲", None)).unwrap()["id"].as_str().unwrap().to_string();
        move_issue(&dir, &流转参(&id, Status::Wip)).unwrap();

        let 越权 = MoveArgs { role: Role::Reviewer, ..流转参(&id, Status::Done) };
        let e = move_issue(&dir, &越权).unwrap_err();
        assert_eq!(e.code(), "permission");
        assert!(e.to_string().contains("wip→done"), "{e}");

        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        备齐完成凭据(&store, &id);
        move_issue(&dir, &流转参(&id, Status::Done)).unwrap();

        let 终态 = move_issue(&dir, &流转参(&id, Status::Wip)).unwrap_err();
        assert_eq!(终态.code(), "permission", "终态只读：这一格不在矩阵里");
        assert_eq!(状态(&store, &id).0, "done", "被拒的转移不落库");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 续期只延长租约不改状态() {
        let dir = 临时仓库("extend");
        let id = create(&dir, &任务参("甲", None)).unwrap()["id"].as_str().unwrap().to_string();
        move_issue(&dir, &流转参(&id, Status::Wip)).unwrap();

        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        let 旧到期 = store::lease_of(store.conn(), &id).unwrap().unwrap().1;
        let 续 = move_issue(&dir, &MoveArgs { extend: true, ..流转参(&id, Status::Wip) }).unwrap();
        let 新到期 = 续["claim_until"].as_str().unwrap().to_string();
        assert!(新到期 >= 旧到期, "续期不缩短：{旧到期} → {新到期}");
        assert_eq!(续["claim_actor"], "a@b", "续期保持持有者");
        assert_eq!(状态(&store, &id).0, "wip", "续期不改状态");
        assert_eq!(事件数(&store, "issue", &id), 3, "续期也留痕");

        let 待办续期 = create(&dir, &任务参("乙", None)).unwrap()["id"].as_str().unwrap().to_string();
        assert_eq!(
            move_issue(&dir, &MoveArgs { extend: true, ..流转参(&待办续期, Status::Wip) }).unwrap_err().code(),
            "validation",
            "不在 wip 的任务无从续期"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 阻塞要写原因且清空后才能被重新领取() {
        let dir = 临时仓库("blocked");
        let id = create(&dir, &任务参("甲", None)).unwrap()["id"].as_str().unwrap().to_string();
        move_issue(&dir, &流转参(&id, Status::Wip)).unwrap();

        let 无因 = MoveArgs { reason: None, ..流转参(&id, Status::Blocked) };
        assert_eq!(move_issue(&dir, &无因).unwrap_err().code(), "validation", "进入 blocked 必须说明原因");

        let 有因 = MoveArgs { reason: Some("等上游确认接口"), ..流转参(&id, Status::Blocked) };
        assert_eq!(move_issue(&dir, &有因).unwrap()["status"], "blocked");

        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        let 受阻 = store::load_issue(store.conn(), &id).unwrap();
        assert_eq!(受阻.claim_actor, None, "主动受阻立即释放租约");
        assert_eq!(受阻.block_reason.as_deref(), Some("等上游确认接口"));

        assert_eq!(move_issue(&dir, &流转参(&id, Status::Wip)).unwrap_err().code(), "guard", "原因没清就领不回来");

        let 领回 = MoveArgs { clear_block: true, ..流转参(&id, Status::Wip) };
        assert_eq!(move_issue(&dir, &领回).unwrap()["status"], "wip");
        let 领回后 = store::load_issue(store.conn(), &id).unwrap();
        assert_eq!(领回后.block_reason, None, "清空要落库");
        assert_eq!(领回后.claim_actor.as_deref(), Some("a@b"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 离开wip必释放租约不依赖配置() {
        let dir = 临时仓库("lease-rule");
        // 删掉配置里所有的 release_lease 效果：§3.2 说「一律释放」，这件事不该由配置决定
        let path = dir.join(config::CONFIG_FILES[2]);
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("effects = [\"release_lease\"]\n", "")).unwrap();

        let id = create(&dir, &任务参("甲", None)).unwrap()["id"].as_str().unwrap().to_string();
        move_issue(&dir, &流转参(&id, Status::Wip)).unwrap();
        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        备齐完成凭据(&store, &id);
        assert_eq!(move_issue(&dir, &流转参(&id, Status::Done)).unwrap()["status"], "done");

        let 完成 = store::load_issue(store.conn(), &id).unwrap();
        assert_eq!(完成.claim_actor, None, "没配 effect 也要清租约");
        assert_eq!(完成.claim_until, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 真实凭据走通完成且终态仍可评论挂产物() {
        let dir = 临时仓库("t42");
        let 验收 = vec!["跑通甲".to_string()];
        let created = create(&dir, &CreateArgs { acceptance: &验收, ..任务参("甲", None) }).unwrap();
        let id = created["id"].as_str().unwrap().to_string();
        move_issue(&dir, &流转参(&id, Status::Wip)).unwrap();

        let 未备齐 = move_issue(&dir, &流转参(&id, Status::Done)).unwrap_err();
        assert_eq!(未备齐.code(), "guard", "验收还没勾完");

        let 勾 = accept(&dir, &AcceptArgs { id: &id, ordinal: 1, role: Role::Implementer, actor: "a@b" }).unwrap();
        assert_eq!(勾["ordinal"], 1);
        assert_eq!(move_issue(&dir, &流转参(&id, Status::Done)).unwrap_err().code(), "guard", "还缺产物");

        artifact(&dir, &ArtifactArgs { id: &id, kind: "commit", reference: "abc1234", role: Role::Implementer, actor: "a@b" }).unwrap();
        assert_eq!(move_issue(&dir, &流转参(&id, Status::Done)).unwrap()["status"], "done");

        // R14：终态仍可评论与挂产物
        assert_eq!(comment(&dir, &CommentArgs { id: &id, body: "验收过了，补一条说明", role: Role::Reviewer, actor: "c@d" }).unwrap()["id"], id);
        artifact(&dir, &ArtifactArgs { id: &id, kind: "note", reference: "完成说明：视图谓词回到 domain，store 只留列条件", role: Role::Reviewer, actor: "c@d" }).unwrap();

        assert_eq!(move_issue(&dir, &流转参(&id, Status::Wip)).unwrap_err().code(), "permission", "终态不可再流转");
        let 终态勾选 = accept(&dir, &AcceptArgs { id: &id, ordinal: 1, role: Role::Lead, actor: "c@d" }).unwrap_err();
        assert_eq!(终态勾选.code(), "guard", "§3.5：验收标准不在终态可写例外里");

        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        assert_eq!(store::load_comments(store.conn(), &id).unwrap().len(), 1);
        assert_eq!(store::load_artifacts(store.conn(), &id).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 勾选验收受角色与持有者约束() {
        let dir = 临时仓库("t42-role");
        let 验收 = vec!["跑通".to_string()];
        let id = create(&dir, &CreateArgs { acceptance: &验收, ..任务参("甲", None) }).unwrap()["id"].as_str().unwrap().to_string();
        move_issue(&dir, &流转参(&id, Status::Wip)).unwrap();

        let 规划者 = AcceptArgs { id: &id, ordinal: 1, role: Role::Planner, actor: "a@b" };
        assert_eq!(accept(&dir, &规划者).unwrap_err().code(), "permission");
        let 非持有者 = AcceptArgs { id: &id, ordinal: 1, role: Role::Implementer, actor: "c@d" };
        assert_eq!(accept(&dir, &非持有者).unwrap_err().code(), "permission", "implementer 只有持有者能勾");
        accept(&dir, &AcceptArgs { id: &id, ordinal: 1, role: Role::Lead, actor: "c@d" }).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 产物与评论入参校验() {
        let dir = 临时仓库("t42-bad");
        let id = create(&dir, &任务参("甲", None)).unwrap()["id"].as_str().unwrap().to_string();
        let 参 = |kind: &'static str, reference: &'static str| ArtifactArgs { id: &id, kind, reference, role: Role::Implementer, actor: "a@b" };

        assert_eq!(artifact(&dir, &参("note", "太短")).unwrap_err().code(), "validation", "note 要够长");
        assert_eq!(artifact(&dir, &参("file", "不存在的路径")).unwrap_err().code(), "validation", "file 要能解析到");
        assert_eq!(artifact(&dir, &参("video", "x")).unwrap_err().code(), "validation");
        assert_eq!(comment(&dir, &CommentArgs { id: &id, body: "  ", role: Role::Implementer, actor: "a@b" }).unwrap_err().code(), "validation");
        assert_eq!(artifact(&dir, &ArtifactArgs { id: "ZEN-404", ..参("commit", "abc") }).unwrap_err().code(), "not_found");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 报告驱动派生阶段且不改状态() {
        let dir = 临时仓库("report");
        let id = create(&dir, &任务参("甲", None)).unwrap()["id"].as_str().unwrap().to_string();
        move_issue(&dir, &流转参(&id, Status::Wip)).unwrap();
        assert_eq!(show(&dir, &id, None).unwrap()["stage"], "working", "有租约即工作中");

        let 缺类型 = ReportArgs { id: &id, kind: "", note: Some("随便"), role: Role::Implementer, actor: "a@b" };
        assert_eq!(report(&dir, &缺类型).unwrap_err().code(), "validation", "R29：缺报告类型要拒");

        report(&dir, &ReportArgs { id: &id, kind: "waiting", note: Some("等接口确认"), role: Role::Implementer, actor: "a@b" }).unwrap();
        let 详情 = show(&dir, &id, None).unwrap();
        assert_eq!(详情["stage"], "working", "租约还在，工作中有优先级");
        assert_eq!(详情["status"], "wip", "报告不改状态");

        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        store::release_lease(store.conn(), &id, "2026-01-01T00:00:00.000Z").unwrap();
        assert_eq!(show(&dir, &id, None).unwrap()["stage"], "waiting_for_person");

        report(&dir, &ReportArgs { id: &id, kind: "ready", note: None, role: Role::Implementer, actor: "a@b" }).unwrap();
        assert_eq!(show(&dir, &id, None).unwrap()["stage"], "waiting_for_claim", "最近一条报告说了算");
        assert_eq!(事件数(&store, "issue", &id), 4, "建、入列、两条报告各一条事件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 最小上下文按角色切片且含四项必需() {
        let dir = 临时仓库("context");
        let 甲 = create(&dir, &CreateArgs { acceptance: &["跑通".into()], ..任务参("甲", Some("P5")) }).unwrap();
        let id = 甲["id"].as_str().unwrap().to_string();
        let 乙 = create(&dir, &任务参("乙", Some("P5"))).unwrap();
        let 乙号 = 乙["id"].as_str().unwrap().to_string();

        // 甲 依赖 乙：直接落一条 blocks（link 命令尚未实现，这里造事实）
        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        store.conn().execute("INSERT INTO link (from_id, to_id, kind) VALUES (?1, ?2, 'blocks')", [&乙号, &id]).unwrap();

        let 实施 = context(&dir, &ContextArgs { id: &id, role: Role::Implementer, actor: "a@b" }).unwrap();
        assert_eq!(实施["id"], id);
        assert_eq!(实施["plan"]["title"], "P5", "计划摘要要在");
        assert_eq!(实施["deps"][0]["id"], 乙号, "直接依赖的状态要在");
        assert_eq!(实施["deps"][0]["status"], "todo");
        assert_eq!(实施["acceptance"][0]["text"], "跑通", "验收标准要在");
        assert_eq!(实施["links"][0]["kind"], "blocks", "相关关联要在");
        assert!(实施.get("artifacts").is_some(), "implementer 额外要看产物");
        assert!(实施.get("description").is_none(), "不带正文，保持最小");
        assert!(实施.get("plan_tasks").is_none(), "不含同计划的其他任务");

        let 规划 = context(&dir, &ContextArgs { id: &id, role: Role::Planner, actor: "a@b" }).unwrap();
        assert_ne!(实施, 规划, "三个角色取同一任务，输出要不同");
        assert!(规划.get("artifacts").is_none(), "planner 不背产物");

        // 配置里没有这个角色的上下文包：拒绝，不回落全量
        let path = dir.join(config::CONFIG_FILES[3]);
        let text = std::fs::read_to_string(&path).unwrap();
        let 去掉 = text.lines().filter(|l| !l.trim_start().starts_with("tester = ")).collect::<Vec<_>>().join("\n");
        std::fs::write(&path, 去掉).unwrap();
        assert_eq!(
            context(&dir, &ContextArgs { id: &id, role: Role::Tester, actor: "a@b" }).unwrap_err().code(),
            "validation",
            "没有上下文包的角色要拒绝，不回落全量"
        );
        assert_eq!(
            context(&dir, &ContextArgs { id: "ZEN-404", role: Role::Lead, actor: "a@b" }).unwrap_err().code(),
            "not_found"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn schema可解析且没有计划状态() {
        let dir = 临时仓库("schema");
        let s = schema(&dir).unwrap();
        assert_eq!(s["schema_version"], 1);
        assert_eq!(s["statuses"].as_array().unwrap().len(), 5);
        assert_eq!(s["priorities"], serde_json::json!(["P0", "P1", "P2", "P3"]));
        let types = s["types"].as_array().unwrap();
        assert_eq!(types.len(), 4);
        let bug = types.iter().find(|t| t["name"] == "bug").unwrap();
        assert_eq!(bug["fields"][0]["name"], "severity");
        assert_eq!(bug["fields"][0]["required"], true, "字段规则要能读到");
        let 计划字段 = s["plan"]["fields"].as_array().unwrap();
        assert!(!计划字段.iter().any(|f| f == "status"), "R2：计划没有状态字段");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn workflow输出转移视图与阈值() {
        let dir = 临时仓库("workflow");
        let w = workflow(&dir).unwrap();
        let 转移 = w["transitions"].as_array().unwrap();
        assert_eq!(转移.len(), 6);
        let 完成 = 转移.iter().find(|t| t["from"] == "wip" && t["to"] == "done").unwrap();
        assert_eq!(完成["guards"], serde_json::json!(["acceptance_all_checked", "artifact_present"]));
        assert_eq!(完成["effects"], serde_json::json!(["release_lease"]));
        assert_eq!(w["views"].as_array().unwrap().len(), 6);
        assert_eq!(w["tunables"]["lease_ttl_minutes"], 30);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn roles只输出当前角色行() {
        let dir = 临时仓库("roles");
        let r = roles(&dir, Role::Planner).unwrap();
        assert!(r["role"]["duty"].as_str().unwrap().contains("计划"));
        assert_eq!(r["context_sections"][0], "id");
        let 可发 = r["can_transitions"].as_array().unwrap();
        assert!(可发.iter().any(|t| t["transition"] == "todo→wip" && t["kind"] == "direct"));
        assert!(可发.iter().any(|t| t["transition"] == "blocked→wip" && t["kind"] == "direct"));
        assert!(!可发.iter().any(|t| t["transition"] == "wip→done"), "planner 在这一格是空的");
        assert!(r.get("roles").is_none(), "只输出当前角色行，不列别人");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 六类坏数据全部报出且只报不修() {
        let dir = 临时仓库("check");
        let 甲 = create(&dir, &CreateArgs { acceptance: &["跑通".into()], ..任务参("甲", Some("P6")) }).unwrap();
        let id = 甲["id"].as_str().unwrap().to_string();
        let 乙 = create(&dir, &任务参("乙", Some("P6"))).unwrap()["id"].as_str().unwrap().to_string();

        // 人为制造六类坏数据
        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        let c = store.conn();
        c.execute("UPDATE issue SET status = 'doing', priority = 'P9' WHERE id = ?1", [&id]).unwrap();       // 结构
        c.execute("UPDATE issue SET claim_actor = 'ghost', claim_until = '2020-01-01T00:00:00.000Z' WHERE id = ?1", [&乙]).unwrap(); // 过期租约
        c.execute("INSERT INTO link (from_id, to_id, kind) VALUES (?1, ?2, 'blocks')", [&id, &乙]).unwrap(); // 环
        c.execute("INSERT INTO link (from_id, to_id, kind) VALUES (?1, ?2, 'blocks')", [&乙, &id]).unwrap();
        c.execute("INSERT INTO artifact (issue_id, kind, \"ref\", at) VALUES (?1, 'note', '短', '2026-01-01T00:00:00.000Z')", [&id]).unwrap(); // 产物不可用
        c.pragma_update(None, "foreign_keys", "OFF").unwrap();
        c.execute("INSERT INTO comment (issue_id, actor, body, at) VALUES ('ZEN-404', 'a@b', '孤儿评论', '2026-01-01T00:00:00.000Z')", []).unwrap(); // 断引用

        // 配置：类型绑定了一个不存在的字段
        let 类型文件 = dir.join(config::CONFIG_FILES[0]);
        let 文本 = std::fs::read_to_string(&类型文件).unwrap().replace(r#"fields = ["severity"]"#, r#"fields = ["severity", "nope"]"#);
        std::fs::write(&类型文件, 文本).unwrap();

        let 报告 = check(&dir).unwrap();
        let 种类: Vec<&str> = 报告["findings"].as_array().unwrap().iter().map(|f| f["kind"].as_str().unwrap()).collect();
        for 期待 in ["structure", "config", "dangling", "cycle", "lease", "artifact"] {
            assert!(种类.contains(&期待), "六类都要报出，缺 {期待}：{种类:?}");
        }
        assert_eq!(报告["ok"], false);
        assert!(报告["findings"].as_array().unwrap().iter().all(|f| f["target"].is_string() && f["detail"].is_string()), "每条都要能定位");

        // 只报不修：坏数据仍在
        let 仍坏: String = store.conn().query_row("SELECT status FROM issue WHERE id = ?1", [&id], |r| r.get(0)).unwrap();
        assert_eq!(仍坏, "doing", "check 不改数据");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 干净库检查通过() {
        let dir = 临时仓库("check-ok");
        create(&dir, &任务参("甲", Some("P6"))).unwrap();
        let 报告 = check(&dir).unwrap();
        assert_eq!(报告["ok"], true);
        assert_eq!(报告["findings"].as_array().unwrap().len(), 0, "干净库不该有发现：{}", 报告);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 存量字段迁移可预演且幂等() {
        let dir = 临时仓库("migrate");
        let 缺陷字段: Map<String, Value> = serde_json::from_str(r#"{"severity":"low"}"#).unwrap();
        let id = create(&dir, &CreateArgs { kind: "bug", fields: Some(&缺陷字段), ..任务参("甲", None) }).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        store
            .conn()
            .execute(
                "UPDATE issue SET fields = ?2 WHERE id = ?1",
                [&id, r#"{"severity":"critical","junk":1}"#],
            )
            .unwrap();

        let 预演 = migrate(&dir, true).unwrap();
        assert_eq!(预演["dry_run"], true);
        assert_eq!(预演["changed"][0]["id"], id);
        assert_eq!(预演["changed"][0]["dropped"].as_array().unwrap().len(), 2, "丢未知键与坏枚举");
        let 未改: String = store.conn().query_row("SELECT fields FROM issue WHERE id = ?1", [&id], |r| r.get(0)).unwrap();
        assert!(未改.contains("critical"), "预演不落库：{未改}");

        let 执行 = migrate(&dir, false).unwrap();
        assert_eq!(执行["dry_run"], false);
        let 迁移后: String = store.conn().query_row("SELECT fields FROM issue WHERE id = ?1", [&id], |r| r.get(0)).unwrap();
        assert_eq!(迁移后, "{}", "坏值全部丢弃后为空对象");
        assert!(
            执行["unresolved"][0]["fields"].as_array().unwrap().contains(&serde_json::json!("severity")),
            "必填且无缺省的要持续标出来，等人补"
        );

        let 再来 = migrate(&dir, false).unwrap();
        assert_eq!(再来["changed"].as_array().unwrap().len(), 0, "连跑两次结果相同：第二次没有可改的");
        assert_eq!(check(&dir).unwrap()["ok"], true, "迁移后 check 全绿");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 看板与统计数字对得上() {
        let dir = 临时仓库("board");
        let 甲 = create(&dir, &任务参("甲", None)).unwrap()["id"].as_str().unwrap().to_string();
        let 缺陷字段: Map<String, Value> = serde_json::from_str(r#"{"severity":"low"}"#).unwrap();
        create(&dir, &CreateArgs { kind: "bug", fields: Some(&缺陷字段), ..任务参("乙", None) }).unwrap();
        move_issue(&dir, &流转参(&甲, Status::Wip)).unwrap();

        let b = board(&dir).unwrap();
        let 文本 = b["text"].as_str().unwrap();
        assert!(文本.contains("wip（1）"), "看板要按状态分组：{文本}");
        assert!(文本.contains(&甲), "看板要列编号");
        assert!(文本.contains("@a@b"), "看板要显示持有者");
        assert_eq!(b["counts"]["wip"], 1);

        // 终态任务不该被算成「未占用」：它根本不可领取
        let 丙 = create(&dir, &任务参("丙", None)).unwrap()["id"].as_str().unwrap().to_string();
        move_issue(&dir, &MoveArgs { to: Status::Dropped, role: Role::Planner, ..流转参(&丙, Status::Dropped) })
            .unwrap();

        let s = stats(&dir).unwrap();
        assert_eq!(s["total"], 3);
        assert_eq!(s["by_status"]["dropped"], 1, "终态只进 by_status，不进 unclaimed");
        assert_eq!(s["by_status"]["wip"], 1);
        assert_eq!(s["by_status"]["todo"], 1);
        assert_eq!(s["by_type"]["bug"], 1);
        assert_eq!(s["by_priority"]["P2"], 3);
        assert_eq!(s["unclaimed"], 1, "只有没租约且非终态的那条算未占用");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 建任务时写入并校验自定义字段() {
        let dir = 临时仓库("fields");
        let 字段 = |s: &str| -> Map<String, Value> { serde_json::from_str(s).unwrap() };

        let 未声明 = create(&dir, &CreateArgs { kind: "bug", fields: Some(&字段(r#"{"nope":"x"}"#)), ..任务参("甲", None) })
            .unwrap_err();
        assert_eq!(未声明.code(), "validation");
        assert!(未声明.to_string().contains("nope"), "要定位到字段：{未声明}");

        let 越界 = create(&dir, &CreateArgs { kind: "bug", fields: Some(&字段(r#"{"severity":"critical"}"#)), ..任务参("甲", None) })
            .unwrap_err();
        assert!(越界.to_string().contains("critical"), "{越界}");

        let 类型错 = create(&dir, &CreateArgs { kind: "bug", fields: Some(&字段(r#"{"severity":3}"#)), ..任务参("甲", None) })
            .unwrap_err();
        assert!(类型错.to_string().contains("severity"), "{类型错}");

        let 缺必填 = create(&dir, &CreateArgs { kind: "bug", ..任务参("甲", None) }).unwrap_err();
        assert!(缺必填.to_string().contains("severity"), "R6：缺必填要拒：{缺必填}");

        let id = create(&dir, &CreateArgs { kind: "bug", fields: Some(&字段(r#"{"severity":"high"}"#)), ..任务参("甲", None) })
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        let issue = store::load_issue(store.conn(), &id).unwrap();
        assert_eq!(issue.fields.get("severity").and_then(|v| v.as_str()), Some("high"), "字段要落库");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 未占用口径只有一处() {
        let dir = 临时仓库("unclaimed");
        let 甲 = create(&dir, &任务参("甲", None)).unwrap()["id"].as_str().unwrap().to_string();
        let 乙 = create(&dir, &任务参("乙", None)).unwrap()["id"].as_str().unwrap().to_string();
        let 丙 = create(&dir, &任务参("丙", None)).unwrap()["id"].as_str().unwrap().to_string();
        move_issue(&dir, &流转参(&乙, Status::Wip)).unwrap();
        move_issue(&dir, &MoveArgs { to: Status::Dropped, role: Role::Planner, ..流转参(&丙, Status::Dropped) }).unwrap();

        let 过滤 = list(&dir, &ListArgs { unclaimed: true, ..列表参(None) }).unwrap();
        let ids: Vec<&str> = 过滤["rows"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec![甲.as_str()], "有租约的不算，终态的也不算");

        assert_eq!(stats(&dir).unwrap()["unclaimed"], 1, "列表与统计同口径");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 重开终态并留痕() {
        let dir = 临时仓库("reopen");
        let 验收 = vec!["跑通".to_string()];
        let id = create(&dir, &CreateArgs { acceptance: &验收, ..任务参("甲", None) }).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        move_issue(&dir, &流转参(&id, Status::Wip)).unwrap();
        accept(&dir, &AcceptArgs { id: &id, ordinal: 1, role: Role::Implementer, actor: "a@b" }).unwrap();
        artifact(&dir, &ArtifactArgs { id: &id, kind: "commit", reference: "abc1234", role: Role::Implementer, actor: "a@b" }).unwrap();
        move_issue(&dir, &流转参(&id, Status::Done)).unwrap();

        let 越权 = ReopenArgs { id: &id, role: Role::Implementer, actor: "a@b" };
        assert_eq!(reopen(&dir, &越权).unwrap_err().code(), "permission", "§4.2 / §5：重开只给 lead");

        let 重开 = reopen(&dir, &ReopenArgs { id: &id, role: Role::Lead, actor: "l@x" }).unwrap();
        assert_eq!(重开["from"], "done");
        assert_eq!(重开["status"], "todo");
        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        assert_eq!(store::load_issue(store.conn(), &id).unwrap().status, Status::Todo);
        let 事件 = store::load_events(store.conn(), Some(&id), 10).unwrap();
        assert_eq!(事件.last().unwrap()["kind"], "reopen", "重开要落事件");
        assert_eq!(事件.last().unwrap()["from"], "done");

        assert_eq!(
            reopen(&dir, &ReopenArgs { id: &id, role: Role::Lead, actor: "l@x" }).unwrap_err().code(),
            "validation",
            "非终态不能再重开"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 事件回放按序且限量() {
        let dir = 临时仓库("log");
        let id = create(&dir, &任务参("甲", None)).unwrap()["id"].as_str().unwrap().to_string();
        move_issue(&dir, &流转参(&id, Status::Wip)).unwrap();
        comment(&dir, &CommentArgs { id: &id, body: "一句", role: Role::Implementer, actor: "a@b" }).unwrap();

        let 全部 = log(&dir, &LogArgs { id: Some(&id), limit: 40 }).unwrap();
        let kinds: Vec<&str> = 全部["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert_eq!(kinds, vec!["create", "status_change", "comment"], "按 seq 升序");
        let seqs: Vec<i64> = 全部["events"].as_array().unwrap().iter().map(|e| e["seq"].as_i64().unwrap()).collect();
        assert!(seqs.windows(2).all(|w| w[0] < w[1]), "seq 必须递增：{seqs:?}");

        let 限量 = log(&dir, &LogArgs { id: None, limit: 2 }).unwrap();
        let 末两条: Vec<&str> = 限量["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
        assert_eq!(末两条, vec!["status_change", "comment"], "限量时取最近的几条，仍按升序");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 导入计划文档且幂等() {
        let dir = 临时仓库("import");
        let 文档 = dir.join("示例计划.md");
        std::fs::write(
            &文档,
            "# P9 示例计划\n\n## 快速检查表\n\n| # | 任务 | 文件 | 状态 |\n|---|---|---|---|\n| T9.1 | 甲任务 | a.rs | [TODO] |\n| T9.2 | 乙任务 | b.rs | [DONE] |\n| T9.3 | 丙任务 | c.rs | [WIP] |\n| T9.4 | 丁任务 | d.rs | [DEPRECATED] |\n",
        )
        .unwrap();

        let r = import(&dir, &文档, false, "a@b").unwrap();
        assert_eq!(r["imported"], 4, "四行都要建出来");
        assert_eq!(r["dropped"], 1, "DEPRECATED 的直接置为 dropped");
        assert_eq!(r["items"][0]["source_status"], "todo");

        let 全部 = list(&dir, &列表参(None)).unwrap();
        assert_eq!(全部["total"], 4, "计数与原文逐条对齐");
        let 丢弃: i64 = 全部["rows"].as_array().unwrap().iter().filter(|r| r["status"] == "dropped").count() as i64;
        assert_eq!(丢弃, 1);

        // 再导一次：靠 external_key 幂等，不重复建
        let 再导 = import(&dir, &文档, false, "a@b").unwrap();
        assert_eq!(再导["imported"], 0);
        assert_eq!(再导["existing"], 4);
        assert_eq!(list(&dir, &列表参(None)).unwrap()["total"], 4, "重复导入不产生新任务");

        // dry_run 不落库
        let 空 = 临时仓库("import-dry");
        let 预 = import(&空, &文档, true, "a@b").unwrap();
        assert_eq!(预["imported"], 4);
        assert_eq!(list(&空, &列表参(None)).unwrap()["total"], 0, "预演不写库");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&空);
    }

    fn 关联参<'a>(from: &'a str, to: &'a str, kind: &'a str) -> LinkArgs<'a> {
        LinkArgs { from, to, kind, role: Role::Planner, actor: "a@b" }
    }

    fn 关联数(store: &Store) -> i64 {
        store.conn().query_row("SELECT COUNT(*) FROM link", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn 三类关联且重复建不重复() {
        let dir = 临时仓库("link");
        let 甲 = create(&dir, &任务参("甲", None)).unwrap()["id"].as_str().unwrap().to_string();
        let 乙 = create(&dir, &任务参("乙", None)).unwrap()["id"].as_str().unwrap().to_string();

        let 依赖 = link_cmd(&dir, &关联参(&甲, &乙, "blocks")).unwrap();
        assert_eq!(依赖["kind"], "blocks");
        assert_eq!(依赖["existing"], false);

        let 重复 = link_cmd(&dir, &关联参(&甲, &乙, "blocks")).unwrap();
        assert_eq!(重复["existing"], true, "同一条重复建返回已有，不报错");

        // duplicates 对称归一化：反向建同一条要合并
        let 反 = link_cmd(&dir, &LinkArgs { role: Role::Implementer, ..关联参(&甲, &乙, "duplicates") }).unwrap();
        let 归一 = link_cmd(&dir, &LinkArgs { role: Role::Implementer, ..关联参(&乙, &甲, "duplicates") }).unwrap();
        assert_eq!(归一["existing"], true, "duplicates 反向建同一条要合并");
        assert_eq!(归一["from"], 反["from"], "归一后两端顺序固定");

        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        assert_eq!(关联数(&store), 2, "blocks 一条 + duplicates 一条");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn blocks仅授权角色可建且成环被拒() {
        let dir = 临时仓库("link-perm");
        let 甲 = create(&dir, &任务参("甲", None)).unwrap()["id"].as_str().unwrap().to_string();
        let 乙 = create(&dir, &任务参("乙", None)).unwrap()["id"].as_str().unwrap().to_string();

        let 越权 = LinkArgs { role: Role::Implementer, ..关联参(&甲, &乙, "blocks") };
        assert_eq!(link_cmd(&dir, &越权).unwrap_err().code(), "permission", "§4.2：blocks 只给 planner / lead");

        link_cmd(&dir, &关联参(&甲, &乙, "blocks")).unwrap();
        let 成环 = link_cmd(&dir, &关联参(&乙, &甲, "blocks")).unwrap_err();
        assert_eq!(成环.code(), "validation");
        assert!(成环.to_string().contains(&甲) && 成环.to_string().contains(&乙), "要报环上的编号：{成环}");

        assert_eq!(link_cmd(&dir, &关联参(&甲, &甲, "blocks")).unwrap_err().code(), "validation", "不能自连");
        assert_eq!(link_cmd(&dir, &关联参(&甲, "ZEN-404", "blocks")).unwrap_err().code(), "not_found");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 定期复盘只建一次() {
        let dir = 临时仓库("review");
        let 验收 = vec!["跑通".to_string()];
        let id = create(&dir, &CreateArgs { acceptance: &验收, ..任务参("甲", Some("P10")) }).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string();
        move_issue(&dir, &流转参(&id, Status::Wip)).unwrap();
        accept(&dir, &AcceptArgs { id: &id, ordinal: 1, role: Role::Implementer, actor: "a@b" }).unwrap();
        artifact(&dir, &ArtifactArgs { id: &id, kind: "commit", reference: "abc1234", role: Role::Implementer, actor: "a@b" }).unwrap();
        move_issue(&dir, &流转参(&id, Status::Done)).unwrap();

        let 越权 = ReviewArgs { role: Role::Implementer, actor: "a@b", dry_run: false };
        assert_eq!(review(&dir, &越权).unwrap_err().code(), "permission", "§4.2：定期 review 只给 reviewer / lead");

        let 第一次 = review(&dir, &ReviewArgs { role: Role::Reviewer, actor: "r@x", dry_run: false }).unwrap();
        assert_eq!(第一次["reviewed"], 1);
        assert_eq!(第一次["created"][0]["source"], id, "复盘任务要指出源任务");
        let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
        let 原 = store::load_issue(store.conn(), &id).unwrap();
        assert!(原.reviewed_at.is_some(), "原任务要打上复盘时间");
        let 复盘号 = 第一次["created"][0]["id"].as_str().unwrap().to_string();
        let 复盘任务 = store::load_issue(store.conn(), &复盘号).unwrap();
        assert_eq!(复盘任务.status, Status::Todo);
        assert!(复盘任务.title.contains(&id), "标题要点出源任务：{}", 复盘任务.title);

        let 第二次 = review(&dir, &ReviewArgs { role: Role::Lead, actor: "l@x", dry_run: false }).unwrap();
        assert_eq!(第二次["reviewed"], 0, "连跑两次第二次不重复建");
        assert_eq!(list(&dir, &列表参(None)).unwrap()["total"], 2, "只有原任务与一条复盘任务");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn 一次流转只改目标字段() {
        let dir = 临时仓库("point-write");
        let 甲 = create(&dir, &任务参("甲", None)).unwrap()["id"].as_str().unwrap().to_string();
        let 乙 = create(&dir, &任务参("乙", None)).unwrap()["id"].as_str().unwrap().to_string();

        let 读 = |id: &str| -> serde_json::Value {
            let store = Store::open(&dir.join(config::DB_FILE)).unwrap();
            serde_json::to_value(store::load_issue(store.conn(), id).unwrap()).unwrap()
        };
        let (甲前, 乙前) = (读(&甲), 读(&乙));

        move_issue(&dir, &流转参(&甲, Status::Wip)).unwrap();

        let (甲后, 乙后) = (读(&甲), 读(&乙));
        assert_eq!(乙前, 乙后, "没碰的那条一个字段都不该变");
        // 只允许状态类字段变：status / rev / updated_at / 租约两列
        let 允许 = ["status", "rev", "updated_at", "claim_actor", "claim_until"];
        let 前 = 甲前.as_object().unwrap();
        for (键, 值) in 甲后.as_object().unwrap() {
            if 允许.contains(&键.as_str()) { continue; }
            assert_eq!(Some(值), 前.get(键), "字段 {键} 不该被这次流转改动");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }}
