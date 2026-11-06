/**
 * dsh-zen 插件：把 Rust 核心暴露成模型可见的内置工具。
 *
 * 形态是两层：本文件在 DSH 宿主进程里注册工具、校验参数、渲染结果；真正的规则与
 * 存储全在 `dsh-zen` 二进制里，每次调用起一个进程，用 NDJSON over stdio 通信。
 *
 * 为什么分两层：宿主崩了会带走整个会话，独立进程只让单次工具调用失败；核心也因此
 * 能用子命令直接跑测试，不必启动宿主。
 *
 * 这个文件只用 node 内置模块：workspace bundle 解析不了裸导入，所以不 import
 * `@deepseek-ai/dsh-tools`，而是直接向 `ctx.tools.register` 传它要求的普通对象。
 */

import { spawn } from 'node:child_process'
import { isAbsolute } from 'node:path'
import { existsSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

export const name = 'dsh-zen'

export const inject = ['tools']

/** 本包目录，用来定位随包分发的二进制。 */
const 包目录 = dirname(fileURLToPath(import.meta.url))

/** 单次调用的超时预算。本地 SQLite 操作，给足但不无限等。 */
const CALL_TIMEOUT_MS = 15_000

/**
 * 二进制定位：配置项 bin → 环境变量 ZEN_BIN → 随包分发的 plugin/bin/ → PATH。
 * 前三个都要求文件真实存在，否则继续往下找，避免配错路径后静默失败。
 */
function 定位二进制(config) {
  const 候选 = []
  if (typeof config?.bin === 'string' && config.bin) 候选.push(config.bin)
  if (process.env.ZEN_BIN) 候选.push(process.env.ZEN_BIN)
  候选.push(join(包目录, 'bin', process.platform === 'win32' ? 'dsh-zen.exe' : 'dsh-zen'))
  for (const p of 候选) {
    if (existsSync(p)) return p
  }
  return 'dsh-zen'
}

/**
 * 向内核发一条 NDJSON 请求并读回一行结果。
 * 内核的错误也按行返回，所以这里只把「进程起不来」当作传输失败。
 */
function callZen(bin, tool, args, signal) {
  return new Promise((resolve, reject) => {
    const child = spawn(bin, ['rpc'], { stdio: ['pipe', 'pipe', 'pipe'] })

    let out = ''
    let err = ''
    const timer = setTimeout(() => {
      child.kill()
      reject(new Error(`zen_${tool} 超时（${CALL_TIMEOUT_MS}ms）`))
    }, CALL_TIMEOUT_MS)

    child.stdout.setEncoding('utf8')
    child.stderr.setEncoding('utf8')
    child.stdout.on('data', (chunk) => { out += chunk })
    child.stderr.on('data', (chunk) => { err += chunk })

    child.on('error', (e) => {
      clearTimeout(timer)
      reject(new Error(`无法启动 ${bin}：${e.message}`))
    })

    child.on('close', (code) => {
      clearTimeout(timer)
      const line = out.trim().split('\n').at(-1)
      if (!line) {
        reject(new Error(`zen_${tool} 无输出（退出码 ${code}）${err ? `：${err.trim()}` : ''}`))
        return
      }
      try {
        resolve(JSON.parse(line))
      } catch (e) {
        reject(new Error(`zen_${tool} 返回的不是 JSON：${e.message}`))
      }
    })

    if (signal) {
      signal.addEventListener('abort', () => child.kill(), { once: true })
    }

    child.stdin.end(`${JSON.stringify({ tool: `zen_${tool}`, args })}\n`)
  })
}

/**
 * 把工具表的简写参数转成 JSON Schema：`required: true` 在表里逐项标注，
 * 而 JSON Schema 要求把必填名收进对象级的 required 数组。
 */
function 参数模式(params) {
  const properties = {}
  const required = []
  for (const [key, spec] of Object.entries(params ?? {})) {
    const { required: 必填, ...rest } = spec
    properties[key] = rest
    if (必填 === true) required.push(key)
  }
  const schema = { type: 'object', properties, additionalProperties: false }
  if (required.length > 0) schema.required = required
  return schema
}

/** 反复出现的参数说明，集中一处改。 */
const 角色说明 = '本次动作戴的角色：planner / implementer / reviewer / tester / lead，缺省 implementer。'
const 操作者说明 = '操作者身份，缺省按 ZEN_ACTOR → git 邮箱 → 用户名@主机名解析。'

const TOOLS = [
  {
    name: 'init',
    description: '在指定的项目根建立 .dsh-zen 数据目录、四份默认配置与空库。这是唯一会创建 .dsh-zen 的动作；已存在的文件跳过，不覆盖。首次使用前调用一次。',
    params: { root: { type: 'string', required: true, description: '项目根目录的绝对路径（或相对当前目录）。必填；祖先目录已有看板时拒绝。' } },
  },
  {
    name: 'create',
    description: '建任务；带 plan 则进计划（同名计划复用），否则进积压。可用 acceptance 一次写下验收标准。返回新编号与归属计划。',
    params: {
      title: { type: 'string', required: true, description: '任务标题。' },
      plan: { type: 'string', description: '计划标题或编号；同名计划复用。省略即进积压。' },
      type: { type: 'string', description: '工作项类型，见 zen_schema；缺省 task。' },
      acceptance: { type: 'array', description: '验收标准，按顺序编号从 1 起。', items: { type: 'string' } },
      fields: { type: 'object', description: '自定义字段值，键集见 zen_schema；必填缺失会被拒。', additionalProperties: true },
      role: { type: 'string', description: 角色说明 },
      actor: { type: 'string', description: 操作者说明 },
    },
  },
  {
    name: 'move',
    description: '状态流转：todo→wip→done，wip→blocked→wip，任意非终态→dropped。进入 wip 即占用租约；离开 wip 一律释放。四类失败分别报 permission / guard / lease / conflict。',
    params: {
      id: { type: 'string', required: true, description: '任务编号。' },
      to: { type: 'string', required: true, description: '目标状态：todo / wip / done / blocked / dropped。' },
      role: { type: 'string', description: 角色说明 },
      actor: { type: 'string', description: 操作者说明 },
      extend: { type: 'boolean', description: '只给 wip 中的任务续租约，不改状态。' },
      reason: { type: 'string', description: '进入 blocked 的原因，必填。' },
      clear_block: { type: 'boolean', description: '重新领取前清空 block_reason，否则 blocked→wip 会被守卫拦住。' },
    },
  },
  {
    name: 'context',
    description: '按角色取最小上下文包：本任务正文摘要（前 200 字，超长标 truncated 并给出取全文的方式）、计划摘要、直接依赖状态、验收标准、相关关联，外加该角色声明的额外字段。不含同计划其它任务正文。',
    params: {
      id: { type: 'string', required: true, description: '任务编号。' },
      role: { type: 'string', description: 角色说明 },
      actor: { type: 'string', description: 操作者说明 },
    },
  },
  {
    name: 'report',
    description: '提交结构化报告：progress / waiting / ready / blocked / failed / dropped。不改状态，只落事件并返回派生阶段。',
    params: {
      id: { type: 'string', required: true, description: '任务编号。' },
      kind: { type: 'string', required: true, description: '报告类型。' },
      note: { type: 'string', description: '一句话说明。' },
      role: { type: 'string', description: 角色说明 },
      actor: { type: 'string', description: 操作者说明 },
    },
  },
  {
    name: 'show',
    description: '单条详情；计划支持 --section 取单节正文（background / context）。任务详情含派生阶段 stage。',
    params: {
      id: { type: 'string', required: true, description: '任务编号或计划编号/标题。' },
      section: { type: 'string', description: '只取计划的某一节正文。' },
    },
  },
  {
    name: 'list',
    description: '查询。给 view 用预设视图（ready / mine / blocked / backlog / stale / review），否则全量；flags 可再收窄。默认最多 40 行，超出报 truncated。',
    params: {
      view: { type: 'string', description: '视图名，见 zen_views。' },
      status: { type: 'string', description: '按状态过滤。' },
      type: { type: 'string', description: '按工作项类型过滤。' },
      priority: { type: 'string', description: '按优先级过滤（P0-P3）。' },
      plan: { type: 'string', description: '按计划编号过滤。' },
      actor: { type: 'string', description: '按租约持有者过滤。' },
      unclaimed: { type: 'boolean', description: '只看无人持有或租约已过期的。' },
    },
  },
  {
    name: 'views',
    description: '列出可用视图：名字、谓词、列集。只凭它就能调 zen_list。',
    params: {},
  },
  {
    name: 'comment',
    description: '写一条评论。终态任务也可评论。',
    params: {
      id: { type: 'string', required: true, description: '任务编号。' },
      body: { type: 'string', required: true, description: '评论正文。' },
      role: { type: 'string', description: 角色说明 },
      actor: { type: 'string', description: 操作者说明 },
    },
  },
  {
    name: 'artifact',
    description: '挂一条产物：file（路径需存在）/ commit / url / note（至少 20 字）。终态任务也可挂。',
    params: {
      id: { type: 'string', required: true, description: '任务编号。' },
      kind: { type: 'string', required: true, description: '产物种类。' },
      ref: { type: 'string', required: true, description: '路径 / 提交哈希 / URL / 说明。' },
      role: { type: 'string', description: 角色说明 },
      actor: { type: 'string', description: 操作者说明 },
    },
  },
  {
    name: 'accept',
    description: '勾选一条验收标准。逐条勾选，重复勾报 conflict；终态任务的验收标准只读。lead 可直接勾，implementer / tester 必须是租约持有者。',
    params: {
      id: { type: 'string', required: true, description: '任务编号。' },
      ordinal: { type: 'number', required: true, description: '验收标准序号，从 1 起。' },
      role: { type: 'string', description: 角色说明 },
      actor: { type: 'string', description: 操作者说明 },
    },
  },
  {
    name: 'schema',
    description: '自描述：schema 版本、状态集、优先级、工作项类型与每个类型的字段规则、计划与任务的字段面。',
    params: {},
  },
  {
    name: 'workflow',
    description: '自描述：转移（含守卫与效果）、视图定义、时间阈值。',
    params: {},
  },
  {
    name: 'roles',
    description: '自描述：当前角色的职责包、上下文包字段集、可发起的转移（direct 直接授权 / holder 需持租约）。',
    params: { role: { type: 'string', description: 角色说明 } },
  },
  {
    name: 'check',
    description: '全量校验，只报不修：结构、配置、断引用、依赖成环、过期租约、产物可解析。每条带 kind + target + detail。',
    params: {},
  },
  {
    name: 'migrate',
    description: '字段方案存量迁移：丢未知字段、丢不合法值、补缺省值。dry_run 只报告不写库。',
    params: { dry_run: { type: 'boolean', description: '只报告，不写库。' } },
  },
  {
    name: 'update',
    description: '改任务标题或正文。只给 planner / lead；title 与 description 至少给一个。',
    params: {
      id: { type: 'string', required: true, description: '任务编号。' },
      title: { type: 'string', description: '新标题。' },
      description: { type: 'string', description: '新正文。' },
      role: { type: 'string', description: 角色说明 },
      actor: { type: 'string', description: 操作者说明 },
    },
  },
  {
    name: 'plan',
    description: '写计划的标题与正文两节（background / context）。只给 planner / lead。',
    params: {
      id: { type: 'string', required: true, description: '计划编号或标题。' },
      title: { type: 'string', description: '新标题。' },
      background: { type: 'string', description: '背景。' },
      context: { type: 'string', description: '上下文。' },
      role: { type: 'string', description: 角色说明 },
      actor: { type: 'string', description: 操作者说明 },
    },
  },
  {
    name: 'board',
    description: '终端看板：按状态分组的任务列表。人读用，不进 agent 主路径。',
    params: {},
  },
  {
    name: 'stats',
    description: '结构化统计：总数、按状态/类型/优先级分布、未占用数。人读用。',
    params: {},
  },
  {
    name: 'review',
    description: '定期复盘：扫 done 且 reviewed_at 为空的任务，各建一条复盘任务并打标。role 必须是 reviewer 或 lead（缺省 implementer 会被拒）；连跑两次第二次零新增。',
    params: {
      role: { type: 'string', description: 角色说明 },
      actor: { type: 'string', description: 操作者说明 },
      dry_run: { type: 'boolean', description: '只报告，不写库。' },
    },
  },
  {
    name: 'reopen',
    description: '重开终态：done / dropped 回到 todo 并落事件。只给 lead（缺省 implementer 会被拒）；非终态会被拒。',
    params: {
      id: { type: 'string', required: true, description: '任务编号。' },
      role: { type: 'string', description: '必须是 lead。' },
      actor: { type: 'string', description: 操作者说明 },
    },
  },
  {
    name: 'log',
    description: '事件回放：按 seq 升序返回最近 N 条事件（含状态变更、评论、产物、报告）。只读。',
    params: {
      id: { type: 'string', description: '只看某个任务或计划；省略则全局。' },
      limit: { type: 'number', description: '最多返回多少条，缺省 40。' },
    },
  },
  {
    name: 'link',
    description: '建关联：blocks（A 阻塞 B，只给 planner/lead，不许成环）/ relates / duplicates（对称，反向建会合并）。重复建返回 existing。',
    params: {
      from: { type: 'string', required: true, description: '起点任务编号。' },
      to: { type: 'string', required: true, description: '终点任务编号。' },
      kind: { type: 'string', required: true, description: 'blocks / relates / duplicates。' },
      role: { type: 'string', description: 角色说明 },
      actor: { type: 'string', description: 操作者说明 },
    },
  },
  {
    name: 'import',
    description: '从 arch-dev 的计划文档（plans/*.md）导入任务。走 create/move，不直插库；靠 external_key 幂等，重复导入不重复建。',
    params: {
      path: { type: 'string', required: true, description: '文件或目录，目录则取其中的 *.md。' },
      dry_run: { type: 'boolean', description: '只报告，不写库。' },
      actor: { type: 'string', description: 操作者说明 },
    },
  },
]
/** 一次 rpc 会话跑多条请求：一次进程，多行请求，多行响应。 */
function zenRpc(bin, requests, cwd) {
  return new Promise((resolve, reject) => {
    const child = spawn(bin, ['rpc'], { stdio: ['pipe', 'pipe', 'pipe'], ...(cwd === undefined ? {} : { cwd }) })
    let out = ''
    let err = ''
    child.stdout.setEncoding('utf8')
    child.stderr.setEncoding('utf8')
    child.stdout.on('data', (chunk) => { out += chunk })
    child.stderr.on('data', (chunk) => { err += chunk })
    child.on('error', (e) => reject(new Error(`无法启动 ${bin}：${e.message}`)))
    child.on('close', (code) => {
      const lines = out.trim().split('\n').filter(Boolean)
      if (lines.length < requests.length) {
        reject(new Error(`rpc 少回一行：期望 ${requests.length}，收到 ${lines.length}（退出码 ${code}）${err ? `：${err.trim()}` : ''}`))
        return
      }
      try { resolve(lines.map((l) => JSON.parse(l))) } catch (e) { reject(new Error(`rpc 输出不是 JSON：${e.message}`)) }
    })
    child.stdin.on('error', () => {}) // 子进程早退时的 EPIPE 不该变成宿主未捕获异常
    child.stdin.end(requests.map((r) => JSON.stringify(r)).join('\n') + '\n')
  })
}

/** 现读一份看板数据：行（计划、验收、描述、持有者）、统计、最近事件。不写盘。 */
async function 读看板(bin, root) {
  let cwd
  if (root !== undefined && root !== '') {
    if (!isAbsolute(root) || !existsSync(root)) throw new Error(`项目根不存在或不是绝对路径：${root}`)
    cwd = root
  }
  const [list, stats, log] = await zenRpc(bin, [
    { tool: 'zen_list', args: {} },
    { tool: 'zen_stats', args: {} },
    { tool: 'zen_log', args: { limit: 6 } },
  ], cwd)
  if (!list.ok || !stats.ok) throw new Error(list.ok ? stats.message : list.message)
  const rows = list.data.rows ?? []
  if (rows.length > 0) {
    const [上下文, 详情] = await Promise.all([
      zenRpc(bin, rows.map((r) => ({ tool: 'zen_context', args: { id: r.id } })), cwd),
      zenRpc(bin, rows.map((r) => ({ tool: 'zen_show', args: { id: r.id } })), cwd),
    ])
    for (const [i, r] of rows.entries()) {
      const c = 上下文[i]
      if (c && c.ok) {
        const d = c.data
        if (d.plan) { r.plan = d.plan.title ?? d.plan.id ?? null; r.plan_id = d.plan.id ?? null }
        const 验收 = d.acceptance ?? []
        r.acceptance = { total: 验收.length, checked: 验收.filter((a) => a.checked_at !== null).length }
        r.acceptance_items = 验收.map((a) => ({ ordinal: a.ordinal, text: a.text, checked: a.checked_at !== null }))
      }
      const s = 详情[i]
      if (s && s.ok) {
        const d = s.data
        r.description = d.description ?? ''
        r.holder = d.claim_actor ?? null
        r.block_reason = d.block_reason ?? null
        r.priority = d.priority ?? r.priority
        r.comments = d.comments ?? []
      }
    }
  }
  // 计划详情：background / context 只在 zen_show 里，列表没有。计划数量少，一次批量取。
  const 计划编号 = [...new Set(rows.map((r) => r.plan_id).filter((v) => typeof v === 'string' && v !== ''))]
  const 计划详情 = {}
  if (计划编号.length > 0) {
    const 答复 = await zenRpc(bin, 计划编号.map((id) => ({ tool: 'zen_show', args: { id } })), cwd)
    for (const [i, id] of 计划编号.entries()) {
      const 答 = 答复[i]
      if (答 && 答.ok) 计划详情[id] = { title: 答.data.title, background: 答.data.background, context: 答.data.context }
    }
  }
  return { rows, total: list.data.total ?? rows.length, stats: stats.data ?? {}, events: log.ok ? (log.data.events ?? []) : [], 计划详情, at: new Date().toISOString() }
}

/** 技能正文：模型按需加载后才读到这些工作流，不常驻系统提示。 */
const 技能正文 = `# dsh-zen 任务看板

项目根存在 \`.dsh-zen/\` 时，任务、计划、验收、租约都在这套工具里，不另开 todo 列表或 Markdown 计划文档。

## 何时用

- 会话开始且 \`.dsh-zen/\` 已存在：\`zen_list(view=ready)\` 取就绪队列。
- 需要跨会话跟踪的事（可能被别人接手、多轮才完成、要留验收）：建任务。
- 一次性的读文件、跑命令、灵光一现：不建任务。

## 没有 \`.dsh-zen/\` 时

要做看板时才创建：\`zen_init(root)\`。**项目根显式给**，与 IDE 打开项目一致；祖先目录已有看板时拒绝。结果里的 \`root\` 是实际落点。

## 一次完整循环

1. \`zen_list(view=ready)\` 选一条
2. \`zen_move(id, to=wip, role=implementer)\` 占用，租约随之生效
3. 干活，关键判断用 \`zen_comment\` 留痕
4. \`zen_artifact(id, kind=file|commit|url|note, ref=…)\` 挂产物
5. \`zen_accept(id, ordinal)\` 逐条勾验收；有未勾的条目时 \`done\` 会被拒
6. \`zen_move(id, to=done)\`

## 硬规则

- 写操作都带 role，缺省 implementer；改优先级与正文只有 planner / lead 能做。
- 进 blocked 必须给 reason；离开 wip 自动释放租约。
- \`zen_board\`、\`zen_stats\` 是人读的，不要放进 agent 主路径。
- 报错码就是语义（permission / guard / lease / conflict / config / validation）：先读码再改动作。
`

const 技能 = {
  name: 'dsh-zen',
  description: '用 zen_* 工具管理本项目的任务看板：计划、任务、租约、验收、复盘。',
  whenToUse: '项目根存在 .dsh-zen/ 时；或用户要求跨会话跟踪任务、计划、验收时。',
  content: 技能正文,
}

/** 内核 `find_data_dir` 的同款规则：向上找 `.dsh-zen`，撞到 `.git` 即停。 */
export function 有看板(cwd) {
  if (typeof cwd !== 'string' || cwd === '') return false
  let dir = cwd
  for (;;) {
    if (existsSync(join(dir, '.dsh-zen'))) return true
    if (existsSync(join(dir, '.git'))) return false
    const 父 = dirname(dir)
    if (父 === dir) return false
    dir = 父
  }
}

/**
 * 有看板时才出现的系统提示：短到能背下来，只讲下一步做什么。
 * 返回空串时 DSH 会在渲染阶段丢掉整个 section，所以没看板的会话一分钱不花。
 */
const 看板引导 = `# dsh-zen

这个项目有任务看板（\`.dsh-zen/\`）。开始工作前先 \`zen_list(view=ready)\` 取就绪队列。
循环：\`zen_move(id, to=wip, role=implementer)\` 占用 → 干活 → \`zen_artifact\` 挂产物 →
\`zen_accept\` 逐条勾验收 → \`zen_move(id, to=done)\`。写操作都带 role。
\`zen_board\` / \`zen_stats\` 是人读的，不进 agent 主路径。
何时该建任务、报错码各是什么意思：加载 dsh-zen 技能。`

/** 从 agent 同步取工作目录；拿不到就当没有看板。 */
export function 会话工作目录(agent) {
  try { return agent?.session?.header?.cwd } catch { return undefined }
}

/** 不论有没有看板都露出来的两个工具：没有它们就没法建看板，也没法读数据模型。 */
const 常驻工具 = new Set(['init', 'schema'])

/**
 * 步骤级准入：没有看板的项目不暴露其余工具，省下 20 多个 schema 的常驻成本。
 * 缺 workingDirectory 服务时全部可见。
 */
function 准入条件(ctx) {
  return async ({ agent, signal }) => {
    if (agent === undefined) return false
    let 工作目录 = undefined
    try { 工作目录 = ctx.get('workingDirectory') } catch { 工作目录 = undefined }
    if (工作目录 === undefined || typeof 工作目录.ensure !== 'function') return true
    try { return 有看板(await 工作目录.ensure(agent, signal)) } catch { return false }
  }
}

export function apply(ctx, config = {}) {
  const bin = 定位二进制(config)
  const 准入 = 准入条件(ctx)
  for (const tool of TOOLS) {
    const 选项 = 常驻工具.has(tool.name) ? {} : { when: 准入 }
    ctx.effect(() => ctx.tools.register({
      name: `zen_${tool.name}`,
      description: tool.description,
      parameters: 参数模式(tool.params),
      output: {
        schema: { type: 'object', additionalProperties: true },
        render: (_args, value) => [{ type: 'text', text: JSON.stringify(value) }],
      },
      timeoutMs: CALL_TIMEOUT_MS,
      async execute(args, exec) {
        const result = await callZen(bin, tool.name, args ?? {}, exec.signal)
        if (!result.ok) throw new Error(`[${result.code}] ${result.message}`)
        return result.data
      },
    }, 选项))
  }

  // 有看板才注入这段：没有看板时文本为空串，section 在渲染阶段被丢弃。
  ctx.inject(['systemPrompt'], (inner) => {
    try {
      const prompt = inner.get('systemPrompt')
      inner.effect(() => prompt.section({
        name: 'tool:dsh-zen',
        order: 3550,
        interpolate: false,
        text: ({ agent }) => (有看板(会话工作目录(agent)) ? 看板引导 : ''),
      }), 'dsh-zen: prompt')
    } catch (e) {
      console.warn('[dsh-zen] 系统提示没挂上：', e && e.message ? e.message : e)
    }
  })

  // 指南按需加载：catalog 里只留一行，正文由模型显式取用，不常驻系统提示。
  ctx.inject(['skills'], (inner) => {
    try {
      inner.effect(() => inner.skills.register(技能), 'dsh-zen: skill')
    } catch (e) {
      console.warn('[dsh-zen] 技能注册失败：', e && e.message ? e.message : e)
    }
  })

  // 面板数据：宿主现跑内核取数，浏览器只收 JSON。磁盘上不留任何中间文件。
  ctx.inject(['webServer'], (inner) => {
    const dispose = inner.get('webServer').register({
      kind: 'exact',
      path: '/zen/board',
      handler: async (req, res) => {
        const connection = ctx.get('connection')
        const 拦截 = connection !== undefined && typeof connection.requestRejection === 'function'
          ? connection.requestRejection(req)
          : (['127.0.0.1', '::1', '::ffff:127.0.0.1'].includes(req.socket?.remoteAddress) ? undefined : 403)
        if (拦截 !== undefined) {
          res.writeHead(拦截, { 'content-type': 'text/plain; charset=utf-8' })
          res.end(拦截 === 401 ? 'unauthorized' : 'forbidden')
          return
        }
        try {
          const query = new URL(req.url ?? '/', 'http://127.0.0.1').searchParams
          const payload = await 读看板(bin, query.get('root') ?? undefined)
          res.writeHead(200, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' })
          res.end(JSON.stringify(payload))
        } catch (e) {
          res.writeHead(500, { 'content-type': 'application/json; charset=utf-8' })
          res.end(JSON.stringify({ error: String((e && e.message) || e) }))
        }
      },
    })
    inner.effect(() => () => dispose(), 'dsh-zen: /zen/board')
  })
}
