//! L2 → L1 编译。只读源文件（`SourceSet`），只产出 `build/` 下的文件。

use std::collections::BTreeMap;

use core_model::*;
use core_solver::{
    solve, BeatInput, BeatPlacement, FillKind, Glue, GlueKind, SolveInput, SolveOutput,
    VisualInput, MAX_RETIME_PERCENT,
};
use core_store::{canon, load, LoadErrors, SourceSet};
use serde_json::{json, Value};

pub const TIMELINE: &str = "build/timeline.json";
pub const REPORT: &str = "build/solve_report.json";

/// 正常速度（ppm）
const UNIT_SPEED: i64 = 1_000_000;

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

/// 画面相对节拍开始的入画时间。
fn visual_offset(p: &Project, vis: &Visual) -> Tick {
    match &vis.from {
        Some(w) => p.tts[&w.narration]
            .word(w.index)
            .map_or(Tick::ZERO, |x| x.t0),
        None => Tick::ZERO,
    }
}

fn solve_input(p: &Project) -> SolveInput {
    let beats = p
        .ordered_beats()
        .into_iter()
        .map(|id| {
            let b = &p.beats[id];
            let mut hard = Vec::new();
            if let Some(v) = &b.voice {
                hard.push(Glue {
                    natural: p.tts[&v.narration].dur,
                    stretch: Tick::ZERO,
                    shrink: Tick::ZERO,
                    kind: GlueKind::Rigid,
                });
            }
            let visuals = b
                .visual
                .iter()
                .map(|vis| VisualInput {
                    offset: visual_offset(p, vis),
                    have: p.media.shots[&vis.shot].duration(),
                    rigid: matches!(vis.fit, Fit::Rigid | Fit::Fixed),
                })
                .collect();
            let pause = Glue {
                natural: b.pause.after,
                stretch: b.pause.max - b.pause.after,
                shrink: b.pause.after - b.pause.min,
                kind: GlueKind::Pause,
            };
            BeatInput {
                id: id.clone(),
                hard,
                visuals,
                pause,
            }
        })
        .collect();
    SolveInput { beats }
}

fn t(x: Tick) -> Value {
    json!(x.flicks())
}

/// 播放速度（ppm）：素材时长 / 成片时长 × 1e6，向下取整。
fn speed_ppm(src_len: Tick, rec_len: Tick) -> i64 {
    if rec_len.flicks() <= 0 {
        return UNIT_SPEED;
    }
    let v = i128::from(src_len.flicks()) * i128::from(UNIT_SPEED) / i128::from(rec_len.flicks());
    i64::try_from(v).unwrap_or(UNIT_SPEED)
}

struct ClipSpec<'a> {
    id: String,
    origin: &'a BeatId,
    src: &'a str,
    src_in: Tick,
    src_len: Tick,
    rec_in: Tick,
    rec_len: Tick,
}

fn clip(c: &ClipSpec<'_>) -> Value {
    json!({
        "id": c.id,
        "origin": c.origin.as_str(),
        "src": c.src,
        "src_in": t(c.src_in),
        "src_out": t(c.src_in + c.src_len),
        "rec_in": t(c.rec_in),
        "rec_out": t(c.rec_in + c.rec_len),
        "speed_ppm": speed_ppm(c.src_len, c.rec_len),
    })
}

/// 一次素材使用：（素材文件, 入点, 出点, 片段 ID）
pub type SourceUse = (String, Tick, Tick, String);

/// 找出同一素材文件里被重复使用的区间。结果与输入顺序无关。
pub fn find_source_reuse(uses: &[SourceUse]) -> Vec<Value> {
    let mut sorted: Vec<&SourceUse> = uses.iter().collect();
    sorted.sort();
    let mut out = Vec::new();
    // 同一素材里，目前出点最靠后的那段
    let mut reach: Option<&SourceUse> = None;
    for cur in sorted {
        if let Some(r) = reach {
            if r.0 == cur.0 && cur.1 < r.2 {
                out.push(json!({
                    "code": "source_reuse",
                    "src": cur.0,
                    "clips": [r.3, cur.3],
                    "overlap": t(r.2.min(cur.2) - cur.1),
                }));
            }
        }
        let advance = match reach {
            Some(r) => r.0 != cur.0 || cur.2 > r.2,
            None => true,
        };
        if advance {
            reach = Some(cur);
        }
    }
    out
}

fn timeline(p: &Project, out: &SolveOutput, warnings: &mut Vec<Value>) -> Value {
    let mut tracks: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    let mut uses: Vec<SourceUse> = Vec::new();
    for pl in &out.beats {
        let b = &p.beats[&pl.id];
        if let Some(v) = &b.voice {
            let e = &p.tts[&v.narration];
            tracks.entry("A1").or_default().push(clip(&ClipSpec {
                id: format!("{}/voice", pl.id),
                origin: &pl.id,
                src: &e.audio,
                src_in: Tick::ZERO,
                src_len: e.dur,
                rec_in: pl.start,
                rec_len: e.dur,
            }));
        }
        for (i, (vis, vp)) in b.visual.iter().zip(&pl.visuals).enumerate() {
            let shot = &p.media.shots[&vis.shot];
            let id = format!("{}/visual[{i}]", pl.id);
            if vp.fill == FillKind::Short {
                warnings.push(json!({
                    "code": "visual_shortfall",
                    "beat": pl.id.as_str(),
                    "shot": vis.shot.as_str(),
                    "missing": t(vp.missing),
                }));
            }
            let track = match vis.role {
                Role::ARoll => "V1",
                Role::BRoll => "V2",
            };
            uses.push((
                shot.src.clone(),
                shot.src_in,
                shot.src_in + vp.src_len,
                id.clone(),
            ));
            tracks.entry(track).or_default().push(clip(&ClipSpec {
                id,
                origin: &pl.id,
                src: &shot.src,
                src_in: shot.src_in,
                src_len: vp.src_len,
                rec_in: pl.start + visual_offset(p, vis),
                rec_len: vp.rec_len,
            }));
        }
    }
    warnings.extend(find_source_reuse(&uses));
    json!({
        "format": 1,
        "unit": "flicks",
        "solver": out.version,
        "fps": {"num": p.config.fps.num(), "den": p.config.fps.den()},
        "duration": t(out.total),
        "tracks": tracks,
    })
}

/// 千分比，向零取整。
fn permille(part: Tick, whole: Tick) -> i64 {
    if whole.flicks() <= 0 {
        return 0;
    }
    let v = i128::from(part.flicks()) * 1000 / i128::from(whole.flicks());
    i64::try_from(v).unwrap_or(0)
}

fn percent_text(pm: i64) -> String {
    format!("{}.{}%", pm / 10, pm % 10)
}

/// 本节拍里每一项非零代价的原因。
fn reasons(p: &Project, pl: &BeatPlacement) -> Vec<String> {
    let b = &p.beats[&pl.id];
    let mut out = Vec::new();
    for (i, (vis, vp)) in b.visual.iter().zip(&pl.visuals).enumerate() {
        match vp.fill {
            FillKind::Retime => out.push(format!(
                "visual[{i}] {} 慢放 {}：素材 {} 撑满 {}",
                vis.shot,
                percent_text(permille(vp.retime_extra(), vp.src_len)),
                vp.src_len,
                vp.rec_len
            )),
            FillKind::Short => out.push(format!(
                "visual[{i}] {} 慢放到上限 {MAX_RETIME_PERCENT}% 仍缺 {}",
                vis.shot, vp.missing
            )),
            FillKind::Exact | FillKind::Trim => {}
        }
    }
    out
}

/// 代价里不为零的项（以名字为键）。
fn nonzero_terms(c: &Cost) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in c.terms() {
        if v != 0 {
            m.insert(k.to_string(), json!(v));
        }
    }
    Value::Object(m)
}

fn beat_report(p: &Project, pl: &BeatPlacement) -> Value {
    let visuals: Vec<Value> = pl
        .visuals
        .iter()
        .enumerate()
        .map(|(i, v)| {
            json!({
                "index": i,
                "fill": v.fill.as_str(),
                "src_len": t(v.src_len),
                "rec_len": t(v.rec_len),
                "missing": t(v.missing),
            })
        })
        .collect();
    json!({
        "start": t(pl.start),
        "dur": t(pl.dur),
        "hard": t(pl.hard),
        "pause": t(pl.pause),
        "visuals": visuals,
        "cost": nonzero_terms(&pl.cost),
        "reasons": reasons(p, pl),
    })
}

fn report(p: &Project, out: &SolveOutput, status: Status, warnings: Vec<Value>) -> Value {
    let (lo, hi) = p.intent.window();
    let by_id: BTreeMap<&BeatId, &BeatPlacement> = out.beats.iter().map(|b| (&b.id, b)).collect();

    let mut beats = serde_json::Map::new();
    for pl in &out.beats {
        beats.insert(pl.id.to_string(), beat_report(p, pl));
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

    // 还没有执行的约束要如实列出，避免“静默忽略”
    let mut not_applied = Vec::new();
    for id in p.ordered_beats() {
        if p.beats[id].music.is_some() {
            not_applied.push(format!("music:{id}"));
        }
    }
    for id in p.overrides.keys() {
        not_applied.push(id.to_string());
    }
    for id in &p.rules {
        not_applied.push(id.to_string());
    }
    if !p.intent.must.is_empty() {
        not_applied.push("intent.must".to_string());
    }
    not_applied.push("pause.flex".to_string());
    not_applied.push("visual.cut_points".to_string());

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

#[cfg(test)]
mod tests {
    use super::*;

    fn u(src: &str, a: i64, b: i64, id: &str) -> SourceUse {
        (
            src.to_string(),
            Tick::from_seconds(a),
            Tick::from_seconds(b),
            id.to_string(),
        )
    }

    #[test]
    fn reuse_detected_regardless_of_order() {
        let uses = vec![
            u("b.mp4", 12, 18, "x"),
            u("a.mp4", 0, 3, "p"),
            u("a.mp4", 2, 4, "q"),
            u("b.mp4", 3, 5, "y"),
        ];
        let found = find_source_reuse(&uses);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0]["clips"], json!(["p", "q"]));
        assert_eq!(found[0]["overlap"], t(Tick::from_seconds(1)));
        let mut reversed = uses.clone();
        reversed.reverse();
        assert_eq!(find_source_reuse(&reversed), found);
    }

    #[test]
    fn adjacent_is_not_reuse() {
        let uses = vec![u("a.mp4", 0, 3, "p"), u("a.mp4", 3, 5, "q")];
        assert!(find_source_reuse(&uses).is_empty());
    }

    #[test]
    fn speed_and_percent() {
        let a = Tick::from_seconds(5);
        let b = Tick::from_seconds(6);
        assert_eq!(speed_ppm(a, b), 833_333);
        assert_eq!(speed_ppm(a, a), UNIT_SPEED);
        assert_eq!(percent_text(permille(b - a, a)), "20.0%");
    }
}
