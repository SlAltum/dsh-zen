//! 领域规则。纯函数，不碰 IO，不依赖 store 与 config。
//!
//! 状态集是常量（架构 §7 取舍 1）：`workflow.toml` 只描述转换与守卫，不定义新状态名。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{Result, ZenError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Todo,
    Wip,
    Done,
    Blocked,
    Dropped,
}

impl Status {
    /// 五个状态，见 §3.1。顺序即展示顺序。
    pub const ALL: [Status; 5] = [Status::Todo, Status::Wip, Status::Done, Status::Blocked, Status::Dropped];

    pub fn is_terminal(self) -> bool {
        matches!(self, Status::Done | Status::Dropped)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Todo => "todo",
            Status::Wip => "wip",
            Status::Done => "done",
            Status::Blocked => "blocked",
            Status::Dropped => "dropped",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        Some(match s {
            "todo" => Status::Todo,
            "wip" => Status::Wip,
            "done" => Status::Done,
            "blocked" => Status::Blocked,
            "dropped" => Status::Dropped,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Planner,
    Implementer,
    Reviewer,
    Tester,
    Lead,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Planner => "planner",
            Role::Implementer => "implementer",
            Role::Reviewer => "reviewer",
            Role::Tester => "tester",
            Role::Lead => "lead",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        Some(match s {
            "planner" => Role::Planner,
            "implementer" => Role::Implementer,
            "reviewer" => Role::Reviewer,
            "tester" => Role::Tester,
            "lead" => Role::Lead,
            _ => return None,
        })
    }
}

/// 一条任务。字段集与 SRS §2 的 Issue 一致；rev 是 CAS 的行版本。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    /// 空 = 在积压里。
    pub plan: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub description: String,
    pub status: Status,
    pub priority: String,
    pub parent: Option<String>,
    pub block_reason: Option<String>,
    /// 自定义字段值，键集由类型方案约束。
    pub fields: Map<String, Value>,
    pub external_key: Option<String>,
    pub claim_actor: Option<String>,
    pub claim_until: Option<String>,
    pub reviewed_at: Option<String>,
    pub rev: i64,
    pub created_at: String,
    pub updated_at: String,
}

/// 字段的数据类型，决定校验方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldKind {
    #[serde(rename = "string")]
    Str,
    #[serde(rename = "enum")]
    Enum,
    #[serde(rename = "int")]
    Int,
    #[serde(rename = "bool")]
    Bool,
}

/// 一个自定义字段的定义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    pub name: String,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    #[serde(default)]
    pub values: Vec<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub max_len: Option<usize>,
    #[serde(default)]
    pub default: Option<String>,
}

/// 一个工作项类型，以及它绑定的字段名。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeDef {
    pub name: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub fields: Vec<String>,
}

/// 字段迁移的结果。
pub struct Normalized {
    pub fields: Map<String, Value>,
    /// 被丢弃的键与原因。
    pub dropped: Vec<(String, String)>,
    /// 必填但没有缺省值的键，需要人工补。
    pub unresolved: Vec<String>,
}

/// 把一组字段值迁移到当前类型方案：丢未知键、丢不合法值、补缺省值。
/// 只依赖传入的方案与值，不碰 IO。
pub fn normalize_fields(type_name: &str, specs: &[FieldDef], values: &Map<String, Value>) -> Normalized {
    let mut fields = Map::new();
    let mut dropped = Vec::new();

    for (key, value) in values {
        match specs.iter().find(|s| &s.name == key) {
            None => dropped.push((key.clone(), format!("类型 {type_name} 不含该字段"))),
            Some(spec) => match check_value(spec, value) {
                Ok(()) => {
                    fields.insert(key.clone(), value.clone());
                }
                Err(e) => dropped.push((key.clone(), e.to_string())),
            },
        }
    }

    let mut unresolved = Vec::new();
    for spec in specs {
        if fields.contains_key(&spec.name) {
            continue;
        }
        match &spec.default {
            Some(d) => {
                if let Ok(v) = coerce_default(spec, d) {
                    fields.insert(spec.name.clone(), v);
                }
            }
            None if spec.required => unresolved.push(spec.name.clone()),
            None => {}
        }
    }

    Normalized { fields, dropped, unresolved }
}

/// 按类型方案校验字段值，顺带补齐缺省值。
///
/// 只接收传入的方案与值，不碰 IO：字段未声明、类型不符、缺必填、枚举越界
/// 各自报 `Validation` 并定位到字段名，不合并成一句“字段不合法”。
pub fn validate_fields(type_name: &str, specs: &[FieldDef], values: &Map<String, Value>) -> Result<Map<String, Value>> {
    for name in values.keys() {
        if !specs.iter().any(|s| &s.name == name) {
            return Err(ZenError::Validation {
                field: name.clone(),
                why: format!("类型 {type_name} 不含该字段"),
            });
        }
    }

    let mut out = Map::new();
    for spec in specs {
        match values.get(&spec.name) {
            Some(Value::Null) | None => {
                if spec.required {
                    return Err(ZenError::Validation { field: spec.name.clone(), why: "缺少必填字段".into() });
                }
                if let Some(d) = &spec.default {
                    out.insert(spec.name.clone(), coerce_default(spec, d)?);
                }
            }
            Some(v) => {
                check_value(spec, v)?;
                out.insert(spec.name.clone(), v.clone());
            }
        }
    }
    Ok(out)
}

fn check_value(spec: &FieldDef, v: &Value) -> Result<()> {
    let bad = |why: String| ZenError::Validation { field: spec.name.clone(), why };
    match spec.kind {
        FieldKind::Str => {
            let s = v.as_str().ok_or_else(|| bad("应为字符串".into()))?;
            if let Some(max) = spec.max_len
                && s.chars().count() > max
            {
                return Err(bad(format!("长度超过上限 {max}")));
            }
        }
        FieldKind::Enum => {
            let s = v.as_str().ok_or_else(|| bad("应为枚举字符串".into()))?;
            if !spec.values.iter().any(|x| x == s) {
                return Err(bad(format!("取值 {s} 不在 {}", spec.values.join(" / "))));
            }
        }
        FieldKind::Int => {
            if !v.is_i64() {
                return Err(bad("应为整数".into()));
            }
        }
        FieldKind::Bool => {
            if !v.is_boolean() {
                return Err(bad("应为布尔".into()));
            }
        }
    }
    Ok(())
}

/// `default` 在配置里是字符串，按字段类型转成对应的 JSON 值。
fn coerce_default(spec: &FieldDef, raw: &str) -> Result<Value> {
    let bad = || ZenError::Validation { field: spec.name.clone(), why: format!("默认值 {raw} 不合法") };
    Ok(match spec.kind {
        FieldKind::Str | FieldKind::Enum => Value::String(raw.to_string()),
        FieldKind::Int => Value::from(raw.parse::<i64>().map_err(|_| bad())?),
        FieldKind::Bool => Value::Bool(raw.parse::<bool>().map_err(|_| bad())?),
    })
}

/// 任务 ID 前缀即项目 key。
pub const ID_PREFIX: &str = "ZEN";

/// 计划 ID 前缀。
pub const PLAN_PREFIX: &str = "PLAN";

/// 优先级取值，见 SRS §9。
pub const PRIORITIES: [&str; 4] = ["P0", "P1", "P2", "P3"];

/// 缺省优先级，见 SRS §9。
pub const DEFAULT_PRIORITY: &str = "P2";

/// 下一个可用 ID：现有同前缀 ID 的最大序号 + 1。
pub fn next_id(prefix: &str, existing: &[String]) -> String {
    let max = existing.iter().filter_map(|s| parse_id(prefix, s)).max().unwrap_or(0);
    format!("{prefix}-{}", max + 1)
}

/// 从 ID 取序号。前缀不符或尾部不是数字就不认。
pub fn parse_id(prefix: &str, id: &str) -> Option<u64> {
    id.strip_prefix(prefix)?.strip_prefix('-')?.parse().ok()
}

/// 一条转移授权。直接授权的角色填 roles；必须持有租约的角色填 holders。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grant {
    /// 形如 `wip→done`，通配写作 `*→dropped`。
    pub transition: String,
    #[serde(default)]
    pub roles: Vec<String>,
    #[serde(default)]
    pub holders: Vec<String>,
}

/// 授予表的一行：角色 → 允许的 actor，星号表示任何 actor。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleGrant {
    pub role: String,
    #[serde(default)]
    pub actors: Vec<String>,
}

/// 勾选验收标准的授权。§4.2 的这一行按动作写而非按转移写，配置层只承载转移授权，
/// 所以按常量实现：lead 可直接勾，implementer / tester 必须是持有者。
pub fn check_accept_permission(role: Role, holder: Option<&str>, actor: &str) -> Result<()> {
    let 可勾 = match role {
        Role::Lead => true,
        Role::Implementer | Role::Tester => holder == Some(actor),
        Role::Planner | Role::Reviewer => false,
    };
    if 可勾 {
        Ok(())
    } else {
        Err(ZenError::Permission { action: "勾选验收标准".into(), role: role.as_str().to_string() })
    }
}

/// 「未被占用」：可以领取，且当前没有有效租约。终态不可领取，所以不算未占用；
/// 租约过期等于空闲（R9）。列表与统计共用这一个判断，避免两处口径。
pub fn is_unclaimed(issue: &Issue, now: &str) -> bool {
    if issue.status.is_terminal() {
        return false;
    }
    match issue.claim_actor.as_deref() {
        None => true,
        Some(_) => matches!(issue.claim_until.as_deref(), Some(until) if until < now),
    }
}

/// 重开终态的授权：§4.2 与 §5 都写的是 lead。
pub fn check_reopen_permission(role: Role) -> Result<()> {
    if role == Role::Lead {
        Ok(())
    } else {
        Err(ZenError::Permission { action: "重开终态".into(), role: role.as_str().to_string() })
    }
}

/// 建关联的授权。§4.2 把 blocks 只给了 planner / lead，relates 与 duplicates 对所有人开放。
pub fn check_link_permission(role: Role, kind: LinkKind) -> Result<()> {
    let 可建 = match kind {
        LinkKind::Blocks => matches!(role, Role::Planner | Role::Lead),
        LinkKind::Relates | LinkKind::Duplicates => true,
    };
    if 可建 {
        Ok(())
    } else {
        Err(ZenError::Permission { action: format!("建 {} 关联", kind.as_str()), role: role.as_str().to_string() })
    }
}

/// 建关联后两端要落库的顺序。duplicates 是对称关系，统一成 (小, 大) 避免同一条存两份。
pub fn normalize_link(from: &str, to: &str, kind: LinkKind) -> (String, String) {
    if kind == LinkKind::Duplicates && to < from {
        (to.to_string(), from.to_string())
    } else {
        (from.to_string(), to.to_string())
    }
}

/// actor 是否被授予该角色。未授予时报验证错误并列出该 actor 可选的角色。
pub fn check_granted(grants: &[RoleGrant], role: Role, actor: &str) -> Result<()> {
    let 覆盖 = |g: &RoleGrant| g.actors.iter().any(|a| a == "*" || a == actor);
    if grants.iter().any(|g| g.role == role.as_str() && 覆盖(g)) {
        return Ok(());
    }
    let 可选: Vec<&str> = grants.iter().filter(|g| 覆盖(g)).map(|g| g.role.as_str()).collect();
    Err(ZenError::Validation {
        field: "role".into(),
        why: format!(
            "{actor} 未被授予角色 {}，该 actor 可用：{}",
            role.as_str(),
            if 可选.is_empty() { "（无）".to_string() } else { 可选.join(" / ") }
        ),
    })
}

/// 角色能否发起这次转移。命中 holders 时还要求租约在 actor 手上；
/// 直接授权的角色不受租约影响。找不到对应格子即拒绝。
pub fn check_permission(
    grants: &[Grant],
    role: Role,
    from: Status,
    to: Status,
    holder: Option<&str>,
    actor: &str,
) -> Result<()> {
    let key = format!("{}→{}", from.as_str(), to.as_str());
    let wildcard = format!("*→{}", to.as_str());
    let grant = grants
        .iter()
        .find(|g| g.transition == key)
        .or_else(|| grants.iter().find(|g| g.transition == wildcard));
    let 含 = |list: &[String]| list.iter().any(|r| r == role.as_str());
    // 进入 wip 本身就是占用（R8）：租约空闲时就能领取；
    // 离开 wip 时必须已经是持有者。离开 wip 会释放租约，若死板要求持有者，
    // 主动受阻的任务将永远无人能重新领取，与 §3.3 的「清空后可被重新领取」矛盾。
    let 可持 = |g: &Grant| {
        含(&g.holders) && if to == Status::Wip { holder.is_none() || holder == Some(actor) } else { holder == Some(actor) }
    };

    match grant {
        Some(g) if 含(&g.roles) => Ok(()),
        Some(g) if 可持(g) => Ok(()),
        _ => Err(ZenError::Permission { action: key, role: role.as_str().to_string() }),
    }
}

/// 验收标准的一项。逐条勾选，不接受批量勾（R13）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Acceptance {
    pub issue: String,
    pub ordinal: u32,
    pub text: String,
    pub checked_at: Option<String>,
    pub checked_by: Option<String>,
}

/// 一条评论。终态任务仍可评论（R14）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub issue: String,
    pub actor: String,
    pub body: String,
    pub at: String,
}

/// 产物的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    File,
    Commit,
    Url,
    Note,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::File => "file",
            ArtifactKind::Commit => "commit",
            ArtifactKind::Url => "url",
            ArtifactKind::Note => "note",
        }
    }

    pub fn parse(s: &str) -> Option<ArtifactKind> {
        Some(match s {
            "file" => ArtifactKind::File,
            "commit" => ArtifactKind::Commit,
            "url" => ArtifactKind::Url,
            "note" => ArtifactKind::Note,
            _ => return None,
        })
    }
}

/// note 型产物至少要这么多字，见 SRS §3.4。
pub const NOTE_MIN_CHARS: usize = 20;

/// 一条产物。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub issue: String,
    pub kind: ArtifactKind,
    /// 路径 / 提交哈希 / URL / 说明。
    #[serde(rename = "ref")]
    pub reference: String,
    /// 只有 file 才填。
    pub sha256: Option<String>,
    pub at: String,
}

impl Artifact {
    /// 能作为完成凭据的产物。note 太短不算。
    pub fn is_usable(&self) -> bool {
        !self.reference.trim().is_empty()
            && (self.kind != ArtifactKind::Note || self.reference.chars().count() >= NOTE_MIN_CHARS)
    }
}

/// 守卫名。名字写在 workflow.toml 的 guard 列表里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Guard {
    AcceptanceAllChecked,
    ArtifactPresent,
    BlockReasonCleared,
}

impl Guard {
    pub fn as_str(self) -> &'static str {
        match self {
            Guard::AcceptanceAllChecked => "acceptance_all_checked",
            Guard::ArtifactPresent => "artifact_present",
            Guard::BlockReasonCleared => "block_reason_cleared",
        }
    }

    pub fn parse(s: &str) -> Option<Guard> {
        Some(match s {
            "acceptance_all_checked" => Guard::AcceptanceAllChecked,
            "artifact_present" => Guard::ArtifactPresent,
            "block_reason_cleared" => Guard::BlockReasonCleared,
            _ => return None,
        })
    }
}

/// 转移的副作用。名字写在 workflow.toml 的 effects 列表里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    ReleaseLease,
}

impl Effect {
    pub fn parse(s: &str) -> Option<Effect> {
        Some(match s {
            "release_lease" => Effect::ReleaseLease,
            _ => return None,
        })
    }
}

/// 结构化报告的类型，见 R29。报告不建表，是事件的一种。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportKind {
    Progress,
    Waiting,
    Ready,
    Blocked,
    Failed,
    Dropped,
}

impl ReportKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReportKind::Progress => "progress",
            ReportKind::Waiting => "waiting",
            ReportKind::Ready => "ready",
            ReportKind::Blocked => "blocked",
            ReportKind::Failed => "failed",
            ReportKind::Dropped => "dropped",
        }
    }

    pub fn parse(s: &str) -> Option<ReportKind> {
        Some(match s {
            "progress" => ReportKind::Progress,
            "waiting" => ReportKind::Waiting,
            "ready" => ReportKind::Ready,
            "blocked" => ReportKind::Blocked,
            "failed" => ReportKind::Failed,
            "dropped" => ReportKind::Dropped,
            _ => return None,
        })
    }
}

/// 派生阶段。只读、不落库，由最近一条报告与租约推出（R30）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    /// 工作中。
    Working,
    /// 等人：缺信息、等决定。
    WaitingForPerson,
    /// 等占用。
    WaitingForClaim,
    /// 空闲。
    Idle,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Working => "working",
            Stage::WaitingForPerson => "waiting_for_person",
            Stage::WaitingForClaim => "waiting_for_claim",
            Stage::Idle => "idle",
        }
    }
}

/// 阶段推导。优先级：终态 → 持有租约 → 最近报告。
pub fn derive_stage(status: Status, last_report: Option<ReportKind>, has_lease: bool) -> Stage {
    if status.is_terminal() {
        return Stage::Idle;
    }
    if has_lease {
        return Stage::Working;
    }
    match last_report {
        Some(ReportKind::Progress) => Stage::Working,
        Some(ReportKind::Ready) => Stage::WaitingForClaim,
        Some(ReportKind::Waiting | ReportKind::Blocked | ReportKind::Failed) => Stage::WaitingForPerson,
        Some(ReportKind::Dropped) | None => Stage::Idle,
    }
}

/// 求值守卫要引用的事实，只由已落库的读数填充。
pub struct GuardFacts<'a> {
    pub acceptance: &'a [Acceptance],
    pub artifacts: &'a [Artifact],
    pub block_reason: Option<&'a str>,
}

/// 按顺序求值守卫。第一条不满足的就报出来，不合并成一句。
pub fn eval_guards(transition: &str, guards: &[Guard], facts: &GuardFacts<'_>) -> Result<()> {
    for guard in guards {
        if let Some(missing) = unmet(*guard, facts) {
            return Err(ZenError::Guard {
                transition: transition.to_string(),
                missing: format!("{}：{missing}", guard.as_str()),
            });
        }
    }
    Ok(())
}

/// 这条守卫缺什么；满足则返回 None。
fn unmet(guard: Guard, facts: &GuardFacts<'_>) -> Option<String> {
    match guard {
        Guard::AcceptanceAllChecked => {
            if facts.acceptance.is_empty() {
                return Some("还没有验收标准".into());
            }
            let 未勾 = facts.acceptance.iter().filter(|a| a.checked_at.is_none()).count();
            (未勾 > 0).then(|| format!("还有 {未勾} 条未勾选"))
        }
        Guard::ArtifactPresent => {
            if facts.artifacts.iter().any(Artifact::is_usable) {
                None
            } else if facts.artifacts.is_empty() {
                Some("还没有产物（file / commit / url / note 之一）".into())
            } else {
                Some(format!("现有产物都不可用：note 需至少 {NOTE_MIN_CHARS} 字"))
            }
        }
        Guard::BlockReasonCleared => match facts.block_reason {
            Some(reason) if !reason.trim().is_empty() => Some("block_reason 未清空".into()),
            _ => None,
        },
    }
}

/// 计划：纯容器，没有状态，进度由任务计数派生。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub title: String,
    pub background: String,
    pub context: String,
    pub created_at: String,
}

/// 关联的种类。只有 blocks 参与就绪判定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkKind {
    Blocks,
    Relates,
    Duplicates,
}

impl LinkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LinkKind::Blocks => "blocks",
            LinkKind::Relates => "relates",
            LinkKind::Duplicates => "duplicates",
        }
    }

    pub fn parse(s: &str) -> Option<LinkKind> {
        Some(match s {
            "blocks" => LinkKind::Blocks,
            "relates" => LinkKind::Relates,
            "duplicates" => LinkKind::Duplicates,
            _ => return None,
        })
    }
}

/// 一条关联：from → to。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Link {
    pub from: String,
    pub to: String,
    pub kind: LinkKind,
}

/// 视图谓词。名字与谓词的绑定写在 workflow.toml 的 views 段，代码里不写死视图名。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Predicate {
    Ready,
    Mine,
    Blocked,
    Backlog,
    Stale,
    Review,
}

impl Predicate {
    pub fn parse(s: &str) -> Option<Predicate> {
        Some(match s {
            "ready" => Predicate::Ready,
            "mine" => Predicate::Mine,
            "blocked" => Predicate::Blocked,
            "backlog" => Predicate::Backlog,
            "stale" => Predicate::Stale,
            "review" => Predicate::Review,
            _ => return None,
        })
    }

}

/// 就绪的任务 ID，保持传入顺序。
///
/// 就绪 = 在计划内、待办、前驱都已完成、且父链上没有受阻或已丢弃的祖先。
/// 只看传入的集合，不查库；依赖成环时报验证错误并列出环上的编号。
pub fn ready(issues: &[Issue], links: &[Link]) -> Result<Vec<String>> {
    check_blocks_acyclic(links)?;
    let by_id: HashMap<&str, &Issue> = issues.iter().map(|i| (i.id.as_str(), i)).collect();
    let mut out = Vec::new();
    for issue in issues {
        if issue.status != Status::Todo || issue.plan.is_none() {
            continue;
        }
        if waiting_for_predecessor(issue, links, &by_id) || under_hindered_parent(issue, &by_id) {
            continue;
        }
        out.push(issue.id.clone());
    }
    Ok(out)
}

/// 前驱未完成即阻塞；前驱不在集合里，同样按未完成处理。
fn waiting_for_predecessor(issue: &Issue, links: &[Link], by_id: &HashMap<&str, &Issue>) -> bool {
    links
        .iter()
        .filter(|l| l.kind == LinkKind::Blocks && l.to == issue.id)
        .any(|l| by_id.get(l.from.as_str()).map(|dep| dep.status != Status::Done).unwrap_or(true))
}

/// 父链上出现受阻或已丢弃，整支都不可动。
fn under_hindered_parent(issue: &Issue, by_id: &HashMap<&str, &Issue>) -> bool {
    let mut seen = HashSet::new();
    let mut cursor = issue.parent.as_deref();
    while let Some(parent_id) = cursor {
        if !seen.insert(parent_id) {
            break;
        }
        match by_id.get(parent_id) {
            Some(parent) => {
                if matches!(parent.status, Status::Blocked | Status::Dropped) {
                    return true;
                }
                cursor = parent.parent.as_deref();
            }
            None => break,
        }
    }
    false
}

/// blocks 成环就无法定序，直接拒绝并报出环上的编号。
pub fn check_blocks_acyclic(links: &[Link]) -> Result<()> {
    let mut graph: HashMap<&str, Vec<&str>> = HashMap::new();
    for link in links.iter().filter(|l| l.kind == LinkKind::Blocks) {
        graph.entry(link.from.as_str()).or_default().push(link.to.as_str());
    }

    // 1 = 在栈上，2 = 已探完
    let mut state: HashMap<&str, u8> = HashMap::new();
    for start in graph.keys().copied() {
        if state.get(start).copied().unwrap_or(0) != 0 {
            continue;
        }
        let mut stack: Vec<(&str, usize)> = vec![(start, 0)];
        let mut path: Vec<&str> = vec![start];
        state.insert(start, 1);
        while let Some(&(node, next_index)) = stack.last() {
            let nexts = graph.get(node).map(Vec::as_slice).unwrap_or(&[]);
            if next_index < nexts.len() {
                stack.last_mut().unwrap().1 = next_index + 1;
                let next = nexts[next_index];
                match state.get(next).copied().unwrap_or(0) {
                    0 => {
                        state.insert(next, 1);
                        path.push(next);
                        stack.push((next, 0));
                    }
                    1 => {
                        let from = path.iter().position(|n| *n == next).unwrap_or(0);
                        return Err(ZenError::Validation {
                            field: "blocks".into(),
                            why: format!("依赖成环：{} → {next}", path[from..].join(" → ")),
                        });
                    }
                    _ => {}
                }
            } else {
                state.insert(node, 2);
                path.pop();
                stack.pop();
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// bug 类型的字段方案：一个必填枚举 + 一个带长度上限的可选字符串。
    fn 方案() -> Vec<FieldDef> {
        vec![
            FieldDef {
                name: "severity".into(),
                kind: FieldKind::Enum,
                values: vec!["low".into(), "high".into()],
                required: true,
                max_len: None,
                default: None,
            },
            FieldDef {
                name: "outcome".into(),
                kind: FieldKind::Str,
                values: vec![],
                required: false,
                max_len: Some(5),
                default: None,
            },
        ]
    }

    fn 值(pairs: &[(&str, Value)]) -> Map<String, Value> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn 未声明字段被拒() {
        let v = 值(&[("severity", Value::from("low")), ("nope", Value::from("x"))]);
        let e = validate_fields("bug", &方案(), &v).unwrap_err();
        assert_eq!(e.code(), "validation");
        assert!(e.to_string().contains("nope"), "{e}");
    }

    #[test]
    fn 类型不符被拒() {
        let v = 值(&[("severity", Value::from(3))]);
        let e = validate_fields("bug", &方案(), &v).unwrap_err();
        assert_eq!(e.code(), "validation");
        assert!(e.to_string().contains("severity"), "{e}");
    }

    #[test]
    fn 缺必填被拒() {
        let v = 值(&[("outcome", Value::from("ok"))]);
        let e = validate_fields("bug", &方案(), &v).unwrap_err();
        assert!(e.to_string().contains("severity"), "{e}");
    }

    #[test]
    fn 枚举越界被拒() {
        let v = 值(&[("severity", Value::from("critical"))]);
        let e = validate_fields("bug", &方案(), &v).unwrap_err();
        assert!(e.to_string().contains("critical"), "{e}");
    }

    #[test]
    fn 长度上限生效且可选字段可缺省() {
        let 超长 = 值(&[("severity", Value::from("low")), ("outcome", Value::from("abcdef"))]);
        assert!(validate_fields("bug", &方案(), &超长).is_err());

        let 恰好 = 值(&[("severity", Value::from("low")), ("outcome", Value::from("abcde"))]);
        let out = validate_fields("bug", &方案(), &恰好).unwrap();
        assert_eq!(out.len(), 2);

        let 缺省 = validate_fields("bug", &方案(), &值(&[("severity", Value::from("low"))])).unwrap();
        assert_eq!(缺省.len(), 1, "可选字段缺席时不写空值");
    }

    #[test]
    fn 缺省值按类型补齐() {
        let specs = vec![FieldDef {
            name: "retries".into(),
            kind: FieldKind::Int,
            values: vec![],
            required: false,
            max_len: None,
            default: Some("3".into()),
        }];
        let out = validate_fields("job", &specs, &Map::new()).unwrap();
        assert_eq!(out.get("retries").and_then(Value::as_i64), Some(3));
    }

    #[test]
    fn 下一个编号取最大序号加一() {
        assert_eq!(next_id(ID_PREFIX, &[]), "ZEN-1");
        let ids = vec!["ZEN-1".to_string(), "ZEN-12".to_string(), "OTHER-99".to_string(), "ZEN-".to_string()];
        assert_eq!(next_id(ID_PREFIX, &ids), "ZEN-13");
        assert_eq!(parse_id(ID_PREFIX, "ZEN-7"), Some(7));
        assert_eq!(parse_id(ID_PREFIX, "ZEN-x"), None);
        assert_eq!(parse_id(ID_PREFIX, "OTHER-1"), None);
    }

    /// 建一个最小任务，只用得上 id / status / plan / parent。
    fn 任务(id: &str, status: Status, plan: Option<&str>) -> Issue {
        Issue {
            id: id.into(),
            plan: plan.map(str::to_string),
            kind: "task".into(),
            title: id.into(),
            description: String::new(),
            status,
            priority: DEFAULT_PRIORITY.into(),
            parent: None,
            block_reason: None,
            fields: Map::new(),
            external_key: None,
            claim_actor: None,
            claim_until: None,
            reviewed_at: None,
            rev: 0,
            created_at: "2026-01-01T00:00:00.000Z".into(),
            updated_at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    fn 子任务(id: &str, status: Status, parent: &str) -> Issue {
        Issue { parent: Some(parent.into()), ..任务(id, status, Some("PLAN-1")) }
    }

    fn 依赖(from: &str, to: &str) -> Link {
        Link { from: from.into(), to: to.into(), kind: LinkKind::Blocks }
    }

    #[test]
    fn 链式阻塞解除后才就绪() {
        let 计划 = Some("PLAN-1");
        let links = vec![依赖("ZEN-1", "ZEN-2")];

        let 都待办 = vec![任务("ZEN-1", Status::Todo, 计划), 任务("ZEN-2", Status::Todo, 计划)];
        assert_eq!(ready(&都待办, &links).unwrap(), vec!["ZEN-1".to_string()], "被阻塞者不就绪");

        let 前驱进行中 = vec![任务("ZEN-1", Status::Wip, 计划), 任务("ZEN-2", Status::Todo, 计划)];
        assert!(ready(&前驱进行中, &links).unwrap().is_empty(), "wip 尚未完成，阻塞仍在");

        let 前驱完成 = vec![任务("ZEN-1", Status::Done, 计划), 任务("ZEN-2", Status::Todo, 计划)];
        assert_eq!(ready(&前驱完成, &links).unwrap(), vec!["ZEN-2".to_string()]);
    }

    #[test]
    fn 父级受阻则子孙不就绪() {
        let 父 = 任务("ZEN-1", Status::Blocked, Some("PLAN-1"));
        let 子 = 子任务("ZEN-2", Status::Todo, "ZEN-1");
        let 孙 = 子任务("ZEN-3", Status::Todo, "ZEN-2");
        let 就绪 = ready(&[父, 子, 孙], &[]).unwrap();
        assert!(就绪.is_empty(), "阻塞沿父链递归传染：{就绪:?}");
    }

    #[test]
    fn 积压与终态不就绪() {
        assert!(ready(&[任务("ZEN-1", Status::Todo, None)], &[]).unwrap().is_empty(), "积压不进就绪");
        assert!(ready(&[任务("ZEN-2", Status::Done, Some("PLAN-1"))], &[]).unwrap().is_empty());
        assert!(ready(&[任务("ZEN-3", Status::Blocked, Some("PLAN-1"))], &[]).unwrap().is_empty());
    }

    #[test]
    fn 依赖成环报出环上的编号() {
        let links = vec![依赖("ZEN-1", "ZEN-2"), 依赖("ZEN-2", "ZEN-1")];
        let issues = vec![任务("ZEN-1", Status::Todo, Some("PLAN-1")), 任务("ZEN-2", Status::Todo, Some("PLAN-1"))];
        let e = ready(&issues, &links).unwrap_err();
        assert_eq!(e.code(), "validation");
        assert!(e.to_string().contains("ZEN-1") && e.to_string().contains("ZEN-2"), "{e}");
    }

    #[test]
    fn 关联不产生阻塞() {
        let links = vec![Link { from: "ZEN-1".into(), to: "ZEN-2".into(), kind: LinkKind::Relates }];
        let issues = vec![任务("ZEN-1", Status::Todo, Some("PLAN-1")), 任务("ZEN-2", Status::Todo, Some("PLAN-1"))];
        assert_eq!(ready(&issues, &links).unwrap().len(), 2, "关联不是依赖");
    }

    fn 验收(text: &str, 勾了: bool) -> Acceptance {
        Acceptance {
            issue: "ZEN-1".into(),
            ordinal: 1,
            text: text.into(),
            checked_at: 勾了.then(|| "2026-01-01T00:00:00.000Z".to_string()),
            checked_by: 勾了.then(|| "a@b".to_string()),
        }
    }

    fn 产物(kind: ArtifactKind, reference: &str) -> Artifact {
        Artifact {
            issue: "ZEN-1".into(),
            kind,
            reference: reference.into(),
            sha256: None,
            at: "2026-01-01T00:00:00.000Z".into(),
        }
    }

    fn 空事实() -> GuardFacts<'static> {
        GuardFacts { acceptance: &[], artifacts: &[], block_reason: None }
    }

    #[test]
    fn 验收未逐条勾完挡住完成() {
        let 半勾 = [验收("跑通甲", true), 验收("跑通乙", false)];
        let facts = GuardFacts { acceptance: &半勾, ..空事实() };
        let e = eval_guards("wip→done", &[Guard::AcceptanceAllChecked], &facts).unwrap_err();
        assert_eq!(e.code(), "guard");
        assert!(e.to_string().contains("acceptance_all_checked"), "要指出是哪条守卫：{e}");
        assert!(e.to_string().contains("未勾选"), "{e}");

        let 空 = eval_guards("wip→done", &[Guard::AcceptanceAllChecked], &空事实()).unwrap_err();
        assert!(空.to_string().contains("验收标准"), "{空}");
    }

    #[test]
    fn 缺产物挡住完成() {
        let e = eval_guards("wip→done", &[Guard::ArtifactPresent], &空事实()).unwrap_err();
        assert!(e.to_string().contains("产物"), "{e}");
    }

    #[test]
    fn 太短的备注不算产物() {
        let 短 = [产物(ArtifactKind::Note, "改好了")];
        let facts = GuardFacts { artifacts: &短, ..空事实() };
        assert!(eval_guards("wip→done", &[Guard::ArtifactPresent], &facts).is_err(), "note 要求至少 20 字");

        let 够长 = [产物(ArtifactKind::Note, "把视图谓词挪回 domain，store 只留列条件")];
        let facts = GuardFacts { artifacts: &够长, ..空事实() };
        assert!(eval_guards("wip→done", &[Guard::ArtifactPresent], &facts).is_ok());
    }

    #[test]
    fn 未清空阻塞原因挡住重新领取() {
        let facts = GuardFacts { block_reason: Some("等上游确认"), ..空事实() };
        let e = eval_guards("blocked→wip", &[Guard::BlockReasonCleared], &facts).unwrap_err();
        assert!(e.to_string().contains("block_reason"), "{e}");
        assert!(eval_guards("blocked→wip", &[Guard::BlockReasonCleared], &空事实()).is_ok());
    }

    #[test]
    fn 守卫全部满足才放行且按序报第一条() {
        let 齐 = [验收("跑通甲", true)];
        let 件 = [产物(ArtifactKind::Commit, "abc1234")];
        let facts = GuardFacts { acceptance: &齐, artifacts: &件, block_reason: None };
        assert!(eval_guards("wip→done", &[Guard::AcceptanceAllChecked, Guard::ArtifactPresent], &facts).is_ok());

        let 缺件 = GuardFacts { acceptance: &齐, artifacts: &[], block_reason: None };
        let e = eval_guards("wip→done", &[Guard::AcceptanceAllChecked, Guard::ArtifactPresent], &缺件).unwrap_err();
        assert!(e.to_string().contains("产物"), "两条同时不满足时报第一条未满足的：{e}");
    }

    fn 授权(transition: &str, roles: &[&str], holders: &[&str]) -> Grant {
        Grant {
            transition: transition.into(),
            roles: roles.iter().map(|r| r.to_string()).collect(),
            holders: holders.iter().map(|r| r.to_string()).collect(),
        }
    }

    fn 授予(role: &str, actors: &[&str]) -> RoleGrant {
        RoleGrant { role: role.into(), actors: actors.iter().map(|a| a.to_string()).collect() }
    }

    /// 逐格照 SRS §4.2 的抽样用表。
    fn 权限表() -> Vec<Grant> {
        vec![
            授权("todo→wip", &["planner", "implementer", "reviewer", "tester", "lead"], &[]),
            授权("wip→done", &["lead"], &["implementer", "tester"]),
            授权("blocked→wip", &["planner", "lead"], &["implementer", "tester"]),
            授权("*→dropped", &["planner", "lead"], &["implementer"]),
        ]
    }

    #[test]
    fn 越权角色被拒并指出是哪条转移() {
        let 表 = 权限表();
        assert!(check_permission(&表, Role::Reviewer, Status::Todo, Status::Wip, None, "a@b").is_ok(), "todo→wip 对所有人开放");
        let e = check_permission(&表, Role::Reviewer, Status::Wip, Status::Done, Some("a@b"), "a@b").unwrap_err();
        assert_eq!(e.code(), "permission");
        assert!(e.to_string().contains("wip→done"), "要指出缺哪条转移：{e}");
        assert!(check_permission(&表, Role::Planner, Status::Wip, Status::Done, Some("a@b"), "a@b").is_err(), "planner 不在 §4.2 这一格");
    }

    #[test]
    fn 持有者规则优先于角色表() {
        let 表 = 权限表();
        assert!(check_permission(&表, Role::Implementer, Status::Wip, Status::Done, Some("a@b"), "a@b").is_ok());
        assert!(check_permission(&表, Role::Implementer, Status::Wip, Status::Done, Some("c@d"), "a@b").is_err(), "租约在别人手上");
        assert!(check_permission(&表, Role::Implementer, Status::Wip, Status::Done, None, "a@b").is_err(), "没租约就不算持有者");
        assert!(check_permission(&表, Role::Lead, Status::Wip, Status::Done, None, "a@b").is_ok(), "直接授权的角色不看租约");
    }

    #[test]
    fn 进入wip可以领取空闲租约但离开wip不行() {
        let 表 = 权限表();
        assert!(
            check_permission(&表, Role::Implementer, Status::Blocked, Status::Wip, None, "a@b").is_ok(),
            "主动受阻后租约已释放，持有者角色应能重新领取"
        );
        assert!(
            check_permission(&表, Role::Implementer, Status::Blocked, Status::Wip, Some("c@d"), "a@b").is_err(),
            "别人端着租约时不能抢"
        );
        assert!(
            check_permission(&表, Role::Implementer, Status::Wip, Status::Done, None, "a@b").is_err(),
            "离开 wip 必须是持有者"
        );
    }

    #[test]
    fn 通配转移覆盖任意起点() {
        let 表 = 权限表();
        for from in [Status::Todo, Status::Wip, Status::Blocked] {
            assert!(check_permission(&表, Role::Planner, from, Status::Dropped, None, "a@b").is_ok(), "{from:?} 可丢弃");
        }
        assert!(check_permission(&表, Role::Implementer, Status::Wip, Status::Dropped, Some("a@b"), "a@b").is_ok());
        assert!(check_permission(&表, Role::Implementer, Status::Wip, Status::Dropped, None, "a@b").is_err());
    }

    #[test]
    fn 未授予的角色被拒并列出可用角色() {
        let 表 = vec![授予("lead", &["a@b"]), 授予("implementer", &["*"])];
        assert!(check_granted(&表, Role::Lead, "a@b").is_ok());
        assert!(check_granted(&表, Role::Implementer, "谁都能").is_ok(), "星号表示任何 actor");

        let e = check_granted(&表, Role::Lead, "e@f").unwrap_err();
        assert_eq!(e.code(), "validation");
        assert!(e.to_string().contains("implementer"), "要列出该 actor 可用的角色：{e}");
    }

    #[test]
    fn 六种报告映射到四阶段() {
        let 期待 = [
            (ReportKind::Progress, Stage::Working),
            (ReportKind::Ready, Stage::WaitingForClaim),
            (ReportKind::Waiting, Stage::WaitingForPerson),
            (ReportKind::Blocked, Stage::WaitingForPerson),
            (ReportKind::Failed, Stage::WaitingForPerson),
            (ReportKind::Dropped, Stage::Idle),
        ];
        for (kind, stage) in 期待 {
            assert_eq!(derive_stage(Status::Wip, Some(kind), false), stage, "{kind:?} 应推出 {stage:?}");
        }
    }

    #[test]
    fn 租约与终态优先于报告() {
        assert_eq!(derive_stage(Status::Wip, Some(ReportKind::Waiting), true), Stage::Working, "手里有活儿就是工作中");
        assert_eq!(derive_stage(Status::Done, Some(ReportKind::Progress), true), Stage::Idle, "终态恒为空闲");
        assert_eq!(derive_stage(Status::Todo, None, false), Stage::Idle, "没有报告也没有租约即空闲");
    }

    #[test]
    fn 字段方案迁移丢未知键丢坏值补缺省() {
        let specs = vec![
            FieldDef { name: "severity".into(), kind: FieldKind::Enum, values: vec!["low".into(), "high".into()], required: true, max_len: None, default: None },
            FieldDef { name: "retries".into(), kind: FieldKind::Int, values: vec![], required: false, max_len: None, default: Some("3".into()) },
        ];
        let 原值 = 值(&[("severity", Value::from("critical")), ("junk", Value::from(1))]);
        let 结果 = normalize_fields("bug", &specs, &原值);
        assert_eq!(结果.fields.get("retries").and_then(Value::as_i64), Some(3), "缺省值要补上");
        assert!(结果.fields.get("severity").is_none(), "枚举越界丢弃");
        assert!(结果.fields.get("junk").is_none(), "未知键丢弃");
        assert_eq!(结果.dropped.len(), 2);
        assert_eq!(结果.unresolved, vec!["severity".to_string()], "必填且无缺省的要标出来");

        // 幂等：再跑一次没有改动
        let 再跑 = normalize_fields("bug", &specs, &结果.fields);
        assert!(再跑.dropped.is_empty(), "第二次不该再有丢弃：{:?}", 再跑.dropped);
        assert_eq!(再跑.fields, 结果.fields);
    }
}
