//! L2 → L1 编译。只读源文件（`SourceSet`），只产出 `build/` 下的文件。

use std::collections::{BTreeMap, BTreeSet};

use core_model::*;
use core_solver::{
    body_min, max_total, min_elements, solve, BeatInput, BeatPlacement, FillKind, MusicInput, Part,
    PauseInput, SolveInput, SolveOutput, VisualInput, VisualKind, ZoneInput, ZonePlacement,
    MAX_RETIME_PERCENT, UNIT_SPEED,
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
    /// 命令参数不对（例如段落不存在）
    #[error("{0}")]
    Usage(String),
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
    let asm = assemble(&p, &out, &mut warnings);
    let report = report(&p, &input, &out, status, warnings, &asm);
    let timeline = asm.timeline;
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
                frozen: p.frozen.get(sid).map(|f| f.dur),
            }
        })
        .collect();
    // 用到每段音乐的第一个、最后一个节拍（冻结段不算）
    // （第一个用到它的节拍, 最后一个用到它的节拍, 延后进入）
    type Cue = ((usize, usize), (usize, usize), Tick);
    let mut cues: BTreeMap<MusicId, Cue> = BTreeMap::new();
    for (zi, sid) in p.sequence.iter().enumerate() {
        if p.frozen.contains_key(sid) {
            continue;
        }
        for (bi, bid) in p.sections[sid].beats.iter().enumerate() {
            if let Some(mu) = &p.beats[bid].music {
                cues.entry(mu.cue.clone())
                    .and_modify(|e| e.1 = (zi, bi))
                    .or_insert(((zi, bi), (zi, bi), mu.start.unwrap_or(Tick::ZERO)));
            }
        }
    }
    let music = cues
        .into_iter()
        .map(|(id, (first, last, start))| {
            let m = &p.media.music[&id];
            MusicInput {
                id,
                first,
                last,
                start,
                beat: m.beat,
                meter: m.meter,
                bars: m.bars,
            }
        })
        .collect();
    SolveInput {
        zones,
        window: p.intent.window(),
        music,
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

fn mul_div(a: Tick, num: Tick, den: Tick) -> Tick {
    if den.flicks() <= 0 {
        return a;
    }
    let v = i128::from(a.flicks()) * i128::from(num.flicks()) / i128::from(den.flicks());
    Tick(i64::try_from(v).unwrap_or(i64::MAX))
}

fn dist(a: Tick, b: Tick) -> Tick {
    if a > b {
        a - b
    } else {
        b - a
    }
}

/// 时间线上的一个片段（补丁在它上面应用）。
#[derive(Clone, Debug)]
struct Clip {
    id: String,
    origin: BeatId,
    track: &'static str,
    src: String,
    shot: Option<ShotId>,
    /// 素材文件里的入点
    src_in: Tick,
    src_len: Tick,
    rec_in: Tick,
    rec_len: Tick,
    attrs: BTreeMap<String, String>,
    patches: Vec<String>,
    /// 画面的角色（规则按它匹配）；旁白和冻结段的片段为空
    role: Option<Role>,
    /// 来自冻结段：补丁和规则都不碰
    frozen: bool,
    /// 改动过这个片段的规则
    rules: Vec<String>,
}

impl Clip {
    fn rec_out(&self) -> Tick {
        self.rec_in + self.rec_len
    }

    /// 素材时间 → 时间线位置；不在已用范围内时返回 None。
    fn src_to_rec(&self, src_t: Tick) -> Option<Tick> {
        if src_t < self.src_in || src_t > self.src_in + self.src_len {
            return None;
        }
        Some(self.rec_in + mul_div(src_t - self.src_in, self.rec_len, self.src_len))
    }

    fn to_json(&self) -> Value {
        let mut v = json!({
            "id": self.id,
            "origin": self.origin.as_str(),
            "src": self.src,
            "src_in": t(self.src_in),
            "src_out": t(self.src_in + self.src_len),
            "rec_in": t(self.rec_in),
            "rec_out": t(self.rec_out()),
            "speed_ppm": core_solver::speed_ppm(self.src_len, self.rec_len),
        });
        if let Some(m) = v.as_object_mut() {
            if !self.attrs.is_empty() {
                m.insert("attrs".to_string(), json!(self.attrs));
            }
            if !self.patches.is_empty() {
                m.insert("patches".to_string(), json!(self.patches));
            }
            if !self.rules.is_empty() {
                m.insert("rules".to_string(), json!(self.rules));
            }
        }
        v
    }
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

/// 一段音乐在时间线上的位置：从第一个用到它的节拍开始，从素材的 first_bar 起播。
#[derive(Clone, Debug)]
struct MusicPlacement {
    src: String,
    src_in: Tick,
    start: Tick,
    /// 最后一个用到它的节拍的结束点
    span_end: Tick,
    /// 实际播放长度（`fit: bars` 时向下取整到整小节）
    len: Tick,
    beat: Tick,
    meter: u32,
    bars: u32,
    fit: Fit,
    beats: Vec<BeatId>,
    origin: BeatId,
}

impl MusicPlacement {
    fn bar_len(&self) -> Tick {
        Tick(self.beat.flicks() * i64::from(self.meter))
    }

    fn bar_pos(&self, b: &BarRef) -> Tick {
        let idx = i64::from(b.bar - 1) * i64::from(self.meter) + i64::from(b.beat - 1);
        self.start + Tick(self.beat.flicks() * idx)
    }
}

fn place_music(
    p: &Project,
    out: &SolveOutput,
    warnings: &mut Vec<Value>,
) -> BTreeMap<MusicId, MusicPlacement> {
    let mut placed: BTreeMap<MusicId, MusicPlacement> = BTreeMap::new();
    for pl in all_beats(out) {
        let Some(mu) = &p.beats[&pl.id].music else {
            continue;
        };
        let end = pl.start + pl.dur;
        if let Some(m) = placed.get_mut(&mu.cue) {
            m.span_end = end;
            m.beats.push(pl.id.clone());
            if mu.start.is_some() {
                warnings.push(json!({
                    "code": "music_start_ignored",
                    "beat": pl.id.as_str(),
                    "cue": mu.cue.as_str(),
                }));
            }
            continue;
        }
        let music = &p.media.music[&mu.cue];
        placed.insert(
            mu.cue.clone(),
            MusicPlacement {
                src: music.src.clone(),
                src_in: music.first_bar,
                start: pl.start + mu.start.unwrap_or(Tick::ZERO),
                span_end: end,
                len: Tick::ZERO,
                beat: music.beat,
                meter: music.meter,
                bars: music.bars,
                fit: mu.fit,
                beats: vec![pl.id.clone()],
                origin: pl.id.clone(),
            },
        );
    }
    for (cue, m) in &mut placed {
        let avail = if m.span_end > m.start {
            m.span_end - m.start
        } else {
            Tick::ZERO
        };
        let bar = m.bar_len();
        let mut len = if m.fit == Fit::Bars && bar > Tick::ZERO {
            Tick(avail.flicks() / bar.flicks() * bar.flicks())
        } else {
            avail
        };
        let max = Tick(bar.flicks() * i64::from(m.bars));
        if len > max {
            len = max;
        }
        m.len = len;
        if len == Tick::ZERO {
            warnings.push(json!({"code": "music_too_short", "cue": cue.as_str()}));
        }
    }
    placed
}

fn anchor_text(a: &Anchor) -> String {
    match a {
        Anchor::Word(w) => w.to_string(),
        Anchor::Event(e) => e.to_string(),
        Anchor::Bar(b) => format!("bar:{}.{}.b{}", b.music, b.bar, b.beat),
        Anchor::Mark(m) => m.to_string(),
        Anchor::Offset(s, off) => format!("{s}@{}", format_duration(*off)),
    }
}

/// 素材坐标 → 时间线位置。全部基于补丁前的几何，补丁之间互不影响。
struct Resolver<'a> {
    p: &'a Project,
    out: &'a SolveOutput,
    clips: &'a [Clip],
    music: &'a BTreeMap<MusicId, MusicPlacement>,
}

impl Resolver<'_> {
    /// 同一镜头被多个片段用到时，优先补丁的目标片段，否则取时间线上最早的那个。
    fn shot_pos(&self, shot: &ShotId, src_t: Tick, prefer: &str) -> Option<Tick> {
        let mut found: Vec<(&Clip, Tick)> = self
            .clips
            .iter()
            .filter(|c| c.shot.as_ref() == Some(shot))
            .filter_map(|c| c.src_to_rec(src_t).map(|x| (c, x)))
            .collect();
        if let Some(&(_, x)) = found.iter().find(|(c, _)| c.id == prefer) {
            return Some(x);
        }
        found.sort_by(|a, b| (a.0.rec_in, &a.0.id).cmp(&(b.0.rec_in, &b.0.id)));
        found.first().map(|&(_, x)| x)
    }

    fn resolve(&self, spec: &AnchorSpec, prefer: &str) -> Option<Tick> {
        let at = match &spec.at {
            Anchor::Word(w) => {
                let pl = all_beats(self.out).find(|pl| {
                    self.p.beats[&pl.id]
                        .voice
                        .as_ref()
                        .is_some_and(|v| v.narration == w.narration)
                })?;
                let vp = pl.voice?;
                let t0 = self.p.tts.get(&w.narration)?.word(w.index)?.t0;
                pl.start + mul_div(t0, vp.len, vp.natural)
            }
            Anchor::Event(e) => {
                let ev = self.p.media.events.get(e)?;
                self.shot_pos(&ev.shot, ev.at, prefer)?
            }
            Anchor::Offset(shot, off) => {
                let s = self.p.media.shots.get(shot)?;
                self.shot_pos(shot, s.src_in + *off, prefer)?
            }
            Anchor::Bar(b) => self.music.get(&b.music)?.bar_pos(b),
            Anchor::Mark(m) => {
                let mk = self.p.media.marks.get(m)?;
                self.shot_pos(&mk.shot, mk.at, prefer)?
            }
        };
        Some(at + spec.offset)
    }
}

fn clip_key(beat: &BeatId, slot: Slot) -> String {
    match slot {
        Slot::Voice => format!("{beat}/voice"),
        Slot::Visual(i) => format!("{beat}/visual[{i}]"),
    }
}

fn op_text(patch: &Patch) -> &'static str {
    match patch {
        Patch::CutIn => "cut_in",
        Patch::CutOut => "cut_out",
        Patch::Attrs(_) => "attrs",
    }
}

enum Applied {
    Ok,
    /// 剪切点不在片段内部：（片段入点, 片段出点）
    OutOfRange(Tick, Tick),
}

/// 应用一个补丁。裁头/裁尾时画面帧在时间线上的位置不变。
fn apply_patch(c: &mut Clip, patch: &Patch, at: Tick, id: &str) -> Applied {
    match patch {
        Patch::Attrs(a) => {
            for (k, v) in a {
                c.attrs.insert(k.clone(), v.clone());
            }
        }
        Patch::CutIn | Patch::CutOut => {
            if at <= c.rec_in || at >= c.rec_out() {
                return Applied::OutOfRange(c.rec_in, c.rec_out());
            }
            let keep_head = at - c.rec_in;
            if *patch == Patch::CutIn {
                let d_src = mul_div(keep_head, c.src_len, c.rec_len);
                c.src_in = c.src_in + d_src;
                c.src_len = c.src_len - d_src;
                c.rec_in = at;
                c.rec_len = c.rec_len - keep_head;
            } else {
                c.src_len = mul_div(keep_head, c.src_len, c.rec_len);
                c.rec_len = keep_head;
            }
        }
    }
    c.patches.push(id.to_string());
    Applied::Ok
}

/// 时间线、补丁、音乐的组装结果。
struct Assembled {
    timeline: Value,
    patches: Value,
    music: Value,
    rules: Value,
    slips: Value,
    conflicts: Vec<Value>,
}

fn static_track(track: &str) -> &'static str {
    match track {
        "A1" => "A1",
        "V1" => "V1",
        _ => "V2",
    }
}

fn role_text(r: Role) -> &'static str {
    match r {
        Role::ARoll => "a-roll",
        Role::BRoll => "b-roll",
    }
}

fn section_of<'a>(p: &'a Project, beat: &BeatId) -> Option<&'a SectionId> {
    p.sequence
        .iter()
        .find(|s| p.sections[*s].beats.contains(beat))
}

fn slot_role(p: &Project, beat: &BeatId, slot: Slot) -> Option<Role> {
    match slot {
        Slot::Visual(i) => p.beats.get(beat)?.visual.get(i).map(|v| v.role),
        Slot::Voice => None,
    }
}

fn rule_matches(scope: &RuleScope, beat: &BeatId, section: Option<&SectionId>, role: Role) -> bool {
    scope.beat.as_ref().is_none_or(|b| b == beat)
        && scope.section.as_ref().is_none_or(|s| Some(s) == section)
        && scope.role.is_none_or(|r| r == role)
}

/// 覆盖这个画面的规则里最具体的那些（具体优先于一般）。
fn covering_rules<'a>(p: &'a Project, beat: &BeatId, role: Role) -> Vec<(&'a RuleId, &'a Rule)> {
    let section = section_of(p, beat);
    let matching: Vec<(&RuleId, &Rule)> = p
        .rules
        .iter()
        .filter(|(_, r)| rule_matches(&r.scope, beat, section, role))
        .collect();
    let top = matching.iter().map(|(_, r)| r.scope.specificity()).max();
    matching
        .into_iter()
        .filter(|(_, r)| Some(r.scope.specificity()) == top)
        .collect()
}

/// 强拍：每小节第 1 拍；偶数拍号时还有中间那一拍（4/4 的第 3 拍）。只算音乐实际播放的范围。
fn strong_beats(m: &MusicPlacement) -> Vec<Tick> {
    let beat = m.beat.flicks();
    if beat <= 0 {
        return Vec::new();
    }
    let meter = i64::from(m.meter);
    let n = m.len.flicks() / beat;
    (0..=n)
        .filter(|&k| k % meter == 0 || (meter % 2 == 0 && k % meter == meter / 2))
        .map(|k| m.start + Tick(k * beat))
        .collect()
}

/// 最近的强拍：（位置, 拍长）。距离相同时取较早的。
fn nearest_strong_beat(x: Tick, music: &BTreeMap<MusicId, MusicPlacement>) -> Option<(Tick, Tick)> {
    music
        .values()
        .flat_map(|m| strong_beats(m).into_iter().map(move |sb| (sb, m.beat)))
        .min_by_key(|(sb, _)| (dist(*sb, x), *sb))
}

fn is_strong_beat(x: Tick, music: &BTreeMap<MusicId, MusicPlacement>, tol: Tick) -> bool {
    nearest_strong_beat(x, music).is_some_and(|(sb, _)| dist(sb, x) <= tol)
}

fn has_cut_patch(p: &Project, c: &Clip) -> bool {
    c.patches.iter().any(|id| {
        OverrideId::parse(id)
            .ok()
            .and_then(|o| p.overrides.get(&o))
            .is_some_and(|o| matches!(o.patch, Patch::CutIn | Patch::CutOut))
    })
}

/// 裁头到 `at`：画面帧在时间线上的位置不变。
fn cut_in(c: &mut Clip, at: Tick) {
    let keep_head = at - c.rec_in;
    let d_src = mul_div(keep_head, c.src_len, c.rec_len);
    c.src_in = c.src_in + d_src;
    c.src_len = c.src_len - d_src;
    c.rec_in = at;
    c.rec_len = c.rec_len - keep_head;
}

/// 执行规则。阶段 0 只有 `cut_on: beat.strong`：入点吸附到最近的强拍，
/// 距离不超过半拍，只向后裁头（不延长片头），且不能露出空白。
fn apply_rules(
    p: &Project,
    clips: &mut [Clip],
    music: &BTreeMap<MusicId, MusicPlacement>,
    tol: Tick,
    conflicts: &mut Vec<Value>,
) -> Value {
    let mut per_rule: BTreeMap<&RuleId, Vec<Value>> =
        p.rules.keys().map(|k| (k, Vec::new())).collect();
    let under: Vec<(Tick, Tick)> = clips
        .iter()
        .filter(|c| c.track == "V1")
        .map(|c| (c.rec_in, c.rec_out()))
        .collect();
    for c in clips.iter_mut() {
        let Some(role) = c.role else {
            continue;
        };
        if c.frozen {
            continue;
        }
        let rid = match covering_rules(p, &c.origin, role).as_slice() {
            [] => continue,
            [(rid, _)] => *rid,
            many => {
                let ids: Vec<&str> = many.iter().map(|(r, _)| r.as_str()).collect();
                conflicts.push(json!({
                    "code": "rule_conflict",
                    "blocking": false,
                    "decidable": "judgment",
                    "assignee": "human",
                    "clip": c.id,
                    "rules": ids,
                    "note": "范围同样具体的规则同时覆盖这个片段：都没有执行",
                }));
                continue;
            }
        };
        let from = c.rec_in;
        let status = if has_cut_patch(p, c) {
            "patched"
        } else {
            match nearest_strong_beat(from, music) {
                None => "no_music",
                Some((sb, beat)) => {
                    let d = dist(sb, from);
                    let covered =
                        c.track == "V2" && under.iter().any(|&(a, b)| a <= from && b >= sb);
                    if d <= tol {
                        "aligned"
                    } else if d > Tick(beat.flicks() / 2) {
                        "no_strong_beat_nearby"
                    } else if sb < from {
                        "needs_head_room"
                    } else if sb >= c.rec_out() {
                        "outside_clip"
                    } else if !covered {
                        "would_expose_gap"
                    } else {
                        cut_in(c, sb);
                        c.rules.push(rid.to_string());
                        "snapped"
                    }
                }
            }
        };
        let mut entry = json!({"clip": c.id, "status": status});
        if status == "snapped" {
            entry["from"] = t(from);
            entry["to"] = t(c.rec_in);
        }
        if let Some(v) = per_rule.get_mut(rid) {
            v.push(entry);
        }
    }
    let mut out = serde_json::Map::new();
    for (rid, r) in &p.rules {
        let scope = json!({
            "section": r.scope.section.as_ref().map(SectionId::as_str),
            "beat": r.scope.beat.as_ref().map(BeatId::as_str),
            "role": r.scope.role.map(role_text),
        });
        out.insert(
            rid.to_string(),
            json!({
                "scope": scope,
                "do": "cut_on: beat.strong",
                "by": r.by,
                "clips": per_rule.remove(rid).unwrap_or_default(),
            }),
        );
    }
    Value::Object(out)
}

fn assemble(p: &Project, out: &SolveOutput, warnings: &mut Vec<Value>) -> Assembled {
    // 1. 求解结果 → 片段
    let mut clips: Vec<Clip> = Vec::new();
    let mut slips = serde_json::Map::new();
    for pl in all_beats(out) {
        let b = &p.beats[&pl.id];
        if let (Some(v), Some(vp)) = (&b.voice, pl.voice) {
            clips.push(Clip {
                id: clip_key(&pl.id, Slot::Voice),
                origin: pl.id.clone(),
                track: "A1",
                src: p.tts[&v.narration].audio.clone(),
                shot: None,
                src_in: Tick::ZERO,
                src_len: vp.natural,
                rec_in: pl.start,
                rec_len: vp.len,
                attrs: BTreeMap::new(),
                patches: Vec::new(),
                role: None,
                frozen: false,
                rules: Vec::new(),
            });
        }
        for (i, (vis, vp)) in b.visual.iter().zip(&pl.visuals).enumerate() {
            let shot = &p.media.shots[&vis.shot];
            if vp.fill == FillKind::Short {
                warnings.push(json!({
                    "code": "visual_shortfall",
                    "beat": pl.id.as_str(),
                    "shot": vis.shot.as_str(),
                    "missing": t(vp.missing),
                }));
            }
            // D-8：截取画面时平移素材窗口，让出点落在最近的可剪点上（时间线上的时长和位置不变）
            let mut src_in = shot.src_in;
            if vp.fill == FillKind::Cut {
                let out0 = shot.src_in + vp.src_len;
                let cp = shot
                    .cut_points
                    .iter()
                    .copied()
                    .filter(|&c| c > out0 && c <= shot.src_out)
                    .min();
                if let Some(cp) = cp {
                    src_in = src_in + (cp - out0);
                    slips.insert(
                        clip_key(&pl.id, Slot::Visual(i)),
                        json!({
                            "shot": vis.shot.as_str(),
                            "cut_point": t(cp),
                            "shift": t(cp - out0),
                            "src_in": [t(shot.src_in), t(src_in)],
                        }),
                    );
                }
            }
            clips.push(Clip {
                id: clip_key(&pl.id, Slot::Visual(i)),
                origin: pl.id.clone(),
                track: match vis.role {
                    Role::ARoll => "V1",
                    Role::BRoll => "V2",
                },
                src: shot.src.clone(),
                shot: Some(vis.shot.clone()),
                src_in,
                src_len: vp.src_len,
                rec_in: pl.start + vp.offset,
                rec_len: vp.rec_len,
                attrs: BTreeMap::new(),
                patches: Vec::new(),
                role: Some(vis.role),
                frozen: false,
                rules: Vec::new(),
            });
        }
    }
    // 冻结段：原样放回，只按段落起点整体平移
    for z in out.zones.iter().filter(|z| z.frozen) {
        let Some(f) = p.frozen.get(&z.id) else {
            continue;
        };
        for fc in &f.clips {
            clips.push(Clip {
                id: fc.id.clone(),
                origin: fc.origin.clone(),
                track: static_track(&fc.track),
                src: fc.src.clone(),
                shot: None,
                src_in: fc.src_in,
                src_len: fc.src_out - fc.src_in,
                rec_in: z.start + fc.rec_in,
                rec_len: fc.rec_out - fc.rec_in,
                attrs: fc.attrs.clone(),
                patches: fc.patches.clone(),
                role: None,
                frozen: true,
                rules: fc.rules.clone(),
            });
        }
    }

    // 2. 音乐
    let music = place_music(p, out, warnings);

    // 3. 解析锚点（补丁前的几何）
    type Resolved = (OverrideId, Option<Tick>, Vec<Option<Tick>>);
    let resolved: Vec<Resolved> = {
        let r = Resolver {
            p,
            out,
            clips: &clips,
            music: &music,
        };
        p.overrides
            .iter()
            .map(|(id, ov)| {
                let key = clip_key(&ov.beat, ov.slot);
                let alts = ov.alt.iter().map(|a| r.resolve(a, &key)).collect();
                (id.clone(), r.resolve(&ov.anchor, &key), alts)
            })
            .collect()
    };

    // 4. 按补丁 ID 顺序应用
    let half_frame = Tick(p.config.fps.frame().flicks() / 2);
    let mut patches = serde_json::Map::new();
    let mut conflicts = Vec::new();
    for (id, at, alts) in resolved {
        let ov = &p.overrides[&id];
        let key = clip_key(&ov.beat, ov.slot);
        let anchor = anchor_text(&ov.anchor.at);
        let alt_json: Vec<Value> = ov
            .alt
            .iter()
            .zip(&alts)
            .map(|(spec, a)| {
                let delta = match (at, a) {
                    (Some(x), Some(y)) => t(*y - x),
                    _ => Value::Null,
                };
                json!({
                    "anchor": anchor_text(&spec.at),
                    "offset": t(spec.offset),
                    "at": a.map_or(Value::Null, t),
                    "delta": delta,
                })
            })
            .collect();
        let diverged: Vec<usize> = match at {
            Some(x) => alts
                .iter()
                .enumerate()
                .filter(|(_, a)| a.is_some_and(|y| dist(y, x) >= half_frame))
                .map(|(i, _)| i)
                .collect(),
            None => Vec::new(),
        };
        let coincide = !alts.is_empty() && alts.iter().all(Option::is_some) && diverged.is_empty();

        // 备选锚点分开时：唯一覆盖这个片段的规则可以在补丁已记录的几种解释里选一个
        let mut chosen = at;
        let mut resolved_by: Option<RuleId> = None;
        if !diverged.is_empty() {
            if let Some(role) = slot_role(p, &ov.beat, ov.slot) {
                if let [(rid, _)] = covering_rules(p, &ov.beat, role).as_slice() {
                    let cands = std::iter::once(at).chain(alts.iter().copied()).flatten();
                    for x in cands {
                        if is_strong_beat(x, &music, half_frame) {
                            chosen = Some(x);
                            resolved_by = Some((*rid).clone());
                            break;
                        }
                    }
                }
            }
        }

        let status = match chosen {
            None => {
                conflicts.push(json!({
                    "code": "unanchored",
                    "blocking": false,
                    "decidable": "judgment",
                    "assignee": "human",
                    "override": id.as_str(),
                    "anchor": anchor,
                    "note": "锚点找不到了：补丁没有应用，也没有丢弃",
                }));
                "unanchored"
            }
            Some(x) => match clips.iter_mut().find(|c| c.id == key) {
                None => "unanchored",
                Some(c) => {
                    if c.frozen {
                        "frozen"
                    } else {
                        match apply_patch(c, &ov.patch, x, id.as_str()) {
                            Applied::Ok => "applied",
                            Applied::OutOfRange(a, b) => {
                                conflicts.push(json!({
                                    "code": "patch_out_of_range",
                                    "blocking": false,
                                    "decidable": "judgment",
                                    "assignee": "human",
                                    "override": id.as_str(),
                                    "at": t(x),
                                    "clip": {"rec_in": t(a), "rec_out": t(b)},
                                    "note": "剪切点不在片段内部：补丁没有应用",
                                }));
                                "out_of_range"
                            }
                        }
                    }
                }
            },
        };

        if !diverged.is_empty() && resolved_by.is_none() {
            let mut options = vec![json!({"id": "A", "op": "follow", "anchor": anchor})];
            for (n, &i) in diverged.iter().enumerate() {
                options.push(json!({
                    "id": option_id(n + 1),
                    "op": "follow",
                    "anchor": anchor_text(&ov.alt[i].at),
                }));
            }
            conflicts.push(json!({
                "code": "anchor_diverged",
                "blocking": false,
                "decidable": "judgment",
                "assignee": "human",
                "override": id.as_str(),
                "kept": "A",
                "alt": alt_json,
                "options": options,
                "note": "两种解释已经分开：先按主锚点保留，等人决定",
            }));
        }

        patches.insert(
            id.to_string(),
            json!({
                "target": ov.target,
                "op": op_text(&ov.patch),
                "by": ov.by,
                "anchor": anchor,
                "offset": t(ov.anchor.offset),
                "at": at.map_or(Value::Null, t),
                "applied_at": chosen.map_or(Value::Null, t),
                "resolved_by": resolved_by.as_ref().map_or(Value::Null, |r| json!(r.as_str())),
                "status": status,
                "alt": alt_json,
                "alt_coincide": coincide,
            }),
        );
    }

    // 4b. 规则：补丁之后执行；不碰挂了剪切补丁的片段和冻结段
    let rules = apply_rules(p, &mut clips, &music, half_frame, &mut conflicts);

    // 5. 全局检查：素材重复使用（补丁之后）
    let uses: Vec<SourceUse> = clips
        .iter()
        .filter(|c| c.track.starts_with('V'))
        .map(|c| (c.src.clone(), c.src_in, c.src_in + c.src_len, c.id.clone()))
        .collect();
    warnings.extend(find_source_reuse(&uses));

    // 6. 输出
    let mut ordered: Vec<&Clip> = clips.iter().collect();
    ordered.sort_by(|a, b| (a.track, a.rec_in, &a.id).cmp(&(b.track, b.rec_in, &b.id)));
    let mut tracks: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    for c in ordered {
        tracks.entry(c.track).or_default().push(c.to_json());
    }
    let mut music_report = serde_json::Map::new();
    for (cue, m) in &music {
        if m.len > Tick::ZERO {
            tracks.entry("A2").or_default().push(json!({
                "id": format!("music/{cue}"),
                "origin": m.origin.as_str(),
                "src": m.src,
                "src_in": t(m.src_in),
                "src_out": t(m.src_in + m.len),
                "rec_in": t(m.start),
                "rec_out": t(m.start + m.len),
                "speed_ppm": UNIT_SPEED,
            }));
        }
        let bar = m.bar_len();
        let bars_used = if bar > Tick::ZERO {
            m.len.flicks() / bar.flicks()
        } else {
            0
        };
        let beats: Vec<&str> = m.beats.iter().map(BeatId::as_str).collect();
        music_report.insert(
            cue.to_string(),
            json!({
                "start": t(m.start),
                "end": t(m.start + m.len),
                "span_end": t(m.span_end),
                "bars_used": bars_used,
                "tail_gap": t(m.span_end - (m.start + m.len)),
                "beats": beats,
            }),
        );
    }
    let timeline = json!({
        "format": 1,
        "unit": "flicks",
        "solver": out.version,
        "fps": {"num": p.config.fps.num(), "den": p.config.fps.den()},
        "duration": t(out.total),
        "tracks": tracks,
    });
    Assembled {
        timeline,
        patches: Value::Object(patches),
        music: Value::Object(music_report),
        rules,
        slips: Value::Object(slips),
        conflicts,
    }
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
    // 停顿的变化分两部分报告：求解（压缩/拉长）和吸附，不重复
    let solved = pl.pause - pl.snap;
    if solved < pl.pause_natural {
        out.push(format!(
            "停顿压缩 {}：{} → {}",
            human(pl.pause_natural - solved),
            human(pl.pause_natural),
            human(solved)
        ));
    }
    if solved > pl.pause_natural {
        out.push(format!(
            "停顿拉长 {}：{} → {}",
            human(solved - pl.pause_natural),
            human(pl.pause_natural),
            human(solved)
        ));
    }
    if pl.snap != Tick::ZERO {
        let sign = if pl.snap > Tick::ZERO { "+" } else { "-" };
        let abs = Tick(pl.snap.flicks().abs());
        out.push(format!("切点吸附到拍子：停顿 {sign}{}", human(abs)));
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
        "snap": t(pl.snap),
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
        "frozen": z.frozen,
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

fn part_target(p: &Project, e: &core_solver::MinElement) -> String {
    let Some(beat) = &e.beat else {
        return format!("frozen:{}", e.zone);
    };
    match e.part {
        Part::Frozen => format!("frozen:{}", e.zone),
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
            "blocking": true,
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
    elems.sort_by_key(|e| {
        (
            std::cmp::Reverse(e.min),
            e.zone.clone(),
            e.beat.clone(),
            e.part,
        )
    });
    let mut set = vec![json!({"target": "intent.duration.max", "value": t(max)})];
    let mut acc = Tick::ZERO;
    for e in &elems {
        acc = acc + e.min;
        set.push(json!({"target": part_target(p, e), "min": t(e.min)}));
        if acc > max {
            break;
        }
    }

    let must: BTreeSet<NarrationId> = p.intent.must.iter().map(|r| r.narration.clone()).collect();
    let mut opts = Vec::new();
    for z in &input.zones {
        // 冻结段归 L1：不能删它里面的句子，只能请人解冻
        if let Some(d) = z.frozen {
            let unfrozen = z
                .beats
                .iter()
                .fold(Tick::ZERO, |acc, b| acc + body_min(b) + b.pause.min);
            if d > unfrozen && d - unfrozen >= over {
                opts.push(Opt::new(
                    "unfreeze",
                    z.id.to_string(),
                    "human_lock",
                    d - unfrozen,
                ));
            }
            continue;
        }
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
        "blocking": true,
        "term": "intent",
        "over": t(over),
        "min_conflict_set": set,
        "options": options,
        "recommend": recommend,
    }))
}

/// 全片短于 L4 下限：拉伸用尽后仍不够。
fn under_conflict(input: &SolveInput, out: &SolveOutput) -> Option<Value> {
    if out.intent_under <= Tick::ZERO {
        return None;
    }
    let under = out.intent_under;
    let (options, recommend) = finish_options(vec![
        Opt::new("add", "content".to_string(), "meaning", under),
        Opt::new("relax", "intent.duration".to_string(), "intent", under),
    ]);
    Some(json!({
        "code": "intent_under",
        "blocking": true,
        "term": "intent",
        "under": t(under),
        "max_total": t(max_total(input)),
        "note": "语速不放慢；停顿和画面已拉到上限",
        "options": options,
        "recommend": recommend,
    }))
}

/// 区结果的指纹：段落报告（去掉绝对起点）+ 节拍报告。
/// 区的最终片段（补丁和规则之后），时间相对段落起点。音乐跨段落，不算在内。
fn zone_clips(p: &Project, z: &ZonePlacement, timeline: &Value) -> Vec<Value> {
    let Some(sec) = p.sections.get(&z.id) else {
        return Vec::new();
    };
    let beats: BTreeSet<&str> = sec.beats.iter().map(BeatId::as_str).collect();
    let mut out = Vec::new();
    if let Some(tracks) = timeline["tracks"].as_object() {
        for (track, list) in tracks {
            if track == "A2" {
                continue;
            }
            for c in list.as_array().into_iter().flatten() {
                if !c["origin"].as_str().is_some_and(|o| beats.contains(o)) {
                    continue;
                }
                let mut c = c.clone();
                for k in ["rec_in", "rec_out"] {
                    let v = c[k].as_i64().unwrap_or(0) - z.start.flicks();
                    c[k] = json!(v);
                }
                c["track"] = json!(track);
                out.push(c);
            }
        }
    }
    out
}

/// 区结果的指纹：段落报告（去掉绝对起点）+ 节拍报告 + 最终片段。
fn zone_hash(sec: &Value, beats: Vec<Value>, clips: Vec<Value>) -> String {
    let mut s = sec.clone();
    if let Some(m) = s.as_object_mut() {
        m.remove("start");
    }
    let v = json!({"section": s, "beats": beats, "clips": clips});
    let bytes = canon::to_bytes(&v).unwrap_or_default();
    format!("{:016x}", canon::fnv1a64(&bytes))
}

fn parse_build(out: &BuildOutput, name: &str) -> Result<Value, CompileError> {
    let bytes = out
        .files
        .get(name)
        .ok_or_else(|| CompileError::Internal(format!("编译没有产出 {name}")))?;
    serde_json::from_slice(bytes).map_err(|e| CompileError::Internal(e.to_string()))
}

/// `clip freeze`：把段落当前的片段存成 `frozen/<段落>.json`（时间相对段落起点）。
pub fn freeze(src: &SourceSet, section: &str) -> Result<(String, Vec<u8>), CompileError> {
    let p = load(src)?;
    let sid = SectionId::parse(section).map_err(|e| CompileError::Usage(e.to_string()))?;
    let sec = p
        .sections
        .get(&sid)
        .ok_or_else(|| CompileError::Usage(format!("段落 {sid} 不存在")))?;
    let out = compile(src)?;
    let report = parse_build(&out, REPORT)?;
    let timeline = parse_build(&out, TIMELINE)?;
    let info = &report["sections"][sid.as_str()];
    let (Some(start), Some(dur)) = (info["start"].as_i64(), info["dur"].as_i64()) else {
        return Err(CompileError::Internal(format!(
            "报告里没有 {sid} 的起点和时长"
        )));
    };
    let beats: BTreeSet<&str> = sec.beats.iter().map(BeatId::as_str).collect();
    let mut clips = Vec::new();
    if let Some(tracks) = timeline["tracks"].as_object() {
        for (track, list) in tracks {
            // 音乐跨段落，不随段落冻结
            if !["A1", "V1", "V2"].contains(&track.as_str()) {
                continue;
            }
            for c in list.as_array().into_iter().flatten() {
                if !c["origin"].as_str().is_some_and(|o| beats.contains(o)) {
                    continue;
                }
                let mut c = c.clone();
                for k in ["rec_in", "rec_out"] {
                    let v = c[k].as_i64().unwrap_or(0) - start;
                    c[k] = json!(v);
                }
                c["track"] = json!(track);
                clips.push(c);
            }
        }
    }
    let v = json!({
        "format": 1,
        "section": sid.as_str(),
        "solver": report["solver"],
        "dur": dur,
        "clips": clips,
    });
    let bytes = canon::to_bytes(&v).map_err(|e| CompileError::Internal(e.to_string()))?;
    Ok((format!("frozen/{sid}.json"), bytes))
}

/// `clip lock accept`：用当前结果生成求解器版本锁。
pub fn lock_file(src: &SourceSet) -> Result<Vec<u8>, CompileError> {
    let out = compile(src)?;
    let report = parse_build(&out, REPORT)?;
    let mut zones = serde_json::Map::new();
    if let Some(secs) = report["sections"].as_object() {
        for (k, v) in secs {
            zones.insert(k.clone(), v["hash"].clone());
        }
    }
    let v = json!({"format": 1, "solver": core_solver::SOLVER_VERSION, "zones": zones});
    canon::to_bytes(&v).map_err(|e| CompileError::Internal(e.to_string()))
}

fn report(
    p: &Project,
    input: &SolveInput,
    out: &SolveOutput,
    status: Status,
    warnings: Vec<Value>,
    asm: &Assembled,
) -> Value {
    let (lo, hi) = p.intent.window();

    let mut sections = serde_json::Map::new();
    let mut beats = serde_json::Map::new();
    let mut resynth = Vec::new();
    let mut hashes: BTreeMap<SectionId, String> = BTreeMap::new();
    for z in &out.zones {
        let mut sec = section_report(z);
        let mut zone_beats = Vec::new();
        for pl in &z.beats {
            let br = beat_report(p, z, pl);
            zone_beats.push(br.clone());
            beats.insert(pl.id.to_string(), br);
            if let (Some(v), Some(vp)) = (&p.beats[&pl.id].voice, pl.voice) {
                if vp.len != vp.natural {
                    resynth.push(
                        json!({"narration": v.narration.as_str(), "speed_ppm": vp.speed_ppm()}),
                    );
                }
            }
        }
        let hash = zone_hash(&sec, zone_beats, zone_clips(p, z, &asm.timeline));
        if let Some(m) = sec.as_object_mut() {
            m.insert("hash".to_string(), json!(hash));
        }
        hashes.insert(z.id.clone(), hash);
        sections.insert(z.id.to_string(), sec);
    }

    let cost: Vec<Value> = out
        .cost
        .terms()
        .iter()
        .map(|(k, v)| json!([k, v]))
        .collect();

    let mut conflicts = hard_conflicts(out);
    conflicts.extend(intent_conflict(p, input, out));
    conflicts.extend(under_conflict(input, out));
    conflicts.extend(asm.conflicts.iter().cloned());

    // 求解器版本锁：版本变了才上报；锁文件只由 clip lock accept 更新
    let solver_lock = match &p.solver_lock {
        None => Value::Null,
        Some(l) => {
            let mut changed: BTreeSet<String> = BTreeSet::new();
            for (sid, h) in &hashes {
                if l.zones.get(sid) != Some(h) {
                    changed.insert(sid.to_string());
                }
            }
            for sid in l.zones.keys() {
                if !hashes.contains_key(sid) {
                    changed.insert(sid.to_string());
                }
            }
            let version_matches = l.solver == core_solver::SOLVER_VERSION;
            if !version_matches {
                conflicts.push(json!({
                    "code": "solver_changed",
                    "blocking": false,
                    "decidable": "judgment",
                    "assignee": "human",
                    "from": l.solver,
                    "to": core_solver::SOLVER_VERSION,
                    "zones_changed": changed,
                    "note": "求解器版本变了：列出的区结果和确认时不同。确认后运行 clip lock accept；锁文件不会自动更新",
                }));
            }
            json!({"solver": l.solver, "version_matches": version_matches, "zones_changed": changed})
        }
    };

    // 还没有执行的约束要如实列出，避免“静默忽略”
    let not_applied: Vec<String> = Vec::new();
    let snaps: Vec<Value> = out
        .snaps
        .iter()
        .map(|s| {
            json!({
                "kind": s.kind.as_str(),
                "music": s.music.as_str(),
                "beat": s.beat.as_str(),
                "next": s.next.as_ref().map(SectionId::as_str),
                "from": t(s.from),
                "to": t(s.to),
                "delta": t(s.to - s.from),
                "status": s.status.as_str(),
            })
        })
        .collect();

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
            "under": t(out.intent_under),
        },
        "sections": sections,
        "beats": beats,
        "cost": cost,
        "conflicts": conflicts,
        "patches": asm.patches,
        "music": asm.music,
        "rules": asm.rules,
        "slips": asm.slips,
        "snaps": snaps,
        "solver_lock": solver_lock,
        "tts_resynth_needed": resynth,
        "warnings": warnings,
        "not_applied": not_applied,
    })
}

fn rule_status_text(c: &Value, tk: &dyn Fn(&Value) -> String) -> String {
    match c["status"].as_str().unwrap_or("") {
        "aligned" => "已经在强拍上".to_string(),
        "snapped" => format!("入点吸附到强拍：{} → {}", tk(&c["from"]), tk(&c["to"])),
        "patched" => "片段有人工剪切补丁，跳过".to_string(),
        "would_expose_gap" => "吸附会露出空白，跳过".to_string(),
        "needs_head_room" => "最近的强拍在入点之前：不延长片头，跳过".to_string(),
        "outside_clip" => "最近的强拍在片段之外，跳过".to_string(),
        "no_strong_beat_nearby" => "半拍以内没有强拍，跳过".to_string(),
        "no_music" => "没有音乐，跳过".to_string(),
        other => other.to_string(),
    }
}

fn patch_status_text(v: &Value, tk: &dyn Fn(&Value) -> String) -> String {
    let base = match v["status"].as_str().unwrap_or("") {
        "applied" => format!("已应用于 {}", tk(&v["applied_at"])),
        "unanchored" => "锚点找不到，没有应用".to_string(),
        "out_of_range" => "剪切点不在片段内，没有应用".to_string(),
        "frozen" => "目标在冻结段里，没有应用".to_string(),
        other => other.to_string(),
    };
    match v["resolved_by"].as_str() {
        Some(r) => format!("{base}（两种解释已分开，由规则 {r} 选定）"),
        None => base,
    }
}

/// `clip explain`：把求解报告整理成人能读的说明。`only` 只展开指定段落。
pub fn explain(r: &Value, only: Option<&str>) -> String {
    let tk = |v: &Value| {
        v.as_i64()
            .map_or_else(|| "-".to_string(), |x| human(Tick(x)))
    };
    let pos = |v: &Value| v.as_i64().is_some_and(|x| x > 0);
    let list = |v: &Value| v.as_array().cloned().unwrap_or_default();
    let mut s = String::new();
    let it = &r["intent"];
    s.push_str(&format!(
        "状态：{}　总长 {}（目标 {}，允许 {}–{}）\n",
        r["status"].as_str().unwrap_or("?"),
        tk(&r["total"]),
        tk(&it["target"]),
        tk(&it["min"]),
        tk(&it["max"])
    ));
    if pos(&it["over"]) {
        s.push_str(&format!("  超出 L4 上限 {}\n", tk(&it["over"])));
    }
    if pos(&it["under"]) {
        s.push_str(&format!("  短于 L4 下限 {}\n", tk(&it["under"])));
    }

    let mut secs: Vec<(i64, &String, &Value)> = r["sections"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| (v["start"].as_i64().unwrap_or(0), k, v))
                .collect()
        })
        .unwrap_or_default();
    secs.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
    for (start, sid, sec) in &secs {
        if only.is_some_and(|o| o != sid.as_str()) {
            continue;
        }
        let end = start + sec["dur"].as_i64().unwrap_or(0);
        s.push_str(&format!(
            "\n{sid}　{}–{}，时长 {}（自然 {}）",
            human(Tick(*start)),
            human(Tick(end)),
            tk(&sec["dur"]),
            tk(&sec["natural"])
        ));
        if sec["budget_kind"].as_str() == Some("declared") {
            s.push_str(&format!("，预算 {}", tk(&sec["budget"])));
            if pos(&sec["over"]) {
                s.push_str(&format!("，超出预算 {}", tk(&sec["over"])));
            }
        } else {
            s.push_str("，预算由求解得出");
        }
        let frozen = sec["frozen"] == json!(true);
        if frozen {
            s.push_str("，已冻结");
        }
        if sec["l4_absorbed"] == json!(true) {
            s.push_str("，参与了全片调整");
        }
        s.push('\n');
        for b in list(&sec["borrowed"]) {
            s.push_str(&format!(
                "  向 {} 借了 {}\n",
                b["from"].as_str().unwrap_or("?"),
                tk(&b["amount"])
            ));
        }
        for b in list(&sec["lent"]) {
            s.push_str(&format!(
                "  借给 {} {}\n",
                b["to"].as_str().unwrap_or("?"),
                tk(&b["amount"])
            ));
        }
        if frozen {
            s.push_str("  冻结段原样输出，不参与求解\n");
            continue;
        }
        for id in list(&sec["beats"]) {
            let Some(id) = id.as_str() else {
                continue;
            };
            let b = &r["beats"][id];
            if b.is_null() {
                continue;
            }
            s.push_str(&format!(
                "  {id}　时长 {}（主体 {} + 停顿 {}）\n",
                tk(&b["dur"]),
                tk(&b["body"]),
                tk(&b["pause"])
            ));
            let reasons = list(&b["reasons"]);
            if reasons.is_empty() {
                s.push_str("    · 无调整\n");
            }
            for x in reasons {
                s.push_str(&format!("    · {}\n", x.as_str().unwrap_or("")));
            }
        }
    }

    let terms: Vec<String> = list(&r["cost"])
        .iter()
        .filter(|x| x[1].as_u64().is_some_and(|v| v > 0))
        .map(|x| {
            let name = x[0].as_str().unwrap_or("?");
            let v = x[1].as_i64().unwrap_or(0);
            let shown = match name {
                "intent" | "declared_budget" | "trim" | "pause" => human(Tick(v)),
                "rate" => format!("+{}（各句合计）", percent_text(ppm_to_permille(v))),
                _ => v.to_string(),
            };
            format!("{name} {shown}")
        })
        .collect();
    if terms.is_empty() {
        s.push_str("\n代价：全部为 0\n");
    } else {
        s.push_str(&format!("\n代价：{}\n", terms.join("，")));
    }

    let conflicts = list(&r["conflicts"]);
    if !conflicts.is_empty() {
        s.push_str("\n需要处理：\n");
    }
    for c in &conflicts {
        let blocking = if c["blocking"] == json!(true) {
            "必须处理"
        } else {
            "可以稍后"
        };
        s.push_str(&format!(
            "  [{}]（{blocking}）",
            c["code"].as_str().unwrap_or("?")
        ));
        for (k, label) in [("over", "超出"), ("under", "差"), ("missing", "缺")] {
            if pos(&c[k]) {
                s.push_str(&format!(" {label} {}", tk(&c[k])));
            }
        }
        if let Some(n) = c["note"].as_str() {
            s.push_str(&format!(" {n}"));
        }
        s.push('\n');
        for o in list(&c["options"]) {
            let id = o["id"].as_str().unwrap_or("?");
            let target = o["target"]
                .as_str()
                .or_else(|| o["anchor"].as_str())
                .unwrap_or("");
            let mut line = format!("    {id}. {} {target}", o["op"].as_str().unwrap_or("?"));
            if o["amount"].is_i64() {
                line.push_str(&format!("（{}）", tk(&o["amount"])));
            }
            if let Some(role) = o["protected_role"].as_str() {
                line.push_str(&format!("［{role} 段］"));
            }
            if c["recommend"].as_str() == Some(id) {
                line.push_str(" ← 推荐");
            }
            s.push_str(&line);
            s.push('\n');
        }
    }

    if let Some(rules) = r["rules"].as_object() {
        if !rules.is_empty() {
            s.push_str("\n规则：\n");
        }
        for (rid, v) in rules {
            for c in list(&v["clips"]) {
                s.push_str(&format!(
                    "  {rid} · {}：{}\n",
                    c["clip"].as_str().unwrap_or("?"),
                    rule_status_text(&c, &tk)
                ));
            }
        }
    }
    let snaps = list(&r["snaps"]);
    if !snaps.is_empty() {
        s.push_str("\n吸附：\n");
    }
    for x in &snaps {
        let what = if x["kind"].as_str() == Some("music_end") {
            format!("{} 片尾 → 小节线", x["music"].as_str().unwrap_or("?"))
        } else {
            format!("切点 {} → 强拍", x["next"].as_str().unwrap_or("?"))
        };
        let st = match x["status"].as_str().unwrap_or("") {
            "snapped" => format!(
                "{} → {}（调 {} 的停顿）",
                tk(&x["from"]),
                tk(&x["to"]),
                x["beat"].as_str().unwrap_or("?")
            ),
            "aligned" => "已经对齐".to_string(),
            "too_far" => format!("差 {}，超过 0.3s，不吸附", tk(&x["delta"])),
            "pause_limit" => "超出停顿的 flex 区间，不吸附".to_string(),
            "visual_limit" => "拉长后画面撑不满，不吸附".to_string(),
            "budget" => "会超出声明预算，不吸附".to_string(),
            "intent" => "会超出 L4 区间，不吸附".to_string(),
            "frozen" => "切点前是冻结段，不吸附".to_string(),
            "conflict" => "全片有必须处理的冲突，先不吸附".to_string(),
            other => other.to_string(),
        };
        s.push_str(&format!("  {what}：{st}\n"));
    }
    if let Some(slips) = r["slips"].as_object() {
        for (clip, v) in slips {
            s.push_str(&format!(
                "  {clip} 素材窗口后移 {}，出点对齐可剪点 {}\n",
                tk(&v["shift"]),
                tk(&v["cut_point"])
            ));
        }
    }
    if let Some(patches) = r["patches"].as_object() {
        if !patches.is_empty() {
            s.push_str("\n补丁：\n");
        }
        for (id, v) in patches {
            s.push_str(&format!(
                "  {id} · {}（{}）：{}\n",
                v["target"].as_str().unwrap_or("?"),
                v["op"].as_str().unwrap_or("?"),
                patch_status_text(v, &tk)
            ));
        }
    }
    let resynth = list(&r["tts_resynth_needed"]);
    if !resynth.is_empty() {
        let items: Vec<String> = resynth
            .iter()
            .map(|x| {
                let ppm = x["speed_ppm"].as_i64().unwrap_or(UNIT_SPEED);
                format!(
                    "{}（+{}）",
                    x["narration"].as_str().unwrap_or("?"),
                    percent_text(ppm_to_permille(ppm - UNIT_SPEED))
                )
            })
            .collect();
        s.push_str(&format!("\n需要重新合成 TTS：{}\n", items.join("、")));
    }
    let warnings: Vec<String> = list(&r["warnings"])
        .iter()
        .filter_map(|w| w["code"].as_str().map(str::to_string))
        .collect();
    if !warnings.is_empty() {
        s.push_str(&format!("\n警告：{}\n", warnings.join("、")));
    }
    let todo: Vec<String> = list(&r["not_applied"])
        .iter()
        .filter_map(|w| w.as_str().map(str::to_string))
        .collect();
    if !todo.is_empty() {
        s.push_str(&format!("\n尚未实现：{}\n", todo.join("、")));
    }
    s
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
