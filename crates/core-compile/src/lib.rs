//! L2 → L1 编译。只读源文件（`SourceSet`），只产出 `build/` 下的文件。

use std::collections::{BTreeMap, BTreeSet};

use core_model::*;
use core_solver::{
    body_min, min_elements, solve, BeatInput, BeatPlacement, FillKind, Part, PauseInput,
    SolveInput, SolveOutput, VisualInput, VisualKind, ZoneInput, ZonePlacement, MAX_RETIME_PERCENT,
    UNIT_SPEED,
};
use core_store::{canon, load, LoadErrors, SourceSet};
use serde_json::{json, Value};

/// 这些段落里的句子仍可删除，但推荐顺序排在“放宽 L4”之后。
pub const PROTECTED_ROLES: &[&str] = &["opening", "closing"];

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
    let input = solve_input(&p);
    let out = solve(&input, None);
    let status = if out.cost.needs_escalation() {
        Status::Conflict
    } else {
        Status::Ok
    };
    let mut warnings = Vec::new();
    let timeline = timeline(&p, &out, &mut warnings);
    let report = report(&p, &input, &out, status, warnings);
    let mut files = BTreeMap::new();
    for (name, v) in [(TIMELINE, timeline), (REPORT, report)] {
        let bytes = canon::to_bytes(&v).map_err(|e| CompileError::Internal(e.to_string()))?;
        files.insert(name.to_string(), bytes);
    }
    Ok(BuildOutput { files, status })
}

/// 画面相对节拍开始的入画时间（旁白原速）。
fn visual_offset(p: &Project, vis: &Visual) -> Tick {
    match &vis.from {
        Some(w) => p.tts[&w.narration]
            .word(w.index)
            .map_or(Tick::ZERO, |x| x.t0),
        None => Tick::ZERO,
    }
}

fn beat_input(p: &Project, id: &BeatId) -> BeatInput {
    let b = &p.beats[id];
    let visuals = b
        .visual
        .iter()
        .map(|vis| {
            let have = p.media.shots[&vis.shot].duration();
            let kind = match vis.fit {
                Fit::Rigid => VisualKind::Rigid,
                Fit::Fixed => VisualKind::Fixed(vis.len.unwrap_or(have)),
                Fit::Fill | Fit::Bars => VisualKind::Elastic,
            };
            VisualInput {
                offset: visual_offset(p, vis),
                have,
                kind,
            }
        })
        .collect();
    BeatInput {
        id: id.clone(),
        voice: b.voice.as_ref().map(|v| p.tts[&v.narration].dur),
        visuals,
        pause: PauseInput {
            natural: b.pause.after,
            min: b.pause.min,
            max: b.pause.max,
        },
    }
}

fn solve_input(p: &Project) -> SolveInput {
    let zones = p
        .sequence
        .iter()
        .map(|sid| {
            let sec = &p.sections[sid];
            ZoneInput {
                id: sid.clone(),
                budget: sec.budget,
                beats: sec.beats.iter().map(|id| beat_input(p, id)).collect(),
            }
        })
        .collect();
    SolveInput {
        zones,
        window: p.intent.window(),
    }
}

fn t(x: Tick) -> Value {
    json!(x.flicks())
}

/// 给人读的时长：四舍五入到毫秒（报告里的说明文字用；数值字段仍是精确的 flicks）。
fn human(x: Tick) -> String {
    let f = i128::from(FLICKS_PER_SECOND);
    let v = i128::from(x.flicks());
    let ms = (v.abs() * 1000 + f / 2) / f;
    let sign = if v < 0 { "-" } else { "" };
    let (s, rest) = (ms / 1000, ms % 1000);
    if rest == 0 {
        return format!("{sign}{s}s");
    }
    let frac = format!("{rest:03}");
    format!("{sign}{s}.{}s", frac.trim_end_matches('0'))
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
        "speed_ppm": core_solver::speed_ppm(c.src_len, c.rec_len),
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

fn all_beats(out: &SolveOutput) -> impl Iterator<Item = &BeatPlacement> {
    out.zones.iter().flat_map(|z| &z.beats)
}

fn timeline(p: &Project, out: &SolveOutput, warnings: &mut Vec<Value>) -> Value {
    let mut tracks: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    let mut uses: Vec<SourceUse> = Vec::new();
    for pl in all_beats(out) {
        let b = &p.beats[&pl.id];
        if let (Some(v), Some(vp)) = (&b.voice, pl.voice) {
            let e = &p.tts[&v.narration];
            tracks.entry("A1").or_default().push(clip(&ClipSpec {
                id: format!("{}/voice", pl.id),
                origin: &pl.id,
                src: &e.audio,
                src_in: Tick::ZERO,
                src_len: vp.natural,
                rec_in: pl.start,
                rec_len: vp.len,
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
                rec_in: pl.start + vp.offset,
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

/// 速度增量（ppm）→ 千分比，四舍五入。
fn ppm_to_permille(delta_ppm: i64) -> i64 {
    (delta_ppm + 500).div_euclid(1000)
}

/// 本节拍里每一项非零代价的原因。
fn reasons(p: &Project, pl: &BeatPlacement) -> Vec<String> {
    let b = &p.beats[&pl.id];
    let mut out = Vec::new();
    if pl.pause < pl.pause_natural {
        out.push(format!(
            "停顿压缩 {}：{} → {}",
            human(pl.pause_natural - pl.pause),
            human(pl.pause_natural),
            human(pl.pause)
        ));
    }
    if pl.elastic_shrink > Tick::ZERO {
        out.push(format!("画面主体缩短 {}", human(pl.elastic_shrink)));
    }
    if let Some(v) = pl.voice {
        if v.len < v.natural {
            out.push(format!(
                "语速 +{}：旁白 {} → {}（需要重新合成 TTS）",
                percent_text(ppm_to_permille(v.speed_ppm() - UNIT_SPEED)),
                human(v.natural),
                human(v.len)
            ));
        }
    }
    for (i, (vis, vp)) in b.visual.iter().zip(&pl.visuals).enumerate() {
        match vp.fill {
            FillKind::Retime => out.push(format!(
                "visual[{i}] {} 慢放 {}：素材 {} 撑满 {}",
                vis.shot,
                percent_text(permille(vp.retime_extra(), vp.src_len)),
                human(vp.src_len),
                human(vp.rec_len)
            )),
            FillKind::Short => out.push(format!(
                "visual[{i}] {} 慢放到上限 {MAX_RETIME_PERCENT}% 仍缺 {}",
                vis.shot,
                human(vp.missing)
            )),
            FillKind::Fixed if vp.missing > Tick::ZERO => out.push(format!(
                "visual[{i}] {} 固定长度超出素材 {}",
                vis.shot,
                human(vp.missing)
            )),
            FillKind::Exact | FillKind::Cut | FillKind::Fixed => {}
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

fn beat_report(p: &Project, zone: &ZonePlacement, pl: &BeatPlacement) -> Value {
    let visuals: Vec<Value> = pl
        .visuals
        .iter()
        .enumerate()
        .map(|(i, v)| {
            json!({
                "index": i,
                "fill": v.fill.as_str(),
                "offset": t(v.offset),
                "src_len": t(v.src_len),
                "rec_len": t(v.rec_len),
                "missing": t(v.missing),
            })
        })
        .collect();
    let voice = pl.voice.map_or(
        Value::Null,
        |v| json!({"natural": t(v.natural), "len": t(v.len), "speed_ppm": v.speed_ppm()}),
    );
    json!({
        // 相对所在段落的起点：段落整体平移时，节拍报告不变
        "offset": t(pl.start - zone.start),
        "dur": t(pl.dur),
        "body": t(pl.body),
        "pause": t(pl.pause),
        "voice": voice,
        "visuals": visuals,
        "cost": nonzero_terms(&pl.cost),
        "reasons": reasons(p, pl),
    })
}

fn section_report(z: &ZonePlacement) -> Value {
    let (kind, budget, over) = match z.budget {
        Some(b) => ("declared", t(b), t(z.over)),
        None => ("derived", Value::Null, Value::Null),
    };
    let borrowed: Vec<Value> = z
        .borrowed
        .iter()
        .map(|(s, a)| json!({"from": s.as_str(), "amount": t(*a)}))
        .collect();
    let lent: Vec<Value> = z
        .lent
        .iter()
        .map(|(s, a)| json!({"to": s.as_str(), "amount": t(*a)}))
        .collect();
    let beats: Vec<&str> = z.beats.iter().map(|b| b.id.as_str()).collect();
    json!({
        "start": t(z.start),
        "dur": t(z.dur),
        "natural": t(z.natural),
        "budget": budget,
        "budget_kind": kind,
        "over": over,
        "borrowed": borrowed,
        "lent": lent,
        "l4_absorbed": z.l4,
        "beats": beats,
    })
}

/// 冲突选项。代价从低到高：改意思（4）< 改意图（3）< 动人工锁定（2）。
struct Opt {
    op: &'static str,
    target: String,
    term: &'static str,
    /// remove / unlock：能省下的时长；relax：需要放宽的时长；set：设成的值
    amount: Tick,
    /// 句子所在段落是开场或结尾：代价仍是第 4 项，但推荐顺序排在放宽 L4 之后
    protected_role: Option<String>,
}

impl Opt {
    fn new(op: &'static str, target: String, term: &'static str, amount: Tick) -> Opt {
        Opt {
            op,
            target,
            term,
            amount,
            protected_role: None,
        }
    }
}

/// 推荐顺序：删句 < 放宽 L4 < 删开场/结尾的句子 < 动人工锁定。
fn option_rank(o: &Opt) -> u8 {
    match (o.term, &o.protected_role) {
        ("meaning", None) => 0,
        ("intent", _) => 1,
        ("meaning", Some(_)) => 2,
        _ => 3,
    }
}

fn option_id(i: usize) -> String {
    u8::try_from(i)
        .ok()
        .and_then(|n| b'A'.checked_add(n))
        .map_or('?', char::from)
        .to_string()
}

fn finish_options(mut opts: Vec<Opt>) -> (Vec<Value>, Value) {
    opts.sort_by_key(|o| (option_rank(o), o.amount, o.target.clone()));
    let values: Vec<Value> = opts
        .iter()
        .enumerate()
        .map(|(i, o)| {
            let mut v = json!({"id": option_id(i), "op": o.op, "target": o.target, "term": o.term, "amount": t(o.amount)});
            if let (Some(role), Some(m)) = (&o.protected_role, v.as_object_mut()) {
                m.insert("protected_role".to_string(), json!(role));
            }
            v
        })
        .collect();
    let recommend = values.first().map_or(Value::Null, |v| v["id"].clone());
    (values, recommend)
}

fn part_target(p: &Project, beat: &BeatId, part: Part) -> String {
    match part {
        Part::Voice => p.beats[beat]
            .voice
            .as_ref()
            .map_or_else(|| format!("{beat}.voice"), |v| v.narration.to_string()),
        Part::Visual(i) => format!("{beat}.visual[{i}]"),
        Part::Elastic => format!("{beat}.visual"),
        Part::Pause => format!("{beat}.pause"),
    }
}

fn hard_conflicts(out: &SolveOutput) -> Vec<Value> {
    out.hard
        .iter()
        .map(|h| {
            let target = format!("{}.visual[{}]", h.beat, h.visual);
            let (options, recommend) = finish_options(vec![
                Opt::new("set", format!("{target}.len"), "human_lock", h.have),
                Opt::new("unlock", target.clone(), "human_lock", h.len - h.have),
            ]);
            json!({
                "code": "fixed_exceeds_source",
                "term": "hard",
                "target": target,
                "len": t(h.len),
                "have": t(h.have),
                "missing": t(h.len - h.have),
                "options": options,
                "recommend": recommend,
            })
        })
        .collect()
}

/// 全片超出 L4 上限：给出最小冲突集和可选方案。含 must 词的句子永远不会作为删除选项。
fn intent_conflict(p: &Project, input: &SolveInput, out: &SolveOutput) -> Option<Value> {
    if out.intent_over <= Tick::ZERO {
        return None;
    }
    let over = out.intent_over;
    let (_, max) = p.intent.window();

    // 最小冲突集：按最短长度从大到小累加，刚好超过上限为止（人数最少的冲突集）
    let mut elems = min_elements(input);
    elems.sort_by_key(|e| (std::cmp::Reverse(e.min), e.beat.clone(), e.part));
    let mut set = vec![json!({"target": "intent.duration.max", "value": t(max)})];
    let mut acc = Tick::ZERO;
    for e in &elems {
        acc = acc + e.min;
        set.push(json!({"target": part_target(p, &e.beat, e.part), "min": t(e.min)}));
        if acc > max {
            break;
        }
    }

    let must: BTreeSet<NarrationId> = p.intent.must.iter().map(|r| r.narration.clone()).collect();
    let mut opts = Vec::new();
    for z in &input.zones {
        let role = &p.sections[&z.id].role;
        let protected = PROTECTED_ROLES.contains(&role.as_str());
        for b in &z.beats {
            if let Some(v) = &p.beats[&b.id].voice {
                if !must.contains(&v.narration) {
                    let without = BeatInput {
                        voice: None,
                        ..b.clone()
                    };
                    let saves = body_min(b) - body_min(&without);
                    if saves >= over {
                        let mut o = Opt::new("remove", v.narration.to_string(), "meaning", saves);
                        if protected {
                            o.protected_role = Some(role.clone());
                        }
                        opts.push(o);
                    }
                }
            }
            for (i, vi) in b.visuals.iter().enumerate() {
                if matches!(vi.kind, VisualKind::Fixed(_)) {
                    let mut unlocked = b.clone();
                    unlocked.visuals[i].kind = VisualKind::Elastic;
                    let saves = body_min(b) - body_min(&unlocked);
                    if saves >= over {
                        opts.push(Opt::new(
                            "unlock",
                            format!("{}.visual[{i}]", b.id),
                            "human_lock",
                            saves,
                        ));
                    }
                }
            }
        }
    }
    opts.push(Opt::new(
        "relax",
        "intent.duration".to_string(),
        "intent",
        over,
    ));
    let (options, recommend) = finish_options(opts);
    Some(json!({
        "code": "intent_over",
        "term": "intent",
        "over": t(over),
        "min_conflict_set": set,
        "options": options,
        "recommend": recommend,
    }))
}

fn report(
    p: &Project,
    input: &SolveInput,
    out: &SolveOutput,
    status: Status,
    warnings: Vec<Value>,
) -> Value {
    let (lo, hi) = p.intent.window();

    let mut sections = serde_json::Map::new();
    let mut beats = serde_json::Map::new();
    let mut resynth = Vec::new();
    for z in &out.zones {
        sections.insert(z.id.to_string(), section_report(z));
        for pl in &z.beats {
            beats.insert(pl.id.to_string(), beat_report(p, z, pl));
            if let (Some(v), Some(vp)) = (&p.beats[&pl.id].voice, pl.voice) {
                if vp.len != vp.natural {
                    resynth.push(
                        json!({"narration": v.narration.as_str(), "speed_ppm": vp.speed_ppm()}),
                    );
                }
            }
        }
    }

    let cost: Vec<Value> = out
        .cost
        .terms()
        .iter()
        .map(|(k, v)| json!([k, v]))
        .collect();

    let mut conflicts = hard_conflicts(out);
    conflicts.extend(intent_conflict(p, input, out));

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
    not_applied.push("pause.stretch".to_string());
    not_applied.push("visual.cut_points".to_string());
    not_applied.push("intent.stretch".to_string());

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
            "over": t(out.intent_over),
        },
        "sections": sections,
        "beats": beats,
        "cost": cost,
        "conflicts": conflicts,
        "tts_resynth_needed": resynth,
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
    fn percent_and_human() {
        let a = Tick::from_seconds(5);
        let b = Tick::from_seconds(6);
        assert_eq!(percent_text(permille(b - a, a)), "20.0%");
        assert_eq!(human(Tick(2_352_000_000)), "3.333s");
        assert_eq!(human(Tick::from_seconds(2)), "2s");
        assert_eq!(human(Tick(282_240_000)), "0.4s");
    }

    #[test]
    fn option_order_and_ids() {
        let mut opening = Opt::new("remove", "n:S1.1".into(), "meaning", Tick(1));
        opening.protected_role = Some("opening".into());
        let (opts, rec) = finish_options(vec![
            Opt::new("relax", "intent.duration".into(), "intent", Tick(5)),
            Opt::new("unlock", "S1.b1.visual[0]".into(), "human_lock", Tick(1)),
            opening,
            Opt::new("remove", "n:S2.2".into(), "meaning", Tick(9)),
        ]);
        let targets: Vec<&str> = opts.iter().map(|o| o["target"].as_str().unwrap()).collect();
        assert_eq!(
            targets,
            vec!["n:S2.2", "intent.duration", "n:S1.1", "S1.b1.visual[0]"]
        );
        assert_eq!(opts[2]["protected_role"], json!("opening"));
        assert!(opts[0].get("protected_role").is_none());
        assert_eq!(opts[0]["id"], json!("A"));
        assert_eq!(rec, json!("A"));
    }

    #[test]
    fn rate_percent_rounds() {
        assert_eq!(percent_text(ppm_to_permille(79_999)), "8.0%");
        assert_eq!(percent_text(ppm_to_permille(80_000)), "8.0%");
        assert_eq!(percent_text(ppm_to_permille(12_449)), "1.2%");
        assert_eq!(percent_text(ppm_to_permille(12_500)), "1.3%");
    }
}
