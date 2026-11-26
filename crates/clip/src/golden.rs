//! 黄金用例运行器。
//!
//! 目录结构：`golden/fixtures/<名字>/`（样例工程）和 `golden/cases/<ID>/case.yaml`（+ `expected/`）。
//! 用例可以用 `overlay` 在样例工程上叠加修改（JSON merge-patch 语义：映射递归合并，
//! 列表和标量整体替换，`null` 删除键），不用复制整个工程。所有比较都是逐字节的。

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use core_model::{format_duration, parse_duration, Tick};
use core_store::{is_source_file, SourceSet};
use serde::Deserialize;
use serde_json::{json, Value as Json};
use serde_yaml_ng::Value;

use crate::{EXIT_CONFLICT, EXIT_INTERNAL};

const REPORT: &str = "build/solve_report.json";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    title: String,
    kind: Kind,
    #[serde(default)]
    fixture: Option<String>,
    /// 叠加在样例工程上的修改：`文件名 → 补丁`
    #[serde(default)]
    overlay: Option<Value>,
    #[serde(default)]
    seeds: Option<u64>,
    #[serde(default)]
    iterations: Option<u32>,
    /// compile：报告里的 status 必须等于它
    #[serde(default)]
    expect_status: Option<String>,
    /// compile：冲突选项的 target 不能出现这些值
    #[serde(default)]
    forbid_option_targets: Vec<String>,
    /// locality：最多允许几个区变化
    #[serde(default)]
    max_changed_zones: Option<usize>,
    /// compile：这些代价项必须为 0
    #[serde(default)]
    expect_zero_costs: Vec<String>,
    /// compile：这些代价项必须大于 0
    #[serde(default)]
    expect_nonzero_costs: Vec<String>,
    /// compile：冲突里必须出现这些 code
    #[serde(default)]
    expect_conflict_codes: Vec<String>,
    /// compile：与不叠加 overlay 的样例工程相比，补丁锚点在时间线上移动的量
    #[serde(default)]
    expect_anchor_delta: BTreeMap<String, String>,
    /// compile：冲突里不能出现这些 code
    #[serde(default)]
    forbid_conflict_codes: Vec<String>,
    /// compile：报告里指定路径的值必须等于给定值
    #[serde(default)]
    expect_report: Vec<ReportCheck>,
    /// freeze：先冻结这些段落，再叠加 overlay
    #[serde(default)]
    freeze: Vec<String>,
    /// rule_effect：在 overlay 之上再叠加它，用来撤销规则
    #[serde(default)]
    revert: Option<Value>,
    /// rule_effect：撤销规则前后，只有这些片段不同
    #[serde(default)]
    expect_moved_clips: Vec<String>,
    /// rule_effect：撤销规则前后，这些片段必须完全相同
    #[serde(default)]
    expect_untouched_clips: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportCheck {
    path: Vec<String>,
    equals: Value,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Kind {
    /// 编译样例工程，与 expected/ 逐字节比对
    Compile,
    /// 连续编译多次（并把 build/ 混入源文件集合），结果必须完全相同
    Idempotent,
    /// 用不同种子打乱 YAML 映射的键顺序，结果必须完全相同
    Shuffle,
    /// 时间刻度往返与累加无漂移
    TickRoundtrip,
    /// 对比叠加修改前后，按区统计变化：段落整体平移不算变化
    Locality,
    /// 冻结段落后修改源文件：冻结段的片段必须逐字节不变
    Freeze,
    /// 撤销规则前后对比：只有规则改动过的片段不同，人的补丁不受影响
    RuleEffect,
}

type Files = BTreeMap<String, Vec<u8>>;

enum Outcome {
    Pass(String),
    Fail(String),
}

fn load_cases(root: &Path) -> Result<BTreeMap<String, Case>, String> {
    let dir = root.join("cases");
    let entries = fs::read_dir(&dir).map_err(|e| format!("读取 {} 失败：{e}", dir.display()))?;
    let mut cases = BTreeMap::new();
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path().join("case.yaml");
        if !path.exists() {
            continue;
        }
        let text = fs::read_to_string(&path).map_err(|e| format!("{}：{e}", path.display()))?;
        let case: Case =
            serde_yaml_ng::from_str(&text).map_err(|e| format!("{}：{e}", path.display()))?;
        cases.insert(case.id.clone(), case);
    }
    Ok(cases)
}

/// 样例工程本身（不含 overlay）。
fn base_fixture(root: &Path, case: &Case) -> Result<SourceSet, String> {
    let name = case
        .fixture
        .as_deref()
        .ok_or_else(|| format!("{} 缺少 fixture 字段", case.id))?;
    let dir = root.join("fixtures").join(name);
    SourceSet::read_dir(&dir).map_err(|e| format!("读取 {} 失败：{e}", dir.display()))
}

/// 样例工程 + overlay。
fn fixture(root: &Path, case: &Case) -> Result<SourceSet, String> {
    let base = base_fixture(root, case)?;
    match &case.overlay {
        None => Ok(base),
        Some(o) => apply_overlay(&base, o).map_err(|e| format!("{} 的 overlay：{e}", case.id)),
    }
}

fn apply_overlay(src: &SourceSet, overlay: &Value) -> Result<SourceSet, String> {
    let Value::Mapping(files) = overlay else {
        return Err("overlay 必须是“文件名 → 补丁”的映射".to_string());
    };
    let mut out = src.clone();
    for (k, patch) in files {
        let name = k.as_str().ok_or("文件名必须是字符串")?;
        if !is_source_file(name) {
            return Err(format!("不能修改 {name}：只能修改源文件"));
        }
        let text = src.get(name).unwrap_or("");
        let merged = if name.ends_with(".json") {
            let mut base: Json = if text.is_empty() {
                Json::Object(serde_json::Map::new())
            } else {
                serde_json::from_str(text).map_err(|e| format!("{name}：{e}"))?
            };
            let p: Json = serde_json::to_value(patch).map_err(|e| format!("{name}：{e}"))?;
            merge_json(&mut base, p);
            let mut s = serde_json::to_string_pretty(&base).map_err(|e| e.to_string())?;
            s.push('\n');
            s
        } else {
            let mut base: Value = if text.is_empty() {
                Value::Mapping(serde_yaml_ng::Mapping::new())
            } else {
                serde_yaml_ng::from_str(text).map_err(|e| format!("{name}：{e}"))?
            };
            merge_yaml(&mut base, patch.clone());
            serde_yaml_ng::to_string(&base).map_err(|e| format!("{name}：{e}"))?
        };
        out.insert(name, merged);
    }
    Ok(out)
}

fn merge_json(base: &mut Json, patch: Json) {
    match patch {
        Json::Object(p) => {
            if !base.is_object() {
                *base = Json::Object(serde_json::Map::new());
            }
            if let Json::Object(b) = base {
                for (k, v) in p {
                    if v.is_null() {
                        b.remove(&k);
                    } else if let Some(slot) = b.get_mut(&k) {
                        merge_json(slot, v);
                    } else {
                        b.insert(k, v);
                    }
                }
            }
        }
        other => *base = other,
    }
}

fn merge_yaml(base: &mut Value, patch: Value) {
    match patch {
        Value::Mapping(p) => {
            if !base.is_mapping() {
                *base = Value::Mapping(serde_yaml_ng::Mapping::new());
            }
            if let Value::Mapping(b) = base {
                for (k, v) in p {
                    if v.is_null() {
                        b.shift_remove(&k);
                    } else if let Some(slot) = b.get_mut(&k) {
                        merge_yaml(slot, v);
                    } else {
                        b.insert(k, v);
                    }
                }
            }
        }
        other => *base = other,
    }
}

fn compile(src: &SourceSet) -> Result<Files, String> {
    core_compile::compile(src)
        .map(|o| o.files)
        .map_err(|e| e.to_string())
}

fn timeline_of(files: &Files) -> Result<Json, String> {
    let bytes = files
        .get("build/timeline.json")
        .ok_or("编译没有产出时间线")?;
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}

/// 时间线上所有片段，以 ID 为键。
fn clips_by_id(timeline: &Json) -> BTreeMap<String, Json> {
    let mut out = BTreeMap::new();
    if let Some(tracks) = timeline["tracks"].as_object() {
        for list in tracks.values() {
            for c in list.as_array().into_iter().flatten() {
                if let Some(id) = c["id"].as_str() {
                    out.insert(id.to_string(), c.clone());
                }
            }
        }
    }
    out
}

/// 某段落的片段，时间改为相对段落起点。
fn section_clips(timeline: &Json, start: i64, beats: &BTreeSet<String>) -> BTreeMap<String, Json> {
    clips_by_id(timeline)
        .into_iter()
        .filter(|(_, c)| c["origin"].as_str().is_some_and(|o| beats.contains(o)))
        .map(|(id, mut c)| {
            for k in ["rec_in", "rec_out"] {
                let v = c[k].as_i64().unwrap_or(0) - start;
                c[k] = json!(v);
            }
            (id, c)
        })
        .collect()
}

fn lookup<'a>(v: &'a Json, path: &[String]) -> &'a Json {
    path.iter().fold(v, |acc, k| &acc[k.as_str()])
}

fn report_of(files: &Files) -> Result<Json, String> {
    let bytes = files.get(REPORT).ok_or("编译没有产出求解报告")?;
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}

fn expected_dir(root: &Path, id: &str) -> std::path::PathBuf {
    root.join("cases").join(id).join("expected")
}

/// 冲突选项里是否出现了这个 target。
fn offered(report: &Json, target: &str) -> bool {
    report["conflicts"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|c| c["options"].as_array().into_iter().flatten())
        .any(|o| o["target"].as_str() == Some(target))
}

/// 一个区的“相对内容”：段落报告（去掉绝对起点）+ 它的节拍报告。
fn zone_signature(report: &Json, sid: &str) -> Json {
    let mut sec = report["sections"][sid].clone();
    let ids: Vec<String> = sec["beats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect();
    if let Some(o) = sec.as_object_mut() {
        o.remove("start");
    }
    let beats: Vec<Json> = ids
        .iter()
        .map(|id| report["beats"][id.as_str()].clone())
        .collect();
    json!({"section": sec, "beats": beats})
}

fn changed_zones(a: &Json, b: &Json) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for r in [a, b] {
        if let Some(m) = r["sections"].as_object() {
            ids.extend(m.keys().cloned());
        }
    }
    ids.into_iter()
        .filter(|sid| zone_signature(a, sid) != zone_signature(b, sid))
        .collect()
}

fn check_compile(root: &Path, case: &Case) -> Result<Outcome, String> {
    let out = compile(&fixture(root, case)?)?;
    if let Some(k) = out.keys().find(|k| !k.starts_with("build/")) {
        return Ok(Outcome::Fail(format!("编译写了 build/ 之外的文件：{k}")));
    }
    let report = report_of(&out)?;
    let status = report["status"].as_str().unwrap_or("").to_string();
    if let Some(want) = &case.expect_status {
        if status != *want {
            return Ok(Outcome::Fail(format!(
                "status 应为 {want}，实际为 {status}"
            )));
        }
    }
    let cost_of = |name: &str| -> Option<u64> {
        report["cost"]
            .as_array()?
            .iter()
            .find(|x| x[0].as_str() == Some(name))
            .and_then(|x| x[1].as_u64())
    };
    for name in &case.expect_zero_costs {
        match cost_of(name) {
            Some(0) => {}
            Some(v) => return Ok(Outcome::Fail(format!("代价 {name} 应为 0，实际为 {v}"))),
            None => return Ok(Outcome::Fail(format!("报告里没有代价项 {name}"))),
        }
    }
    for name in &case.expect_nonzero_costs {
        match cost_of(name) {
            Some(v) if v > 0 => {}
            Some(_) => return Ok(Outcome::Fail(format!("代价 {name} 应大于 0"))),
            None => return Ok(Outcome::Fail(format!("报告里没有代价项 {name}"))),
        }
    }
    let codes: BTreeSet<&str> = report["conflicts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["code"].as_str())
        .collect();
    if let Some(c) = case
        .forbid_conflict_codes
        .iter()
        .find(|c| codes.contains(c.as_str()))
    {
        return Ok(Outcome::Fail(format!("冲突里不应有 {c}")));
    }
    for check in &case.expect_report {
        let want: Json = serde_json::to_value(&check.equals).map_err(|e| e.to_string())?;
        let got = lookup(&report, &check.path);
        if *got != want {
            return Ok(Outcome::Fail(format!(
                "报告 {} 应为 {want}，实际为 {got}",
                check.path.join(".")
            )));
        }
    }
    for code in &case.expect_conflict_codes {
        let found = report["conflicts"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c["code"].as_str() == Some(code.as_str()));
        if !found {
            return Ok(Outcome::Fail(format!("冲突里应有 {code}")));
        }
    }
    if !case.expect_anchor_delta.is_empty() {
        let before = report_of(&compile(&base_fixture(root, case)?)?)?;
        for (id, want) in &case.expect_anchor_delta {
            let want = parse_duration(want, None).map_err(|e| format!("{id}：{e}"))?;
            let a = before["patches"][id.as_str()]["at"].as_i64();
            let b = report["patches"][id.as_str()]["at"].as_i64();
            match (a, b) {
                (Some(a), Some(b)) if b - a == want.flicks() => {}
                (Some(a), Some(b)) => {
                    return Ok(Outcome::Fail(format!(
                        "{id} 的锚点应移动 {}，实际移动 {}",
                        format_duration(want),
                        format_duration(Tick(b - a))
                    )))
                }
                _ => return Ok(Outcome::Fail(format!("{id} 的锚点没有解析出位置"))),
            }
        }
    }
    for target in &case.forbid_option_targets {
        if offered(&report, target) {
            return Ok(Outcome::Fail(format!("冲突选项里不应出现 {target}")));
        }
    }
    let dir = expected_dir(root, &case.id);
    if !dir.exists() {
        return Ok(Outcome::Fail(format!(
            "缺少期望输出。运行 `clip golden bless --case {}`，逐行审阅后提交",
            case.id
        )));
    }
    let mut expected = Files::new();
    for name in out.keys() {
        let file = name.strip_prefix("build/").unwrap_or(name);
        let path = dir.join(file);
        let bytes = fs::read(&path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
        expected.insert(name.clone(), bytes);
    }
    let listed = fs::read_dir(&dir).map_err(|e| e.to_string())?.count();
    if listed != out.len() {
        return Ok(Outcome::Fail(format!(
            "expected/ 有 {listed} 个文件，编译产出 {} 个",
            out.len()
        )));
    }
    match describe(&expected, &out) {
        None => Ok(Outcome::Pass(format!(
            "{} 个文件逐字节一致（status: {status}）",
            out.len()
        ))),
        Some(d) => Ok(Outcome::Fail(d)),
    }
}

fn run_case(root: &Path, case: &Case) -> Result<Outcome, String> {
    match case.kind {
        Kind::TickRoundtrip => {
            let n = case.iterations.unwrap_or(1000);
            core_model::roundtrip_selftest(n)?;
            Ok(Outcome::Pass(format!("{n} 次往返和累加无漂移")))
        }
        Kind::Compile => check_compile(root, case),
        Kind::Idempotent => check_idempotent(&fixture(root, case)?),
        Kind::Shuffle => check_shuffle(&fixture(root, case)?, case.seeds.unwrap_or(20)),
        Kind::Freeze => {
            if case.freeze.is_empty() {
                return Err(format!("{} 是 freeze 用例，必须写 freeze", case.id));
            }
            let base = base_fixture(root, case)?;
            let before = compile(&base)?;
            let mut frozen = base.clone();
            for s in &case.freeze {
                let (name, bytes) = core_compile::freeze(&base, s).map_err(|e| e.to_string())?;
                frozen.insert(&name, String::from_utf8_lossy(&bytes).into_owned());
            }
            let after_src = match &case.overlay {
                None => frozen,
                Some(o) => apply_overlay(&frozen, o)?,
            };
            let after = compile(&after_src)?;
            let (rb, ra) = (report_of(&before)?, report_of(&after)?);
            let (tb, ta) = (timeline_of(&before)?, timeline_of(&after)?);
            let mut n = 0;
            for s in &case.freeze {
                if ra["sections"][s.as_str()]["frozen"] != json!(true) {
                    return Ok(Outcome::Fail(format!("{s} 没有被当作冻结段")));
                }
                let beats: BTreeSet<String> = rb["sections"][s.as_str()]["beats"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|b| b.as_str().map(str::to_string))
                    .collect();
                let start = |r: &Json| r["sections"][s.as_str()]["start"].as_i64().unwrap_or(0);
                let a = section_clips(&tb, start(&rb), &beats);
                let b = section_clips(&ta, start(&ra), &beats);
                if a.is_empty() {
                    return Ok(Outcome::Fail(format!("{s} 没有片段")));
                }
                if let Some((id, _)) = a.iter().find(|(id, c)| b.get(*id) != Some(*c)) {
                    return Ok(Outcome::Fail(format!("冻结段 {s} 的片段 {id} 变了")));
                }
                if b.len() != a.len() {
                    return Ok(Outcome::Fail(format!("冻结段 {s} 的片段数量变了")));
                }
                n += a.len();
            }
            if tb == ta {
                return Ok(Outcome::Fail(
                    "overlay 没有改变任何输出，用例没有意义".to_string(),
                ));
            }
            Ok(Outcome::Pass(format!(
                "冻结段的 {n} 个片段逐字节不变，其余部分照常变化"
            )))
        }
        Kind::RuleEffect => {
            let revert = case
                .revert
                .as_ref()
                .ok_or_else(|| format!("{} 是 rule_effect 用例，必须写 revert", case.id))?;
            let with = fixture(root, case)?;
            let without = apply_overlay(&with, revert)?;
            let ca = clips_by_id(&timeline_of(&compile(&with)?)?);
            let cb = clips_by_id(&timeline_of(&compile(&without)?)?);
            let ids: BTreeSet<&String> = ca.keys().chain(cb.keys()).collect();
            let moved: BTreeSet<String> = ids
                .into_iter()
                .filter(|id| ca.get(*id) != cb.get(*id))
                .cloned()
                .collect();
            let want: BTreeSet<String> = case.expect_moved_clips.iter().cloned().collect();
            if moved != want {
                return Ok(Outcome::Fail(format!(
                    "规则改动的片段应为 {want:?}，实际为 {moved:?}"
                )));
            }
            for id in &case.expect_untouched_clips {
                match (ca.get(id), cb.get(id)) {
                    (Some(a), Some(b)) if a == b => {}
                    _ => return Ok(Outcome::Fail(format!("片段 {id} 应完全不受规则影响"))),
                }
            }
            Ok(Outcome::Pass(format!(
                "规则只改动了 {}；撤销后恢复原样",
                moved.into_iter().collect::<Vec<_>>().join("、")
            )))
        }
        Kind::Locality => {
            if case.overlay.is_none() {
                return Err(format!("{} 是 locality 用例，必须写 overlay", case.id));
            }
            let before = report_of(&compile(&base_fixture(root, case)?)?)?;
            let after = report_of(&compile(&fixture(root, case)?)?)?;
            let zones = changed_zones(&before, &after);
            let max = case.max_changed_zones.unwrap_or(2);
            let detail = format!(
                "{} 个区变化（{}），上限 {max}",
                zones.len(),
                if zones.is_empty() {
                    "无".to_string()
                } else {
                    zones.join("、")
                }
            );
            if zones.len() <= max {
                Ok(Outcome::Pass(detail))
            } else {
                Ok(Outcome::Fail(detail))
            }
        }
    }
}

/// 连续编译 3 次（并把 build/ 产物混进源文件集合），结果必须逐字节一致。
fn check_idempotent(src: &SourceSet) -> Result<Outcome, String> {
    let first = compile(src)?;
    let mut with_build = src.clone();
    for (name, bytes) in &first {
        with_build.insert(name, String::from_utf8_lossy(bytes).into_owned());
    }
    for round in 1..=3 {
        let again = compile(&with_build)?;
        if let Some(d) = describe(&first, &again) {
            return Ok(Outcome::Fail(format!("第 {round} 次重新编译结果不同：{d}")));
        }
    }
    Ok(Outcome::Pass("连续 3 次编译逐字节一致".to_string()))
}

/// 用 `n` 个种子打乱 YAML 映射的键顺序，结果必须逐字节一致。
fn check_shuffle(src: &SourceSet, n: u64) -> Result<Outcome, String> {
    let base = compile(src)?;
    for seed in 1..=n {
        let shuffled = shuffle_sources(src, seed)?;
        let out = compile(&shuffled)?;
        if let Some(d) = describe(&base, &out) {
            return Ok(Outcome::Fail(format!("种子 {seed}：打乱后输出不同：{d}")));
        }
    }
    Ok(Outcome::Pass(format!("{n} 个种子打乱后输出一致")))
}

/// 把第一个能加长的停顿加长一帧，变化的区不能超过 2 个。
fn check_locality(src: &SourceSet) -> Result<Outcome, String> {
    let p = core_store::load(src).map_err(|e| e.to_string())?;
    let frame = p.config.fps.frame();
    let pick = p
        .sequence
        .iter()
        .filter(|s| !p.frozen.contains_key(*s))
        .flat_map(|s| p.sections[s].beats.iter())
        .find(|b| {
            let ps = &p.beats[*b].pause;
            ps.after + frame <= ps.max
        });
    let Some(beat) = pick else {
        return Ok(Outcome::Pass("没有可以加长的停顿，跳过".to_string()));
    };
    let after = p.beats[beat].pause.after + frame;
    let text = format!(
        "content.yaml:\n  beats:\n    \"{beat}\":\n      pause: {{after: \"{}\"}}\n",
        format_duration(after)
    );
    let overlay: Value = serde_yaml_ng::from_str(&text).map_err(|e| e.to_string())?;
    let before = report_of(&compile(src)?)?;
    let changed = report_of(&compile(&apply_overlay(src, &overlay)?)?)?;
    let zones = changed_zones(&before, &changed);
    let detail = format!(
        "{beat} 的停顿加长一帧：{} 个区变化（{}），上限 2",
        zones.len(),
        if zones.is_empty() {
            "无".to_string()
        } else {
            zones.join("、")
        }
    );
    if zones.len() <= 2 {
        Ok(Outcome::Pass(detail))
    } else {
        Ok(Outcome::Fail(detail))
    }
}

/// `clip determinism <dir>`：对任意工程跑幂等、顺序无关、局部性三项检查。
pub fn determinism(dir: &Path, json_out: bool) -> ExitCode {
    let src = match SourceSet::read_dir(dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("读取工程失败：{e}");
            return ExitCode::from(EXIT_INTERNAL);
        }
    };
    let checks = [
        ("idempotent", "幂等", check_idempotent(&src)),
        ("shuffle", "顺序无关", check_shuffle(&src, 20)),
        ("locality", "局部性", check_locality(&src)),
    ];
    let mut failed = 0;
    let mut results = Vec::new();
    for (key, name, r) in checks {
        let (pass, detail) = match r {
            Ok(Outcome::Pass(d)) => (true, d),
            Ok(Outcome::Fail(d)) => (false, d),
            Err(e) => (false, format!("运行出错：{e}")),
        };
        if !pass {
            failed += 1;
        }
        if json_out {
            results.push(json!({"check": key, "pass": pass, "detail": detail}));
        } else {
            println!("{} {name} — {detail}", if pass { "PASS" } else { "FAIL" });
        }
    }
    if json_out {
        println!("{}", json!({"failed": failed, "results": results}));
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_CONFLICT)
    }
}

pub fn run(root: &Path, only: Option<&str>, json_out: bool) -> ExitCode {
    let cases = match load_cases(root) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(EXIT_INTERNAL);
        }
    };
    if let Some(id) = only {
        if !cases.contains_key(id) {
            eprintln!("找不到用例 {id}");
            return ExitCode::from(EXIT_INTERNAL);
        }
    }
    let mut failed = 0;
    let mut results = Vec::new();
    for (id, case) in &cases {
        if only.is_some_and(|o| o != id) {
            continue;
        }
        let (pass, detail) = match run_case(root, case) {
            Ok(Outcome::Pass(d)) => (true, d),
            Ok(Outcome::Fail(d)) => (false, d),
            Err(e) => (false, format!("运行出错：{e}")),
        };
        if !pass {
            failed += 1;
        }
        if json_out {
            results.push(json!({"id": id, "title": case.title, "pass": pass, "detail": detail}));
        } else {
            println!(
                "{} {id} {} — {detail}",
                if pass { "PASS" } else { "FAIL" },
                case.title
            );
        }
    }
    if json_out {
        println!("{}", json!({"failed": failed, "results": results}));
    } else {
        let total = if only.is_some() { 1 } else { cases.len() };
        println!("\n{total} 个用例，{failed} 个失败");
    }
    if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_CONFLICT)
    }
}

fn bless_inner(root: &Path, id: &str) -> Result<String, String> {
    let cases = load_cases(root)?;
    let case = cases.get(id).ok_or_else(|| format!("找不到用例 {id}"))?;
    if case.kind != Kind::Compile {
        return Err(format!("{id} 不是 compile 类用例，没有期望文件"));
    }
    let out = compile(&fixture(root, case)?)?;
    let dir = expected_dir(root, id);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (name, bytes) in &out {
        let file = name.strip_prefix("build/").unwrap_or(name);
        fs::write(dir.join(file), bytes).map_err(|e| e.to_string())?;
    }
    Ok(dir.display().to_string())
}

pub fn bless(root: &Path, id: &str) -> ExitCode {
    match bless_inner(root, id) {
        Ok(dir) => {
            println!("已写入 {dir}。请逐行审阅，确认无误后再提交。");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(EXIT_INTERNAL)
        }
    }
}

/// 第一个差异的位置；完全相同返回 None。
fn describe(want: &Files, got: &Files) -> Option<String> {
    for (name, w) in want {
        match got.get(name) {
            None => return Some(format!("缺少 {name}")),
            Some(g) => {
                if let Some(d) = first_diff(w, g) {
                    return Some(format!("{name} {d}"));
                }
            }
        }
    }
    got.keys()
        .find(|k| !want.contains_key(*k))
        .map(|k| format!("多出 {k}"))
}

fn first_diff(want: &[u8], got: &[u8]) -> Option<String> {
    if want == got {
        return None;
    }
    let i = want
        .iter()
        .zip(got)
        .position(|(a, b)| a != b)
        .unwrap_or(want.len().min(got.len()));
    let line = want[..i].iter().filter(|&&b| b == b'\n').count() + 1;
    let show = |s: &[u8]| {
        String::from_utf8_lossy(s)
            .lines()
            .nth(line - 1)
            .unwrap_or("<文件结束>")
            .to_string()
    };
    Some(format!(
        "第 {line} 行\n    期望：{}\n    实际：{}",
        show(want),
        show(got)
    ))
}

/// 确定的伪随机数（SplitMix64），不依赖外部 crate，种子相同结果相同。
struct SplitMix64(u64);

impl SplitMix64 {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn shuffle<T>(&mut self, v: &mut [T]) {
        for i in (1..v.len()).rev() {
            let bound = u64::try_from(i + 1).unwrap_or(u64::MAX);
            let j = usize::try_from(self.next_u64() % bound).unwrap_or(0);
            v.swap(i, j);
        }
    }
}

/// 只打乱 YAML 映射的键顺序；序列（列表）有顺序语义，不动。
/// 重新序列化也顺带改变了引号、缩进等书写细节，一并验证格式无关。
fn shuffle_sources(src: &SourceSet, seed: u64) -> Result<SourceSet, String> {
    let mut rng = SplitMix64(seed);
    let mut out = SourceSet::default();
    for (name, text) in src.iter() {
        if name.ends_with(".yaml") {
            let v: Value = serde_yaml_ng::from_str(text).map_err(|e| format!("{name}：{e}"))?;
            let v = shuffle_value(v, &mut rng);
            out.insert(
                name,
                serde_yaml_ng::to_string(&v).map_err(|e| format!("{name}：{e}"))?,
            );
        } else {
            out.insert(name, text.to_string());
        }
    }
    Ok(out)
}

fn shuffle_value(v: Value, rng: &mut SplitMix64) -> Value {
    match v {
        Value::Mapping(m) => {
            let mut entries: Vec<(Value, Value)> = Vec::new();
            for (k, x) in m {
                entries.push((k, shuffle_value(x, rng)));
            }
            rng.shuffle(&mut entries);
            Value::Mapping(entries.into_iter().collect())
        }
        Value::Sequence(items) => {
            let mut out = Vec::with_capacity(items.len());
            for x in items {
                out.push(shuffle_value(x, rng));
            }
            Value::Sequence(out)
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_patch_semantics() {
        let mut base: Value = serde_yaml_ng::from_str("a: {b: 1, c: [1, 2]}\nd: 3\n").unwrap();
        let patch: Value = serde_yaml_ng::from_str("a: {b: 9, c: [7]}\nd: null\ne: 5\n").unwrap();
        merge_yaml(&mut base, patch);
        let want: Value = serde_yaml_ng::from_str("a: {b: 9, c: [7]}\ne: 5\n").unwrap();
        assert_eq!(base, want);

        let mut j = json!({"x": {"y": 1, "z": 2}});
        merge_json(&mut j, json!({"x": {"y": null, "w": 3}}));
        assert_eq!(j, json!({"x": {"z": 2, "w": 3}}));
    }

    #[test]
    fn zone_signature_ignores_absolute_start() {
        let a = json!({"sections": {"S1": {"start": 0, "dur": 5, "beats": ["S1.b1"]}},
                       "beats": {"S1.b1": {"offset": 0, "dur": 5}}});
        let mut b = a.clone();
        b["sections"]["S1"]["start"] = json!(100);
        assert!(changed_zones(&a, &b).is_empty());
        b["beats"]["S1.b1"]["dur"] = json!(6);
        assert_eq!(changed_zones(&a, &b), vec!["S1".to_string()]);
    }
}
