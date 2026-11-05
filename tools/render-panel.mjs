#!/usr/bin/env node
// 把 plugin/client.js 里真实导出的 BoardView 渲成静态 HTML，用于看板面板的目视验收。
// 数据来源与宿主路由一致：跑内核 rpc（list + stats + log + 未完成项的 context）。
import { execFileSync } from 'node:child_process'
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const 根 = join(dirname(fileURLToPath(import.meta.url)), '..')
const dsh = process.env.DSH_CHECKOUT ?? 'D:/source_code/deepseek-harness'
const store = join(dsh, 'node_modules', '.pnpm')
const React = (await import(pathToFileURL(join(store, 'react@18.3.1', 'node_modules', 'react', 'index.js')).href)).default
const SSR = (await import(pathToFileURL(join(store, 'react-dom@18.3.1_react@18.3.1', 'node_modules', 'react-dom', 'server.js')).href)).default


const bin = join(根, 'plugin', 'bin', process.platform === 'win32' ? 'dsh-zen.exe' : 'dsh-zen')
const rpc = (requests) => execFileSync(bin, ['rpc'], { cwd: 根, encoding: 'utf8', input: requests.map((r) => JSON.stringify(r)).join('\n') + '\n' }).trim().split('\n').map((l) => JSON.parse(l))
const [list, stats, log] = rpc([{ tool: 'zen_list', args: {} }, { tool: 'zen_stats', args: {} }, { tool: 'zen_log', args: { limit: 6 } }])
const rows = list.data.rows
if (rows.length) {
  const 上下文 = rpc(rows.map((r) => ({ tool: 'zen_context', args: { id: r.id } })))
  const 详情 = rpc(rows.map((r) => ({ tool: 'zen_show', args: { id: r.id } })))
  for (const [i, r] of rows.entries()) {
    const c = 上下文[i]
    if (c && c.ok) {
      const d = c.data
      if (d.plan) { r.plan = d.plan.title ?? d.plan.id ?? null; r.plan_id = d.plan.id ?? null }
      const 验收 = d.acceptance ?? []
      r.acceptance = { total: 验收.length, checked: 验收.filter((a) => a.checked_at !== null).length }
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
const 计划编号 = [...new Set(rows.map((r) => r.plan_id).filter((v) => typeof v === 'string' && v !== ''))]
const 计划详情 = {}
if (计划编号.length) {
  const 答 = rpc(计划编号.map((id) => ({ tool: 'zen_show', args: { id } })))
  for (const [i, id] of 计划编号.entries()) if (答[i] && 答[i].ok) 计划详情[id] = { title: 答[i].data.title, background: 答[i].data.background, context: 答[i].data.context }
}
const payload = { rows, total: list.data.total, stats: stats.data, events: log.data.events, 计划详情, at: new Date().toISOString() }

let 捕获
globalThis.window = { __ModuleLoader__: { load: (m) => { 捕获 = m } } }
await import(pathToFileURL(join(根, 'plugin', 'client.js')).href)
const mod = 捕获.factory((name) => (name === 'react'
  ? { createElement: React.createElement, useState: (v) => [v, () => {}], useCallback: (f) => f, useEffect: () => {}, useMemo: (f) => f() }
  : {}))

const 渲染 = (state) => SSR.renderToStaticMarkup(React.createElement(mod.BoardView, { state, onRefresh: null }))
const 视图数据 = payload
const 面板 = 渲染({ phase: 'data', data: 视图数据 })
// 展开态：SSR 不能点击，用 defaultExpanded 指定展开哪一行
const 展开行 = 视图数据.rows.find((r) => (r.acceptance_items ?? []).length > 0) ?? 视图数据.rows[0]
const 展开计划 = Object.keys(视图数据.计划详情 ?? {})[0]
const 展开态 = SSR.renderToStaticMarkup(React.createElement(mod.BoardView, {
  state: { phase: 'data', data: 视图数据 }, onRefresh: null, defaultExpanded: 展开计划 ? 'plan:' + 展开计划 : 展开行.id,
}))
const 空态 = 渲染({ phase: 'empty' })
const 错态 = 渲染({ phase: 'error', message: '不是合法的看板视图' })

const tokens = (dark) => `
  --dsw-alias-bg-base: ${dark ? 'rgb(35,35,36)' : 'rgb(255,255,255)'};
  --dsw-alias-label-primary: ${dark ? 'rgb(249,250,251)' : 'rgb(15,17,21)'};
  --dsw-alias-label-secondary: ${dark ? 'rgb(207,211,214)' : 'rgb(97,102,107)'};
  --dsw-alias-border-l1: ${dark ? 'rgba(255,255,255,.06)' : 'rgba(0,0,0,.06)'};
  --dsw-alias-border-l2: ${dark ? 'rgba(255,255,255,.1)' : 'rgba(0,0,0,.1)'};
  --dsw-alias-bg-skeleton: ${dark ? 'rgba(255,255,255,.08)' : 'rgba(0,0,0,.04)'};
  --dsw-static-deepseek-500: rgb(65,118,230);`

const html = `<!doctype html><meta charset="utf-8"><style>
  html,body{margin:0;background:#f4f5f7;font-family:"Noto Sans SC","Segoe UI",system-ui,sans-serif}
  .wrap{display:flex;gap:24px;padding:32px;align-items:flex-start}
  .frame{width:400px;border:0.5px solid rgba(0,0,0,.1);border-radius:12px;overflow:hidden;background:#fff}
  .frame.dark{border-color:rgba(255,255,255,.1)}
  .tag{font-size:11px;color:#61666b;padding:0 0 8px 2px}
  .light{${tokens(false)}background:var(--dsw-alias-bg-base)}
  .dark{${tokens(true)}background:var(--dsw-alias-bg-base)}
</style>
<div class="wrap">
  <div><div class="tag">看板页签 · 浅色</div><div class="frame light">${面板}</div></div>
  <div><div class="tag">看板页签 · 深色</div><div class="frame dark">${面板}</div></div>
</div>
<div class="wrap" style="padding-top:0">
  <div><div class="tag">展开一行 · 详情</div><div class="frame light">${展开态}</div></div>
  <div><div class="tag">空态 / 错误态</div><div class="frame light">${空态}</div><div style="height:12px"></div><div class="frame light">${错态}</div></div>
</div>`

const 输出 = process.argv[2] ?? join(根, '.tmp-panel.html')
writeFileSync(输出, html)
const 解析 = payload
console.log(`渲染完成：${输出}（${解析.rows.length} 条任务 / ${解析.events.length} 条事件）`)
