//! 接口层。两个入口共用同一套用例：
//! - 子命令：人、测试与脚本使用
//! - `rpc`：DSH 插件通过 NDJSON over stdio 调用，模型只接触这一条路径

mod commands;
mod config;
mod domain;
mod error;
mod store;

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use serde_json::json;

use commands::{DataCommand, Invocation, ListArgs, Report};
use domain::{Role, Status};
use error::ZenError;

#[derive(Parser)]
#[command(name = "dsh-zen", about = "agent 用的项目管理系统", disable_version_flag = true)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 建立 .dsh-zen 目录、默认配置与空库
    Init {
        /// 项目根目录（显式指定，不做猜测）
        #[arg(long)]
        root: PathBuf,
    },
    /// 建计划或任务
    Create {
        /// 任务标题
        title: String,
        /// 计划标题；同名复用，缺省进积压
        #[arg(long)]
        plan: Option<String>,
        /// 工作项类型
        #[arg(long = "type", default_value = "task")]
        kind: String,
        /// 本次动作戴的角色
        #[arg(long, default_value = "implementer")]
        role: String,
        /// 验收标准，可重复
        #[arg(long = "acceptance")]
        acceptance: Vec<String>,
        /// 正文（任务描述）
        #[arg(long)]
        description: Option<String>,
        /// 外部键；同键重复创建返回已有任务
        #[arg(long = "external-key")]
        external_key: Option<String>,
        /// 自定义字段，格式 name=value，可重复
        #[arg(long = "field")]
        field: Vec<String>,
        /// 操作者身份，缺省按 ZEN_ACTOR → git 邮箱 → 用户名@主机名
        #[arg(long)]
        actor: Option<String>,
    },
    /// 评论；终态任务也可写
    Comment {
        id: String,
        /// 评论正文
        #[arg(long)]
        body: String,
        #[arg(long, default_value = "implementer")]
        role: String,
        #[arg(long)]
        actor: Option<String>,
    },
    /// 挂产物：file / commit / url / note
    Artifact {
        id: String,
        /// 产物种类
        #[arg(long = "kind")]
        kind: String,
        /// 路径 / 提交哈希 / URL / 说明
        #[arg(long = "ref")]
        reference: String,
        #[arg(long, default_value = "implementer")]
        role: String,
        #[arg(long)]
        actor: Option<String>,
    },
    /// 勾选一条验收标准
    Accept {
        id: String,
        /// 验收标准序号，从 1 起
        #[arg(long)]
        ordinal: u32,
        #[arg(long, default_value = "implementer")]
        role: String,
        #[arg(long)]
        actor: Option<String>,
    },
    /// 状态流转；进入 wip 即占用租约
    Move {
        /// 任务编号
        id: String,
        /// 目标状态：todo / wip / done / blocked / dropped
        #[arg(long)]
        to: String,
        #[arg(long, default_value = "implementer")]
        role: String,
        #[arg(long)]
        actor: Option<String>,
        /// 只续期，不改状态
        #[arg(long)]
        extend: bool,
        /// 进入 blocked 的原因
        #[arg(long)]
        reason: Option<String>,
        /// 重新领取前清空 block_reason
        #[arg(long)]
        clear_block: bool,
    },
    /// 全量校验，只报不修
    Check,
    /// 字段方案存量迁移
    Migrate {
        /// 只报告，不写库
        #[arg(long)]
        dry_run: bool,
    },
    /// 定期复盘：扫 done 且未复盘的，建复盘任务并打标
    Review {
        #[arg(long, default_value = "implementer")]
        role: String,
        #[arg(long)]
        actor: Option<String>,
        /// 只报告，不写库
        #[arg(long)]
        dry_run: bool,
    },
    /// 重开终态（lead）
    Reopen {
        id: String,
        #[arg(long, default_value = "lead")]
        role: String,
        #[arg(long)]
        actor: Option<String>,
    },
    /// 事件回放
    Log {
        /// 只看某个任务或计划；省略则全局
        id: Option<String>,
        /// 最多返回多少条
        #[arg(long, default_value_t = 40)]
        limit: usize,
    },
    /// 建关联：blocks / relates / duplicates
    Link {
        /// 起点任务
        #[arg(long)]
        from: String,
        /// 终点任务
        #[arg(long)]
        to: String,
        /// 关联种类
        #[arg(long = "kind")]
        kind: String,
        #[arg(long, default_value = "implementer")]
        role: String,
        #[arg(long)]
        actor: Option<String>,
    },
    /// 从 arch-dev 计划文档导入任务
    Import {
        /// 文件或目录，目录取其中的 *.md
        #[arg(long)]
        path: String,
        /// 只报告，不写库
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    /// 改任务标题或正文（planner / lead）
    Update {
        /// 任务编号
        id: String,
        /// 新标题
        #[arg(long)]
        title: Option<String>,
        /// 新正文
        #[arg(long)]
        description: Option<String>,
        /// 本次动作戴的角色
        #[arg(long, default_value = "implementer")]
        role: String,
        /// 操作者身份
        #[arg(long)]
        actor: Option<String>,
    },
    /// 写计划标题与正文两节（planner / lead）
    Plan {
        /// 计划编号或标题
        id: String,
        /// 新标题
        #[arg(long)]
        title: Option<String>,
        /// 背景
        #[arg(long)]
        background: Option<String>,
        /// 上下文
        #[arg(long)]
        context: Option<String>,
        /// 本次动作戴的角色
        #[arg(long, default_value = "implementer")]
        role: String,
        /// 操作者身份
        #[arg(long)]
        actor: Option<String>,
    },
    /// 终端看板（人读）
    Board,
    /// 结构化统计（人读）
    Stats,
    /// 自描述：类型与字段方案
    Schema,
    /// 自描述：转移、视图、阈值
    Workflow,
    /// 自描述：当前角色这一行
    Roles {
        #[arg(long, default_value = "implementer")]
        role: String,
    },
    /// 按角色取最小上下文包
    Context {
        id: String,
        #[arg(long, default_value = "implementer")]
        role: String,
        #[arg(long)]
        actor: Option<String>,
    },
    /// 提交结构化报告；不改状态
    Report {
        id: String,
        /// progress / waiting / ready / blocked / failed / dropped
        #[arg(long = "kind")]
        kind: String,
        /// 一句话说明
        #[arg(long)]
        note: Option<String>,
        #[arg(long, default_value = "implementer")]
        role: String,
        #[arg(long)]
        actor: Option<String>,
    },
    /// 查询；可带预设视图名
    List {
        /// 视图名，见 views
        view: Option<String>,
        #[arg(long)]
        status: Option<String>,
        /// 工作项类型
        #[arg(long = "type")]
        kind: Option<String>,
        #[arg(long)]
        priority: Option<String>,
        #[arg(long)]
        plan: Option<String>,
        /// 按持有者过滤
        #[arg(long)]
        actor: Option<String>,
        /// 只看无人持有或租约已过期的
        #[arg(long)]
        unclaimed: bool,
    },
    /// 列出可用视图
    Views,
    /// 单条详情
    Show {
        /// 任务编号或计划编号/标题
        id: String,
        /// 只取计划正文的一节：background / context
        #[arg(long)]
        section: Option<String>,
    },
    /// NDJSON 服务：每行一个 {tool, args} 请求，每行一个结果
    Rpc,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let outcome = match cli.command {
        Cmd::Init { root } => commands::dispatch(Invocation::Init { root: Some(root) }),
        Cmd::Create { title, plan, kind, acceptance, description, external_key, field, role, actor } => {
            match role_from(&role) {
                Ok(role) => match 解析字段(&field) {
                    Ok(fields) => commands::dispatch(Invocation::Data(DataCommand::Create {
                        title,
                        plan,
                        kind,
                        role,
                        acceptance,
                        description,
                        external_key,
                        fields,
                        actor: actor.unwrap_or_else(commands::resolve_actor),
                    })),
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            }
        }
        Cmd::List { view, status, kind, priority, plan, actor, unclaimed } => commands::dispatch(
            Invocation::Data(DataCommand::List(ListArgs { view, status, kind, priority, plan, actor, unclaimed })),
        ),
        Cmd::Move { id, to, role, actor, extend, reason, clear_block } => {
            match (status_from(&to), role_from(&role)) {
            (Ok(to), Ok(role)) => commands::dispatch(Invocation::Data(DataCommand::Move {
                id,
                to,
                role,
                actor: actor.unwrap_or_else(commands::resolve_actor),
                extend,
                reason,
                clear_block,
            })),
            (Err(e), _) | (_, Err(e)) => Err(e),
            }
        }
        Cmd::Comment { id, body, role, actor } => match role_from(&role) {
            Ok(role) => commands::dispatch(Invocation::Data(DataCommand::Comment {
                id,
                body,
                role,
                actor: actor.unwrap_or_else(commands::resolve_actor),
            })),
            Err(e) => Err(e),
        },
        Cmd::Artifact { id, kind, reference, role, actor } => match role_from(&role) {
            Ok(role) => commands::dispatch(Invocation::Data(DataCommand::Artifact {
                id,
                kind,
                reference,
                role,
                actor: actor.unwrap_or_else(commands::resolve_actor),
            })),
            Err(e) => Err(e),
        },
        Cmd::Accept { id, ordinal, role, actor } => match role_from(&role) {
            Ok(role) => commands::dispatch(Invocation::Data(DataCommand::Accept {
                id,
                ordinal,
                role,
                actor: actor.unwrap_or_else(commands::resolve_actor),
            })),
            Err(e) => Err(e),
        },
        Cmd::Context { id, role, actor } => match role_from(&role) {
            Ok(role) => commands::dispatch(Invocation::Data(DataCommand::Context {
                id,
                role,
                actor: actor.unwrap_or_else(commands::resolve_actor),
            })),
            Err(e) => Err(e),
        },
        Cmd::Report { id, kind, note, role, actor } => match role_from(&role) {
            Ok(role) => commands::dispatch(Invocation::Data(DataCommand::Report {
                id,
                kind,
                note,
                role,
                actor: actor.unwrap_or_else(commands::resolve_actor),
            })),
            Err(e) => Err(e),
        },
        Cmd::Check => commands::dispatch(Invocation::Data(DataCommand::Check)),
        Cmd::Migrate { dry_run } => commands::dispatch(Invocation::Data(DataCommand::Migrate { dry_run })),
        Cmd::Reopen { id, role, actor } => match role_from(&role) {
            Ok(role) => commands::dispatch(Invocation::Data(DataCommand::Reopen {
                id,
                role,
                actor: actor.unwrap_or_else(commands::resolve_actor),
            })),
            Err(e) => Err(e),
        },
        Cmd::Log { id, limit } => commands::dispatch(Invocation::Data(DataCommand::Log { id, limit })),
        Cmd::Review { role, actor, dry_run } => match role_from(&role) {
            Ok(role) => commands::dispatch(Invocation::Data(DataCommand::Review {
                role,
                actor: actor.unwrap_or_else(commands::resolve_actor),
                dry_run,
            })),
            Err(e) => Err(e),
        },
        Cmd::Link { from, to, kind, role, actor } => match role_from(&role) {
            Ok(role) => commands::dispatch(Invocation::Data(DataCommand::Link {
                from,
                to,
                kind,
                role,
                actor: actor.unwrap_or_else(commands::resolve_actor),
            })),
            Err(e) => Err(e),
        },
        Cmd::Import { path, dry_run, actor } => commands::dispatch(Invocation::Data(DataCommand::Import {
            path,
            dry_run,
            actor: actor.unwrap_or_else(commands::resolve_actor),
        })),
        Cmd::Update { id, title, description, role, actor } => match role_from(&role) {
            Ok(role) => commands::dispatch(Invocation::Data(DataCommand::Update {
                id, title, description, role, actor: actor.unwrap_or_else(commands::resolve_actor),
            })),
            Err(e) => Err(e),
        },
        Cmd::Plan { id, title, background, context, role, actor } => match role_from(&role) {
            Ok(role) => commands::dispatch(Invocation::Data(DataCommand::Plan {
                id, title, background, context, role, actor: actor.unwrap_or_else(commands::resolve_actor),
            })),
            Err(e) => Err(e),
        },
        Cmd::Board => commands::dispatch(Invocation::Data(DataCommand::Board)),
        Cmd::Stats => commands::dispatch(Invocation::Data(DataCommand::Stats)),
        Cmd::Schema => commands::dispatch(Invocation::Data(DataCommand::Schema)),
        Cmd::Workflow => commands::dispatch(Invocation::Data(DataCommand::Workflow)),
        Cmd::Roles { role } => match role_from(&role) {
            Ok(role) => commands::dispatch(Invocation::Data(DataCommand::Roles { role })),
            Err(e) => Err(e),
        },
        Cmd::Views => commands::dispatch(Invocation::Data(DataCommand::Views)),
        Cmd::Show { id, section } => commands::dispatch(Invocation::Data(DataCommand::Show { id, section })),
        Cmd::Rpc => return rpc_loop(),
    };
    emit(outcome)
}

/// 一次请求一行。错误也按行返回，不中断后续请求。
fn rpc_loop() -> ExitCode {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => return fail(ZenError::Io(e.to_string())),
        };
        if line.trim().is_empty() {
            continue;
        }
        let result = handle_rpc_line(&line);
        let _ = writeln!(stdout, "{result}");
        let _ = stdout.flush();
    }
    ExitCode::SUCCESS
}

fn handle_rpc_line(line: &str) -> serde_json::Value {
    let req: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return error_json(ZenError::Validation {
                field: "request".into(),
                why: e.to_string(),
            })
        }
    };
    let tool = req.get("tool").and_then(|v| v.as_str()).unwrap_or_default();
    let args = req.get("args").cloned().unwrap_or_else(|| json!({}));

    let outcome = match tool.strip_prefix("zen_").unwrap_or(tool) {
        "init" => commands::dispatch(Invocation::Init {
            root: args.get("root").and_then(|v| v.as_str()).map(PathBuf::from),
        }),
        "create" => rpc_create(&args),
        "list" => commands::dispatch(Invocation::Data(DataCommand::List(ListArgs {
            view: owned(&args, "view"),
            status: owned(&args, "status"),
            kind: owned(&args, "type").or_else(|| owned(&args, "kind")),
            priority: owned(&args, "priority"),
            plan: owned(&args, "plan"),
            actor: owned(&args, "actor"),
            unclaimed: args.get("unclaimed").and_then(|v| v.as_bool()).unwrap_or(false),
        }))),
        "views" => commands::dispatch(Invocation::Data(DataCommand::Views)),
        "show" => rpc_show(&args),
        "move" => rpc_move(&args),
        "comment" => rpc_comment(&args),
        "artifact" => rpc_artifact(&args),
        "accept" => rpc_accept(&args),
        "report" => rpc_report(&args),
        "context" => rpc_context(&args),
        "check" => commands::dispatch(Invocation::Data(DataCommand::Check)),
        "migrate" => commands::dispatch(Invocation::Data(DataCommand::Migrate {
            dry_run: args.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false),
        })),
        "link" => rpc_link(&args),
        "review" => rpc_review(&args),
        "reopen" => rpc_reopen(&args),
        "log" => rpc_log(&args),
        "import" => rpc_import(&args),
        "update" => rpc_update(&args),
        "plan" => rpc_plan(&args),
        "board" => commands::dispatch(Invocation::Data(DataCommand::Board)),
        "stats" => commands::dispatch(Invocation::Data(DataCommand::Stats)),
        "schema" => commands::dispatch(Invocation::Data(DataCommand::Schema)),
        "workflow" => commands::dispatch(Invocation::Data(DataCommand::Workflow)),
        "roles" => rpc_roles(&args),
        other => Err(ZenError::NotFound {
            what: format!("工具 zen_{other}"),
        }),
    };

    match outcome {
        Ok(v) => json!({ "ok": true, "data": v }),
        Err(e) => error_json(e),
    }
}

/// 工具请求的字符串参数。缺失即 None，交由命令层报验证错误。
fn str_arg<'a>(req: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    req.get(key).and_then(|v| v.as_str())
}

fn rpc_comment(args: &serde_json::Value) -> error::Result<Report> {
    commands::dispatch(Invocation::Data(DataCommand::Comment {
        id: need(args, "id")?,
        body: need(args, "body")?,
        role: role_from(str_arg(args, "role").unwrap_or("implementer"))?,
        actor: owned(args, "actor").unwrap_or_else(commands::resolve_actor),
    }))
}

fn rpc_artifact(args: &serde_json::Value) -> error::Result<Report> {
    commands::dispatch(Invocation::Data(DataCommand::Artifact {
        id: need(args, "id")?,
        kind: need(args, "kind")?,
        reference: str_arg(args, "ref").or_else(|| str_arg(args, "reference")).map(str::to_string).ok_or_else(|| {
            ZenError::Validation { field: "ref".into(), why: "缺少 ref".into() }
        })?,
        role: role_from(str_arg(args, "role").unwrap_or("implementer"))?,
        actor: owned(args, "actor").unwrap_or_else(commands::resolve_actor),
    }))
}

fn rpc_accept(args: &serde_json::Value) -> error::Result<Report> {
    let ordinal = args.get("ordinal").and_then(|v| v.as_u64()).ok_or_else(|| ZenError::Validation {
        field: "ordinal".into(),
        why: "缺少 ordinal".into(),
    })?;
    commands::dispatch(Invocation::Data(DataCommand::Accept {
        id: need(args, "id")?,
        ordinal: ordinal as u32,
        role: role_from(str_arg(args, "role").unwrap_or("implementer"))?,
        actor: owned(args, "actor").unwrap_or_else(commands::resolve_actor),
    }))
}

fn rpc_log(args: &serde_json::Value) -> error::Result<Report> {
    commands::dispatch(Invocation::Data(DataCommand::Log {
        id: owned(args, "id"),
        limit: args.get("limit").and_then(|v| v.as_u64()).unwrap_or(40) as usize,
    }))
}

fn rpc_reopen(args: &serde_json::Value) -> error::Result<Report> {
    commands::dispatch(Invocation::Data(DataCommand::Reopen {
        id: need(args, "id")?,
        role: role_from(str_arg(args, "role").unwrap_or("lead"))?,
        actor: owned(args, "actor").unwrap_or_else(commands::resolve_actor),
    }))
}

fn rpc_review(args: &serde_json::Value) -> error::Result<Report> {
    commands::dispatch(Invocation::Data(DataCommand::Review {
        role: role_from(str_arg(args, "role").unwrap_or("implementer"))?,
        actor: owned(args, "actor").unwrap_or_else(commands::resolve_actor),
        dry_run: args.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false),
    }))
}

fn rpc_link(args: &serde_json::Value) -> error::Result<Report> {
    commands::dispatch(Invocation::Data(DataCommand::Link {
        from: need(args, "from")?,
        to: need(args, "to")?,
        kind: need(args, "kind")?,
        role: role_from(str_arg(args, "role").unwrap_or("implementer"))?,
        actor: owned(args, "actor").unwrap_or_else(commands::resolve_actor),
    }))
}

fn rpc_import(args: &serde_json::Value) -> error::Result<Report> {
    commands::dispatch(Invocation::Data(DataCommand::Import {
        path: need(args, "path")?,
        dry_run: args.get("dry_run").and_then(|v| v.as_bool()).unwrap_or(false),
        actor: owned(args, "actor").unwrap_or_else(commands::resolve_actor),
    }))
}

fn rpc_roles(args: &serde_json::Value) -> error::Result<Report> {
    commands::dispatch(Invocation::Data(DataCommand::Roles {
        role: role_from(str_arg(args, "role").unwrap_or("implementer"))?,
    }))
}

fn rpc_update(args: &serde_json::Value) -> error::Result<Report> {
    commands::dispatch(Invocation::Data(DataCommand::Update {
        id: need(args, "id")?,
        title: owned(args, "title"),
        description: owned(args, "description"),
        role: role_from(str_arg(args, "role").unwrap_or("implementer"))?,
        actor: owned(args, "actor").unwrap_or_else(commands::resolve_actor),
    }))
}

fn rpc_plan(args: &serde_json::Value) -> error::Result<Report> {
    commands::dispatch(Invocation::Data(DataCommand::Plan {
        id: need(args, "id")?,
        title: owned(args, "title"),
        background: owned(args, "background"),
        context: owned(args, "context"),
        role: role_from(str_arg(args, "role").unwrap_or("implementer"))?,
        actor: owned(args, "actor").unwrap_or_else(commands::resolve_actor),
    }))
}

fn rpc_context(args: &serde_json::Value) -> error::Result<Report> {
    commands::dispatch(Invocation::Data(DataCommand::Context {
        id: need(args, "id")?,
        role: role_from(str_arg(args, "role").unwrap_or("implementer"))?,
        actor: owned(args, "actor").unwrap_or_else(commands::resolve_actor),
    }))
}

fn rpc_report(args: &serde_json::Value) -> error::Result<Report> {
    commands::dispatch(Invocation::Data(DataCommand::Report {
        id: need(args, "id")?,
        kind: need(args, "kind")?,
        note: owned(args, "note"),
        role: role_from(str_arg(args, "role").unwrap_or("implementer"))?,
        actor: owned(args, "actor").unwrap_or_else(commands::resolve_actor),
    }))
}

/// 必填字符串参数。
fn need(args: &serde_json::Value, key: &str) -> error::Result<String> {
    str_arg(args, key).map(str::to_string).ok_or_else(|| ZenError::Validation {
        field: key.into(),
        why: format!("缺少 {key}"),
    })
}

fn rpc_move(args: &serde_json::Value) -> error::Result<Report> {
    let id = str_arg(args, "id")
        .ok_or_else(|| ZenError::Validation { field: "id".into(), why: "缺少 id".into() })?;
    let to = str_arg(args, "to")
        .ok_or_else(|| ZenError::Validation { field: "to".into(), why: "缺少 to".into() })?;
    commands::dispatch(Invocation::Data(DataCommand::Move {
        id: id.to_string(),
        to: status_from(to)?,
        role: role_from(str_arg(args, "role").unwrap_or("implementer"))?,
        actor: owned(args, "actor").unwrap_or_else(commands::resolve_actor),
        extend: args.get("extend").and_then(|v| v.as_bool()).unwrap_or(false),
        reason: owned(args, "reason"),
        clear_block: args.get("clear_block").and_then(|v| v.as_bool()).unwrap_or(false),
    }))
}

/// 命令行字段参数 k=v → 对象。值的类型由字段方案决定，这里一律当字符串交给命令层转换。
fn 解析字段(pairs: &[String]) -> error::Result<Option<serde_json::Map<String, serde_json::Value>>> {
    if pairs.is_empty() {
        return Ok(None);
    }
    let mut map = serde_json::Map::new();
    for pair in pairs {
        let (name, value) = pair.split_once('=').ok_or_else(|| ZenError::Validation {
            field: "field".into(),
            why: format!("应为 name=value，收到 {pair}"),
        })?;
        if name.trim().is_empty() {
            return Err(ZenError::Validation { field: "field".into(), why: "字段名不能为空".into() });
        }
        map.insert(name.trim().to_string(), serde_json::Value::String(value.to_string()));
    }
    Ok(Some(map))
}

/// 状态名解析。非法值在入口处拒绝。
fn status_from(name: &str) -> error::Result<Status> {
    Status::parse(name).ok_or_else(|| ZenError::Validation {
        field: "to".into(),
        why: format!("未知状态 {name}，可用：todo / wip / done / blocked / dropped"),
    })
}

fn rpc_show(args: &serde_json::Value) -> error::Result<Report> {
    let id = str_arg(args, "id")
        .ok_or_else(|| ZenError::Validation { field: "id".into(), why: "缺少 id".into() })?;
    commands::dispatch(Invocation::Data(DataCommand::Show { id: id.to_string(), section: owned(args, "section") }))
}

fn owned(req: &serde_json::Value, key: &str) -> Option<String> {
    str_arg(req, key).map(str::to_string)
}

fn rpc_create(args: &serde_json::Value) -> error::Result<Report> {
    let title = str_arg(args, "title").ok_or_else(|| ZenError::Validation {
        field: "title".into(),
        why: "缺少 title".into(),
    })?;
    commands::dispatch(Invocation::Data(DataCommand::Create {
        title: title.to_string(),
        description: owned(args, "description"),
        external_key: owned(args, "external_key"),
        fields: match args.get("fields") {
            None => None,
            Some(serde_json::Value::Object(map)) => Some(map.clone()),
            Some(_) => {
                return Err(ZenError::Validation {
                    field: "fields".into(),
                    why: "应是对象，值为字段名到字段值的映射".into(),
                });
            }
        },
        acceptance: args
            .get("acceptance")
            .and_then(|v| v.as_array())
            .map(|list| list.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
            .unwrap_or_default(),
        plan: str_arg(args, "plan").map(str::to_string),
        kind: str_arg(args, "type").or_else(|| str_arg(args, "kind")).unwrap_or("task").to_string(),
        role: role_from(str_arg(args, "role").unwrap_or("implementer"))?,
        actor: str_arg(args, "actor").map(str::to_string).unwrap_or_else(commands::resolve_actor),
    }))
}

/// 角色名解析。非法值在入口处拒绝，不进业务层。
fn role_from(name: &str) -> error::Result<Role> {
    Role::parse(name).ok_or_else(|| ZenError::Validation {
        field: "role".into(),
        why: format!("未知角色 {name}"),
    })
}

fn error_json(e: ZenError) -> serde_json::Value {
    json!({
        "ok": false,
        "code": e.code(),
        "message": e.to_string(),
        "retryable": e.retryable(),
    })
}

fn emit(outcome: error::Result<serde_json::Value>) -> ExitCode {
    match outcome {
        Ok(v) => {
            match v.get("text").and_then(|t| t.as_str()) {
                Some(text) => print!("{text}"),
                None => println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default()),
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(e),
    }
}

fn fail(e: ZenError) -> ExitCode {
    eprintln!("{e}");
    ExitCode::from(1)
}
