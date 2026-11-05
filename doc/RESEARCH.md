# Agent 项目管理工具：预研与需求分析

## 0. 结论先行

- **这片地已经有人踩过，且已经收敛出三种形态**：单文件 Markdown+frontmatter（Backlog.md）、单文件 JSON（Task Master）、JSONL 真相源 + 索引（beads）。三者都已开源可用，没有一种同时满足“人类可读 diff 友好 + agent 可并发写入 + 有依赖图”。
- **真正的空白不在存储格式，在并发语义**：beads 官方 FAQ 承认“两个 agent 抢同一个 issue 时最后提交者赢”；Symphony 的 `claimed` 集合是进程内存态，重启即失。**“认领租约”是现成工具都缺、而我们的多 subagent 工作流确实需要的一块**。
- **自定义字段与工作流引擎是必需品，且本身不贵**：文件存储没有 SQL schema，字段就是 schemaless 的 frontmatter，工作流就是一次状态转移校验。两者加起来不到一个配置文件和两个校验函数。真正贵的是它们的**外围**：存量数据迁移、表单渲染、多级审批、字段索引、可视化编排。取舍边界画在外围，不画在能力本身。
- **Vibe Kanban 已于 2026 年停止运营**（README 首行 `Vibe Kanban is sunsetting`），说明“纯编排 UI + 无自有任务数据”这条路的商业模型不成立。反例是 Symphony（无 UI，仅 SPEC.md，2026-04-23 已 15K stars）。

## 1. 预研范围与方法

三类各取代表，共 8 个产品 + 1 份第三方横评。取数方式：直接读各仓库 README / 规范原文，不依赖二手评测。
判定维度：**数据存储形态、agent 接口形态、依赖与就绪模型、并发/认领语义、状态机可配置性、给人看的能力**。

## 2. 竞品地图

### 2.1 传统 PM（人类是主用户，agent 通过 MCP 旁挂）

| 产品 | 数据形态 | agent 接口 | 对我们的意义 |
|---|---|---|---|
| Jira / 禅道 | 关系库；禅道为“产品-项目-执行”三层 + 需求/任务/Bug/用例四类对象 | 官方 MCP（Atlassian Rovo） | 概念资产：多层级容器 + 对象类型分离。**不要**继承其工作流引擎与自定义字段 |
| Linear | 托管库 + GraphQL | 官方 remote MCP | 被 Symphony 直接当作**状态机**用：状态名就是调度信号 |
| GitHub Issues | 仓库内对象 | 官方 MCP | “repo 即工作流”的天花板：离线不可用、有速率限制 |
| Asana / Notion | 托管库 | 官方 MCP（workspace 级 scope） | Notion 的反面教材：把看板做成文档库后，任务追踪只是副产品 |

第三方横评（SyntracAI，2026-05）结论值得记：**六家都有官方 MCP，MCP 不是选型依据**；真正的变量是“你的工作本来住在哪”和“给 agent 的写权限 scope 有多窄”。

### 2.2 agent-native 看板（人在同一块板上监督）

| 产品 | 数据形态 | 接口 | 关键设计 | 结局 |
|---|---|---|---|---|
| **Backlog.md** | 一任务一 `.md` 文件（frontmatter + 正文），可选 Git | CLI 默认 + MCP 可选 | 三个 review checkpoint：审规格 → 审计划 → 审代码；验收标准 + 全局 DoD；`--json` 输出**版本化稳定**；archive 保留 ID 与依赖但只读 | 活跃，MIT |
| **Vibe Kanban** | 无自有任务库（卡片 + workspace） | 无（UI 驱动 agent） | 每个 task 一个 git worktree + 终端 + dev server + diff review | **2026 停止运营** |
| **Paca** | PostgreSQL + Redis，自托管 | MCP + `/paca` skills 套件 | agent 作为 Scrum 正式成员；IAM 式角色策略；WASM 插件；事件驱动自动化 | 活跃，Apache-2.0 |

### 2.3 agent 编排器（ticket 即控制平面）

**Symphony**（OpenAI 开源，仅一份 `SPEC.md` + Elixir 参考实现）是这类里唯一把“agent 面对的数据契约”写清楚的，本报告的技术基准：

- 核心不变式一句话：**每个打开的任务，保证有一个 agent 在它自己的工作区里跑着**。人不再按会话管理 agent，改为按 ticket 管理。
- ticket 状态即状态机；`active_states` / `terminal_states` 是可配置字符串列表，不做枚举硬编码。
- 任务树是 DAG：agent 先出计划，人批准后展开成任务树并定义依赖，**未被阻塞的任务自然并行**。
- agent 可以自己建 issue（实现/审查中发现的问题），形成“发现即入账”。
- 工作流策略写在仓库内的 `WORKFLOW.md`（YAML front matter + prompt 正文），支持热重载、非法重载保留上一份可用配置。
- 显式非目标：**无 Web UI、非通用工作流引擎、不做分布式调度器、不内置“怎么改 ticket”的业务逻辑**。
- 已知缺口（作者自述扩展 TODO）：重试队列与会话元数据**不跨进程重启持久化**。

### 2.4 任务存储工具（无编排，纯账本）

| 产品 | 数据形态 | 就绪/依赖 | 并发 | 值得抄的 |
|---|---|---|---|---|
| **beads** (`bd`) | JSONL 为真相源（进 Git）+ SQLite 本地缓存（gitignore） | 四种依赖：`blocks` / `related` / `parent-child` / `discovered-from`；`ready` = open 且无未关闭 blocks；父被阻塞则子递归阻塞 | **无锁**，最后提交者赢 | ID 冲突自动重映射（按引用次数计分重编号并改写全部文本/依赖引用，词边界匹配避免 bd-10 命中 bd-100） |
| **Task Master** | 单 `tasks.json` + `.taskmaster/` | 任务内 subtask + 依赖 | 单写者 | MCP 工具集**可按需裁剪**：36 个工具约 21K tokens，`core` 档 7 个约 5K —— 工具数量本身就是上下文成本 |

## 3. 反复出现的六个设计（可直接取用）

1. **ticket 状态 = 调度信号**。不是给人看的标签，是 agent 的行动依据。状态集合必须可配置，因为“完成”的语义各团队不同（Symphony 允许 run 停在 `Human Review` 而非 `Done`）。
2. **就绪查询是一等公民**。`ready` / `blocked` 是一个算法而非一个筛选器，因为 agent 每次启动都要回答“我现在该干什么”。
3. **依赖要有类型，不能只有一个“阻塞”**。`discovered-from` 让“执行中发现的新工作”有归处，这是 agent 场景特有且高频的关系。
4. **ID 稳定且人类可读**（`ABC-123`），因为 ID 要进工作区目录名、分支名、日志、prompt。
5. **JSON 输出是接口契约，要版本化**。Backlog.md 明确写了 `--json` 是“stable, versioned”，并支持 `--watch` 流式替换 —— 这是给程序读的，不是给人读的顺手开关。
6. **工作流策略放仓库里、可热重载**。`WORKFLOW.md` / `AGENTS.md` 承载流程，非法配置降级到上一份可用配置而不是崩掉。

## 4. 共同踩过的坑（反面证据）

| 坑 | 证据 | 对我们的约束 |
|---|---|---|
| 派生索引与真相源不同步 | beads 用 SQLite 缓存 + 自动导入，代价是双向同步与“database is locked”；本项目历史经验同款：内容已改、索引未改、命令报成功 | 首版**不引入独立索引**，每次调用全量加载建内存索引 |
| 无认领语义导致重复劳动 | beads FAQ“最后提交者赢”；Symphony `claimed` 仅内存态 | 必须加租约字段，见 §6.3 |
| 工具数量吃掉上下文 | Task Master 36 工具 ≈ 21K tokens | CLI 子命令优先，MCP 后置 |
| 单文件巨型 JSON 的合并地狱 | Task Master `tasks.json` 单写者假设 | 一任务一文件，冲突粒度降到最小 |
| 用 UI 承载进度 → 无 UI 就没有数据 | Vibe Kanban 停运 | 数据先于界面；界面永远可选 |
| 人类 PM 复杂度迁移 | Paca 的 IAM 式角色策略 + WASM 插件 + 事件驱动可视化编排 | 字段与状态机**保留**；IAM、插件沙箱、可视化编排列入非目标 |

## 5. 我们现状的差距（以本仓库为样本）

当前用纯文本加 arch-dev 约定管理进度：`.context/plans/*.md` 里一张快速检查表，状态写成 `[TODO]` 这类单元格。实测样本：`plans/` 下 11 份计划文档，`archive/` 仅 1 份，归档靠人主动想起来。

| 现状能力 | 后果 | 需求映射 |
|---|---|---|
| 状态是表格里的字符串 | 无法跨计划查询“现在所有 TODO 是什么”；只能全文 grep，还得解析中文表格 | R1 结构化状态 + 跨计划查询 |
| 无依赖表达 | 并行子 agent 无法判断谁阻塞谁；arch-dev 的原子任务拆分是隐含顺序 | R2 依赖图 + 就绪查询 |
| 无认领/归属 | 同一 session 内多 subagent 或重试可能抢同一任务 | R3 认领租约 |
| 无稳定 ID | 任务是“T3.2 新增 reset 接口”这种一行文字，改名即断引用 | R4 稳定 ID |
| 写入靠整文件替换 | 两个 agent 同时改一份 md → 行级冲突；无原子操作 | R5 单任务单文件 + 原子命令 |
| 无审计 | 谁在什么时候把 WIP 改回 TODO，无痕迹 | R6 追加式事件记录 |

## 6. 需求分析

### 6.1 用户与场景

主用户是 **agent 自身**（含 subagent），次用户是**监督的人**。场景按频率排序：

1. 会话启动：“有哪些可做的工作”→ 就绪查询。
2. 执行中：“这块要不要现在做 / 我先占住”→ 认领、状态流转。
3. 执行中发现新工作 → 建任务并挂 `discovered-from`。
4. 人审查：看板视图 + 变更历史。

### 6.2 需求分级

**必须（MVP）**

| ID | 需求 | 验收判据 |
|---|---|---|
| R1 | 任务以结构化记录存储，状态枚举固定四态 | 能列出全部任务并按其状态过滤，无需解析正文 |
| R2 | 依赖关系（阻塞、父级、发现来源三类）与就绪查询 | 给定 DAG，`ready` 只返回无未完成阻塞项的任务；成环时明确报错 |
| R3 | 认领租约：认领带 owner + 过期时间，认领冲突时命令**失败**而非覆盖 | 两个并发认领，第二个返回失败并给出当前持有者 |
| R4 | 稳定人类可读 ID（前缀 + 序号），创建后不可变 | 重命名标题不影响 ID 与全部引用 |
| R5 | 一任务一文件；所有写操作是原子命令，不留半写状态 | 写入过程中断后，读数仍为旧值或新值之一 |
| R6 | 追加式事件记录（状态变更、认领、评论），可回溯 | 任取一任务，能按时间序重建其状态变化 |
| R7 | 机器可读输出：全部读命令支持 `--json`，格式版本化 | 同一命令在版本内输出 schema 不变 |
| R15 | 自定义字段：项目级 schema 声明字段（名称 / 类型 / 必填 / 枚举取值 / 默认值 / 适用任务类型），任务记录以字段包承载，写入时校验 | 声明 `estimate: int` 后写 `estimate: "abc"` 被拒绝并报类型错；写未声明字段被拒绝；缺必填字段被拒绝 |
| R16 | 工作流状态机：状态集合、合法流转、守卫条件、流转效果均由项目级配置声明，不硬编码在代码里 | 配置禁止 `todo→done` 后该流转被拒绝并列出允许的目标态；守卫不满足时报告缺哪一条而非只说“不合法” |

**应该（第二批）**

- R8 计划文档与任务的双向关联：计划记录自身的状态与背景，任务挂在计划下。
- R9 归档语义：归档保留 ID 与依赖但只读，可恢复。
- R10 校验命令（只读）：非法文件名、断引用、环、重复 ID、租约过期，**以及 schema / 工作流配置自身的合法性**。
- R11 人类只读视图：终端看板，按状态分列并可显示指定自定义字段（不做 Web）。
- R17 字段迁移：schema 变更时对存量记录的迁移命令，带 dry-run 与逐条变更报告。
- R18 工作流效果：流转时执行有限动作——创建子任务、修改字段、追加事件、派发 agent 命令。动作集封闭，不支持内联任意脚本。

**可以（按需）**

- R12 MCP 封装（在 CLI 稳定之后）。
- R13 跨仓库 / 多项目聚合。
- R14 与 Git 的自动提交钩子。
- R19 流转的定时与外部事件触发（cron / webhook 类），不在 v1。

### 6.3 数据模型草案

单任务一份记录，最小字段集（多余字段一律不加，用到再加）：

```yaml
id: PM-12                 # 不可变
title: 修复登录限流
type: task                # task | bug | epic | research
status: todo              # 取值来自工作流定义，非硬编码
priority: 2               # 0 最高
depends_on: [PM-11]       # blocks：PM-12 需 PM-11 完成
parent: PM-3              # parent-child
discovered_from: PM-7     # 执行中发现
claim: {owner: null, until: null}   # 租约
fields:                   # 自定义字段，写入时按 schema 校验
  module: auth
  estimate: 3
acceptance: ["..."]
created_at: ...
updated_at: ...
```

正文（Markdown）承载描述与计划，不参与查询。**这是“结构化数据”与“人可读”的分界：可查询的进字段，可阅读的进正文。**

两类项目级配置文件随仓库进 Git、支持热重载、非法时保留上一份可用配置（同 Symphony 的 `WORKFLOW.md` 语义）：

- `schema.yml`：自定义字段声明，`name` / `type`（string｜int｜bool｜date｜enum｜list）/ `required` / `enum` 取值 / `default` / `applies_to`（任务类型白名单）。
- `workflow.yml`：状态与流转，`initial` 初始态、`states`（名称 + 是否终态）、`transitions`（`from → to` + `guard` + `effects`）。

默认值取 arch-dev 既有四态（`todo → wip → done`，任意态可 `deprecated`），但它是**配置而非代码**：团队改成 `todo → in_review → done` 不需要改工具。

### 6.4 待裁定的四个岔路口

**A. 存储格式**

| 方案 | 优点 | 代价 | 现成先例 |
|---|---|---|---|
| 单任务一 `.md`（frontmatter + 正文） | 人直接读得到、Git diff 按任务隔离、合并冲突粒度最小 | 查询要解析全部文件（10²–10³ 量级可接受） | Backlog.md |
| 单文件 `.jsonl` | 追加友好、解析零成本 | 改一行要重写整行；合并冲突集中在一个文件 | beads |
| 单 `.json` | 最省事的原型 | 单写者假设，并发即坏 | Task Master |

建议：**一任务一 `.md`**。理由是本仓库是文档型仓库，人读的价值高于解析成本；查询成本用“每次调用全量加载建内存索引”抵消，不引 SQLite。
`ponytail:` 全量扫描的内存索引在 10⁴ 任务以上会开始慢，届时再衍生出 SQLite 缓存；现在加缓存等于先把 beads 的同步 bug 抄一遍。

**B. agent 接口形态**

| 方案 | 判断 |
|---|---|
| 文件即接口（agent 用自带文件工具读写） | 与本项目既有决策一致（LeafWiki 定案）；但**无法表达原子写与租约**，两者都是本工具的核心价值 |
| CLI + `--json` | 能原子写、能加租约、能校验；Backlog.md 已把“CLI 为准、MCP 可选”走通 |
| MCP server | 工具数量直接吃上下文（Task Master 教训），且要先有稳定 CLI |

建议：**CLI 为唯一写入口**，文件格式对人类可读但不承诺手工编辑安全；MCP 放第三批。

**C. 落地形态**

本仓库是技能集。这个工具最终是“独立 CLI + 一个 skill 包装”，还是“纯 skill 约定（无二进制）”，取决于是否接受崩溃半写与租约这两个只有程序才能保证的东西。建议前者。

**D. 工作流引擎的深度**

| 档 | 内容 | 成本 |
|---|---|---|
| 一 | 声明式状态机：`from → to` + 守卫（字段谓词、依赖是否全部完成）+ 封闭动作集（建子任务 / 改字段 / 追加事件 / 派发命令） | 低：一份配置 + 一个校验器 |
| 二 | 加上外部副作用：调用外部命令、通知、失败重试与幂等 | 中：要定超时、重试、去重 |
| 三 | 加上可视化编排器、任意脚本节点、定时与 webhook 触发 | 高：等于自建小型工作流平台 |

建议取**第一档**（R16 + R18）。第二档的扩展口子留成“动作集可增项”——加一个动作等于加一个函数，引擎本身不动；第三档不做。

### 6.5 非目标

边界画在外围，不画在能力本身：

- 可视化工作流编排器、任意脚本节点、多级审批链（D 档三）
- 自定义字段的表单渲染与 Web UI（字段在终端以文本显示）
- 字段级索引与增量物化（查询走 §6.4-A 的全量加载）
- 插件系统与沙箱
- 角色 / 权限 / 多租户
- 自带 LLM 调用（工具不产生模型流量）
- 编排与执行 agent 进程（只派发命令，不托管）

## 7. 复用结论与涉及模块

**复用**

| 复用什么 | 位置 | 用法 |
|---|---|---|
| 四态看板语义与状态流转规则 | `arch-dev/SKILL.md`“看板与状态” | 作为 `workflow.yml` 的默认值，不新造枚举 |
| 计划文档模板（背景 / 上下文范围 / 快速检查表 / 需求跟踪矩阵 / 伪代码） | `arch-dev/documentation.md`、`.context/plans/*.md` | 结构化后，字段从“表格列”映射为“记录字段”，正文保留原章节 |
| CLI 打包与入口先例 | `biblio-kd/bin`、`biblio-kd/script` | 目录布局与调用方式照抄 |
| “文件即接口”的取舍经验 | 跨项目记忆（LeafWiki 定案） | 用于判定 §6.4-B 中哪些能力必须走 CLI |
| 根目录 `.venv` | 本仓库 | 运行环境 |

**新建**：一个 `pm` CLI 模块（唯一新模块，无既有同类可扩展）。

## 8. 下一步

需要四项裁定后进入架构设计（arch-dev 步骤 3）：存储格式（§6.4-A）、接口形态（§6.4-B）、落地形态（§6.4-C）、工作流引擎深度（§6.4-D）。默认取各节“建议”项。

自定义字段与工作流引擎已升为 MVP（R15 / R16），对应新增两个配置文件和两个校验器；它们不改变存储格式的选型，因为文件存储本身就是 schemaless 的。

## 附录：来源

- Symphony 规范：<https://github.com/openai/symphony>（`SPEC.md`）；发布说明 <https://openai.com/index/open-source-codex-orchestration-symphony/>
- beads：<https://pkg.go.dev/github.com/steveyegge/beads@v0.9.4>
- Backlog.md：<https://github.com/MrLesk/Backlog.md>
- Task Master：<https://github.com/eyaltoledano/claude-task-master>
- Vibe Kanban（停止运营公告）：<https://github.com/BloopAI/vibe-kanban>、<https://www.vibekanban.com/blog/shutdown>
- Paca：<https://github.com/Paca-AI/paca>
- 传统 PM 的 MCP 横评：<https://github.com/SyntracAI/project-management-for-ai-coding-agents>
- 禅道开源版：<https://www.zentao.net/zentao-os.html>
- DeepSeek dsh-superagent（编排框架，非 PM 工具；授权与分工模型可参考）：<https://github.com/deepseek-ai/dsh-superagent-prototype>（`docs/000-philosophy.md`、`001-domain.md`、`002-authorization.md`、`003-execution.md`）
