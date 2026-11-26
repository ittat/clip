//! 源文件 → 校验后的 Project。
//!
//! 错误全部收集后一起返回，每条带位置和修复建议。集合一律是以 ID 为键的映射，
//! 列表只用于有顺序语义的地方（sequence、beats、visual、alt、words）。

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use core_model::*;
use serde::de::DeserializeOwned;

use crate::source::{is_frozen_file, SourceSet, SOLVER_LOCK};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    pub file: String,
    pub at: String,
    pub message: String,
    pub fix: Option<String>,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}: {}", self.file, self.at, self.message)?;
        if let Some(fix) = &self.fix {
            write!(f, "（建议：{fix}）")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LoadErrors(pub Vec<LoadError>);

impl fmt::Display for LoadErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for e in &self.0 {
            writeln!(f, "{e}")?;
        }
        Ok(())
    }
}

impl std::error::Error for LoadErrors {}

#[derive(Default)]
struct Ctx {
    errors: Vec<LoadError>,
}

impl Ctx {
    fn err(&mut self, file: &str, at: &str, msg: impl fmt::Display, fix: Option<&str>) {
        self.errors.push(LoadError {
            file: file.to_string(),
            at: at.to_string(),
            message: msg.to_string(),
            fix: fix.map(str::to_string),
        });
    }

    fn id<T>(&mut self, file: &str, at: &str, r: Result<T, IdError>) -> Option<T> {
        match r {
            Ok(v) => Some(v),
            Err(e) => {
                self.err(file, at, e, None);
                None
            }
        }
    }

    fn dur(&mut self, file: &str, at: &str, s: &str, fps: &Fps) -> Option<Tick> {
        match parse_duration(s, Some(fps)) {
            Ok(t) => Some(t),
            Err(e) => {
                self.err(file, at, e, Some("例如 `1.5s` 或 `+2f`"));
                None
            }
        }
    }
}

/// 源文件的原始结构，只负责反序列化。
#[allow(dead_code)] // 部分字段在后续 PR 才使用
mod raw {
    use std::collections::BTreeMap;

    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(untagged)]
    pub enum NumOrStr {
        Num(u64),
        Str(String),
    }

    impl NumOrStr {
        pub fn text(&self) -> String {
            match self {
                NumOrStr::Num(n) => n.to_string(),
                NumOrStr::Str(s) => s.clone(),
            }
        }
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Project {
        pub format: u32,
        pub fps: NumOrStr,
        pub sample_rate: u32,
        pub solver: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Intent {
        pub goal: String,
        pub duration: Duration,
        #[serde(default)]
        pub pace: Option<String>,
        #[serde(default)]
        pub must: Vec<String>,
        #[serde(default)]
        pub forbid: Vec<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Duration {
        pub target: String,
        pub tol: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Structure {
        pub sequence: Vec<String>,
        pub sections: BTreeMap<String, Section>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Section {
        pub role: String,
        #[serde(default)]
        pub budget: Option<String>,
        pub beats: Vec<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Content {
        pub beats: BTreeMap<String, Beat>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Beat {
        #[serde(default)]
        pub voice: Option<Voice>,
        #[serde(default)]
        pub visual: Vec<Visual>,
        #[serde(default)]
        pub music: Option<MusicUse>,
        pub pause: Pause,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Voice {
        pub narration: String,
        pub fit: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Visual {
        pub shot: String,
        pub role: String,
        #[serde(default)]
        pub fit: Option<String>,
        #[serde(default)]
        pub from: Option<String>,
        #[serde(default)]
        pub len: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct MusicUse {
        pub cue: String,
        pub fit: String,
        #[serde(default)]
        pub start: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Pause {
        pub after: String,
        pub flex: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Narration {
        pub text: String,
        pub voice: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Media {
        #[serde(default)]
        pub shots: BTreeMap<String, Shot>,
        #[serde(default)]
        pub events: BTreeMap<String, Event>,
        #[serde(default)]
        pub music: BTreeMap<String, Music>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Shot {
        pub src: String,
        #[serde(rename = "in")]
        pub src_in: String,
        #[serde(rename = "out")]
        pub src_out: String,
        #[serde(default)]
        pub desc: BTreeMap<String, String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Event {
        pub shot: String,
        pub at: String,
        pub kind: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Music {
        pub src: String,
        pub bpm: u32,
        pub meter: u32,
        pub first_bar: String,
        pub bars: u32,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct TtsLock {
        pub format: u32,
        pub entries: BTreeMap<String, Tts>,
    }

    /// 锁文件是机器写的规范化 JSON，时间直接是 flicks 整数。
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Tts {
        pub audio: String,
        pub dur: i64,
        pub hash: String,
        pub words: Vec<Word>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Word {
        pub i: u32,
        pub t0: i64,
        pub t1: i64,
        pub txt: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Override {
        pub anchor: Anchor,
        #[serde(default)]
        pub alt: Vec<Anchor>,
        pub target: String,
        pub patch: serde_yaml_ng::Value,
        pub by: String,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Anchor {
        pub at: String,
        #[serde(default)]
        pub offset: Option<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Rule {
        pub scope: serde_yaml_ng::Value,
        #[serde(rename = "do")]
        pub action: serde_yaml_ng::Value,
        pub by: String,
        #[serde(default)]
        pub examples: Vec<String>,
    }

    /// `frozen/<段落>.json`：由 `clip freeze` 生成的规范化 JSON，时间是 flicks 整数。
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct Frozen {
        pub format: u32,
        pub section: String,
        pub solver: String,
        pub dur: i64,
        pub clips: Vec<FrozenClip>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct FrozenClip {
        pub id: String,
        pub origin: String,
        pub track: String,
        pub src: String,
        pub src_in: i64,
        pub src_out: i64,
        pub rec_in: i64,
        pub rec_out: i64,
        #[serde(default)]
        pub speed_ppm: Option<i64>,
        #[serde(default)]
        pub attrs: BTreeMap<String, String>,
        #[serde(default)]
        pub patches: Vec<String>,
        #[serde(default)]
        pub rules: Vec<String>,
    }

    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct SolverLock {
        pub format: u32,
        pub solver: String,
        pub zones: BTreeMap<String, String>,
    }
}

fn parse_yaml<T: DeserializeOwned>(
    src: &SourceSet,
    cx: &mut Ctx,
    file: &str,
    required: bool,
) -> Option<T> {
    match src.get(file) {
        None => {
            if required {
                cx.err(
                    file,
                    "文件",
                    "缺少必需的源文件",
                    Some("参考 golden/fixtures/min_explainer"),
                );
            }
            None
        }
        Some(text) => match serde_yaml_ng::from_str::<T>(text) {
            Ok(v) => Some(v),
            Err(e) => {
                let at = e
                    .location()
                    .map(|l| format!("第 {} 行第 {} 列", l.line(), l.column()))
                    .unwrap_or_else(|| "文件".to_string());
                cx.err(file, &at, e, None);
                None
            }
        },
    }
}

fn parse_json<T: DeserializeOwned>(
    src: &SourceSet,
    cx: &mut Ctx,
    file: &str,
    required: bool,
) -> Option<T> {
    match src.get(file) {
        None => {
            if required {
                cx.err(
                    file,
                    "文件",
                    "缺少必需的源文件",
                    Some("阶段 0 使用 TTS 替身：手工写一份锁文件"),
                );
            }
            None
        }
        Some(text) => match serde_json::from_str::<T>(text) {
            Ok(v) => Some(v),
            Err(e) => {
                cx.err(
                    file,
                    &format!("第 {} 行第 {} 列", e.line(), e.column()),
                    e,
                    None,
                );
                None
            }
        },
    }
}

pub fn load(src: &SourceSet) -> Result<Project, LoadErrors> {
    let mut cx = Ctx::default();
    let project: Option<raw::Project> = parse_yaml(src, &mut cx, "project.yaml", true);
    let intent: Option<raw::Intent> = parse_yaml(src, &mut cx, "intent.yaml", true);
    let structure: Option<raw::Structure> = parse_yaml(src, &mut cx, "structure.yaml", true);
    let content: Option<raw::Content> = parse_yaml(src, &mut cx, "content.yaml", true);
    let narration: Option<BTreeMap<String, raw::Narration>> =
        parse_yaml(src, &mut cx, "narration.yaml", true);
    let media: Option<raw::Media> = parse_yaml(src, &mut cx, "media/index.yaml", true);
    let tts: Option<raw::TtsLock> = parse_json(src, &mut cx, "locks/tts.lock.json", true);
    let overrides: Option<BTreeMap<String, raw::Override>> =
        parse_yaml(src, &mut cx, "overrides.yaml", false);
    let rules: Option<BTreeMap<String, raw::Rule>> = parse_yaml(src, &mut cx, "rules.yaml", false);
    let solver_lock: Option<raw::SolverLock> = parse_json(src, &mut cx, SOLVER_LOCK, false);
    // 阶段 0 只检查语法
    let _: Option<serde_yaml_ng::Value> = parse_yaml(src, &mut cx, "voices.yaml", false);
    let _: Option<serde_yaml_ng::Value> = parse_yaml(src, &mut cx, "queue.yaml", false);

    let config = project.and_then(|p| build_config(&mut cx, p));
    let (
        Some(config),
        Some(intent),
        Some(structure),
        Some(content),
        Some(narration),
        Some(media),
        Some(tts),
    ) = (config, intent, structure, content, narration, media, tts)
    else {
        return Err(LoadErrors(cx.errors));
    };
    let fps = config.fps;

    let narration = build_narration(&mut cx, narration);
    let tts = build_tts(&mut cx, tts, &narration);
    let media = build_media(&mut cx, media, &fps);
    let intent = build_intent(&mut cx, intent, &fps, &tts);
    let (sequence, sections) = build_structure(&mut cx, structure, &fps);
    let beats = build_beats(&mut cx, content, &fps, &narration, &tts, &media);
    check_membership(&mut cx, &sections, &beats);
    let overrides = overrides
        .map(|o| build_overrides(&mut cx, o, &fps, &tts, &media, &beats))
        .unwrap_or_default();
    let rules = rules
        .map(|r| build_rules(&mut cx, r, &sections, &beats, &overrides))
        .unwrap_or_default();
    let frozen = build_frozen(&mut cx, src, &sections);
    let solver_lock = solver_lock.and_then(|l| build_solver_lock(&mut cx, l));

    if !cx.errors.is_empty() {
        return Err(LoadErrors(cx.errors));
    }
    Ok(Project {
        config,
        intent,
        sequence,
        sections,
        beats,
        narration,
        media,
        tts,
        overrides,
        rules,
        frozen,
        solver_lock,
    })
}

fn build_config(cx: &mut Ctx, p: raw::Project) -> Option<Config> {
    const F: &str = "project.yaml";
    if p.format != 1 {
        cx.err(
            F,
            "format",
            format!("不支持的格式版本 {}", p.format),
            Some("当前只支持 format: 1"),
        );
    }
    let fps = match Fps::parse(&p.fps.text()) {
        Ok(f) => Some(f),
        Err(e) => {
            cx.err(F, "fps", e, Some("写成 25 或 \"30000/1001\""));
            None
        }
    };
    let sample_rate = match SampleRate::new(p.sample_rate) {
        Ok(s) => Some(s),
        Err(e) => {
            cx.err(F, "sample_rate", e, Some("常用值：48000、44100"));
            None
        }
    };
    Some(Config {
        fps: fps?,
        sample_rate: sample_rate?,
        solver: p.solver,
    })
}

fn build_narration(
    cx: &mut Ctx,
    raw: BTreeMap<String, raw::Narration>,
) -> BTreeMap<NarrationId, Narration> {
    let mut out = BTreeMap::new();
    for (k, v) in raw {
        if let Some(id) = cx.id("narration.yaml", &k, NarrationId::parse(&k)) {
            out.insert(
                id,
                Narration {
                    text: v.text,
                    voice: v.voice,
                },
            );
        }
    }
    out
}

fn build_tts(
    cx: &mut Ctx,
    raw: raw::TtsLock,
    narration: &BTreeMap<NarrationId, Narration>,
) -> BTreeMap<NarrationId, TtsEntry> {
    const F: &str = "locks/tts.lock.json";
    if raw.format != 1 {
        cx.err(
            F,
            "format",
            format!("不支持的格式版本 {}", raw.format),
            None,
        );
    }
    let mut out = BTreeMap::new();
    for (k, e) in raw.entries {
        let Some(id) = cx.id(F, &k, NarrationId::parse(&k)) else {
            continue;
        };
        if !narration.contains_key(&id) {
            cx.err(
                F,
                &k,
                "锁文件中的句子在 narration.yaml 里不存在",
                Some("删除这条锁记录，或补回旁白"),
            );
        }
        let mut ok = e.dur > 0;
        if !ok {
            cx.err(F, &format!("{k}.dur"), "句长必须为正", None);
        }
        let mut words = Vec::new();
        let mut prev_end = 0;
        for (pos, w) in e.words.iter().enumerate() {
            let at = format!("{k}.words[{pos}]");
            let expected = u32::try_from(pos + 1).unwrap_or(u32::MAX);
            if w.i != expected {
                cx.err(
                    F,
                    &at,
                    format!("词序号应为 {expected}，实际为 {}", w.i),
                    None,
                );
                ok = false;
            }
            if w.t0 < prev_end || w.t1 < w.t0 || w.t1 > e.dur {
                cx.err(F, &at, "词时间不单调，或超出句长", None);
                ok = false;
            }
            prev_end = w.t1;
            words.push(WordTiming {
                index: w.i,
                t0: Tick(w.t0),
                t1: Tick(w.t1),
                text: w.txt.clone(),
            });
        }
        if ok {
            out.insert(
                id,
                TtsEntry {
                    audio: e.audio,
                    dur: Tick(e.dur),
                    words,
                },
            );
        }
    }
    for id in narration.keys() {
        if !out.contains_key(id) {
            cx.err(
                F,
                id.as_str(),
                "旁白没有可用的 TTS 锁记录",
                Some("阶段 0 使用 TTS 替身：手工补一条锁记录"),
            );
        }
    }
    out
}

fn build_media(cx: &mut Ctx, raw: raw::Media, fps: &Fps) -> Media {
    const F: &str = "media/index.yaml";
    let mut m = Media::default();
    for (k, s) in raw.shots {
        let id = cx.id(F, &k, ShotId::parse(&k));
        let a = cx.dur(F, &format!("{k}.in"), &s.src_in, fps);
        let b = cx.dur(F, &format!("{k}.out"), &s.src_out, fps);
        if let (Some(id), Some(a), Some(b)) = (id, a, b) {
            if b <= a {
                cx.err(F, &k, "out 必须大于 in", None);
                continue;
            }
            m.shots.insert(
                id,
                Shot {
                    src: s.src,
                    src_in: a,
                    src_out: b,
                },
            );
        }
    }
    for (k, e) in raw.events {
        let id = cx.id(F, &k, EventId::parse(&k));
        let shot = cx.id(F, &format!("{k}.shot"), ShotId::parse(&e.shot));
        let at = cx.dur(F, &format!("{k}.at"), &e.at, fps);
        let (Some(id), Some(shot), Some(at)) = (id, shot, at) else {
            continue;
        };
        if id.shot() != shot {
            cx.err(
                F,
                &k,
                format!("事件 ID 属于 {}，但 shot 写的是 {shot}", id.shot()),
                Some("事件 ID 的前半部分必须是所属镜头"),
            );
            continue;
        }
        match m.shots.get(&shot) {
            None => cx.err(F, &format!("{k}.shot"), format!("镜头 {shot} 不存在"), None),
            Some(s) if at < s.src_in || at >= s.src_out => {
                cx.err(
                    F,
                    &format!("{k}.at"),
                    "事件时间不在镜头的 in/out 范围内",
                    None,
                );
            }
            Some(_) => {
                m.events.insert(
                    id,
                    Event {
                        shot,
                        at,
                        kind: e.kind,
                    },
                );
            }
        }
    }
    for (k, mu) in raw.music {
        let id = cx.id(F, &k, MusicId::parse(&k));
        let first = cx.dur(F, &format!("{k}.first_bar"), &mu.first_bar, fps);
        if mu.bpm == 0 || mu.meter == 0 || mu.bars == 0 {
            cx.err(F, &k, "bpm、meter、bars 必须为正", None);
            continue;
        }
        let minute = 60 * FLICKS_PER_SECOND;
        if minute % i64::from(mu.bpm) != 0 {
            cx.err(
                F,
                &format!("{k}.bpm"),
                "这个 bpm 的拍长不是整数个 flick",
                Some("阶段 0 只支持能整除的整数 bpm"),
            );
            continue;
        }
        if let (Some(id), Some(first)) = (id, first) {
            let beat = Tick(minute / i64::from(mu.bpm));
            m.music.insert(
                id,
                Music {
                    src: mu.src,
                    bpm: mu.bpm,
                    meter: mu.meter,
                    first_bar: first,
                    bars: mu.bars,
                    beat,
                },
            );
        }
    }
    m
}

fn check_word(
    cx: &mut Ctx,
    file: &str,
    at: &str,
    w: &WordRef,
    tts: &BTreeMap<NarrationId, TtsEntry>,
) -> bool {
    match tts.get(&w.narration) {
        None => {
            cx.err(file, at, format!("{} 没有 TTS 锁记录", w.narration), None);
            false
        }
        Some(e) if e.word(w.index).is_none() => {
            cx.err(
                file,
                at,
                format!("{} 只有 {} 个词，没有 {w}", w.narration, e.words.len()),
                None,
            );
            false
        }
        Some(_) => true,
    }
}

fn build_intent(
    cx: &mut Ctx,
    raw: raw::Intent,
    fps: &Fps,
    tts: &BTreeMap<NarrationId, TtsEntry>,
) -> Intent {
    const F: &str = "intent.yaml";
    let target = cx
        .dur(F, "duration.target", &raw.duration.target, fps)
        .unwrap_or(Tick::ZERO);
    let tol = match parse_percent(&raw.duration.tol) {
        Ok(r) => r,
        Err(e) => {
            cx.err(F, "duration.tol", e, Some("例如 10%"));
            Ratio { num: 0, den: 1 }
        }
    };
    let mut must = Vec::new();
    for (i, s) in raw.must.iter().enumerate() {
        let at = format!("must[{i}]");
        if let Some(r) = cx.id(F, &at, WordRange::parse(s)) {
            let a = check_word(cx, F, &at, &r.start(), tts);
            if a {
                check_word(cx, F, &at, &r.end(), tts);
            }
            must.push(r);
        }
    }
    Intent { target, tol, must }
}

fn build_structure(
    cx: &mut Ctx,
    raw: raw::Structure,
    fps: &Fps,
) -> (Vec<SectionId>, BTreeMap<SectionId, Section>) {
    const F: &str = "structure.yaml";
    let mut sections = BTreeMap::new();
    for (k, s) in raw.sections {
        let at = format!("sections.{k}");
        let Some(id) = cx.id(F, &at, SectionId::parse(&k)) else {
            continue;
        };
        let budget = match s.budget.as_deref() {
            Some(b) => cx.dur(F, &format!("{at}.budget"), b, fps),
            None => None,
        };
        let mut beats = Vec::new();
        for (i, b) in s.beats.iter().enumerate() {
            let bat = format!("{at}.beats[{i}]");
            let Some(bid) = cx.id(F, &bat, BeatId::parse(b)) else {
                continue;
            };
            if !bid.as_str().starts_with(&format!("{id}.")) {
                cx.err(
                    F,
                    &bat,
                    format!("节拍 {bid} 不属于段落 {id}"),
                    Some("节拍 ID 必须以所属段落 ID 开头"),
                );
            }
            beats.push(bid);
        }
        sections.insert(
            id,
            Section {
                role: s.role,
                budget,
                beats,
            },
        );
    }
    let mut sequence = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, s) in raw.sequence.iter().enumerate() {
        let at = format!("sequence[{i}]");
        let Some(id) = cx.id(F, &at, SectionId::parse(s)) else {
            continue;
        };
        if !sections.contains_key(&id) {
            cx.err(F, &at, format!("段落 {id} 未在 sections 中定义"), None);
            continue;
        }
        if !seen.insert(id.clone()) {
            cx.err(F, &at, format!("段落 {id} 在 sequence 中重复出现"), None);
            continue;
        }
        sequence.push(id);
    }
    for id in sections.keys() {
        if !seen.contains(id) {
            cx.err(
                F,
                &format!("sections.{id}"),
                "段落没有出现在 sequence 中",
                Some("把它加入 sequence，或删除"),
            );
        }
    }
    (sequence, sections)
}

fn parse_fit(cx: &mut Ctx, file: &str, at: &str, s: &str) -> Option<Fit> {
    match s {
        "rigid" => Some(Fit::Rigid),
        "fill" => Some(Fit::Fill),
        "bars" => Some(Fit::Bars),
        "fixed" => Some(Fit::Fixed),
        other => {
            cx.err(
                file,
                at,
                format!("未知的 fit `{other}`"),
                Some("可选：rigid、fill、bars、fixed"),
            );
            None
        }
    }
}

fn build_beats(
    cx: &mut Ctx,
    raw: raw::Content,
    fps: &Fps,
    narration: &BTreeMap<NarrationId, Narration>,
    tts: &BTreeMap<NarrationId, TtsEntry>,
    media: &Media,
) -> BTreeMap<BeatId, Beat> {
    const F: &str = "content.yaml";
    let mut out = BTreeMap::new();
    for (k, b) in raw.beats {
        let p = format!("beats.{k}");
        let Some(id) = cx.id(F, &p, BeatId::parse(&k)) else {
            continue;
        };

        let voice = match b.voice {
            None => None,
            Some(v) => {
                let at = format!("{p}.voice");
                let n = cx.id(
                    F,
                    &format!("{at}.narration"),
                    NarrationId::parse(&v.narration),
                );
                let fit = parse_fit(cx, F, &format!("{at}.fit"), &v.fit);
                if let Some(n) = &n {
                    if !narration.contains_key(n) {
                        cx.err(
                            F,
                            &at,
                            format!("旁白 {n} 不存在"),
                            Some("先在 narration.yaml 中添加"),
                        );
                    }
                }
                match (n, fit) {
                    (Some(narration), Some(fit)) => Some(Voice { narration, fit }),
                    _ => None,
                }
            }
        };

        let mut visual = Vec::new();
        for (i, v) in b.visual.iter().enumerate() {
            let at = format!("{p}.visual[{i}]");
            let Some(shot) = cx.id(F, &format!("{at}.shot"), ShotId::parse(&v.shot)) else {
                continue;
            };
            if !media.shots.contains_key(&shot) {
                cx.err(
                    F,
                    &at,
                    format!("镜头 {shot} 不在素材索引中"),
                    Some("检查 media/index.yaml"),
                );
            }
            let role = match v.role.as_str() {
                "a-roll" => Role::ARoll,
                "b-roll" => Role::BRoll,
                other => {
                    cx.err(
                        F,
                        &format!("{at}.role"),
                        format!("未知角色 `{other}`"),
                        Some("可选：a-roll、b-roll"),
                    );
                    continue;
                }
            };
            let fit = match v.fit.as_deref() {
                None => Fit::Fill,
                Some(f) => match parse_fit(cx, F, &format!("{at}.fit"), f) {
                    Some(x) => x,
                    None => continue,
                },
            };
            let len = match v.len.as_deref() {
                None => None,
                Some(s) => {
                    let Some(l) = cx.dur(F, &format!("{at}.len"), s, fps) else {
                        continue;
                    };
                    Some(l)
                }
            };
            match (fit, len) {
                (Fit::Fixed, None) => {
                    cx.err(
                        F,
                        &format!("{at}.len"),
                        "fit: fixed 必须写 len",
                        Some("例如 len: 3.2s"),
                    );
                    continue;
                }
                (Fit::Fixed, Some(l)) if l <= Tick::ZERO => {
                    cx.err(F, &format!("{at}.len"), "len 必须为正", None);
                    continue;
                }
                (Fit::Fixed, Some(_)) | (_, None) => {}
                (_, Some(_)) => {
                    cx.err(
                        F,
                        &format!("{at}.len"),
                        "len 只能和 fit: fixed 一起用",
                        None,
                    );
                    continue;
                }
            }
            let from = match v.from.as_deref() {
                None => None,
                Some(s) => {
                    let fat = format!("{at}.from");
                    let Some(w) = cx.id(F, &fat, WordRef::parse(s)) else {
                        continue;
                    };
                    if voice.as_ref().map(|vc| &vc.narration) != Some(&w.narration) {
                        cx.err(F, &fat, "from 只能引用本节拍旁白里的词", None);
                    } else {
                        check_word(cx, F, &fat, &w, tts);
                    }
                    Some(w)
                }
            };
            visual.push(Visual {
                shot,
                role,
                fit,
                from,
                len,
            });
        }

        let music = match b.music {
            None => None,
            Some(m) => {
                let at = format!("{p}.music");
                let cue = cx.id(F, &format!("{at}.cue"), MusicId::parse(&m.cue));
                let fit = parse_fit(cx, F, &format!("{at}.fit"), &m.fit);
                if let Some(c) = &cue {
                    if !media.music.contains_key(c) {
                        cx.err(F, &at, format!("音乐 {c} 不在素材索引中"), None);
                    }
                }
                let start = match m.start.as_deref() {
                    None => None,
                    Some(s) => match cx.dur(F, &format!("{at}.start"), s, fps) {
                        Some(t) if t < Tick::ZERO => {
                            cx.err(F, &format!("{at}.start"), "start 不能为负", None);
                            None
                        }
                        other => other,
                    },
                };
                match (cue, fit) {
                    (Some(cue), Some(fit)) => Some(MusicUse { cue, fit, start }),
                    _ => None,
                }
            }
        };

        let at = format!("{p}.pause");
        let after = cx.dur(F, &format!("{at}.after"), &b.pause.after, fps);
        let range = match parse_range(&b.pause.flex, Some(fps)) {
            Ok(r) => Some(r),
            Err(e) => {
                cx.err(F, &format!("{at}.flex"), e, Some("例如 0.2s..0.8s"));
                None
            }
        };
        let pause = match (after, range) {
            (Some(a), Some((lo, hi))) => {
                if lo < Tick::ZERO || a < lo || a > hi {
                    cx.err(F, &at, "after 必须落在 flex 区间内，且不能为负", None);
                }
                Pause {
                    after: a,
                    min: lo,
                    max: hi,
                }
            }
            _ => Pause {
                after: Tick::ZERO,
                min: Tick::ZERO,
                max: Tick::ZERO,
            },
        };

        out.insert(
            id,
            Beat {
                voice,
                visual,
                music,
                pause,
            },
        );
    }
    out
}

fn check_membership(
    cx: &mut Ctx,
    sections: &BTreeMap<SectionId, Section>,
    beats: &BTreeMap<BeatId, Beat>,
) {
    let mut owner: BTreeMap<&BeatId, &SectionId> = BTreeMap::new();
    for (sid, s) in sections {
        for b in &s.beats {
            let at = format!("sections.{sid}");
            if let Some(prev) = owner.insert(b, sid) {
                cx.err(
                    "structure.yaml",
                    &at,
                    format!("节拍 {b} 同时属于 {prev} 和 {sid}"),
                    None,
                );
            }
            if !beats.contains_key(b) {
                cx.err(
                    "structure.yaml",
                    &at,
                    format!("节拍 {b} 在 content.yaml 中不存在"),
                    None,
                );
            }
        }
    }
    for b in beats.keys() {
        if !owner.contains_key(b) {
            cx.err(
                "content.yaml",
                &format!("beats.{b}"),
                "节拍不属于任何段落",
                Some("在 structure.yaml 对应段落的 beats 中加入它"),
            );
        }
    }
}

fn valid_author(by: &str) -> bool {
    by == "human" || by == "ai" || by.starts_with("ai:")
}

fn anchor_spec(
    cx: &mut Ctx,
    file: &str,
    at: &str,
    a: &raw::Anchor,
    fps: &Fps,
    tts: &BTreeMap<NarrationId, TtsEntry>,
    media: &Media,
) -> Option<AnchorSpec> {
    let anchor = cx.id(file, &format!("{at}.at"), Anchor::parse(&a.at, fps))?;
    let ok = match &anchor {
        Anchor::Word(w) => check_word(cx, file, at, w, tts),
        Anchor::Event(e) => {
            let found = media.events.contains_key(e);
            if !found {
                cx.err(file, at, format!("事件 {e} 不在素材索引中"), None);
            }
            found
        }
        Anchor::Mark(m) => {
            cx.err(
                file,
                at,
                format!("标记 {m} 不存在"),
                Some("阶段 0 的素材索引还没有标记表"),
            );
            false
        }
        Anchor::Offset(s, off) => match media.shots.get(s) {
            None => {
                cx.err(file, at, format!("镜头 {s} 不在素材索引中"), None);
                false
            }
            Some(shot) if *off < Tick::ZERO || *off >= shot.duration() => {
                cx.err(file, at, "偏移超出镜头长度", None);
                false
            }
            Some(_) => true,
        },
        Anchor::Bar(b) => match media.music.get(&b.music) {
            None => {
                cx.err(file, at, format!("音乐 {} 不在素材索引中", b.music), None);
                false
            }
            Some(m) if b.bar > m.bars || b.beat > m.meter => {
                cx.err(
                    file,
                    at,
                    format!("{} 只有 {} 小节、每小节 {} 拍", b.music, m.bars, m.meter),
                    None,
                );
                false
            }
            Some(_) => true,
        },
    };
    let offset = match a.offset.as_deref() {
        None => Tick::ZERO,
        Some(s) => cx.dur(file, &format!("{at}.offset"), s, fps)?,
    };
    ok.then_some(AnchorSpec { at: anchor, offset })
}

/// `S2.b1.visual[1]` / `S2.b1.voice` → （节拍, 元素）。
fn parse_target(s: &str) -> Option<(BeatId, Slot)> {
    if let Some(beat) = s.strip_suffix(".voice") {
        return BeatId::parse(beat).ok().map(|b| (b, Slot::Voice));
    }
    let (beat, rest) = s.split_once(".visual[")?;
    let idx = rest.strip_suffix(']')?;
    if idx.is_empty() || !idx.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let i: usize = idx.parse().ok()?;
    BeatId::parse(beat).ok().map(|b| (b, Slot::Visual(i)))
}

fn yaml_scalar(v: &serde_yaml_ng::Value) -> Option<String> {
    match v {
        serde_yaml_ng::Value::String(s) => Some(s.clone()),
        serde_yaml_ng::Value::Number(n) => Some(n.to_string()),
        serde_yaml_ng::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn parse_patch(cx: &mut Ctx, at: &str, v: &serde_yaml_ng::Value) -> Option<Patch> {
    const F: &str = "overrides.yaml";
    let serde_yaml_ng::Value::Mapping(m) = v else {
        cx.err(
            F,
            at,
            "patch 必须是映射",
            Some("例如 {cut: in} 或 {exposure: \"+0.3\"}"),
        );
        return None;
    };
    let mut attrs = BTreeMap::new();
    for (k, x) in m {
        let (Some(k), Some(x)) = (yaml_scalar(k), yaml_scalar(x)) else {
            cx.err(F, at, "patch 的键和值都必须是标量", None);
            return None;
        };
        attrs.insert(k, x);
    }
    match attrs.get("cut").map(String::as_str) {
        None => Some(Patch::Attrs(attrs)),
        Some(edge) if attrs.len() > 1 => {
            cx.err(
                F,
                at,
                format!("cut: {edge} 不能和其他属性写在同一个补丁里"),
                None,
            );
            None
        }
        Some("in") => Some(Patch::CutIn),
        Some("out") => Some(Patch::CutOut),
        Some(other) => {
            cx.err(
                F,
                at,
                format!("未知的 cut `{other}`"),
                Some("可选：in、out"),
            );
            None
        }
    }
}

fn build_overrides(
    cx: &mut Ctx,
    raw: BTreeMap<String, raw::Override>,
    fps: &Fps,
    tts: &BTreeMap<NarrationId, TtsEntry>,
    media: &Media,
    beats: &BTreeMap<BeatId, Beat>,
) -> BTreeMap<OverrideId, Override> {
    const F: &str = "overrides.yaml";
    let mut out = BTreeMap::new();
    for (k, o) in raw {
        let Some(id) = cx.id(F, &k, OverrideId::parse(&k)) else {
            continue;
        };
        let Some(anchor) = anchor_spec(cx, F, &format!("{k}.anchor"), &o.anchor, fps, tts, media)
        else {
            continue;
        };
        let mut alt = Vec::new();
        for (i, a) in o.alt.iter().enumerate() {
            if let Some(s) = anchor_spec(cx, F, &format!("{k}.alt[{i}]"), a, fps, tts, media) {
                alt.push(s);
            }
        }
        let target = match parse_target(&o.target) {
            Some((beat, slot)) => match beats.get(&beat) {
                None => {
                    cx.err(
                        F,
                        &format!("{k}.target"),
                        format!("节拍 {beat} 不存在"),
                        None,
                    );
                    None
                }
                Some(b) => {
                    let ok = match slot {
                        Slot::Voice => b.voice.is_some(),
                        Slot::Visual(i) => i < b.visual.len(),
                    };
                    if !ok {
                        cx.err(
                            F,
                            &format!("{k}.target"),
                            format!("`{}` 在节拍里不存在", o.target),
                            None,
                        );
                    }
                    ok.then_some((beat, slot))
                }
            },
            None => {
                cx.err(
                    F,
                    &format!("{k}.target"),
                    format!("无法解析目标 `{}`", o.target),
                    Some("例如 S2.b1.visual[1] 或 S2.b1.voice"),
                );
                None
            }
        };
        let patch = parse_patch(cx, &format!("{k}.patch"), &o.patch);
        if !valid_author(&o.by) {
            cx.err(
                F,
                &format!("{k}.by"),
                format!("未知作者 `{}`", o.by),
                Some("可选：human、ai、ai:<名字>"),
            );
        }
        let (Some((beat, slot)), Some(patch)) = (target, patch) else {
            continue;
        };
        out.insert(
            id,
            Override {
                anchor,
                alt,
                target: o.target,
                beat,
                slot,
                patch,
                by: o.by,
            },
        );
    }
    out
}

fn parse_scope(
    cx: &mut Ctx,
    k: &str,
    v: &serde_yaml_ng::Value,
    sections: &BTreeMap<SectionId, Section>,
    beats: &BTreeMap<BeatId, Beat>,
) -> Option<RuleScope> {
    const F: &str = "rules.yaml";
    let at = format!("{k}.scope");
    let serde_yaml_ng::Value::Mapping(m) = v else {
        cx.err(
            F,
            &at,
            "scope 必须是映射",
            Some("例如 {section: S2, role: b-roll}"),
        );
        return None;
    };
    let mut scope = RuleScope::default();
    let mut ok = true;
    for (key, val) in m {
        let (Some(key), Some(val)) = (yaml_scalar(key), yaml_scalar(val)) else {
            cx.err(F, &at, "scope 的键和值都必须是标量", None);
            ok = false;
            continue;
        };
        let kat = format!("{at}.{key}");
        match key.as_str() {
            "section" => match cx.id(F, &kat, SectionId::parse(&val)) {
                Some(s) if sections.contains_key(&s) => scope.section = Some(s),
                Some(s) => {
                    cx.err(F, &kat, format!("段落 {s} 不存在"), None);
                    ok = false;
                }
                None => ok = false,
            },
            "beat" => match cx.id(F, &kat, BeatId::parse(&val)) {
                Some(b) if beats.contains_key(&b) => scope.beat = Some(b),
                Some(b) => {
                    cx.err(F, &kat, format!("节拍 {b} 不存在"), None);
                    ok = false;
                }
                None => ok = false,
            },
            "role" => match val.as_str() {
                "a-roll" => scope.role = Some(Role::ARoll),
                "b-roll" => scope.role = Some(Role::BRoll),
                other => {
                    cx.err(
                        F,
                        &kat,
                        format!("未知角色 `{other}`"),
                        Some("可选：a-roll、b-roll"),
                    );
                    ok = false;
                }
            },
            other => {
                cx.err(
                    F,
                    &kat,
                    format!("未知的范围 `{other}`"),
                    Some("可选：section、beat、role"),
                );
                ok = false;
            }
        }
    }
    if let (Some(s), Some(b)) = (&scope.section, &scope.beat) {
        if !sections[s].beats.contains(b) {
            cx.err(F, &at, format!("节拍 {b} 不属于段落 {s}"), None);
            ok = false;
        }
    }
    ok.then_some(scope)
}

fn parse_action(cx: &mut Ctx, k: &str, v: &serde_yaml_ng::Value) -> Option<RuleAction> {
    let at = format!("{k}.do");
    if let serde_yaml_ng::Value::Mapping(m) = v {
        if m.len() == 1 {
            if let Some((key, val)) = m.iter().next() {
                if yaml_scalar(key).as_deref() == Some("cut_on")
                    && yaml_scalar(val).as_deref() == Some("beat.strong")
                {
                    return Some(RuleAction::CutOnStrongBeat);
                }
            }
        }
    }
    cx.err(
        "rules.yaml",
        &at,
        "不支持的规则动作",
        Some("阶段 0 只支持 do: {cut_on: beat.strong}"),
    );
    None
}

fn build_rules(
    cx: &mut Ctx,
    raw: BTreeMap<String, raw::Rule>,
    sections: &BTreeMap<SectionId, Section>,
    beats: &BTreeMap<BeatId, Beat>,
    overrides: &BTreeMap<OverrideId, Override>,
) -> BTreeMap<RuleId, Rule> {
    const F: &str = "rules.yaml";
    let mut out = BTreeMap::new();
    for (k, r) in raw {
        let Some(id) = cx.id(F, &k, RuleId::parse(&k)) else {
            continue;
        };
        let scope = parse_scope(cx, &k, &r.scope, sections, beats);
        let action = parse_action(cx, &k, &r.action);
        if !valid_author(&r.by) {
            cx.err(
                F,
                &format!("{k}.by"),
                format!("未知作者 `{}`", r.by),
                Some("可选：human、ai、ai:<名字>"),
            );
        }
        let mut examples = Vec::new();
        for (i, e) in r.examples.iter().enumerate() {
            let at = format!("{k}.examples[{i}]");
            if let Some(o) = cx.id(F, &at, OverrideId::parse(e)) {
                if overrides.contains_key(&o) {
                    examples.push(o);
                } else {
                    cx.err(F, &at, format!("补丁 {o} 不存在"), None);
                }
            }
        }
        if let (Some(scope), Some(action)) = (scope, action) {
            out.insert(
                id,
                Rule {
                    scope,
                    action,
                    by: r.by,
                    examples,
                },
            );
        }
    }
    out
}

fn build_frozen(
    cx: &mut Ctx,
    src: &SourceSet,
    sections: &BTreeMap<SectionId, Section>,
) -> BTreeMap<SectionId, Frozen> {
    let names: Vec<String> = src
        .iter()
        .map(|(n, _)| n.to_string())
        .filter(|n| is_frozen_file(n))
        .collect();
    let mut out = BTreeMap::new();
    for name in names {
        let Some(f) = parse_json::<raw::Frozen>(src, cx, &name, true) else {
            continue;
        };
        let stem = &name["frozen/".len()..name.len() - ".json".len()];
        if f.format != 1 {
            cx.err(
                &name,
                "format",
                format!("不支持的格式版本 {}", f.format),
                None,
            );
            continue;
        }
        let Some(sid) = cx.id(&name, "section", SectionId::parse(&f.section)) else {
            continue;
        };
        if sid.as_str() != stem {
            cx.err(
                &name,
                "section",
                format!("文件名是 {stem}，内容写的是 {sid}"),
                None,
            );
            continue;
        }
        let Some(sec) = sections.get(&sid) else {
            cx.err(
                &name,
                "section",
                format!("段落 {sid} 不存在"),
                Some("删除这个冻结文件，或用 clip unfreeze"),
            );
            continue;
        };
        if f.dur <= 0 {
            cx.err(&name, "dur", "冻结段的时长必须为正", None);
            continue;
        }
        let mut clips = Vec::new();
        let mut ok = true;
        for (i, c) in f.clips.iter().enumerate() {
            let at = format!("clips[{i}]");
            let Some(origin) = cx.id(&name, &format!("{at}.origin"), BeatId::parse(&c.origin))
            else {
                ok = false;
                continue;
            };
            if !sec.beats.contains(&origin) {
                cx.err(&name, &at, format!("节拍 {origin} 不属于段落 {sid}"), None);
                ok = false;
            }
            if !["A1", "V1", "V2"].contains(&c.track.as_str()) {
                cx.err(
                    &name,
                    &format!("{at}.track"),
                    format!("未知轨道 `{}`", c.track),
                    Some("可选：A1、V1、V2"),
                );
                ok = false;
            }
            if c.rec_in < 0 || c.rec_in > c.rec_out || c.rec_out > f.dur || c.src_in > c.src_out {
                cx.err(
                    &name,
                    &at,
                    "片段时间不合法（超出冻结段，或入点晚于出点）",
                    None,
                );
                ok = false;
            }
            clips.push(FrozenClip {
                id: c.id.clone(),
                origin,
                track: c.track.clone(),
                src: c.src.clone(),
                src_in: Tick(c.src_in),
                src_out: Tick(c.src_out),
                rec_in: Tick(c.rec_in),
                rec_out: Tick(c.rec_out),
                attrs: c.attrs.clone(),
                patches: c.patches.clone(),
                rules: c.rules.clone(),
            });
        }
        if ok {
            out.insert(
                sid,
                Frozen {
                    dur: Tick(f.dur),
                    solver: f.solver,
                    clips,
                },
            );
        }
    }
    out
}

fn build_solver_lock(cx: &mut Ctx, l: raw::SolverLock) -> Option<SolverLock> {
    if l.format != 1 {
        cx.err(
            SOLVER_LOCK,
            "format",
            format!("不支持的格式版本 {}", l.format),
            None,
        );
        return None;
    }
    let mut zones = BTreeMap::new();
    for (k, h) in l.zones {
        if let Some(sid) = cx.id(SOLVER_LOCK, &format!("zones.{k}"), SectionId::parse(&k)) {
            zones.insert(sid, h);
        }
    }
    Some(SolverLock {
        solver: l.solver,
        zones,
    })
}
