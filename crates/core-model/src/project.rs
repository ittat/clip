//! 校验后的工程模型。只由 core-store 构造；字段都已通过引用检查。

use std::collections::{BTreeMap, BTreeSet};

use crate::id::*;
use crate::tick::{Fps, Ratio, SampleRate, Tick};

#[derive(Clone, Debug)]
pub struct Project {
    pub config: Config,
    pub intent: Intent,
    /// 段落的成片顺序（structure.yaml 的 `sequence`）
    pub sequence: Vec<SectionId>,
    pub sections: BTreeMap<SectionId, Section>,
    pub beats: BTreeMap<BeatId, Beat>,
    pub narration: BTreeMap<NarrationId, Narration>,
    pub media: Media,
    pub tts: BTreeMap<NarrationId, TtsEntry>,
    pub overrides: BTreeMap<OverrideId, Override>,
    pub rules: BTreeSet<RuleId>,
}

impl Project {
    /// 按成片顺序列出全部节拍。
    pub fn ordered_beats(&self) -> Vec<&BeatId> {
        self.sequence
            .iter()
            .flat_map(|s| self.sections[s].beats.iter())
            .collect()
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub fps: Fps,
    pub sample_rate: SampleRate,
    pub solver: String,
}

#[derive(Clone, Debug)]
pub struct Intent {
    pub target: Tick,
    pub tol: Ratio,
    pub must: Vec<WordRange>,
}

impl Intent {
    pub fn window(&self) -> (Tick, Tick) {
        let d = self.tol.of(self.target);
        (self.target - d, self.target + d)
    }
}

#[derive(Clone, Debug)]
pub struct Section {
    pub role: String,
    /// `Some` = 声明的预算；`None` = 由求解得出
    pub budget: Option<Tick>,
    pub beats: Vec<BeatId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fit {
    Rigid,
    Fill,
    Bars,
    Fixed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    ARoll,
    BRoll,
}

#[derive(Clone, Debug)]
pub struct Voice {
    pub narration: NarrationId,
    pub fit: Fit,
}

#[derive(Clone, Debug)]
pub struct Visual {
    pub shot: ShotId,
    pub role: Role,
    pub fit: Fit,
    /// 从本节拍旁白的第几个词开始
    pub from: Option<WordRef>,
    /// `fit: fixed` 的固定长度（人工锁定）
    pub len: Option<Tick>,
}

#[derive(Clone, Debug)]
pub struct MusicUse {
    pub cue: MusicId,
    pub fit: Fit,
}

#[derive(Clone, Copy, Debug)]
pub struct Pause {
    pub after: Tick,
    pub min: Tick,
    pub max: Tick,
}

#[derive(Clone, Debug)]
pub struct Beat {
    pub voice: Option<Voice>,
    /// 列表顺序有语义（层次），不能打乱
    pub visual: Vec<Visual>,
    pub music: Option<MusicUse>,
    pub pause: Pause,
}

#[derive(Clone, Debug)]
pub struct Narration {
    pub text: String,
    pub voice: String,
}

#[derive(Clone, Debug, Default)]
pub struct Media {
    pub shots: BTreeMap<ShotId, Shot>,
    pub events: BTreeMap<EventId, Event>,
    pub music: BTreeMap<MusicId, Music>,
}

#[derive(Clone, Debug)]
pub struct Shot {
    pub src: String,
    pub src_in: Tick,
    pub src_out: Tick,
}

impl Shot {
    pub fn duration(&self) -> Tick {
        self.src_out - self.src_in
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    pub shot: ShotId,
    /// 素材坐标（镜头所在文件内的时间）
    pub at: Tick,
    pub kind: String,
}

#[derive(Clone, Debug)]
pub struct Music {
    pub src: String,
    pub bpm: u32,
    pub meter: u32,
    pub first_bar: Tick,
    pub bars: u32,
    /// 一拍的长度
    pub beat: Tick,
}

#[derive(Clone, Debug)]
pub struct TtsEntry {
    pub audio: String,
    pub dur: Tick,
    pub words: Vec<WordTiming>,
}

impl TtsEntry {
    pub fn word(&self, index: u32) -> Option<&WordTiming> {
        self.words.iter().find(|w| w.index == index)
    }
}

#[derive(Clone, Debug)]
pub struct WordTiming {
    pub index: u32,
    pub t0: Tick,
    pub t1: Tick,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct AnchorSpec {
    pub at: Anchor,
    pub offset: Tick,
}

#[derive(Clone, Debug)]
pub struct Override {
    pub anchor: AnchorSpec,
    pub alt: Vec<AnchorSpec>,
    pub target: String,
    pub by: String,
}
