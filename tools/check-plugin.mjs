#!/usr/bin/env node
// 插件自检：加载 entry、模拟 ctx、核对工具名与内核 rpc 分支一致、校验参数 schema 形状。
// 跑法：node tools/check-plugin.mjs
import { existsSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath, pathToFileURL } from 'node:url'

const 根 = join(dirname(fileURLToPath(import.meta.url)), '..')
const 插件 = await import(pathToFileURL(join(根, 'plugin', 'index.js')).href)

const 注册 = []
const ctx = {
  tools: {
    register(definition) {
      注册.push(definition)
      return () => {}
    },
  },
  // ctx.effect 收到返回 disposer 的回调；这里直接执行并忽略清理
  effect(回调) {
    return 回调()
  },
  // 这个上下文不提供任何服务：inject 不回调，等价于没有 webServer 的 headless profile
  inject() {},
  get() {
    return undefined
  },
}
插件.apply(ctx, {})

const 内核 = [...readFileSync(join(根, 'src', 'main.rs'), 'utf8').matchAll(/^\s+"([a-z]+)" =>/gm)].map((m) => m[1]).sort()
const 插件工具 = 注册.map((t) => t.name).sort()
const 失败 = []

if (插件工具.length === 0) 失败.push('一个工具都没注册')
for (const 名 of 插件工具) {
  if (!名.startsWith('zen_')) 失败.push(`工具名不符合 zen_<命令>：${名}`)
}
const 缺 = 内核.map((n) => `zen_${n}`).filter((n) => !插件工具.includes(n))
const 多 = 插件工具.filter((n) => !内核.includes(n.slice(4)))
if (缺.length) 失败.push(`插件缺工具：${缺.join(', ')}`)
if (多.length) 失败.push(`插件多出内核没有的工具：${多.join(', ')}`)

const 类型白名单 = new Set(['object', 'array', 'string', 'number', 'integer', 'boolean', 'null'])
for (const 工具 of 注册) {
  const 参数 = 工具.parameters
  if (参数?.type !== 'object') 失败.push(`${工具.name}：parameters 必须是 object`)
  if (typeof 参数?.additionalProperties !== 'boolean') 失败.push(`${工具.name}：parameters.additionalProperties 必须显式为布尔`)
  for (const [键, 值] of Object.entries(参数?.properties ?? {})) {
    if (值.type !== undefined && !类型白名单.has(值.type)) 失败.push(`${工具.name}.${键}：不支持的类型 ${值.type}`)
    if ('required' in 值) 失败.push(`${工具.name}.${键}：required 应当收进对象级数组，不能留在属性里`)
  }
  const 必填 = 参数?.required ?? []
  for (const 名 of 必填) {
    if (!(名 in (参数?.properties ?? {}))) 失败.push(`${工具.name}：required 里的 ${名} 不在 properties`)
  }
  if (工具.output?.schema?.type !== 'object') 失败.push(`${工具.name}：output.schema 必须是 object`)
  if (typeof 工具.output?.render !== 'function') 失败.push(`${工具.name}：缺 output.render`)
  if (typeof 工具.execute !== 'function') 失败.push(`${工具.name}：缺 execute`)
}

// Client 半：加载 client.js。重点是「可选能力坏掉也不能把插件打成 failed」。
{
  const reactStub = { createElement: () => null, useState: (v) => [v, () => {}], useCallback: (f) => f, useEffect: () => {}, useMemo: (f) => f() }
  let 捕获
  globalThis.window = { __ModuleLoader__: { load: (m) => { 捕获 = m } } }
  await import(pathToFileURL(join(根, 'plugin', 'client.js')).href)
  if (!捕获) 失败.push('client.js 没有向 __ModuleLoader__.load 注册')
  else {
    const mod = 捕获.factory((名字) => (名字 === 'react' ? reactStub : {}))
    // hard-inject 只允许是稳定存在的服务；其它必须走 ctx.inject
    const 注入 = mod.inject ?? []
    // 文档要求 documentPreviews 由文本页签 owner 提供，渲染器必须声明它；remote 可选，不得进 module inject。
    const 期望注入 = ['slots', 'documentPreviews']
    if (JSON.stringify(注入) !== JSON.stringify(期望注入)) {
      失败.push(`module inject 应为 ${JSON.stringify(期望注入)}，实际 ${JSON.stringify(注入)}`)
    }
    if (注入.includes('remote') || 注入.some((n) => n.startsWith('remote.'))) 失败.push('remote 属可选能力，不能进 module inject（缺服务会让插件 PENDING → boot 失败）')
    // 回归：DSH 的 remote.workspaceFiles 是独立命名空间，必须按全名 ctx.inject，取 remote 再点它会抛
    if (!捕获.factory.toString().includes("ctx.inject(['remote.workspaceFiles']")) {
      失败.push("必须以 ctx.inject(['remote.workspaceFiles']) 注入命名空间；用 remote.workspaceFiles 取值会抛 without inject")
    }
    const 造ctx = (opts = {}) => {
      const slots = []
      const previews = []
      // documentPreviews 由文本页签 owner provide，按 inject 声明直接出现在 ctx 上
      const ctx = {
        slots: {
          inject: (_n, fn) => fn(),
          register: opts.slotsThrow ? () => { throw new Error('duplicate slot declaration') } : (e) => { slots.push(e); return () => {} },
        },
        documentPreviews: {
          register: opts.previewThrow ? () => { throw new Error('duplicate preview id') } : (d) => { previews.push(d); return () => {} },
        },
        inject: (names, cb) => { if (names.includes('remote')) cb({ get: () => ({ workspaceFiles: {} }) }) },
        effect: (fn) => fn(),
      }
      return { ctx, slots, previews }
    }

    // 1) 一切正常
    {
      const { ctx, slots, previews } = 造ctx({})
      try { mod.apply(ctx) } catch (e) { 失败.push('正常服务下 apply 抛错：' + e.message) }
      if (previews.length !== 1) 失败.push(`应注册 1 个预览类型，实际 ${previews.length}`)
      else {
        const def = previews[0]
        if (!(def.extensions ?? []).includes('zendb')) 失败.push('预览类型没有声明 zendb')
        if (!(def.binaryExtensions ?? []).includes('zendb')) 失败.push('zendb 未标记为二进制')
        if (def.loading !== 'bytes-complete') 失败.push('应以 bytes-complete 模式加载（renderer 模式下 body 可能等不到渲染时机）')
      }
      if (!slots.some((s) => s.name === 'sidebar.right.tab.document')) 失败.push('没有注册预览 body')
    }
    // 2) 服务全缺：只应少功能，不得抛错
    {
      const { ctx } = 造ctx({ previewThrow: true, slotsThrow: true })
      try { mod.apply(ctx) } catch (e) { 失败.push('服务缺失时 apply 抛错：' + e.message) }
    }
    // 3) 预览注册被拒
    {
      const { ctx } = 造ctx({ 有服务: ['documentPreviews', 'remote'], previewThrow: true })
      try { mod.apply(ctx) } catch (e) { 失败.push('预览注册抛错时 apply 把它冒了出去：' + e.message) }
    }
    // 4) 槽位注册被拒
    {
      const { ctx } = 造ctx({ 有服务: ['documentPreviews', 'remote'], slotsThrow: true })
      try { mod.apply(ctx) } catch (e) { 失败.push('槽位注册抛错时 apply 把它冒了出去：' + e.message) }
    }
  }
  delete globalThis.window
}

if (失败.length) {
  console.error('插件自检失败：')
  for (const 项 of 失败) console.error('  - ' + 项)
  process.exit(1)
}
console.log(`插件自检通过：${插件工具.length} 个工具与内核 rpc 一一对应，参数 schema 合法；client 挂载 3 个槽位`)
