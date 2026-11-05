//! 跨层错误契约。与 SRS §7 的错误码一一对应。
//!
//! 本模块与 `domain` 同处最内层：所有层都返回它，因此它不得依赖任何外层类型。

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZenError {
    /// 角色无该动作。
    Permission { action: String, role: String },
    /// 转移非法或守卫不满足。
    Guard { transition: String, missing: String },
    /// 非持有者、已过期或已被接管。
    Lease { holder: String, until: String },
    /// CAS 失败，调用方应重读后重试。
    Conflict { expected: String, actual: String },
    /// 字段类型、必填或枚举不合法。
    Validation { field: String, why: String },
    /// 配置文件非法。
    Config { file: String, why: String },
    /// 数据库忙，可原样重试。
    Retryable { why: String },
    NotFound { what: String },
    Io(String),
}

impl ZenError {
    /// 稳定错误码，用于 `--json` 输出与工具返回。
    pub fn code(&self) -> &'static str {
        match self {
            ZenError::Permission { .. } => "permission",
            ZenError::Guard { .. } => "guard",
            ZenError::Lease { .. } => "lease",
            ZenError::Conflict { .. } => "conflict",
            ZenError::Validation { .. } => "validation",
            ZenError::Config { .. } => "config",
            ZenError::Retryable { .. } => "retryable",
            ZenError::NotFound { .. } => "not_found",
            ZenError::Io(_) => "io",
        }
    }

    /// 原样重试是否会得到不同结果。
    pub fn retryable(&self) -> bool {
        matches!(self, ZenError::Retryable { .. })
    }
}

impl fmt::Display for ZenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ZenError::Permission { action, role } => {
                write!(f, "角色 {role} 无权执行 {action}")
            }
            ZenError::Guard { transition, missing } => {
                write!(f, "转移 {transition} 不满足守卫：{missing}")
            }
            ZenError::Lease { holder, until } => {
                write!(f, "租约被 {holder} 持有至 {until}")
            }
            ZenError::Conflict { expected, actual } => {
                write!(f, "并发冲突：期望 {expected}，实际 {actual}")
            }
            ZenError::Validation { field, why } => write!(f, "字段 {field} 不合法：{why}"),
            ZenError::Config { file, why } => write!(f, "配置 {file} 不合法：{why}"),
            ZenError::Retryable { why } => write!(f, "可重试：{why}"),
            ZenError::NotFound { what } => write!(f, "找不到 {what}"),
            ZenError::Io(why) => write!(f, "IO 错误：{why}"),
        }
    }
}

impl std::error::Error for ZenError {}

impl From<std::io::Error> for ZenError {
    fn from(e: std::io::Error) -> Self {
        ZenError::Io(e.to_string())
    }
}

impl From<rusqlite::Error> for ZenError {
    fn from(e: rusqlite::Error) -> Self {
        // 忙与锁是并发下的正常现象，原样重试即可（R21）；其余才是 IO 故障。
        if matches!(
            e.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
        ) {
            return ZenError::Retryable { why: e.to_string() };
        }
        ZenError::Io(format!("sqlite: {e}"))
    }
}

pub type Result<T> = std::result::Result<T, ZenError>;
