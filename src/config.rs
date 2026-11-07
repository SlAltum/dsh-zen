//! 配置层。只读加载；唯一写例外是 `write_defaults`（供 `init`）。
//!
//! 数据目录 `.dsh-zen/` 从当前目录向上查找，遇到仓库根（含 `.git`）而仍未找到即报错。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::domain::{Effect, FieldDef, Grant, Guard, Predicate, RoleGrant, Status, TypeDef};
use crate::error::{Result, ZenError};

/// 数据目录名，被管理仓库里的位置。
pub const DATA_DIR: &str = ".dsh-zen";
/// 数据库文件名。
pub const DB_FILE: &str = "zen.zendb";
/// 四份配置文件。
pub const CONFIG_FILES: [&str; 4] = ["issuetypes.toml", "fields.toml", "workflow.toml", "roles.toml"];

#[derive(Debug, Deserialize)]
pub struct Config {
    pub types: Vec<TypeDef>,
    pub fields: Vec<FieldDef>,
    pub workflow: Workflow,
    pub roles: Roles,
}

#[derive(Debug, Deserialize)]
struct TypesFile {
    #[serde(default)]
    types: Vec<TypeDef>,
}

#[derive(Debug, Deserialize)]
struct FieldsFile {
    #[serde(default)]
    fields: Vec<FieldDef>,
}

impl Config {
    /// 类型名 → 该类型绑定的字段定义，顺序即配置里的声明顺序。
    /// 未知类型报 `Validation` 并列出可用类型，不回落默认方案。
    pub fn field_specs(&self, type_name: &str) -> Result<Vec<FieldDef>> {
        self.type_def(type_name)?.fields.iter().map(|name| self.field_def(name).cloned()).collect()
    }

    pub fn type_def(&self, type_name: &str) -> Result<&TypeDef> {
        self.types.iter().find(|t| t.name == type_name).ok_or_else(|| ZenError::Validation {
            field: "type".into(),
            why: format!("未声明的类型 {type_name}，可用：{}", self.available_types()),
        })
    }

    /// 加载时校验：状态名必须是代码里的常量（R7），视图谓词必须是已知谓词。
    /// 配置写错在加载处就炸，别等到某个命令才用上这条转移。
    fn validate(&self) -> Result<()> {
        for t in &self.workflow.transitions {
            if t.from != "*" && Status::parse(&t.from).is_none() {
                return Err(ZenError::Config {
                    file: "workflow.toml".into(),
                    why: format!("转移的起点 {} 不是已知状态（todo / wip / done / blocked / dropped，或通配 *）", t.from),
                });
            }
            if Status::parse(&t.to).is_none() {
                return Err(ZenError::Config {
                    file: "workflow.toml".into(),
                    why: format!("转移的终点 {} 不是已知状态", t.to),
                });
            }
        }
        for v in &self.workflow.views {
            if Predicate::parse(&v.predicate).is_none() {
                return Err(ZenError::Config {
                    file: "workflow.toml".into(),
                    why: format!("视图 {} 的谓词 {} 不认识", v.name, v.predicate),
                });
            }
        }
        Ok(())
    }

    /// 按 from→to 找转移，通配 `*` 也认。找不到报 guard 并列出允许的目标态。
    pub fn transition(&self, from: Status, to: Status) -> Result<&TransitionDef> {
        let 命中 = self
            .workflow
            .transitions
            .iter()
            .find(|t| (t.from == from.as_str() || t.from == "*") && t.to == to.as_str());
        命中.ok_or_else(|| ZenError::Guard {
            transition: format!("{}→{}", from.as_str(), to.as_str()),
            missing: format!("状态机不允许；{} 可去往：{}", from.as_str(), self.targets_from(from)),
        })
    }

    /// 某个状态能去往的目标态，按配置顺序。
    fn targets_from(&self, from: Status) -> String {
        let 目标: Vec<&str> = self
            .workflow
            .transitions
            .iter()
            .filter(|t| t.from == from.as_str() || t.from == "*")
            .map(|t| t.to.as_str())
            .collect();
        if 目标.is_empty() { "（无）".to_string() } else { 目标.join(" / ") }
    }

    /// 视图名 → 定义。未知名字报验证错误并列出可用视图。
    pub fn view(&self, name: &str) -> Result<&ViewDef> {
        self.workflow.views.iter().find(|v| v.name == name).ok_or_else(|| ZenError::Validation {
            field: "view".into(),
            why: format!("未声明的视图 {name}，可用：{}", self.available_views()),
        })
    }

    fn available_views(&self) -> String {
        self.workflow.views.iter().map(|v| v.name.as_str()).collect::<Vec<_>>().join(" / ")
    }

    pub fn field_def(&self, name: &str) -> Result<&FieldDef> {
        self.fields.iter().find(|f| f.name == name).ok_or_else(|| ZenError::Validation {
            field: name.to_string(),
            why: "字段方案里没有定义".into(),
        })
    }

    fn available_types(&self) -> String {
        self.types.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(" / ")
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct Workflow {
    #[serde(default)]
    pub transitions: Vec<TransitionDef>,
    #[serde(default)]
    pub views: Vec<ViewDef>,
    #[serde(default)]
    pub tunables: Tunables,
}

#[derive(Debug, Deserialize)]
pub struct TransitionDef {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub guard: Vec<String>,
    #[serde(default)]
    pub effects: Vec<String>,
}

impl TransitionDef {
    /// 配置里的守卫名 → 领域守卫。拼错即报，不静默跳过。
    pub fn guard_kinds(&self) -> Result<Vec<Guard>> {
        self.guard
            .iter()
            .map(|name| {
                Guard::parse(name).ok_or_else(|| ZenError::Config {
                    file: "workflow.toml".into(),
                    why: format!("转移 {}→{} 的守卫 {name} 不认识", self.from, self.to),
                })
            })
            .collect()
    }
}

impl TransitionDef {
    /// 配置里的副作用名 → 领域效果。拼错即报，不静默跳过。
    pub fn effect_kinds(&self) -> Result<Vec<Effect>> {
        self.effects
            .iter()
            .map(|name| {
                Effect::parse(name).ok_or_else(|| ZenError::Config {
                    file: "workflow.toml".into(),
                    why: format!("转移 {}→{} 的效果 {name} 不认识", self.from, self.to),
                })
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
pub struct ViewDef {
    pub name: String,
    pub predicate: String,
    #[serde(default)]
    pub columns: Vec<String>,
}

impl ViewDef {
    /// 配置里的谓词名 → 领域谓词。拼错即报，不静默当成空视图。
    pub fn predicate_kind(&self) -> Result<Predicate> {
        Predicate::parse(&self.predicate).ok_or_else(|| ZenError::Config {
            file: "workflow.toml".into(),
            why: format!("视图 {} 的谓词 {} 不认识", self.name, self.predicate),
        })
    }
}

/// 时间相关的阈值。是运维参数，不参与进度计算。
#[derive(Debug, Serialize, Deserialize)]
pub struct Tunables {
    #[serde(default = "default_lease_ttl")]
    pub lease_ttl_minutes: u32,
    #[serde(default = "default_review_days")]
    pub review_after_days: u32,
    #[serde(default = "default_stale_days")]
    pub stale_after_days: u32,
}

impl Default for Tunables {
    fn default() -> Self {
        Tunables {
            lease_ttl_minutes: default_lease_ttl(),
            review_after_days: default_review_days(),
            stale_after_days: default_stale_days(),
        }
    }
}

fn default_lease_ttl() -> u32 {
    30
}
fn default_review_days() -> u32 {
    7
}
fn default_stale_days() -> u32 {
    14
}

/// 一个角色的职责包（R34：新建角色无需改代码）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RoleDef {
    #[serde(default)]
    pub duty: String,
    #[serde(default)]
    pub workdir: String,
    #[serde(default)]
    pub ttl_minutes: u32,
    #[serde(default)]
    pub tools: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Roles {
    #[serde(default)]
    pub roles: std::collections::HashMap<String, RoleDef>,
    #[serde(default)]
    pub grants: toml::Table,
    #[serde(default)]
    pub permissions: Vec<Grant>,
    #[serde(default)]
    pub context_sections: toml::Table,
}

impl Roles {
    pub fn role_def(&self, role: &str) -> Option<&RoleDef> {
        self.roles.get(role)
    }

    /// 角色的上下文包字段集。没有配就返回 None，由调用方拒绝而不是回落全量。
    pub fn sections_for(&self, role: &str) -> Option<Vec<String>> {
        self.context_sections.get(role).and_then(|v| v.as_array()).map(|list| {
            list.iter().filter_map(|v| v.as_str()).map(str::to_string).collect()
        })
    }

    /// 配了上下文包的角色名，供报错时列举。
    pub fn section_roles(&self) -> String {
        let mut names: Vec<&str> = self.context_sections.keys().map(|k| k.as_str()).collect();
        names.sort_unstable();
        names.join(" / ")
    }

    /// 授予表转成值：角色 → 允许的 actor。
    pub fn role_grants(&self) -> Vec<RoleGrant> {
        self.grants
            .iter()
            .map(|(role, actors)| RoleGrant {
                role: role.clone(),
                actors: actors
                    .as_array()
                    .map(|list| list.iter().filter_map(|v| v.as_str()).map(str::to_string).collect())
                    .unwrap_or_default(),
            })
            .collect()
    }
}

/// `init` 的落点：调用方显式给的项目根。
///
/// 这是 IDE 的模型——你打开哪个目录，哪个就是项目；我们不去看 `.git`、不从当前目录推。
/// 祖先目录里已有 `.dsh-zen` 时拒绝，保证一个项目一个看板。
pub fn init_root(requested: Option<&Path>, cwd: &Path) -> Result<PathBuf> {
    let path = requested.ok_or_else(|| ZenError::Config {
        file: DATA_DIR.into(),
        why: "init 需要显式的项目根：给 root（命令行用 --root）".into(),
    })?;
    let abs = std::path::absolute(if path.is_absolute() { path.to_path_buf() } else { cwd.join(path) })
        .unwrap_or_else(|_| path.to_path_buf());
    if !abs.is_dir() {
        return Err(ZenError::Config {
            file: DATA_DIR.into(),
            why: format!("{} 不是目录", abs.display()),
        });
    }
    if let Some(上层) = 祖先看板(&abs) {
        return Err(ZenError::Config {
            file: DATA_DIR.into(),
            why: format!(
                "{} 已经存在；一个项目只允许一个看板，请改用它的项目根",
                上层.display()
            ),
        });
    }
    Ok(abs)
}

/// 从 `start` 的父目录向上找已有的 `.dsh-zen`。
fn 祖先看板(start: &Path) -> Option<PathBuf> {
    let mut cur = start.parent();
    while let Some(dir) = cur {
        let candidate = dir.join(DATA_DIR);
        if candidate.is_dir() {
            return Some(candidate);
        }
        cur = dir.parent();
    }
    None
}

/// 从 `start` 向上查找数据目录。跨过仓库根即报错，避免误写到父仓库。
pub fn find_data_dir(start: &Path) -> Result<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        let candidate = dir.join(DATA_DIR);
        if candidate.is_dir() {
            return Ok(candidate);
        }
        if dir.join(".git").exists() {
            break;
        }
        cur = dir.parent();
    }
    Err(ZenError::Config {
        file: DATA_DIR.into(),
        why: format!(
            "从 {} 向上未找到 .dsh-zen（跨过仓库根即停）。请先在项目根运行 zen_init(root=<项目根>)，或将会话工作目录切到已建看板的项目",
            start.display()
        ),
    })
}

/// 加载四份配置。任一缺失或不可解析即报 `config`，不降级。
pub fn load(data_dir: &Path) -> Result<Config> {
    let mut raw = Vec::with_capacity(CONFIG_FILES.len());
    for name in CONFIG_FILES {
        let path = data_dir.join(name);
        let text = fs::read_to_string(&path).map_err(|e| ZenError::Config {
            file: name.into(),
            why: if e.kind() == std::io::ErrorKind::NotFound {
                format!("配置缺失：先运行 zen_init(root=<项目根>) 生成默认配置（数据目录 {}）", data_dir.display())
            } else {
                e.to_string()
            },
        })?;
        let value: toml::Table = toml::from_str(&text).map_err(|e| ZenError::Config {
            file: name.into(),
            why: e.to_string(),
        })?;
        raw.push(value);
    }
    let [issuetypes, fields, workflow, roles] = raw
        .try_into()
        .map_err(|_| ZenError::Config { file: DATA_DIR.into(), why: "配置数量不符".into() })?;
    let type_file: TypesFile = TypesFile::deserialize(issuetypes).map_err(|e| ZenError::Config {
        file: "issuetypes.toml".into(),
        why: e.to_string(),
    })?;
    let field_file: FieldsFile = FieldsFile::deserialize(fields).map_err(|e| ZenError::Config {
        file: "fields.toml".into(),
        why: e.to_string(),
    })?;
    let workflow: Workflow = Workflow::deserialize(workflow).map_err(|e| ZenError::Config {
        file: "workflow.toml".into(),
        why: e.to_string(),
    })?;
    let roles: Roles = Roles::deserialize(roles).map_err(|e| ZenError::Config {
        file: "roles.toml".into(),
        why: e.to_string(),
    })?;
    let cfg = Config { types: type_file.types, fields: field_file.fields, workflow, roles };
    cfg.validate()?;
    Ok(cfg)
}

/// 写默认配置。已存在的文件跳过，不覆盖。返回实际写入的路径。
pub fn write_defaults(root: &Path) -> Result<Vec<PathBuf>> {
    let dir = root.join(DATA_DIR);
    fs::create_dir_all(&dir)?;
    let mut written = Vec::new();
    for (name, body) in DEFAULTS {
        let path = dir.join(name);
        if path.exists() {
            continue;
        }
        fs::write(&path, body)?;
        written.push(path);
    }
    Ok(written)
}
const DEFAULTS: [(&str, &str); 4] = [
    ("issuetypes.toml", include_str!("../defaults/issuetypes.toml")),
    ("fields.toml", include_str!("../defaults/fields.toml")),
    ("workflow.toml", include_str!("../defaults/workflow.toml")),
    ("roles.toml", include_str!("../defaults/roles.toml")),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{FieldKind, Guard, Role, check_granted, check_permission};

    /// 一份真实的默认配置，写在独立临时目录。
    fn 临时配置(tag: &str) -> (PathBuf, Config) {
        let root = std::env::temp_dir().join(format!("dsh-zen-cfg-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write_defaults(&root).unwrap();
        let cfg = load(&root.join(DATA_DIR)).unwrap();
        (root, cfg)
    }

    /// 找不到看板的报错要带起始目录与处置指引，让使用者能自己定位。
    #[test]
    fn 找不到看板时报起始目录() {
        let 根 = std::env::temp_dir().join(format!("dsh-zen-find-none-{}", std::process::id()));
        let _ = fs::remove_dir_all(&根);
        let 深 = 根.join("a").join("b");
        fs::create_dir_all(&深).unwrap();

        let err = find_data_dir(&深).unwrap_err();
        assert_eq!(err.code(), "config");
        let msg = err.to_string();
        assert!(msg.contains("a") && msg.contains("b"), "报错要带起始路径：{msg}");
        assert!(msg.contains("zen_init"), "报错要给下一步：{msg}");

        // 配置缺失与解析失败分开报：缺文件指向 zen_init，不是 os error 原文。
        write_defaults(&根).unwrap();
        fs::remove_file(根.join(DATA_DIR).join("fields.toml")).unwrap();
        let err = load(&根.join(DATA_DIR)).unwrap_err();
        assert!(err.to_string().contains("zen_init"), "缺配置要指向 init：{err}");

        let _ = fs::remove_dir_all(&根);
    }

    /// init 只认调用方显式给的项目根；祖先已有看板则拒绝。
    #[test]
    fn init_只认显式项目根() {        let 根 = std::env::temp_dir().join(format!("dsh-zen-init-root-{}", std::process::id()));
        let _ = fs::remove_dir_all(&根);
        fs::create_dir_all(根.join("a").join("b")).unwrap();

        assert_eq!(init_root(Some(&根), &根).unwrap(), 根, "显式给的项目根原样使用");
        assert_eq!(init_root(Some(&根.join("a").join("b")), &根).unwrap(), 根.join("a").join("b"), "子目录原样作为项目根");

        let 没给 = init_root(None, &根).unwrap_err();
        assert_eq!(没给.code(), "config");
        assert!(没给.to_string().contains("显式"), "要说清缺什么：{没给}");

        let 缺 = 根.join("不存在");
        assert_eq!(init_root(Some(&缺), &根).unwrap_err().code(), "config");

        // 祖先已有看板：拒绝
        fs::create_dir_all(根.join(DATA_DIR)).unwrap();
        let 嵌套 = init_root(Some(&根.join("a")), &根).unwrap_err();
        assert_eq!(嵌套.code(), "config");
        assert!(嵌套.to_string().contains(DATA_DIR), "要说清冲突在哪：{嵌套}");
        // 同一个目录重复 init 仍然放行（幂等修复）
        assert_eq!(init_root(Some(&根), &根).unwrap(), 根);

        let _ = fs::remove_dir_all(&根);
    }

    #[test]
    fn 两个类型的字段集不同() {
        let (root, cfg) = 临时配置("kinds");
        let task: Vec<_> = cfg.field_specs("task").unwrap().into_iter().map(|f| f.name).collect();
        let bug: Vec<_> = cfg.field_specs("bug").unwrap().into_iter().map(|f| f.name).collect();
        assert!(task.is_empty(), "task 没有自定义字段：{task:?}");
        assert_eq!(bug, vec!["severity".to_string()]);
        assert_eq!(cfg.field_specs("bug").unwrap()[0].kind, FieldKind::Enum);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 字段方案带出校验规则() {
        let (root, cfg) = 临时配置("rules");
        let severity = cfg.field_specs("bug").unwrap().remove(0);
        assert!(severity.required);
        assert_eq!(severity.values, vec!["low".to_string(), "medium".into(), "high".into()]);
        let outcome = cfg.field_specs("research").unwrap().remove(0);
        assert!(!outcome.required);
        assert_eq!(outcome.max_len, Some(200));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 未知类型被拒并列出可用类型() {
        let (root, cfg) = 临时配置("unknown");
        let e = cfg.field_specs("epic").unwrap_err();
        assert_eq!(e.code(), "validation");
        assert!(e.to_string().contains("task"), "应列出可用类型：{e}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 视图名从配置解析() {
        let (root, cfg) = 临时配置("views");
        assert_eq!(cfg.view("ready").unwrap().predicate_kind().unwrap(), Predicate::Ready);
        assert_eq!(cfg.view("review").unwrap().predicate_kind().unwrap(), Predicate::Review);
        assert_eq!(cfg.view("ready").unwrap().columns.len(), 4, "列集来自配置");
        let e = cfg.view("sprint").unwrap_err();
        assert_eq!(e.code(), "validation");
        assert!(e.to_string().contains("backlog"), "应列出可用视图：{e}");
        let _ = fs::remove_dir_all(&root);
    }

    fn 找转移<'a>(cfg: &'a Config, from: &str, to: &str) -> &'a TransitionDef {
        cfg.workflow.transitions.iter().find(|t| t.from == from && t.to == to).expect("配置里应有这条转移")
    }

    #[test]
    fn 转移的守卫名从配置解析() {
        let (root, cfg) = 临时配置("guards");
        let 完成 = 找转移(&cfg, "wip", "done");
        assert_eq!(完成.guard_kinds().unwrap(), vec![Guard::AcceptanceAllChecked, Guard::ArtifactPresent]);
        assert_eq!(完成.effects, vec!["release_lease".to_string()]);
        assert!(找转移(&cfg, "todo", "wip").guard_kinds().unwrap().is_empty());

        // 拼错守卫名：加载仍成功，用到这条转移时才报配置错误
        let path = root.join(DATA_DIR).join("workflow.toml");
        let bad = fs::read_to_string(&path).unwrap().replace("acceptance_all_checked", "acceptance_checked");
        fs::write(&path, bad).unwrap();
        let cfg = load(&root.join(DATA_DIR)).unwrap();
        let e = 找转移(&cfg, "wip", "done").guard_kinds().unwrap_err();
        assert_eq!(e.code(), "config");
        assert!(e.to_string().contains("acceptance_checked"), "{e}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 权限矩阵与规格逐格对齐() {
        let (root, cfg) = 临时配置("perms");
        use crate::domain::Status::*;
        let 允许 = |role, from, to, holder: Option<&str>| {
            check_permission(&cfg.roles.permissions, role, from, to, holder, "a@b").is_ok()
        };

        assert!(允许(Role::Reviewer, Todo, Wip, None), "§4.2：todo→wip 对所有人开放");
        assert!(允许(Role::Lead, Wip, Done, None), "lead 是直接授权，不看租约");
        assert!(允许(Role::Implementer, Wip, Done, Some("a@b")), "implementer 是持有者时可以完成");
        assert!(!允许(Role::Implementer, Wip, Done, Some("c@d")), "租约在别人手上就不行");
        assert!(!允许(Role::Planner, Wip, Done, Some("a@b")), "§4.2 这一格 planner 是空的");
        assert!(!允许(Role::Reviewer, Wip, Done, Some("a@b")));
        assert!(允许(Role::Planner, Blocked, Wip, None));
        assert!(允许(Role::Tester, Blocked, Wip, Some("a@b")));
        assert!(!允许(Role::Reviewer, Wip, Todo, Some("a@b")));
        assert!(允许(Role::Planner, Todo, Dropped, None), "丢弃是通配转移");
        assert!(!允许(Role::Tester, Todo, Dropped, None));

        let 授予 = cfg.roles.role_grants();
        assert_eq!(授予.len(), 5, "五个角色各一行授予");
        assert!(check_granted(&授予, Role::Tester, "任何人").is_ok(), "默认授予表对所有人开放");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn 配置里的状态名与谓词名在加载时校验() {
        let (root, _) = 临时配置("badstate");
        let path = root.join(DATA_DIR).join("workflow.toml");
        let 改坏的转移 = fs::read_to_string(&path).unwrap().replace(r#"from = "todo""#, r#"from = "doing""#);
        fs::write(&path, &改坏的转移).unwrap();
        let e = load(&root.join(DATA_DIR)).unwrap_err();
        assert_eq!(e.code(), "config");
        assert!(e.to_string().contains("doing"), "要指出坏在哪：{e}");

        let 改坏的谓词 = 改坏的转移.replace(r#"from = "doing""#, r#"from = "todo""#).replace(r#"predicate = "ready""#, r#"predicate = "readee""#);
        fs::write(&path, &改坏的谓词).unwrap();
        let e = load(&root.join(DATA_DIR)).unwrap_err();
        assert_eq!(e.code(), "config");
        assert!(e.to_string().contains("readee"), "{e}");
        let _ = fs::remove_dir_all(&root);
    }
}

