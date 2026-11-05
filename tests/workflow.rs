//! 端到端集成测试：所有命令都走真实二进制，不碰内部函数。
//!
//! 为什么单独一处：单元测试证明每个零件对，这里证明「一条真实工作流」接得上——
//! 建计划、被阻塞、领取、勾验收、挂产物、完成、复盘、重开、快照搬迁、导入，
//! 中间还穿插错误路径。它同时是 CLI 参数与 JSON 输出的契约测试。

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_dsh-zen")
}

/// 子进程原始结果：成功与否 + stdout 与 stderr 合并文本。
fn 跑(root: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(bin()).args(args).current_dir(root).output().expect("子进程起不来");
    let mut 文本 = String::from_utf8_lossy(&out.stdout).into_owned();
    文本.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), 文本)
}

/// 成功即解析 JSON；失败直接把输出拼进 panic，省得猜。
fn json(root: &Path, args: &[&str]) -> Value {
    let (ok, 文本) = 跑(root, args);
    assert!(ok, "`{args:?}` 应当成功，实际输出：\n{文本}");
    serde_json::from_str(&文本).unwrap_or_else(|e| panic!("`{args:?}` 输出不是 JSON（{e}）：\n{文本}"))
}

/// 失败用例：断言错误码，顺带把消息带回来便于看细节。
fn 失败(root: &Path, args: &[&str], 期望说明: &str) {
    let (ok, 文本) = 跑(root, args);
    assert!(!ok, "`{args:?}` 本应失败，实际成功：\n{文本}");
    assert!(文本.contains(期望说明), "`{args:?}` 的错误应含「{期望说明}」，实际：\n{文本}");
}

fn 临时仓库(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("dsh-zen-flow-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let (ok, 文本) = 跑(&root, &["init"]);
    assert!(ok, "init 失败：{文本}");
    root
}

fn 编号(v: &Value) -> String {
    v["id"].as_str().expect("缺少 id").to_string()
}

#[test]
fn 一条完整工作流从头走到尾() {
    let root = 临时仓库("lifecycle");

    // 自描述先就位：模型是先读 schema/workflow 再动手的
    let schema = json(&root, &["schema"]);
    assert_eq!(schema["schema_version"], 1);
    assert_eq!(schema["statuses"].as_array().unwrap().len(), 5);
    let 转移 = json(&root, &["workflow"])["transitions"].as_array().unwrap().len();
    assert_eq!(转移, 6);
    assert_eq!(json(&root, &["roles", "--role", "planner"])["can_transitions"].as_array().unwrap().len(), 4);

    // 建计划与两条任务
    let 甲 = json(&root, &["create", "甲", "--plan", "P 端到端", "--acceptance", "跑通", "--actor", "a@b"]);
    let 甲号 = 编号(&甲);
    assert_eq!(甲["plan"], "PLAN-1");
    let 乙 = json(&root, &["create", "乙", "--plan", "P 端到端", "--actor", "a@b"]);
    let 乙号 = 编号(&乙);

    // blocks 只给 planner；建完乙就从就绪里消失
    失败(&root, &["link", "--from", &甲号, "--to", &乙号, "--kind", "blocks"], "无权执行");
    assert_eq!(json(&root, &["link", "--from", &甲号, "--to", &乙号, "--kind", "blocks", "--role", "planner"])["existing"], false);
    let 就绪 = json(&root, &["list", "ready"]);
    assert_eq!(就绪["total"], 1, "被阻塞的乙不该进就绪：{就绪}");
    assert_eq!(就绪["rows"][0]["id"], 甲号);

    // 领取：进入 wip 即占用
    let 领取 = json(&root, &["move", &甲号, "--to", "wip", "--actor", "a@b"]);
    assert_eq!(领取["claim_actor"], "a@b");
    assert_eq!(json(&root, &["list", "mine", "--actor", "a@b"])["total"], 1, "mine 要看到自己持有的");
    json(&root, &["move", &甲号, "--to", "wip", "--extend", "--actor", "a@b"]);

    // 完成前先被守卫挡住，补齐凭据后才过
    失败(&root, &["move", &甲号, "--to", "done", "--actor", "a@b"], "acceptance_all_checked");
    json(&root, &["accept", &甲号, "--ordinal", "1", "--actor", "a@b"]);
    失败(&root, &["move", &甲号, "--to", "done", "--actor", "a@b"], "artifact_present");
    json(&root, &["artifact", &甲号, "--kind", "commit", "--ref", "abc1234", "--actor", "a@b"]);
    assert_eq!(json(&root, &["move", &甲号, "--to", "done", "--actor", "a@b"])["status"], "done");

    // 终态：能评论，不能再流转
    json(&root, &["comment", &甲号, "--body", "验收过了", "--actor", "c@d"]);
    失败(&root, &["move", &甲号, "--to", "wip", "--actor", "a@b"], "无权执行");

    // 复盘：第一次建一条，第二次零新增
    let 复盘 = json(&root, &["review", "--role", "reviewer", "--actor", "r@x"]);
    assert_eq!(复盘["reviewed"], 1);
    assert_eq!(json(&root, &["review", "--role", "reviewer", "--actor", "r@x"])["reviewed"], 0, "连跑两次不重复建");

    // 重开：只有 lead 能动，重开后回到 todo
    失败(&root, &["reopen", &甲号, "--role", "implementer", "--actor", "i@x"], "无权执行");
    let 重开 = json(&root, &["reopen", &甲号, "--role", "lead", "--actor", "l@x"]);
    assert_eq!(重开["from"], "done");
    assert_eq!(重开["status"], "todo");

    // 报告不改状态，只改派生阶段
    json(&root, &["report", &甲号, "--kind", "waiting", "--note", "等确认", "--actor", "a@b"]);
    let 详情 = json(&root, &["show", &甲号]);
    assert_eq!(详情["status"], "todo");
    assert_eq!(详情["stage"], "waiting_for_person");

    // 事件序列能解释上面每一次写
    let 事件 = json(&root, &["log", &甲号, "--limit", "50"]);
    let 种类: Vec<&str> = 事件["events"].as_array().unwrap().iter().map(|e| e["kind"].as_str().unwrap()).collect();
    for 期望 in ["create", "link", "status_change", "acceptance_checked", "artifact", "comment", "reopen", "report"] {
        assert!(种类.contains(&期望), "事件序列缺 {期望}：{种类:?}");
    }
    let seqs: Vec<i64> = 事件["events"].as_array().unwrap().iter().map(|e| e["seq"].as_i64().unwrap()).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "seq 必须严格递增：{seqs:?}");

    // 全量校验只报不修，此时应当干净
    let 检查 = json(&root, &["check"]);
    assert_eq!(检查["ok"], true, "工作流走完数据应当自洽：{检查}");

    let _ = std::fs::remove_dir_all(&root);
}


#[test]
fn 导入计划文档两次不重复() {
    let root = 临时仓库("import");
    let 文档 = root.join("计划.md");
    std::fs::write(
        &文档,
        "# P 导入\n\n## 快速检查表\n\n| # | 任务 | 文件 | 状态 |\n|---|---|---|---|\n| T1 | 甲 | a.rs | [TODO] |\n| T2 | 乙 | b.rs | [DONE] |\n| T3 | 丙 | c.rs | [DEPRECATED] |\n",
    )
    .unwrap();

    let 第一次 = json(&root, &["import", "--path", 文档.to_str().unwrap(), "--actor", "a@b"]);
    assert_eq!(第一次["imported"], 3);
    assert_eq!(第一次["dropped"], 1, "DEPRECATED 直接置为 dropped");
    assert_eq!(json(&root, &["list"])["total"], 3, "计数与原文逐条对齐");

    let 第二次 = json(&root, &["import", "--path", 文档.to_str().unwrap(), "--actor", "a@b"]);
    assert_eq!(第二次["imported"], 0);
    assert_eq!(第二次["existing"], 3);
    assert_eq!(json(&root, &["list"])["total"], 3, "重复导入不产生新任务");

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn 工具面与子命令等价() {
    let root = 临时仓库("rpc");
    json(&root, &["create", "甲", "--plan", "P", "--actor", "a@b"]);

    // 插件走的就是这条路径：一行请求一行结果
    let 请求 = "{\"tool\":\"zen_list\",\"args\":{\"view\":\"ready\"}}\n";
    let out = Command::new(bin())
        .args(["rpc"])
        .current_dir(&root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.as_mut().unwrap().write_all(请求.as_bytes())?;
            child.wait_with_output()
        })
        .expect("rpc 起不来");

    let 响应: Value = serde_json::from_slice(&out.stdout).expect("rpc 输出应是 JSON");
    assert_eq!(响应["ok"], true, "工具调用失败：{响应}");
    assert_eq!(响应["data"], json(&root, &["list", "ready"]), "工具与子命令必须同结果");

    // 错误也按行返回，不中断
    let 坏请求 = "{\"tool\":\"zen_move\",\"args\":{\"id\":\"ZEN-99\",\"to\":\"wip\"}}\n";
    let out = Command::new(bin())
        .args(["rpc"])
        .current_dir(&root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.as_mut().unwrap().write_all(坏请求.as_bytes())?;
            child.wait_with_output()
        })
        .expect("rpc 起不来");
    let 响应: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(响应["ok"], false);
    assert_eq!(响应["code"], "not_found");

    let _ = std::fs::remove_dir_all(&root);
}
