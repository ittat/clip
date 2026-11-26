//! `clip diff <a> <b>`：比较两个工程的编译结果。退出码：0 相同 / 1 不同。

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::ExitCode;

use core_compile::CompileError;
use core_model::{format_duration, Tick};
use core_store::SourceSet;
use serde_json::{json, Value};

use crate::{EXIT_CONFLICT, EXIT_FORMAT, EXIT_INTERNAL};

fn build(dir: &Path) -> Result<(Value, Value), ExitCode> {
    let src = SourceSet::read_dir(dir).map_err(|e| {
        eprintln!("读取 {} 失败：{e}", dir.display());
        ExitCode::from(EXIT_INTERNAL)
    })?;
    let out = core_compile::compile(&src).map_err(|e| {
        eprintln!("{}：{e}", dir.display());
        match e {
            CompileError::Internal(_) => ExitCode::from(EXIT_INTERNAL),
            CompileError::Load(_) | CompileError::Usage(_) => ExitCode::from(EXIT_FORMAT),
        }
    })?;
    let get = |name: &str| -> Result<Value, ExitCode> {
        let Some(bytes) = out.files.get(name) else {
            eprintln!("编译没有产出 {name}");
            return Err(ExitCode::from(EXIT_INTERNAL));
        };
        serde_json::from_slice(bytes).map_err(|e| {
            eprintln!("{name}：{e}");
            ExitCode::from(EXIT_INTERNAL)
        })
    };
    Ok((get(core_compile::REPORT)?, get(core_compile::TIMELINE)?))
}

fn clips(timeline: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    if let Some(tracks) = timeline["tracks"].as_object() {
        for (track, list) in tracks {
            for c in list.as_array().into_iter().flatten() {
                if let Some(id) = c["id"].as_str() {
                    let mut c = c.clone();
                    c["track"] = json!(track);
                    out.insert(id.to_string(), c);
                }
            }
        }
    }
    out
}

fn terms(r: &Value) -> BTreeMap<String, Value> {
    r["cost"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| Some((x[0].as_str()?.to_string(), x[1].clone())))
        .collect()
}

fn codes(r: &Value) -> BTreeSet<String> {
    r["conflicts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["code"].as_str().map(str::to_string))
        .collect()
}

fn time(v: &Value) -> String {
    v.as_i64()
        .map_or_else(|| v.to_string(), |x| format_duration(Tick(x)))
}

/// 片段里不同的字段：字段名 → [a, b]。
fn field_diff(x: &Value, y: &Value) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    if let (Some(mx), Some(my)) = (x.as_object(), y.as_object()) {
        let keys: BTreeSet<&String> = mx.keys().chain(my.keys()).collect();
        for k in keys {
            let a = mx.get(k).cloned().unwrap_or(Value::Null);
            let b = my.get(k).cloned().unwrap_or(Value::Null);
            if a != b {
                out.insert(k.clone(), json!([a, b]));
            }
        }
    }
    out
}

pub fn run(a: &Path, b: &Path, json_out: bool) -> ExitCode {
    let ((ra, ta), (rb, tb)) = match (build(a), build(b)) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(c), _) | (_, Err(c)) => return c,
    };

    let keys: BTreeSet<String> = [&ra, &rb]
        .iter()
        .filter_map(|r| r["sections"].as_object())
        .flat_map(|m| m.keys().cloned())
        .collect();
    let mut sections = Vec::new();
    for k in &keys {
        let (x, y) = (&ra["sections"][k.as_str()], &rb["sections"][k.as_str()]);
        if x["hash"] != y["hash"] {
            sections.push(
                json!({"section": k, "dur": [x["dur"], y["dur"]], "hash": [x["hash"], y["hash"]]}),
            );
        }
    }

    let (ca, cb) = (terms(&ra), terms(&rb));
    let names: BTreeSet<&String> = ca.keys().chain(cb.keys()).collect();
    let mut cost = Vec::new();
    for k in names {
        let x = ca.get(k).cloned().unwrap_or(Value::Null);
        let y = cb.get(k).cloned().unwrap_or(Value::Null);
        if x != y {
            cost.push(json!({"term": k, "a": x, "b": y}));
        }
    }

    let (la, lb) = (clips(&ta), clips(&tb));
    let ids: BTreeSet<&String> = la.keys().chain(lb.keys()).collect();
    let (mut added, mut removed, mut changed) = (Vec::new(), Vec::new(), Vec::new());
    for id in ids {
        match (la.get(id), lb.get(id)) {
            (None, Some(_)) => added.push(id.clone()),
            (Some(_), None) => removed.push(id.clone()),
            (Some(x), Some(y)) if x != y => {
                changed.push(json!({"id": id, "fields": field_diff(x, y)}));
            }
            _ => {}
        }
    }

    let (xa, xb) = (codes(&ra), codes(&rb));
    let new_conflicts: Vec<&String> = xb.difference(&xa).collect();
    let gone_conflicts: Vec<&String> = xa.difference(&xb).collect();

    let same = ra["status"] == rb["status"]
        && ra["total"] == rb["total"]
        && sections.is_empty()
        && cost.is_empty()
        && added.is_empty()
        && removed.is_empty()
        && changed.is_empty()
        && new_conflicts.is_empty()
        && gone_conflicts.is_empty();

    if json_out {
        println!(
            "{}",
            json!({
                "same": same,
                "status": [ra["status"], rb["status"]],
                "total": [ra["total"], rb["total"]],
                "sections": sections,
                "cost": cost,
                "clips": {"added": added, "removed": removed, "changed": changed},
                "conflicts": {"added": new_conflicts, "removed": gone_conflicts},
            })
        );
    } else if same {
        println!("两个工程的编译结果相同");
    } else {
        println!(
            "状态：{} → {}；总长 {} → {}",
            ra["status"],
            rb["status"],
            time(&ra["total"]),
            time(&rb["total"])
        );
        for s in &sections {
            println!(
                "区 {}：时长 {} → {}",
                s["section"].as_str().unwrap_or("?"),
                time(&s["dur"][0]),
                time(&s["dur"][1])
            );
        }
        for c in &cost {
            println!(
                "代价 {}：{} → {}",
                c["term"].as_str().unwrap_or("?"),
                c["a"],
                c["b"]
            );
        }
        if !added.is_empty() {
            println!("新增片段：{}", added.join("、"));
        }
        if !removed.is_empty() {
            println!("删除片段：{}", removed.join("、"));
        }
        for c in &changed {
            println!("片段 {}：", c["id"].as_str().unwrap_or("?"));
            if let Some(f) = c["fields"].as_object() {
                for (k, v) in f {
                    let show = |x: &Value| {
                        if k.starts_with("rec_") || k.starts_with("src_") {
                            time(x)
                        } else {
                            x.to_string()
                        }
                    };
                    println!("  {k}：{} → {}", show(&v[0]), show(&v[1]));
                }
            }
        }
        if !new_conflicts.is_empty() {
            let v: Vec<&str> = new_conflicts.iter().map(|s| s.as_str()).collect();
            println!("新出现的冲突：{}", v.join("、"));
        }
        if !gone_conflicts.is_empty() {
            let v: Vec<&str> = gone_conflicts.iter().map(|s| s.as_str()).collect();
            println!("消失的冲突：{}", v.join("、"));
        }
    }
    if same {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_CONFLICT)
    }
}
