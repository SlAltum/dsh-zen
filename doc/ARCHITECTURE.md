# dsh-zen 架构设计

依据：[SRS](SRS.md) · [PRD](PRD.md)（引用 SRS 的 §x 与 R 编号）
性质：本文件只回答**模块边界、依赖方向、层级、技术选型**。表结构、SQL 谓词、错误码、索引、打包细节属详细设计，随任务下放到实施计划，按代码对照编写。

## 1. 约束

三条外部约束决定了全部选型，其余设计由它们推出：

| 约束 | 推论 |
|---|---|
| 主用户是 agent，不是人 | 输出可编程解析、错误可分类、配置自描述 |
| **运行在 DSH 宿主内，形态是插件** | 能力以**内置工具**暴露给模型；不走 skill，不做 MCP server |
| 单仓库单库，多进程共享，**无守护进程** | 无内存态 → 无热重载、无会话、无后台任务；每次调用重读配置、重连库 |
| 纯本地，无网络，**运行时零依赖** | 交付单个自包含二进制；构建期允许引入 crate，但锁版本并固定清单 |

## 2. 技术选型

| 维度 | 选型 | 备选与否决理由 |
|---|---|---|
| 交付形态 | **DSH 插件**：TS 适配层注册内置工具，Rust 核心以 sidecar 运行 | 独立 CLI：模型看不到，得靠 shell 调，等于让 agent 自己拼命令；skill：能力靠提示词约束，没有类型与校验；MCP server：在 DSH 内是多余的一跳 |
| 核心语言 | Rust（2024 edition） | Python：解释执行，类型约束不足；Go：GC 与错误处理风格不符合本项目对确定性的要求 |
| 适配层语言 | TypeScript | 这是 DSH 插件模型的约束，不是选择：Cordis 插件是 `apply(ctx)` 的 TS 模块，工具注册在 `ctx.tools` |
| 插件与核心通信 | NDJSON over stdio（每次调用一个进程） | napi-rs 原生绑定：无进程开销但需按平台预编译，收益不抵复杂度；常驻 sidecar：要自己管生命周期，而核心本是无状态的 |
| 存储 | SQLite，WAL | JSONL + 索引：要自建事务与 CAS；单 JSON：单写者假设，并发即坏 |
| SQLite 绑定 | `rusqlite`，启用 `bundled` | 链接系统库：各平台版本不一致；`sqlx`：异步运行时对本项目是净负担 |
| 配置格式 | TOML（`toml` crate） | YAML：Rust 生态无稳定官方解析器（`serde_yaml` 已归档）；JSON：不可注释，方案类配置改动频繁需要说明 |
| 进程模型 | 一次性进程 | 守护进程：带来生命周期、重启、状态恢复三类成本，收益为零 |
| 并发仲裁 | SQLite 自身锁 | 文件锁：只适合做“礼让”，不能仲裁 |

**分发形态**：插件包随 DSH 安装（`package.json` 的 `dsh.bundle.patch` + `cordis.patch.yml`）；Rust 二进制作为插件资源随包分发，由插件按配置定位。跨平台各自构建。

## 3. 分层与依赖方向

依赖只能指向内层。业务逻辑在最内层，基础设施在外层。

```
DSH 宿主进程 ──────────────────────────────
  plugin (TS)   注册工具 · 参数校验 · 结果渲染
      │
      │  NDJSON over stdio（进程边界）
      ▼
  dsh-zen (Rust) ──────────────────────────
  cli ──► commands ──► store
                   └─► config
                           │ 依赖
                           ▼
                        domain          最内层，零依赖
```

箭头指向被依赖者。进程内所有箭头最终汇到 `domain`，没有一条从 `domain` 指出；跨进程那条是唯一的调用入口，用 NDJSON 传递，不共享内存。

**为什么适配层在进程外**：DSH 插件跑在宿主进程里，崩溃会带走整个会话；Rust 核心独立成进程，解析错误、SQLite 异常都只让一次工具调用失败。这条边界同时让核心可以用子命令直接跑测试，不必启动宿主。

| 从 | 依赖 | 理由 |
|---|---|---|
| `plugin`（TS） | `cli` 的进程接口 | 只做工具注册、参数校验、结果渲染；不实现任何规则 |
| `cli` | `commands` | 子命令与 NDJSON 两个入口，都是薄适配 |
| `commands` | `domain` · `store` · `config` | 编排用例，调用两侧 |
| `store` | `domain` | 落库与查询都要用领域类型 |
| `config` | `domain` | 把 toml 解析成领域类型 |
| `domain` | **无** | 最内层：不碰 IO、不碰 crate 的 IO 能力 |

**存储层不是业务逻辑。** 它是可以替换的实现细节：换成别的库、换成内存实现，业务规则不该跟着改。把存储放进内层，等于让规则依赖数据库，规则就再也无法脱离 IO 断言。

所以 `domain` 只接收值、返回判定，不 import `store` 与 `config`。这是全部规则能脱离数据库测试的前提。

> 当前 `commands` 直接依赖 `store` 的具体 API。若将来需要脱离数据库测试 `commands`，把 `store` 的接口抽成 trait，由 `commands` 声明、`store` 实现。那是依赖倒置，加入时机由测试需要触发；现在加只多一层间接。

| 层 | 可依赖 | 禁止 |
|---|---|---|
| cli | commands | 碰 SQL、读 toml |
| commands | domain · store · config | 自行实现规则 |
| **domain** | 仅标准库与 serde / serde_json（纯数据） | 任何 IO、任何 IO 类 crate |
| store | domain · rusqlite · 标准库 | 业务规则 |
| config | domain · toml · serde · 标准库 | 连库 |

## 4. 模块

| 模块 | 职责 | 对外接口 | 依赖 | 被依赖 |
|---|---|---|---|---|
| `plugin`（TS） | 向 `ctx.tools` 注册工具；把调用转成 NDJSON；把响应渲染成模型可读文本与结构化字段 | 每个工具一条 `registerTool`，schema 用 schemastery | Rust 进程 | DSH 宿主 |
| `cli` | 两个入口：子命令（人用与测试）与 NDJSON 服务（插件用）；参数解析与退出码 | `fn main() -> ExitCode` | commands | plugin |
| `commands` | 每条命令一个用例；编排事务与判定顺序；**批量类命令（check / review / migrate / import）的归属层** | 每命令一个函数，返回 `Result<Report, ZenError>` | domain · store · config | cli |
| `store` | 持久化与并发原语；**快照导出/恢复在此**，因表清单本就属它 | `open() · tx() · write_cas() · append_event() · next_id() · export() · restore()` | domain | commands |
| `config` | toml 加载、校验、默认值；**唯一写例外 `write_defaults()`（供 init）** | `load(root) -> Result<Config>` · `write_defaults(root)` | domain | commands |
| `domain` | 全部规则与谓词，纯函数 | `eval_guards() · check_permission() · check_link_permission() · ready() · derive_stage() · normalize_fields()`，入参为值、返回 `Result` | **无** | commands · store · config |

Rust 侧五个模块，对应 `src/` 下五个文件；TS 侧一个 `plugin/` 目录。另有 `error.rs` 与 `domain` 同处最内层：`ZenError` 是全部层共用的契约类型，只允许依赖更内层，所以它不能放在 `store` 或 `commands` 里。

> 错误类型属于接口契约：所有跨模块调用返回 `Result<_, ZenError>`，错误码枚举与 §7 一致。分层边界因此不只是目录结构，而是编译器强制的结果。

五个模块。原设计里的 `views` 与 `snapshot` 已取消：视图是**配置数据**（`workflow.toml` 的 `views:` 段），快照并入 `store`。

**视图不是代码**：每个视图 = 名字 → `domain` 里的谓词引用 + 列集。视图层没有独立的布尔逻辑，因此也就没有第二处需要同步的规则。

## 5. 数据

实体与关系（字段定义见详细设计）：

```
Plan(容器) ─1:N─ Issue ─1:N─ Artifact
                  │  ├─1:N─ Comment
                  │  ├─N:M─ Issue（Link: blocks / relates / duplicates）
                  │  └─1:N─ Acceptance
Issue ─1:N─ Event（审计与报告共同载体；subject 可指向 Issue 或 Plan）
Role ──授予── Actor
```

三条数据层约定，属于架构而非实现：

1. **派生量不落库**：就绪、派生阶段、计划进度、权限矩阵全部由查询或配置得出。存了就要同步，同步就会错。
2. **报告即事件**：结构化报告不建表，是 `event` 的一种。（偏差 1）
3. **终态例外有名单**：`done` / `dropped` 只读，例外仅限 `reviewed_at`、评论、产物三类。

## 6. 关键流程

### 6.1 写路径的判定顺序

一次写操作的判定顺序是架构约定，不可调换：

```text
权限（角色能否发起这次转移）
  → 转移合法性（from 能否到 to）
  → 守卫（字段 / 依赖 / 租约是否满足）
  → 落库（同事务内写状态与事件）
```

四类失败各自独立上报，不合并成一句“操作失败”。理由是 agent 需要据此分流：换角色、换目标态、补条件，还是等租约。

### 6.2 读路径的切片

`context <id> --role <r>` 只取三样：该任务本身、其直接依赖的状态、按角色选定的字段集。不取同计划其它任务正文，不取评论全文。**角色决定字段集，未知角色拒绝而非回落全量**。

### 6.3 导出与恢复

- `export`：只读事务内遍历全表写单文件快照
- `restore`：**不替换数据库文件**，而是连到目标库、在独占事务内清空全部表后重灌
- 全量覆盖，不做增量合并；跨机协作是顺序的（A 导出 → 提交 → B 恢复）

选“事务内重灌”而非“换文件”，是因为前者的并发仲裁由 SQLite 的锁负责，后者只能靠文件锁礼让，中间有窗口。

### 6.4 周期任务

无守护进程，所以没有定时器。review 的周期由 `status` / `check` 在每次调用时比对时间戳得出，执行者是 reviewer 角色的会话。

## 7. 架构取舍

| # | 取舍 | 否决的备选 |
|---|---|---|
| 1 | **状态是常量**：五态写在 `domain`，`workflow.toml` 只能配转换、守卫、效果，不能新增状态名 | 状态完全配置化：会把“终态”从常量变成运行时可变量，租约清理与完成判定全部要动态查配置 |
| 2 | **权限按转移判**，不按动作判 | 按动作判：无法表达 `todo→wip` 与 `wip→done` 权限不同 |
| 3 | **规则只存在一处**：谓词全在 `domain`，视图与工具只引用 | 视图自带过滤逻辑：同一条规则两处实现，必漂移 |
| 4 | **快照并入 store** | 独立 snapshot 模块：要复制一份表清单认知，加一张表改两处 |
| 5 | **视图是配置数据** | 视图是代码：为六个名字建一个模块，不产生任何隔离收益 |
| 6 | **审计覆盖所有写路径** | 只记状态变更：重建出的序列缺产物、勾选、评论，`log` 无法解释 |

## 8. 非目标

无自有 Web 服务与独立前端应用（看板面板寄生在 DSH 宿主的路由里，不另起端口与进程）、无多租户、无跨机并发同步、无查询语言、无守护进程、无审批流。

## 9. 与规格的一致性

架构确认的 4 条架构级取舍已全部落进 [SRS](SRS.md)：

| 取舍 | SRS 落点 |
|---|---|
| 报告即事件 | §2“报告不建表” |
| 权限矩阵入配置 | §4.2、R36 |
| 视图定义入配置 | §5、R17 |
| 状态为常量 | §3.1、R7 |

字段级细节（`block_reason`、`Event` 锚点、actor 推导、CAS 谓词、命令面、`tools`）同样已进 SRS，本文件不再重复。两份文档当前无偏差；新发现的偏差按同一流程回写 SRS。
