#!/usr/bin/env bash
# dsh-zen SRS 机械校验。
#
# 校验六条跨表不变量，它们是改写时最容易掉东西的地方：
#   1. §6 需求编号连续且不重复
#   2. 章节引用（§x.y）都指向真实存在的章节
#   3. §5 命令面的每个命令在 §6 里至少被引用一次
#   4. §6 反引号里的标识符都在 §2 模型 / §5 命令 / 枚举白名单里
#   5. §3.1 定义的每条状态转移都在 §4.2 权限矩阵里有一格
#   6. 已裁定删除的概念不出现在正文
#
# 用法：bash tools/check-spec.sh [规格文件]
# 退出码 0 = 全部通过；1 = 有失配。
#
# 每个解析器都带零命中守卫：解析规则一旦失效、返回空集，直接报错而不是绿着放行。
set -uo pipefail
SRS="${1:-$(dirname "$0")/../doc/SRS.md}"; fail=0
sec(){ awk -v s="$2" 'index($0,s)==1{f=1;next} f&&/^#+ /{exit} f' "$1"; }
err(){ echo "  $*"; fail=1; }

ids=$(grep -oE '^\| R[0-9]+ \|' "$SRS" | grep -oE '[0-9]+' | sort -n)
[ -z "$ids" ] && err "[编号] 未解析到需求行"
if [ -n "$ids" ]; then
  d=$(echo "$ids" | uniq -d); [ -n "$d" ] && err "[编号] 重复: $(echo $d)"
  lo=$(echo "$ids"|head -1); hi=$(echo "$ids"|tail -1)
  miss=$(seq "$lo" "$hi" | comm -23 - <(echo "$ids"))
  [ -n "$miss" ] && err "[编号] 缺号: $(echo $miss)"
fi

refs=$(grep -oE '§[0-9]+(\.[0-9]+)?' "$SRS" | tr -d '§' | sort -u)
heads=$(grep -oE '^#{2,3} [0-9]+(\.[0-9]+)?' "$SRS" | awk '{print $2}' | sort -u)
[ -z "$refs" ] && err "[引用] 未解析到任何章节引用（解析器可能已失效）"
bad=$(comm -23 <(echo "$refs") <(echo "$heads"))
[ -n "$bad" ] && err "[引用] 悬空: $(echo $bad)"

req=$(sec "$SRS" "## 6.")
[ -z "$req" ] && err "[章节] 未找到 §6 需求章节"
nc=0
while IFS= read -r c; do
  [ -z "$c" ] && continue
  nc=$((nc+1))
  # 必须是反引号开头的代码片段：裸词会撞上同名字段或 flag（status 撞 R18 的过滤项、accept 撞 acceptance）
  grep -q -- "\`$c" <<<"$req" || err "[命令] §5 有 $c 但 §6 未引用"
done < <(sec "$SRS" "## 5." | grep -oE '^\| `[a-z-]+' | tr -d '|` ')
[ "$nc" -eq 0 ] && err "[命令] §5 未解析到任何命令（解析器可能已失效）"

known=$( { sec "$SRS" "## 2."; sec "$SRS" "## 5."; } | grep -oE '`[a-z][a-z0-9_]*`' | tr -d '`' | sort -u )
[ -z "$known" ] && err "[模型] §2/§5 未解析到任何标识符"
tok=$(grep -oE '`[a-z][a-z0-9_]*`' <<<"$req" | tr -d '`' | sort -u)
ENUMS='^(todo|wip|done|blocked|dropped|ready|mine|backlog|stale|review|planner|implementer|reviewer|tester|lead|file|commit|url|note|blocks|relates|duplicates|parent|progress|waiting|failed|issue|plan|config|git|diff|assert|json|sqlite|wal|python|rust|toml|utf|force|role|actor|unclaimed|permission)$'
unk=$(comm -23 <(echo "$tok") <(echo "$known") | grep -vE "$ENUMS")
[ -n "$unk" ] && err "[模型] §6 未定义: $(echo $unk)"

life=$(sec "$SRS" "### 3.1")
[ -z "$life" ] && err "[章节] 未找到 §3.1"
cells=$(sec "$SRS" "### 4.2" | grep -oE '`[^`]*→[^`]*`' | tr -d '`' | tr -d ' ' | sort -u)
np=0
while IFS= read -r L; do
  seg=$(printf '%s' "$L" | sed 's/→/\n/g' | grep -oE '`[a-z]+`' | tr -d '`')
  q=$(printf '%s\n' "$seg" | awk 'NR>1{print p"→"$0}{p=$0}')
  [ -z "$q" ] && continue
  while IFS= read -r p; do
    [ -z "$p" ] && continue
    np=$((np+1)); b=${p#*→}
    grep -qx -- "$p" <<<"$cells" || grep -qx -- "→$b" <<<"$cells" || err "[转移] §3.1 有 $p 但 §4.2 无此格"
  done <<<"$q"
done < <(grep -- '→' <<<"$life" | tr '；' '\n')
[ "$np" -eq 0 ] && err "[转移] 解析到 0 条（解析器可能已失效）"

zone=$(grep -n '^## 1\.' "$SRS" | head -1 | cut -d: -f1)
for term in 工单 Mandate 放权档位; do
  hits=$(tail -n +"${zone:-1}" "$SRS" | grep -n -- "$term" | cut -d: -f1)
  [ -n "$hits" ] && err "[残留] 已删概念「$term」出现在正文"
done

[ $fail -eq 0 ] && echo "PASS: 需求 $(echo "$ids"|wc -l) 条 / 命令 $nc 个 / 转移 $np 条"
exit $fail
