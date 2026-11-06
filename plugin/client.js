window.__ModuleLoader__.load({
  id: '@local/dsh-plugin-zen',
  factory(require) {
    const React = require('react')
    const h = React.createElement

    const KIND = 'dsh-zen.board'
    /** 由 ctx.inject(['remote.workspaceFiles']) 填：命名空间必须单独注入，否则取 remote.workspaceFiles 会抛。 */
    let 文件远端 = undefined
    const DATA_PATH = '/zen/board'

    /*
     * 视觉决策（读法：宿主侧栏里的信息密集页签，ENERGY 1 / RHYTHM 2 / MOTION 1）：
     * 所有颜色取宿主 token，深浅两套主题因此自动成立，面板不另立一套色板。
     * 强调色只出现在三处：在办行的底色、展开块的左线、状态字形。小号正文一律用
     * label-primary，因为强调色在浅色底上只有约 4:1，不到正文 4.5:1 的门槛。
     * 行和计划头是真正的 button，键盘 Tab 可达、回车空格可展开，焦点环用浏览器默认。
     */
    const C = {
      text: 'var(--dsw-alias-label-primary, #0f1115)',
      sub: 'var(--dsw-alias-label-secondary, #61666b)',
      line: 'var(--dsw-alias-border-l1, rgba(0,0,0,.06))',
      stroke: 'var(--dsw-alias-border-l2, rgba(0,0,0,.1))',
      accent: 'var(--dsw-static-deepseek-500, #4176e6)',
      tint: 'color-mix(in srgb, var(--dsw-static-deepseek-500, #4176e6) 8%, transparent)',
      tintLine: 'color-mix(in srgb, var(--dsw-static-deepseek-500, #4176e6) 40%, transparent)',
      skeleton: 'var(--dsw-alias-bg-skeleton, rgba(0,0,0,.04))',
    }
    const num = { fontVariantNumeric: 'tabular-nums' }
    const 分组 = [
      { key: 'wip', label: '在办' }, { key: 'blocked', label: '受阻' },
      { key: 'todo', label: '待办' }, { key: 'done', label: '完成' }, { key: 'dropped', label: '丢弃' },
    ]

    /** dsh-resource://file/session/<sessionId>/<工作区相对路径> → { sessionId, 相对路径 } */
    function sessionPathOf(address) {
      if (typeof address !== 'string') return undefined
      const m = /^[a-z-]+:\/\/file\/session\/([^/]+)\/(.+)$/i.exec(address)
      if (!m) return undefined
      let p
      try { p = decodeURIComponent(m[2]) } catch { p = m[2] }
      return { sessionId: decodeURIComponent(m[1]), path: p }
    }

    /** 从绝对路径推出仓库根（.dsh-zen 的上一级）。 */
    function rootOfAbsolute(abs) {
      if (typeof abs !== 'string' || abs === '') return undefined
      const parts = abs.replace(/\\/g, '/').split('/')
      if (parts.length < 3) return undefined
      let dir = parts.slice(0, -2).join('/')
      if (/^\/[A-Za-z]:/.test(dir)) dir = dir.slice(1)
      return dir
    }

    /** 旧路径保留：纯地址解析（相对路径形态算不出根，会返回 undefined）。 */
    function rootOfAddress(address) {
      if (typeof address !== 'string') return undefined
      const m = /^[a-z-]+:\/\/file\/(?:session\/[^/]+\/)?(.+)$/i.exec(address)
      if (!m) return undefined
      let p
      try { p = decodeURIComponent(m[1]) } catch { p = m[1] }
      // …/.dsh-zen/zen.zendb → 去两段
      const parts = p.replace(/\\/g, '/').split('/')
      if (parts.length < 3) return undefined
      let dir = parts.slice(0, -2).join('/')
      // 部分地址形如 /D:/projects/...，去掉盘符前的斜杠才是 Windows 绝对路径
      if (/^\/[A-Za-z]:/.test(dir)) dir = dir.slice(1)
      return dir
    }

    function useBoard(files, address) {
      const [state, setState] = React.useState({ phase: 'loading' })
      const load = React.useCallback(() => {
        setState({ phase: 'loading' })
        const 目标 = sessionPathOf(address)
        const 解析根 = 目标 && files && typeof files.stat === 'function'
          ? files.stat(目标.sessionId, 目标.path)
              .then((结果) => rootOfAbsolute(结果 && 结果.ok ? 结果.value.absolutePath : undefined))
              .catch(() => undefined)
          : Promise.resolve(rootOfAddress(address))
        return Promise.resolve(解析根)
          .catch(() => undefined)
          .then((root) => ({ root, url: root ? DATA_PATH + '?root=' + encodeURIComponent(root) : DATA_PATH }))
          .then(({ root, url }) => fetch(url, { headers: { accept: 'application/json' } })
            .then(async (r) => {
              if (r.ok) return r.json()
              let 原因 = `HTTP ${r.status}`
              try { const b = await r.json(); if (b && b.error) 原因 = String(b.error) } catch {}
              throw new Error(`${原因}（root=${root ?? '未解析'}，地址=${address ?? '无'}）`)
            }))
          .then((data) => setState({ phase: (data.total ?? 0) > 0 ? 'data' : 'empty', data }))
          .catch((e) => setState({ phase: 'error', message: String((e && e.message) || e) }))
      }, [])
      React.useEffect(() => { load() }, [load])
      return [state, load]
    }

    function 详情(r) {
      const 块 = (标题, 内容) => (内容 && 内容.length
        ? h('div', { key: 标题, style: { paddingBottom: 6 } }, [
            h('div', { key: 'h', style: { fontSize: 10, letterSpacing: '0.06em', color: C.sub } }, 标题),
            ...内容,
          ])
        : null)
      const 清单 = (r.acceptance_items ?? []).map((a) => h('div', {
        key: 'a' + a.ordinal, style: { fontSize: 12, color: a.checked ? C.sub : C.text, lineHeight: 1.5 },
      }, `${a.checked ? '✓' : '☐'} ${a.ordinal}. ${a.text}`))
      const 评论 = (r.comments ?? []).slice(0, 5).map((m, i) => h('div', {
        key: 'c' + i, style: { fontSize: 12, color: C.text, lineHeight: 1.5 },
      }, `${m.actor}：${m.body}`))
      const 产物 = (r.artifacts ?? []).slice(0, 5).map((a, i) => h('div', {
        key: 'p' + i, style: { fontSize: 12, color: C.sub, lineHeight: 1.5 },
      }, `${a.kind} · ${a.ref}`))
      if (!r.description && !清单.length && !评论.length && !产物.length) {
        return h('div', { style: { fontSize: 11.5, color: C.sub, padding: '2px 0 8px 64px' } }, '没有更多细节')
      }
      return h('div', { style: { padding: '6px 10px 10px 64px', margin: '0 -8px 4px', borderLeft: `2px solid ${C.tintLine}` } }, [
        r.description ? h('div', { key: 'd', style: { fontSize: 12, color: C.text, lineHeight: 1.55, whiteSpace: 'pre-wrap', paddingBottom: 6 } }, r.description) : null,
        块('验收', 清单),
        块('评论', 评论),
        块('产物', 产物),
      ])
    }

    function 行组件(r, 展开, onToggle) {
      const live = r.status === 'wip'
      const 验收 = r.acceptance
      return [h('button', {
        key: r.id, type: 'button', onClick: onToggle, 'aria-expanded': 展开 === r.id,
        style: {
          cursor: 'pointer', display: 'grid', gridTemplateColumns: '12px 54px 1fr auto', columnGap: 8,
          alignItems: 'baseline', padding: '8px 8px', margin: '0 -8px',
          width: 'calc(100% + 16px)', font: 'inherit', textAlign: 'left',
          background: live ? C.tint : 'transparent',
          border: 0, borderBottom: `0.5px solid ${C.line}`, borderRadius: live ? 8 : 0,
          ...(live ? { borderBottomColor: 'transparent' } : null),
        },
      }, [
        h('span', { key: 'g', style: { fontSize: 10, color: live ? C.accent : C.sub } }, live ? '▸' : r.status === 'done' ? '✓' : '·'),
        h('span', { key: 'i', style: { fontSize: 11, color: live ? C.text : C.sub, ...num } }, r.id),
        h('span', { key: 't', style: { fontSize: 13, lineHeight: 1.4, overflowWrap: 'anywhere', fontWeight: live ? 500 : 400 } }, r.title),
        h('span', { key: 'm', style: { fontSize: 10.5, color: C.sub, whiteSpace: 'nowrap', ...num } },
          live && r.holder ? r.holder
          : r.status === 'blocked' && r.block_reason ? String(r.block_reason).slice(0, 12)
          : 验收 ? `验收 ${验收.checked}/${验收.total}` : (r.priority ?? '')),
      ]), 展开 ? 详情(r) : null]
    }

    /** 分组标题：计划编号在前，便于和事件流里的 PLAN-x 对上。 */
    function 计划标题(键, 行) {
      if (键 === '（积压）') return 键
      const 名 = (行[0] && 行[0].plan) ?? ''
      return 名 && 名 !== 键 ? `${键} · ${名}` : 键
    }

    function BoardView({ state, onRefresh, defaultExpanded }) {
      const [展开, set展开] = React.useState(defaultExpanded ?? null)
      const 头 = h('div', { key: 'head', style: { display: 'flex', alignItems: 'baseline', justifyContent: 'space-between', paddingBottom: 10 } }, [
        h('span', { key: 't', style: { fontSize: 14, fontWeight: 500, color: C.text } }, '看板'),
        onRefresh ? h('button', { key: 'r', type: 'button', onClick: onRefresh,
          style: { border: 'none', background: 'transparent', padding: '4px 8px', margin: '-4px -8px', cursor: 'pointer', color: C.sub, fontSize: 12 } }, '刷新') : null,
      ])
      if (state.phase === 'loading') {
        return h('div', { style: { padding: 16, color: C.text } }, [头,
          h('div', { key: 'l', style: { fontSize: 12, color: C.sub, paddingBottom: 10 } }, '正在读 zen.zendb'),
          ...[0, 1, 2, 3].map((i) => h('div', { key: i, style: { height: 8, borderRadius: 4, background: C.skeleton, margin: '10px 0' } })),
        ])
      }
      if (state.phase === 'empty') {
        return h('div', { style: { padding: 16, color: C.text } }, [头,
          h('div', { key: 'e', style: { fontSize: 12, color: C.sub } }, '这个仓库还没有任务。让 agent 跑一次 zen_create。')])
      }
      if (state.phase === 'error') {
        return h('div', { style: { padding: 16, color: C.text } }, [头,
          h('div', { key: 'e', style: { fontSize: 12, color: C.sub, paddingBottom: 10 } }, `读不到看板：${state.message}`),
          onRefresh ? h('button', { key: 'b', type: 'button', onClick: onRefresh,
            style: { fontSize: 12, padding: '4px 10px', borderRadius: 8, border: `0.5px solid ${C.stroke}`, background: 'transparent', color: C.text, cursor: 'pointer' } }, '重试') : null])
      }
      const d = state.data
      const 计划详情 = d.计划详情 ?? {}
      const rows = d.rows ?? []
      const by = (d.stats && d.stats.by_status) || {}
      const 计划 = new Map()
      for (const r of rows) {
        const 键 = r.plan_id ?? r.plan ?? '（积压）'
        if (!计划.has(键)) 计划.set(键, [])
        计划.get(键).push(r)
      }
      return h('div', { style: { padding: 16, color: C.text } }, [
        头,
        h('div', { key: 'sum', style: { display: 'flex', flexWrap: 'wrap', gap: 12, fontSize: 12, color: C.sub, paddingBottom: 10, borderBottom: `0.5px solid ${C.line}` } },
          [['全部', d.total ?? rows.length], ...分组.map((g) => [g.label, by[g.key] ?? 0])]
            .map(([k, v]) => h('span', { key: k }, k + ' ', h('b', { key: 'v', style: { ...num, color: C.text } }, String(v))))),
        ...[...计划.entries()].map(([名, 行]) => h('div', { key: 'plan-' + 名, style: { paddingTop: 12 } }, [
          h('button', {
            key: 'ph', type: 'button',
            onClick: () => set展开(展开 === 'plan:' + 名 ? null : 'plan:' + 名),
            'aria-expanded': 展开 === 'plan:' + 名,
            style: {
              display: 'flex', justifyContent: 'space-between', gap: 8, font: 'inherit', width: '100%',
              fontSize: 11, color: C.sub, padding: 0, paddingBottom: 4, cursor: 'pointer',
              background: 'transparent', border: 0, textAlign: 'left',
            },
          }, [
            h('span', { key: 'n' }, 计划标题(名, 行) + (计划详情[名] ? (展开 === 'plan:' + 名 ? '  ▾' : '  ▸') : '')),
            h('span', { key: 'c', style: num }, `${行.filter((r) => r.status === 'done').length}/${行.length}`),
          ]),
          (展开 === 'plan:' + 名) ? (() => {
            const 详情 = 计划详情[名] ?? {}
            const 段 = (标签, 文本) => 文本
              ? h('div', { key: 标签, style: { paddingBottom: 6 } }, [
                  h('div', { key: 'h', style: { fontSize: 10, letterSpacing: '0.06em', color: C.sub } }, 标签),
                  h('div', { key: 'b', style: { fontSize: 12, color: C.text, lineHeight: 1.55, whiteSpace: 'pre-wrap' } }, 文本),
                ])
              : null
            const 无 = !详情.background && !详情.context
            return h('div', { key: 'pd', style: { padding: '4px 10px 8px 12px', margin: '0 -8px 6px', borderLeft: `2px solid ${C.tintLine}` } },
              无 ? [h('div', { key: 'e', style: { fontSize: 11.5, color: C.sub } }, '这个计划还没有 background / context 正文')]
                 : [段('background', 详情.background), 段('context', 详情.context)])
          })() : null,
          ...[...行].sort((a, b) => (分组.findIndex((g) => g.key === a.status) - 分组.findIndex((g) => g.key === b.status)) || String(a.id).localeCompare(String(b.id)))
            .flatMap((r) => 行组件(r, 展开 === r.id, () => set展开(展开 === r.id ? null : r.id))),
        ])),
        d.events && d.events.length
          ? h('div', { key: 'ev', style: { paddingTop: 14, borderTop: `0.5px solid ${C.line}`, marginTop: 12 } }, [
              h('div', { key: 'h', style: { fontSize: 10.5, letterSpacing: '0.08em', color: C.sub, paddingBottom: 6 } }, `近期事件 · 最近 ${d.events.length} 条`),
              ...d.events.slice(0, 4).map((e, i) => h('div', {
                key: 'e' + i, style: { display: 'grid', gridTemplateColumns: '42px minmax(0, 118px) 1fr', columnGap: 8, fontSize: 11, color: C.sub, padding: '2px 0' },
              }, [
                h('span', { key: 't', style: num }, String(e.at ?? '').slice(11, 16) + 'Z'),
                h('span', { key: 'k', style: { overflow: 'hidden', textOverflow: 'ellipsis' } }, e.kind),
                h('span', { key: 's', style: { color: C.text, ...num } }, e.subject_id ?? ''),
              ])),
            ])
          : null,
      ])
    }

    /** 槽位重渲染时 resourceAddress 可能变成 undefined，记住上一次的值。 */
    let 上次地址 = undefined
    function BoardPanel(props) {
      if (props && typeof props.resourceAddress === 'string' && props.resourceAddress !== '') 上次地址 = props.resourceAddress
      const [state, load] = useBoard(文件远端, (props && props.resourceAddress) ?? 上次地址)
      return h(BoardView, { state, onRefresh: load })
    }

    const BoardFileBody = BoardPanel

    function TabTitle() {
      return h('span', null, '看板')
    }

    return {
      // 依据 docs/subsystems/sidebar-right.md「Document renderers」：文本页签是 Document Preview 的 owner，
      // 它 provide 了 ctx.documentPreviews。渲染器在自己的 effect 里注册元数据，再等槽位挂 body。
      inject: ['slots', 'documentPreviews'],
      apply(ctx) {
        // remote.workspaceFiles 是独立命名空间，必须按全名注入（预览包自己也这么写）。
        // 缺了只影响仓库根解析，不影响插件激活，所以走 ctx.inject 而不是 module inject。
        try {
          ctx.inject(['remote.workspaceFiles'], (inner) => {
            try {
              文件远端 = inner.get('remote.workspaceFiles')
            } catch (e) { console.warn('[dsh-zen] remote.workspaceFiles 取值失败：', e && e.message ? e.message : e) }
          })
        } catch (e) { console.warn('[dsh-zen] ctx.inject 不可用：', e && e.message ? e.message : e) }
        try {
          ctx.effect(() => ctx.documentPreviews.register({
            id: KIND,
            extensions: ['zendb'],
            binaryExtensions: ['zendb'],
            priority: 'builtin',
            title: () => '看板',
            loading: 'bytes-complete',
            wrap: false,
          }), 'dsh-zen: board viewer')
        } catch (e) {
          console.warn('[dsh-zen] 看板预览注册失败：', e && e.message ? e.message : e)
        }
        try {
          ctx.slots.inject('sidebar.right.tab.document', () => ctx.slots.register({
            name: 'sidebar.right.tab.document', key: KIND,
          }, BoardPanel))
        } catch (e) {
          console.warn('[dsh-zen] 看板预览 body 注册失败：', e && e.message ? e.message : e)
        }
      },
      KIND,
      BoardView,
      BoardFileBody,
    }
  },
})
