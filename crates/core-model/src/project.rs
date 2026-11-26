//! 校验后的工程模型。只由 core-store 构造；字段都已通过引用检查。

use std::collections::BTreeMap;

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
    pub rules: BTreeMap<RuleId, Rule>,
    /// 冻结的段落：所有权归 L1，编译时当作硬盒子原样输出
    pub frozen: BTreeMap<SectionId, Frozen>,
    pub solver_lock: Option<SolverLock>,
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
    /// 音乐相对节拍开始延后多久进入。只有第一个用到这段音乐的节拍里的值生效
    pub start: Option<Tick>,
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

/// 补丁作用在节拍里的哪个元素上。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Slot {
    Voice,
    Visual(usize),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Patch {
    /// 裁头：入点挪到锚点，素材入点按播放速度同步挪动
    CutIn,
    /// 裁尾：出点挪到锚点
    CutOut,
    /// 渲染属性（曝光等），原样传给渲染器
    Attrs(BTreeMap<String, String>),
}

#[derive(Clone, Debug)]
pub struct Override {
    pub anchor: AnchorSpec,
    pub alt: Vec<AnchorSpec>,
    pub target: String,
    pub beat: BeatId,
    pub slot: Slot,
    pub patch: Patch,
    pub by: String,
}

/// 规则的作用范围。字段都为空表示全片。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RuleScope {
    pub section: Option<SectionId>,
    pub beat: Option<BeatId>,
    pub role: Option<Role>,
}

impl RuleScope {
    /// 越具体越大：节拍 2 > 段落 1 > 全片 0。
    pub fn specificity(&self) -> u8 {
        if self.beat.is_some() {
            2
        } else if self.section.is_some() {
            1
        } else {
            0
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuleAction {
    /// 画面入点吸附到最近的强拍
    CutOnStrongBeat,
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub scope: RuleScope,
    pub action: RuleAction,
    pub by: String,
    pub examples: Vec<OverrideId>,
}

/// 冻结段里的一个片段。时间相对段落起点。
#[derive(Clone, Debug)]
pub struct FrozenClip {
    pub id: String,
    pub origin: BeatId,
    pub track: String,
    pub src: String,
    pub src_in: Tick,
    pub src_out: Tick,
    pub rec_in: Tick,
    pub rec_out: Tick,
    pub attrs: BTreeMap<String, String>,
    pub patches: Vec<String>,
    pub rules: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Frozen {
    pub dur: Tick,
    pub solver: String,
    pub clips: Vec<FrozenClip>,
}

/// 求解器版本锁：确认时的版本和每个区的结果哈希。
#[derive(Clone, Debug)]
pub struct SolverLock {
    pub solver: String,
    pub zones: BTreeMap<SectionId, String>,
}
