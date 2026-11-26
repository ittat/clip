//! L2 → L1 编译。只读源文件（`SourceSet`），只产出 `build/` 下的文件。

use std::collections::BTreeMap;

use core_model::*;
use core_solver::{solve, BeatInput, BeatPlacement, Glue, GlueKind, SolveInput, SolveOutput};
use core_store::{canon, load, LoadErrors, SourceSet};
use serde_json::{json, Value};

pub const TIMELINE: &str = "build/timeline.json";
pub const REPORT: &str = "build/solve_report.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    /// 求解出现第 1–4 项代价：需要上报
    Conflict,
}

#[derive(Debug)]
pub struct BuildOutput {
    pub files: BTreeMap<String, Vec<u8>>,
    pub status: Status,
}

#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error("工程格式错误：\n{0}")]
    Load(#[from] LoadErrors),
    #[error("内部错误：{0}")]
    Internal(String),
}

pub fn compile(src: &SourceSet) -> Result<BuildOutput, CompileError> {
    let p = load(src)?;
    let out = solve(&solve_input(&p), None);
    let status = if out.cost.needs_escalation() {
        Status::Conflict
    } else {
        Status::Ok
    };
    let mut warnings = Vec::new();
    let timeline = timeline(&p, &out, &mut warnings);
    let report = report(&p, &out, status, warnings);
    let mut files = BTreeMap::new();
    for (name, v) in [(TIMELINE, timeline), (REPORT, report)] {
        let bytes = canon::to_bytes(&v).map_err(|e| CompileError::Internal(e.to_string()))?;
        files.insert(name.to_string(), bytes);
    }
    Ok(BuildOutput { files, status })
}

fn solve_input(p: &Project) -> SolveInput {
    let beats = p
        .ordered_beats()
        .into_iter()
        .map(|id| {
            let b = &p.beats[id];
            let mut hard = Vec::new();
            if let Some(v) = &b.voice {
                let dur = p.tts[&v.narration].dur;
                hard.push(Glue {
                    natural: dur,
                    stretch: Tick::ZERO,
                    shrink: Tick::ZERO,
                    kind: GlueKind::Rigid,
                });
            }
            let pause = Glue {
                natural: b.pause.after,
                stretch: b.pause.max - b.pause.after,
                shrink: b.pause.after - b.pause.min,
                kind: GlueKind::Pause,
            };
            BeatInput {
                id: id.clone(),
                hard,
                pause,
            }
        })
        .collect();
    SolveInput { beats }
}

fn t(x: Tick) -> Value {
    json!(x.flicks())
}

fn clip(id: &str, origin: &BeatId, src: &str, src_in: Tick, src_out: Tick, rec_in: Tick) -> Value {
    json!({
        "id": id,
        "origin": origin.as_str(),
        "src": src,
        "src_in": t(src_in),
        "src_out": t(src_out),
        "rec_in": t(rec_in),
        "rec_out": t(rec_in + (src_out - src_in)),
    })
}

fn timeline(p: &Project, out: &SolveOutput, warnings: &mut Vec<Value>) -> Value {
    let mut tracks: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    for pl in &out.beats {
        let b = &p.beats[&pl.id];
        let end = pl.start + pl.dur;
        if let Some(v) = &b.voice {
            let e = &p.tts[&v.narration];
            let id = format!("{}/voice", pl.id);
            tracks.entry("A1").or_default().push(clip(
                &id,
                &pl.id,
                &e.audio,
                Tick::ZERO,
                e.dur,
                pl.start,
            ));
        }
        for (i, vis) in b.visual.iter().enumerate() {
            let shot = &p.media.shots[&vis.shot];
            let rec_in = match &vis.from {
                Some(w) => {
                    pl.start
                        + p.tts[&w.narration]
                            .word(w.index)
                            .map_or(Tick::ZERO, |x| x.t0)
                }
                None => pl.start,
            };
            let want = end - rec_in;
            let have = shot.duration();
            let len = if have < want {
                // PR 1 不做填充：如实报告缺口（PR 2 起由求解器接候选或慢放）
                warnings.push(json!({
                    "code": "visual_shortfall",
                    "beat": pl.id.as_str(),
                    "shot": vis.shot.as_str(),
                    "missing": (want - have).flicks(),
                }));
                have
            } else {
                want
            };
            let track = match vis.role {
                Role::ARoll => "V1",
                Role::BRoll => "V2",
            };
            let id = format!("{}/visual[{i}]", pl.id);
            tracks.entry(track).or_default().push(clip(
                &id,
                &pl.id,
                &shot.src,
                shot.src_in,
                shot.src_in + len,
                rec_in,
            ));
        }
    }
    json!({
        "format": 1,
        "unit": "flicks",
        "solver": out.version,
        "fps": {"num": p.config.fps.num(), "den": p.config.fps.den()},
        "duration": t(out.total),
        "tracks": tracks,
    })
}

fn report(p: &Project, out: &SolveOutput, status: Status, warnings: Vec<Value>) -> Value {
    let (lo, hi) = p.intent.window();
    let by_id: BTreeMap<&BeatId, &BeatPlacement> = out.beats.iter().map(|b| (&b.id, b)).collect();

    let mut beats = serde_json::Map::new();
    for pl in &out.beats {
        beats.insert(
            pl.id.to_string(),
            json!({"start": t(pl.start), "dur": t(pl.dur), "hard": t(pl.hard), "pause": t(pl.pause)}),
        );
    }

    let mut sections = serde_json::Map::new();
    for sid in &p.sequence {
        let s = &p.sections[sid];
        let natural = s.beats.iter().fold(Tick::ZERO, |acc, b| acc + by_id[b].dur);
        let (kind, budget, over) = match s.budget {
            Some(bud) => ("declared", t(bud), t(natural - bud)),
            None => ("derived", Value::Null, Value::Null),
        };
        sections.insert(
            sid.to_string(),
            json!({"natural": t(natural), "budget": budget, "budget_kind": kind, "over": over}),
        );
    }

    let cost: Vec<Value> = out
        .cost
        .terms()
        .iter()
        .map(|(k, v)| json!([k, v]))
        .collect();

    // PR 1 没有执行的约束要如实列出，避免“静默忽略”
    let mut not_applied = Vec::new();
    for id in p.ordered_beats() {
        if p.beats[id].music.is_some() {
            not_applied.push(format!("music:{id}"));
        }
    }
    for id in p.overrides.keys() {
        not_applied.push(format!("override:{id}"));
    }
    for id in &p.rules {
        not_applied.push(format!("rule:{id}"));
    }
    if !p.intent.must.is_empty() {
        not_applied.push("intent.must".to_string());
    }
    not_applied.push("pause.flex".to_string());

    let status = match status {
        Status::Ok => "ok",
        Status::Conflict => "conflict",
    };
    let within = out.total >= lo && out.total <= hi;
    json!({
        "format": 1,
        "solver": out.version,
        "project_solver": p.config.solver,
        "status": status,
        "total": t(out.total),
        "intent": {
            "target": t(p.intent.target),
            "min": t(lo),
            "max": t(hi),
            "within": within,
        },
        "sections": sections,
        "beats": beats,
        "cost": cost,
        "warnings": warnings,
        "not_applied": not_applied,
    })
}
