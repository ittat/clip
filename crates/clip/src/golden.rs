//! 黄金用例运行器。
//!
//! 目录结构：`golden/fixtures/<名字>/`（样例工程）和 `golden/cases/<ID>/case.yaml`（+ `expected/`）。
//! 所有比较都是逐字节的。

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use core_store::SourceSet;
use serde::Deserialize;
use serde_json::json;
use serde_yaml_ng::Value;

use crate::{EXIT_CONFLICT, EXIT_INTERNAL};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    title: String,
    kind: Kind,
    #[serde(default)]
    fixture: Option<String>,
    #[serde(default)]
    seeds: Option<u64>,
    #[serde(default)]
    iterations: Option<u32>,
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

fn fixture(root: &Path, case: &Case) -> Result<SourceSet, String> {
    let name = case
        .fixture
        .as_deref()
        .ok_or_else(|| format!("{} 缺少 fixture 字段", case.id))?;
    let dir = root.join("fixtures").join(name);
    SourceSet::read_dir(&dir).map_err(|e| format!("读取 {} 失败：{e}", dir.display()))
}

fn compile(src: &SourceSet) -> Result<Files, String> {
    core_compile::compile(src)
        .map(|o| o.files)
        .map_err(|e| e.to_string())
}

fn expected_dir(root: &Path, id: &str) -> std::path::PathBuf {
    root.join("cases").join(id).join("expected")
}

fn run_case(root: &Path, case: &Case) -> Result<Outcome, String> {
    match case.kind {
        Kind::TickRoundtrip => {
            let n = case.iterations.unwrap_or(1000);
            core_model::roundtrip_selftest(n)?;
            Ok(Outcome::Pass(format!("{n} 次往返和累加无漂移")))
        }
        Kind::Compile => {
            let out = compile(&fixture(root, case)?)?;
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
                let bytes =
                    fs::read(&path).map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
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
                None => Ok(Outcome::Pass(format!("{} 个文件逐字节一致", out.len()))),
                Some(d) => Ok(Outcome::Fail(d)),
            }
        }
        Kind::Idempotent => {
            let src = fixture(root, case)?;
            let first = compile(&src)?;
            // 把 build/ 产物混进源文件集合：编译器必须忽略它们
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
        Kind::Shuffle => {
            let src = fixture(root, case)?;
            let base = compile(&src)?;
            let n = case.seeds.unwrap_or(20);
            for seed in 1..=n {
                let shuffled = shuffle_sources(&src, seed)?;
                let out = compile(&shuffled)?;
                if let Some(d) = describe(&base, &out) {
                    return Ok(Outcome::Fail(format!("种子 {seed}：打乱后输出不同：{d}")));
                }
            }
            Ok(Outcome::Pass(format!("{n} 个种子打乱后输出一致")))
        }
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
