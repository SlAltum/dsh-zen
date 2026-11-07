# dsh-zen

给 agent 用的项目管理系统，以 **DSH 插件**形态交付：模型通过 25 个 `zen_*` 内置工具
管理一个项目里的计划、任务、租约与复盘。无守护进程、无网络，数据在本地 SQLite 里。

## 两层结构

| 层 | 位置 | 职责 |
|---|---|---|
| 适配层 | `plugin/`（Cordis 插件） | 注册工具、把调用转成 NDJSON、渲染结果与看板页签数据；只有 node 内置依赖 |
| 核心 | `src/`（Rust） | 规则、状态机、权限、租约、存储；每次调用起一个进程 |

适配层在宿主进程里，核心是独立进程：宿主崩了只让单次工具调用失败，核心也能用子命令直接跑测试。

## 构建

```bash
cargo build --release        # 或 tools/build-plugin.sh，它会顺带把二进制放进 plugin/bin
tools/build-plugin.sh        # 缺省 --release；--debug 用调试产物
```

## 安装

用 `plugin_manager` 的 `install_bundle`，target 指向本仓库的 **`plugin/`** 目录
（绝对路径）。安装前先跑一次 `tools/build-plugin.sh`，否则插件只能走 PATH 或 `ZEN_BIN`。

## 二进制定位

按顺序找第一个存在的：

1. 插件行 `config.bin`
2. 环境变量 `ZEN_BIN`
3. 随包分发的 `plugin/bin/dsh-zen`（Windows 上是 `.exe`）
4. PATH 上的 `dsh-zen`

## 数据目录

`.dsh-zen/` 只会由 `zen_init` 创建，项目根由调用方显式给出，与 IDE 打开项目的方式一致。
祖先目录里已有看板时拒绝，一个项目一个看板；落点回显在结果的 `root` 字段。其余命令只读不建，
未初始化的项目直接报 `config`。`zen.zendb*`（SQLite 库本身）与 `export/` 里的快照按 `.gitignore` 处理，只有四份
toml 配置入库。

看板的作用域因此是一块板对一个项目：所有 `zen_*` 工具按调用方会话的工作目录解析数据目录，
不提供跨看板投递。要把问题报给另一块板的维护者，走那个项目自己的会话，或直接落文件/提 issue；
`zen_show(计划号)` 是装配整张计划的入口，一次返回全部任务行，库文件 `zen.zendb` 不作为读取面。

## 工具面

| 分组 | 工具 |
|---|---|
| 起步与自描述 | `zen_init` `zen_schema` `zen_workflow` `zen_roles` `zen_views` |
| 任务 | `zen_create` `zen_show` `zen_list` `zen_context` |
| 流转与租约 | `zen_move` `zen_reopen` |
| 凭据 | `zen_accept` `zen_artifact` `zen_comment` |
| 协作 | `zen_link` `zen_report` `zen_review` |
| 维护与导入 | `zen_check` `zen_migrate` `zen_update` `zen_plan` `zen_log` `zen_import` |

插件另注册一个 `dsh-zen` 技能：catalog 里只占一行（名字 + 何时用），工作流正文由模型需要时
自己加载，不写进系统提示——不需要看板的会话只花那一行。

工具本身也按需暴露：`zen_init` 与 `zen_schema` 常驻（没有它们就没法建看板、也读不到数据模型），
其余 23 个只在项目根存在 `.dsh-zen/` 时进入工具清单——走 DSH 的步骤级准入 `when`，每步判一次。

引导同理：一段系统提示按同一规则判空，没有看板的项目返回空串、被渲染阶段丢弃；有看板才给
“先 `zen_list(view=ready)`、六步循环、人读命令留给人”的摘要。
| 人读 | `zen_board` `zen_stats` |

## 开发

```bash
bash tools/ci.sh             # 四道闸门一起跑：单测 / 发布构建 / 插件对账 / 规格（CI 与本地共用这一条）
```

单独跑：

```bash
cargo test                   # 83 单元 + 2 并发 + 3 端到端
node tools/check-plugin.mjs  # 插件工具名与内核 rpc 对齐、参数 schema 合法
bash tools/check-spec.sh     # SRS 内部一致性
node tools/render-panel.mjs [out.html]  # 用真实库数据把看板面板渲成静态 HTML，截图目视验收
```

规格与设计见 [doc/](doc/)：`SRS.md`（可验收需求）、`ARCHITECTURE.md`（模块边界与分层）、`PRD.md`（产品边界）、`RESEARCH.md`（竞品预研）。
