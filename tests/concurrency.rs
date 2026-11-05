//! 跨进程并发：R9 / R21 / R23 的验收要求真并发证据，这里起真实子进程。
//!
//! 为什么放在 tests/：需要 CARGO_BIN_EXE_dsh-zen 拿到构建产物路径，单元测试拿不到；
//! 而且并发只有在独立进程里才有意义——同进程内的多个线程会共享「同一个」连接池假设。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn zen() -> &'static str {
    env!("CARGO_BIN_EXE_dsh-zen")
}

fn 跑(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(zen()).args(args).current_dir(dir).output().expect("子进程起不来");
    let mut 文本 = String::from_utf8_lossy(&out.stdout).into_owned();
    文本.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), 文本)
}

fn 临时仓库(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dsh-zen-conc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let (ok, out) = 跑(&dir, &["init", "--root", "."]);
    assert!(ok, "init 失败：{out}");
    dir
}

fn 统计(dir: &Path) -> serde_json::Value {
    let (ok, out) = 跑(dir, &["list"]);
    assert!(ok, "list 失败：{out}");
    serde_json::from_str(&out).expect("list 输出应是 JSON")
}

#[test]
fn 五人抢同一个任务只有一个成功() {
    let dir = 临时仓库("claim");
    let (ok, out) = 跑(&dir, &["create", "被抢的任务", "--actor", "seed@x"]);
    assert!(ok, "create 失败：{out}");

    let children: Vec<_> = (0..5)
        .map(|i| {
            let actor = format!("p{i}@x");
            Command::new(zen())
                .args(["move", "ZEN-1", "--to", "wip", "--actor", &actor])
                .current_dir(&dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("子进程起不来")
        })
        .collect();

    let mut 成功 = 0;
    let mut 失败 = 0;
    for child in children {
        let out = child.wait_with_output().unwrap();
        if out.status.success() {
            成功 += 1;
        } else {
            失败 += 1;
        }
    }
    assert_eq!(成功, 1, "只有一个人能进 wip");
    assert_eq!(失败, 4, "另外四个必须失败，不能静默丢更新");

    let (ok, out) = 跑(&dir, &["check"]);
    assert!(ok && out.contains("\"ok\": true"), "并发抢完之后数据要完好：{out}");
    let (ok, show) = 跑(&dir, &["show", "ZEN-1"]);
    assert!(ok && show.contains("\"status\": \"wip\""), "状态要落在 wip：{show}");
}

#[test]
fn 五进程并发写不损坏() {
    let dir = 临时仓库("write");

    let children: Vec<_> = (0..5)
        .map(|i| {
            Command::new(zen())
                .args(["create", &format!("并发第{i}条"), "--actor", &format!("p{i}@x")])
                .current_dir(&dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("子进程起不来")
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "并发建任务不该失败：{}", String::from_utf8_lossy(&out.stderr));
    }

    let 列表 = 统计(&dir);
    assert_eq!(列表["total"], 5, "五条都要落库：{列表}");
    let 编号: std::collections::HashSet<String> = 列表["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(编号.len(), 5, "编号不能撞车：{编号:?}");

    let (ok, out) = 跑(&dir, &["check"]);
    assert!(ok && out.contains("\"ok\": true"), "并发写完之后数据要完好：{out}");
}
