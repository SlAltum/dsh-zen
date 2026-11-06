# dsh-zen SRS（软件需求规格）

上游：[PRD](PRD.md) · [RESEARCH](RESEARCH.md)
性质：可验收的需求基线。PRD 说明“为什么”，本文件说明“要什么、怎么判通过”。

## 1. 范围与术语

本系统以 **DSH 插件**形态交付：模型通过插件注册的内置工具管理单个项目内的计划与任务。数据存于 SQLite，配置以 TOML 随代码入库，无守护进程、无网络，运行时零依赖。

术语：**任务**（issue）、**计划**（plan）、**积压**（backlog）、**租约**（lease）、**产物**（artifact）、**视图**（view）、**转移**（transition）。

## 2. 数据模型

```
Plan(容器) ─1:N─ Issue ─1:N─ Artifact
                  │  ├─1:N─ Comment
                  │  ├─1:N─ Acceptance
                  │  └─N:M─ Issue（Link：blocks / relates / duplicates）
Issue ─1:N─ Event（审计与报告的共同载体）
Role ──授予── Actor
```

| 实体 | 关键属性 |
|---|---|
| Plan | `id`、`title`、`background`、`context`（分节正文）、`created_at`；**无状态** |
| Issue | `id`、`plan`（空 = backlog）、`type`、`title`、`description`、`status`、`priority`、`parent`、`block_reason`、`fields`、`external_key`、`claim{actor, until}`、`reviewed_at`、`rev`、`created_at`、`updated_at` |
| Acceptance | `issue`、`ordinal`、`text`、`checked_at`、`checked_by` |
| Artifact | `issue`、`kind`（file / commit / url / note）、`ref`、`sha256`（仅 file）、`at` |
| Link | `from`、`to`、`kind` |
| Comment | `issue`、`actor`、`body`、`at` |
| Event | `seq`（自增）、`subject_type`（issue / plan / config）、`subject_id`、`kind`、`from`、`to`、`actor`、`role`、`payload`、`at` |
| Role | `name`、`duty`、`tools`（声明性，供插件生成工具说明）、`workdir`、`ttl`（租约 TTL，秒） |

**报告不建表**：结构化报告是 `Event` 的一种（`kind='report'`，`payload` 含类型与说明）。

配置（仓库内，随代码入库）：`issuetypes.toml`、`fields.toml`、`workflow.toml`、`roles.toml`。

## 3. 状态与生命周期

### 3.1 状态集是常量

`todo`、`wip`、`done`、`blocked`、`dropped` 五个状态由代码定义。`workflow.toml` 描述**转换、守卫、效果**，不定义新状态名。`done` 与 `dropped` 是终态。

`todo` → `wip` → `done`；`wip` → `blocked` → `wip`；任意态 → `dropped`。

### 3.2 进入 wip 即获得租约

没有独立的“认领”命令。`move --to wip` 一步完成占用与起租约。

- 租约冲突时命令失败，报出持有者与到期时间
- 租约过期视为空闲，可被接管
- 离开 `wip` 一律释放租约（含转入 `dropped`）
- `move --extend` 由持有者续期

### 3.3 两种阻塞

| | 载体 | 语义 | 租约 |
|---|---|---|---|
| 依赖阻塞 | `Link(kind=blocks)` | 等对方完成，自动解除 | 任务仍在 `todo`，不占租约，不进 `ready` |
| 主动受阻 | `status=blocked` + `block_reason` | 缺信息、缺权限、等决定 | 立即释放；需清空 `block_reason` 或 lead 强制释放才能被重新领取 |

### 3.4 done 的判据

守卫要求两项事实，不设审批人：

1. `acceptance` 非空，且**逐条**勾选完成（不接受批量勾）
2. 至少一条 `Artifact`：`file`（校验路径存在）/ `commit` / `url` / `note`（≥ 20 字）

### 3.5 终态只读

`done` / `dropped` 不可再改。例外仅三类：`reviewed_at`、评论、产物。重开需 lead 显式操作并落事件。

## 4. 角色与权限

### 4.1 角色是每次动作戴的帽子

每条命令用 `--role` 声明，缺省 `implementer`。声明对照 `roles.toml` 的 `grants` 校验。一个 actor 可在同一会话中先后以不同角色操作。

actor 身份解析顺序：`ZEN_ACTOR` 环境变量 → `git config user.email` → `用户名@主机名`。同机多会话并行时须显式设 `ZEN_ACTOR`。

### 4.2 权限矩阵

按**转移**授权，不按动作授权。矩阵存于 `roles.toml`。

| 转移 / 操作 | planner | implementer | reviewer | tester | lead |
|---|---|---|---|---|---|
| 建计划 | ✓ | | | | ✓ |
| 建任务 | ✓ | ✓ | ✓ | ✓ | ✓ |
| 改标题、描述 | ✓ | ✓ | | | ✓ |
| **改验收标准内容** | ✓ | | | | ✓ |
| 勾选验收标准 | | 持有者 | | 持有者 | ✓ |
| 改 `priority` | ✓ | | | | ✓ |
| `todo→wip` | ✓ | ✓ | ✓ | ✓ | ✓ |
| `wip→done` | | 持有者 | | 持有者 | ✓ |
| `wip→blocked` | | 持有者 | | 持有者 | ✓ |
| `blocked→wip` | ✓ | 持有者 | | 持有者 | ✓ |
| `wip→todo` | ✓ | 持有者 | | | ✓ |
| `→dropped` | ✓ | 持有者 | | | ✓ |
| 重开终态 | ✓ | | | | ✓ |
| 挂产物、评论 | ✓ | ✓ | ✓ | ✓ | ✓ |
| 建 `blocks` | ✓ | | | | ✓ |
| 建 `relates` / `duplicates` | ✓ | ✓ | ✓ | ✓ | ✓ |
| 改配置文件、跑 migrate | | | | | ✓ |
| 定期 review | | | ✓ | | ✓ |

三条设计意图：验收标准的“内容”与“勾选”分开授权；`blocks` 只给 planner / lead；`todo→wip` 对所有人开放。

## 5. 工具面与视图

**模型只接触工具。** 每个命令对应一个内置工具，命名 `zen_<命令>`（如 `zen_move`）。工具是薄别名：参数校验与结果渲染在插件层完成，语义与命令完全等同。命令是内核契约，供测试与人使用；工具是模型可见面。

**工作流按需加载。** 插件注册一个 `dsh-zen` 技能，catalog 只暴露名字与“何时用”，正文（完整工作流与硬规则）由模型自己取用。技能本身不进系统提示：不需要看板的会话不为它付常驻 token。

**引导随项目存在。** 插件另注册一个系统提示段落（order 3550）：文本 provider 从会话工作目录向上找 `.dsh-zen/`，找不到返回空串——DSH 在渲染阶段丢弃空段，所以没有看板的会话成本为零；找到才给出六步循环与“人读命令留给人”的摘要。细节仍走技能。

**工具按需暴露。** 注册时带 DSH 的步骤级准入 `when`：`init` 与 `schema` 常驻，其余工具只在会话工作目录向上能找到 `.dsh-zen/` 时进入工具清单。缺少工作目录服务时全部可见。

| 命令 / 工具 | 用途 |
|---|---|
| `init` | 建 `.dsh-zen/` 与默认配置；已存在则跳过，不覆盖。唯一会创建 `.dsh-zen` 的命令，且必须显式给项目根；祖先目录已有看板时拒绝；落点回显在结果 `root` 字段 |
| `create` | 建计划或任务；`--plan` 进计划，否则进 backlog |
| `move` | 状态流转；进入 wip 即占用；`--extend` 续期 |
| `context <id>` | 按角色返回最小上下文包 |
| `show <id>` | 单条详情；`--section` 只取一节正文 |
| `list` | 查询；可带预设视图名 |
| `views` | 列出可用视图 |
| `schema` / `workflow` / `roles` | 自描述；`roles` 只输出当前角色行 |
| `link` | 建关联（blocks / relates / duplicates） |
| `review` | 扫已完成且未复盘的任务，建复盘任务并打标（reviewer / lead） |
| `import` | 从 arch-dev 的 `plans/*.md` 导入任务，靠外部键幂等 |
| `artifact` | 挂产物 |
| `comment` | 评论 |
| `accept` | 勾选验收项 |
| `update` | 改任务标题或正文（planner / lead） |
| `plan` | 写计划 background / context（planner / lead） |
| `report` | 提交结构化报告 |
| `reopen` | 重开终态（lead） |
| `check` | 全量校验，只报不修 |
| `migrate` | 字段方案存量迁移，支持 dry-run |
| `board` | 终端只读看板（人视图） |
| `stats` | 结构化统计（人视图） |
| `log` | 事件回放 |

预设视图（定义于 `workflow.toml` 的 `views:` 段）：`ready`、`mine`、`blocked`、`backlog`、`stale`、`review`。

## 6. 功能需求

**A 容器**

| ID | 需求 | 级别 | 验收 |
|---|---|---|---|
| R1 | 项目 = 显式声明的项目根，单项目，ID 前缀 `ZEN` | M | `init` 为不同项目根各自建库，ID 不冲突 |
| R2 | 计划是纯容器，无状态字段 | M | `schema` 输出中不存在任何计划级状态 |
| R3 | 任务归属：`create --plan` 进计划，否则进 backlog；单计划归属 | M | 不带 `--plan` 的任务进 backlog，`list backlog` 可见 |

**B 类型与字段**

| ID | 需求 | 级别 | 验收 |
|---|---|---|---|
| R4 | 工作项类型可配置（ship 任务 / Bug / 子任务 / 调研） | M | 新建类型无需改代码 |
| R5 | 类型 → 字段方案绑定 | M | 同一字段在一类型必填、在另一类型不存在 |
| R6 | 字段定义：类型 / 必填 / 枚举 / 默认 | M | 类型错、未声明、缺必填三类拒绝并定位到字段 |

**C 状态与流转**

| ID | 需求 | 级别 | 验收 |
|---|---|---|---|
| R7 | 五状态为常量，配置只能声明转换、守卫、效果 | M | 在 `workflow.toml` 写新状态名被拒绝并说明原因 |
| R8 | 进入 wip 即获得租约，无独立认领命令 | M | `move --to wip` 后租约字段被填 |
| R9 | 租约：冲突失败并报持有者；过期可接管；离开 wip 一律释放 | M | 两进程并发进入 wip，只有一个成功 |
| R10 | 守卫为结构化谓词，只引用已落库事实 | M | 不满足时报未满足的那一条 |
| R11 | 两种阻塞分离；主动受阻立即释放租约 | M | `list blocked` 只列主动受阻，不含依赖阻塞 |
| R12 | 就绪 = `todo` 且无未完成 `blocks` 且不在 backlog | M | 成环时报出环上 ID |
| R13 | done 判据：acceptance 非空且逐条 `accept` 勾完 + 至少一条 `artifact` | M | 缺任一项拒绝并说明缺哪个 |
| R14 | 终态只读，例外仅 `reviewed_at` / `comment` / `artifact`；`reopen` 需 lead | M | 对 done 任务 `move` 被拒而 `comment` 成功；`reopen` 成功且落事件 |

**D 关联**

| ID | 需求 | 级别 | 验收 |
|---|---|---|---|
| R15 | 三类关联；`duplicates` 对称归一化后唯一 | M | 用 `link` 反向重复建同一条，被合并为一条 |
| R16 | `blocks` 仅 planner / lead 可建，成环即拒绝 | M | implementer 建 blocks 被拒；A↔B 成环时报环上 ID |

**E 视图与检索**

| ID | 需求 | 级别 | 验收 |
|---|---|---|---|
| R17 | 六个预设视图，定义于配置，可按名调用 | M | 每个视图各有一条断言；`stale` 阈值取自配置 |
| R18 | flags 过滤：status / type / priority / plan / actor / 未占用 | M | 可组合；`--unclaimed` 含租约过期者，不含终态（终态不可领取） |
| R19 | 全部读操作返回结构化输出，schema 版本化 | M | 同版本内字段不增不减 |
| R20 | `views` / `schema` / `workflow` / `roles` 可机器读取；`roles` 只输出当前角色行 | M | 只凭输出即可组装合法写操作 |

**F 并发与一致性**

| ID | 需求 | 级别 | 验收 |
|---|---|---|---|
| R21 | SQLite 单文件 + WAL；同机多进程共享；不跨机并发 | M | 五进程并发读写下无损坏 |
| R22 | 所有写操作在单事务内，崩溃不留半写 | M | 故障注入后读到旧值或新值之一 |
| R23 | 同行并发写失败而非丢更新，报出前值 | M | 两人同改一字段，第二个失败 |
| R24 | 幂等创建：`external_key` 唯一，重复返回已有 ID | M | 两进程同键并发创建只产生一条 |

**G 最小读写单元**

| ID | 需求 | 级别 | 验收 |
|---|---|---|---|
| R25 | `context <id>` 返回最小上下文：本任务正文摘要（前 200 字，标截断与取全文的方式）、计划摘要、直接依赖状态、验收标准、相关关联 | M | 输出既不含同计划其他任务，**也必须含**上述各项；正文超 200 字时标 truncated 并给出取全文的命令 |
| R26 | 定点写：一条命令只改一条记录的最少字段 | M | 导出后除目标字段与 `updated_at` 外无差异 |
| R27 | 计划正文按节取用 | M | `show --section background` 只返回该节 |
| R28 | 默认输出预算：`list`（含 `ready` / `mine` 视图）≤ 40 行且不含正文；`log` 默认 ≤ 40 条；`context` 不受行数限制 | M | `list` 有一条 40 行上限断言，`log` 有一条限量断言 |

**H 报告与派生**

| ID | 需求 | 级别 | 验收 |
|---|---|---|---|
| R29 | 结构化报告：progress / waiting / ready / blocked / failed / dropped | M | `report` 缺字段被拒 |
| R30 | 派生阶段由最近报告与租约推出（等人 / 等占用 / 工作中 / 空闲），只读 | M | 报告 ready 后阶段自动变化，无需改 status |
| R31 | 定期 `review`：扫 `done` 且 `reviewed_at` 空，建新任务并标 `reviewed_at` | S | 连跑两次，第二次不重复建任务 |

**I 审计**

| ID | 需求 | 级别 | 验收 |
|---|---|---|---|
| R32 | 追加式事件，自增 `seq`；可按任务重建完整变更序列 | M | 同毫秒两次变更仍能定序 |
| R33 | 所有写路径同事务落一条事件，含 actor 与 role；subject 可指向计划或配置 | M | 建计划、改配置均可从 `log` 查到 |

**J 权限**

| ID | 需求 | 级别 | 验收 |
|---|---|---|---|
| R34 | 角色是职责包：职责 / 工作目录 / 租约 TTL，可配置；`tools` 为声明性字段供插件生成工具说明 | M | 新建角色无需改代码 |
| R35 | 角色每命令声明，缺省 implementer，对照授予表校验 | M | 未授予的角色声明被拒并列出当前可用角色 |
| R36 | 权限按转移判定，按 §4.2 执行 | M | 逐格抽样；失败报 `permission` 并指出缺哪个转移 |

**K 版本化与迁移**

| ID | 需求 | 级别 | 验收 |
|---|---|---|---|
| R37 | `check` 全量校验：结构、配置、断引用、环、过期租约、产物可解析 | M | 六类坏数据各制造一例全部报出 |
| R38 | 字段方案变更的存量迁移，带 dry-run 与逐条报告 | S | `migrate --dry-run` 后执行，前后 `check` 全绿；连跑两次结果相同 |
| R39 | 从 arch-dev 的 `plans/*.md` 一次性 `import` | S | 导入后计数与原文逐条对齐 |

**L 人视图**

| ID | 需求 | 级别 | 验收 |
|---|---|---|---|
| R40 | `board` 与 `stats` 是只读人视图，不进入 agent 主路径 | S | 删除这两个命令后 agent 工作流不受影响 |
| R41 | 任务标题与正文可改：`update` 写 title / description，只给 planner 与 lead | M | implementer 改正文被拒；planner 改后 show 立即读到 |
| R42 | 计划正文两节可写：`plan` 写 background / context，只给 planner 与 lead | M | 写后 show 该计划返回两节；两节都缺参数被拒 |

## 7. 非功能需求

| 维度 | 要求 |
|---|---|
| 依赖 | 运行时零依赖；Rust 核心为单个自包含二进制，TS 适配层随插件包分发 |
| 并发 | 同机多进程安全；跨机靠拷贝库文件或 `VACUUM INTO`（内核不管搬迁） |
| 性能 | 10⁴ 任务：单字段过滤 < 50ms；含依赖闭包的 `ready` < 200ms |
| 可恢复 | 库是唯一真相；视图 `board.zendb` 每次写命令重建，丢了不影响任何状态 |
| 可移植 | Windows / macOS / Linux 各自构建；控制台输出统一 UTF-8 |
| 可测试 | `cargo test` 覆盖正常路径、边界与并发用例，全绿即通过 |
| 配置版本化 | 四份 toml 与代码同版本 |

面向 agent 的四条硬约束：上下文预算（R28）、按需披露（帮助分两层）、自描述（R20）、人视图不阻塞（R40）。

## 8. 存储与一致性约束

- 单文件 SQLite，WAL 模式，同机多进程共享
- 一次命令 = 一个事务；批量命令（review / migrate）逐条事务并记录进度
- 时间一律 UTC，固定宽度格式，比较等价于时间比较
- 数据库与配置各带版本号；不匹配时报 `config` 而 `validation`

## 9. 默认值

| 项 | 默认 |
|---|---|
| 状态集 | `todo` / `wip` / `done` / `blocked` / `dropped` |
| `priority` | `P0`–`P3`，缺省 `P2` |
| 租约 TTL | 30 分钟 |
| review 周期 | 7 天 |
| `stale` 阈值 | 14 天 |
| `--role` 缺省 | `implementer` |
| 导出格式 | 仅 JSONL |

## 10. 验收方式

1. `bash tools/check-spec.sh` 校验需求规格的内部一致性（编号、引用、命令面↔需求、转移↔权限矩阵）。
2. 实现完成后跑 `cargo test`，覆盖正常路径、边界、并发与故障注入。
3. 插件冒烟：装入 DSH 后，`zen_views`、`zen_create`、`zen_move`、`zen_context` 四个工具可调用并返回结构化结果。
4. 逐条对照 §6 的验收列，缺一不签。
