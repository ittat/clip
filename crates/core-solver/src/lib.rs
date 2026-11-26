//! 弹性求解器。纯函数：不读文件、不看时钟、不用随机数。
//!
//! PR 3：以区（段落）为单位消化超时，顺序由代价公式决定（便宜的先用）：
//! 1. 区内：段落超出声明预算时，依次用停顿（第 9 项）→ 纯画面节拍的主体（第 8 项）→ 语速 ±8%（第 7 项）
//! 2. 全片：超出 L4 意图上限时，在所有区剩余的弹性里继续消化；仍超出的部分记为第 3 项，必须上报
//! 3. 借用：仍超预算的区，向相邻区未用完的声明预算借时间（第 6 项，每个出借区计 1）
//! 4. 借不到的部分记为偏离声明预算（第 5 项）：编译器可以接受，写进报告
//!
//! 每一步按各节拍的可用量按比例分配，余数按成片顺序逐个补 1 flick，结果确定。

use core_model::{BeatId, Cost, SectionId, Tick, FLICKS_PER_SECOND};

pub const SOLVER_VERSION: &str = "zone-2";

/// 慢放上限：成片时长最多比素材长 20%。
pub const MAX_RETIME_PERCENT: i64 = 20;

/// 语速上限：最多加快 8%。
pub const MAX_RATE_PERCENT: i64 = 8;

/// 纯画面节拍的主体最短压到 1 秒（素材本身更短时取素材长度）。
pub const MIN_ELASTIC_BODY: Tick = Tick(FLICKS_PER_SECOND);

/// 原速（ppm）。
pub const UNIT_SPEED: i64 = 1_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VisualKind {
    /// 填满节拍：截取或慢放
    Elastic,
    /// 按素材原长播放，参与决定节拍长度
    Rigid,
    /// 人工固定的长度，参与决定节拍长度
    Fixed(Tick),
}

/// 节拍里的一条画面。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualInput {
    /// 相对节拍开始的入画时间（按旁白原速；语速变化时等比缩放）
    pub offset: Tick,
    /// 素材可用长度
    pub have: Tick,
    pub kind: VisualKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PauseInput {
    pub natural: Tick,
    pub min: Tick,
    pub max: Tick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeatInput {
    pub id: BeatId,
    /// 旁白原速时长
    pub voice: Option<Tick>,
    pub visuals: Vec<VisualInput>,
    pub pause: PauseInput,
}

/// 一个区 = 一个段落。`budget` 为 `None` 表示预算由求解得出。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZoneInput {
    pub id: SectionId,
    pub budget: Option<Tick>,
    pub beats: Vec<BeatInput>,
}

/// 求解输入。`zones` 按成片顺序排列；`window` 是 L4 意图的时长区间。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolveInput {
    pub zones: Vec<ZoneInput>,
    pub window: (Tick, Tick),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillKind {
    /// 素材长度刚好
    Exact,
    /// 素材够长，截取前段
    Cut,
    /// 慢放补足（不超过上限）
    Retime,
    /// 慢放到上限仍不够
    Short,
    /// 人工固定长度
    Fixed,
}

impl FillKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FillKind::Exact => "exact",
            FillKind::Cut => "cut",
            FillKind::Retime => "retime",
            FillKind::Short => "short",
            FillKind::Fixed => "fixed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualPlacement {
    /// 相对节拍开始的入画时间（已按语速缩放）
    pub offset: Tick,
    /// 在成片里占的时长
    pub rec_len: Tick,
    /// 用掉的素材时长
    pub src_len: Tick,
    pub fill: FillKind,
    /// 仍缺的时长（慢放到上限后，或固定长度超出素材）
    pub missing: Tick,
}

impl VisualPlacement {
    /// 慢放多出来的时长（计入代价第 8 项）。
    pub fn retime_extra(&self) -> Tick {
        match self.fill {
            FillKind::Retime | FillKind::Short => self.rec_len - self.src_len,
            FillKind::Exact | FillKind::Cut | FillKind::Fixed => Tick::ZERO,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VoicePlacement {
    /// 原速时长
    pub natural: Tick,
    /// 实际时长
    pub len: Tick,
}

impl VoicePlacement {
    pub fn speed_ppm(&self) -> i64 {
        speed_ppm(self.natural, self.len)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeatPlacement {
    pub id: BeatId,
    /// 成片里的绝对起点
    pub start: Tick,
    pub dur: Tick,
    /// 节拍主体（不含停顿）
    pub body: Tick,
    pub pause: Tick,
    pub pause_natural: Tick,
    /// 纯画面节拍的主体被压缩的量
    pub elastic_shrink: Tick,
    pub voice: Option<VoicePlacement>,
    /// 与 `BeatInput::visuals` 一一对应
    pub visuals: Vec<VisualPlacement>,
    pub cost: Cost,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZonePlacement {
    pub id: SectionId,
    pub start: Tick,
    pub dur: Tick,
    /// 未消化前的自然长度
    pub natural: Tick,
    pub budget: Option<Tick>,
    /// 借用之后仍超出声明预算的部分（第 5 项）
    pub over: Tick,
    pub borrowed: Vec<(SectionId, Tick)>,
    pub lent: Vec<(SectionId, Tick)>,
    /// 是否为满足 L4 上限而被压缩
    pub l4: bool,
    pub beats: Vec<BeatPlacement>,
}

/// 固定长度超出素材：物理上做不到，计入第 1 项。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HardIssue {
    pub beat: BeatId,
    pub visual: usize,
    pub len: Tick,
    pub have: Tick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolveOutput {
    pub version: &'static str,
    pub zones: Vec<ZonePlacement>,
    pub total: Tick,
    pub cost: Cost,
    /// 用尽全部弹性后仍超出 L4 上限的部分（第 3 项）
    pub intent_over: Tick,
    pub hard: Vec<HardIssue>,
}

fn mul_div(a: Tick, num: i64, den: i64) -> Tick {
    let v = i128::from(a.flicks()) * i128::from(num) / i128::from(den);
    Tick(i64::try_from(v).expect("tick overflow"))
}

fn ceil_mul_div(a: Tick, num: i64, den: i64) -> Tick {
    let n = i128::from(a.flicks()) * i128::from(num);
    let d = i128::from(den);
    Tick(i64::try_from((n + d - 1).div_euclid(d)).expect("tick overflow"))
}

fn sum(v: &[Tick]) -> Tick {
    v.iter().fold(Tick::ZERO, |a, &b| a + b)
}

fn to_cost(t: Tick) -> u64 {
    u64::try_from(t.flicks()).unwrap_or(0)
}

/// 播放速度（ppm）：素材时长 / 成片时长 × 1e6，四舍五入到最近的整数。
///
/// 旁白最短时长 `voice_min` 向上取整，保证真实速度不超过 +8%；
/// 这里四舍五入，所以语速上限显示为整 1,080,000，而不是 1,079,999。
pub fn speed_ppm(src_len: Tick, rec_len: Tick) -> i64 {
    if rec_len.flicks() <= 0 {
        return UNIT_SPEED;
    }
    let n = i128::from(src_len.flicks()) * i128::from(UNIT_SPEED);
    let d = i128::from(rec_len.flicks());
    let v = (2 * n + d).div_euclid(2 * d);
    i64::try_from(v).unwrap_or(UNIT_SPEED)
}

/// 慢放上限下，素材最多能撑多长。
pub fn retime_limit(have: Tick) -> Tick {
    have + mul_div(have, MAX_RETIME_PERCENT, 100)
}

/// 语速加快到上限时，旁白的最短时长（向上取整，保证不超过上限）。
pub fn voice_min(natural: Tick) -> Tick {
    ceil_mul_div(natural, 100, 100 + MAX_RATE_PERCENT)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BeatState {
    voice: Option<Tick>,
    /// 纯画面节拍的主体
    elastic: Tick,
    pause: Tick,
}

fn elastic_only(b: &BeatInput) -> bool {
    b.voice.is_none() && b.visuals.iter().all(|v| v.kind == VisualKind::Elastic)
}

fn elastic_natural(b: &BeatInput) -> Tick {
    b.visuals
        .iter()
        .map(|v| v.offset + v.have)
        .max()
        .unwrap_or(Tick::ZERO)
}

fn elastic_min(b: &BeatInput) -> Tick {
    elastic_natural(b).min(MIN_ELASTIC_BODY)
}

fn natural_state(b: &BeatInput) -> BeatState {
    BeatState {
        voice: b.voice,
        elastic: elastic_natural(b),
        pause: b.pause.natural,
    }
}

fn min_state(b: &BeatInput) -> BeatState {
    BeatState {
        voice: b.voice.map(voice_min),
        elastic: elastic_min(b),
        pause: b.pause.min,
    }
}

/// 画面入画时间随语速等比缩放。
fn scaled_offset(b: &BeatInput, voice: Option<Tick>, offset: Tick) -> Tick {
    match (b.voice, voice) {
        (Some(nat), Some(cur)) if nat > Tick::ZERO => mul_div(offset, cur.flicks(), nat.flicks()),
        _ => offset,
    }
}

fn locked_len(v: &VisualInput) -> Option<Tick> {
    match v.kind {
        VisualKind::Elastic => None,
        VisualKind::Rigid => Some(v.have),
        VisualKind::Fixed(len) => Some(len),
    }
}

/// 不可伸缩的画面里结束得最晚的那条：（结束时间, 序号）。
fn locked_end(b: &BeatInput, voice: Option<Tick>) -> Option<(Tick, usize)> {
    let mut best: Option<(Tick, usize)> = None;
    for (i, v) in b.visuals.iter().enumerate() {
        if let Some(len) = locked_len(v) {
            let end = scaled_offset(b, voice, v.offset) + len;
            if best.is_none_or(|(e, _)| end > e) {
                best = Some((end, i));
            }
        }
    }
    best
}

fn body(b: &BeatInput, s: &BeatState) -> Tick {
    if elastic_only(b) {
        return s.elastic;
    }
    let locked = locked_end(b, s.voice).map(|(e, _)| e);
    match (s.voice, locked) {
        (Some(v), Some(r)) => v.max(r),
        (Some(v), None) => v,
        (None, Some(r)) => r,
        (None, None) => Tick::ZERO,
    }
}

/// 节拍主体在所有弹性用尽时的最短长度。
pub fn body_min(b: &BeatInput) -> Tick {
    body(b, &min_state(b))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Lever {
    Pause,
    Elastic,
    Rate,
}

/// 代价从低到高：停顿（9）→ 画面（8）→ 语速（7）。
const LEVERS: [Lever; 3] = [Lever::Pause, Lever::Elastic, Lever::Rate];

fn capacity(b: &BeatInput, s: &BeatState, lever: Lever) -> Tick {
    match lever {
        Lever::Pause => s.pause - b.pause.min,
        Lever::Elastic => {
            if elastic_only(b) {
                s.elastic - elastic_min(b)
            } else {
                Tick::ZERO
            }
        }
        Lever::Rate => match b.voice {
            Some(nat) => {
                let fastest = BeatState {
                    voice: Some(voice_min(nat)),
                    ..*s
                };
                body(b, s) - body(b, &fastest)
            }
            None => Tick::ZERO,
        },
    }
}

/// 让节拍缩短至多 `x`，返回实际缩短量。
fn apply(b: &BeatInput, s: &mut BeatState, lever: Lever, x: Tick) -> Tick {
    if x <= Tick::ZERO {
        return Tick::ZERO;
    }
    match lever {
        Lever::Pause => {
            s.pause = s.pause - x;
            x
        }
        Lever::Elastic => {
            s.elastic = s.elastic - x;
            x
        }
        Lever::Rate => {
            let (Some(nat), Some(cur)) = (b.voice, s.voice) else {
                return Tick::ZERO;
            };
            let before = body(b, s);
            let goal = before - x;
            let at = |v: i64| {
                body(
                    b,
                    &BeatState {
                        voice: Some(Tick(v)),
                        ..*s
                    },
                )
            };
            // 最短的旁白长度，使主体仍不短于目标
            let (mut lo, mut hi) = (voice_min(nat).flicks(), cur.flicks());
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                if at(mid) >= goal {
                    hi = mid;
                } else {
                    lo = mid + 1;
                }
            }
            let reached = at(lo);
            // 主体相同的前提下取最长的旁白：语速改动最小
            let (mut a, mut z) = (lo, cur.flicks());
            while a < z {
                let mid = a + (z - a + 1) / 2;
                if at(mid) <= reached {
                    a = mid;
                } else {
                    z = mid - 1;
                }
            }
            s.voice = Some(Tick(a));
            before - reached
        }
    }
}

/// 按可用量比例分配 `need`；余数按顺序逐个补 1 flick。
fn distribute(need: Tick, caps: &[Tick]) -> Vec<Tick> {
    let total: i128 = caps.iter().map(|c| i128::from(c.flicks())).sum();
    let need_i = i128::from(need.flicks());
    if total <= 0 || need_i <= 0 {
        return vec![Tick::ZERO; caps.len()];
    }
    if need_i >= total {
        return caps.to_vec();
    }
    let mut out: Vec<i64> = caps
        .iter()
        .map(|c| i64::try_from(need_i * i128::from(c.flicks()) / total).unwrap_or(0))
        .collect();
    let mut rem = need.flicks() - out.iter().sum::<i64>();
    for (o, c) in out.iter_mut().zip(caps) {
        if rem == 0 {
            break;
        }
        if *o < c.flicks() {
            *o += 1;
            rem -= 1;
        }
    }
    out.into_iter().map(Tick).collect()
}

fn zone_dur(z: &ZoneInput, states: &[BeatState]) -> Tick {
    z.beats
        .iter()
        .zip(states)
        .fold(Tick::ZERO, |acc, (b, s)| acc + body(b, s) + s.pause)
}

fn zone_durs(input: &SolveInput, states: &[Vec<BeatState>]) -> Vec<Tick> {
    input
        .zones
        .iter()
        .zip(states)
        .map(|(z, s)| zone_dur(z, s))
        .collect()
}

/// 依次用各种弹性消化 `need`，返回实际消化量。`targets` 是（区, 节拍）序号。
fn absorb(
    input: &SolveInput,
    states: &mut [Vec<BeatState>],
    targets: &[(usize, usize)],
    need: Tick,
) -> Tick {
    let mut left = need;
    for lever in LEVERS {
        if left <= Tick::ZERO {
            break;
        }
        let caps: Vec<Tick> = targets
            .iter()
            .map(|&(z, i)| capacity(&input.zones[z].beats[i], &states[z][i], lever))
            .collect();
        let shares = distribute(left, &caps);
        for (&(z, i), x) in targets.iter().zip(shares) {
            let got = apply(&input.zones[z].beats[i], &mut states[z][i], lever, x);
            left = left - got;
        }
    }
    need - left
}

fn fill(v: &VisualInput, offset: Tick, dur: Tick) -> VisualPlacement {
    let slot = if dur > offset {
        dur - offset
    } else {
        Tick::ZERO
    };
    let placed = |rec_len: Tick, src_len: Tick, fill: FillKind, missing: Tick| VisualPlacement {
        offset,
        rec_len,
        src_len,
        fill,
        missing,
    };
    match v.kind {
        VisualKind::Rigid => {
            let len = v.have.min(slot);
            placed(len, len, FillKind::Exact, Tick::ZERO)
        }
        VisualKind::Fixed(len) => {
            if len <= v.have {
                placed(len, len, FillKind::Fixed, Tick::ZERO)
            } else {
                placed(v.have, v.have, FillKind::Fixed, len - v.have)
            }
        }
        VisualKind::Elastic => {
            if v.have >= slot {
                let kind = if v.have == slot {
                    FillKind::Exact
                } else {
                    FillKind::Cut
                };
                placed(slot, slot, kind, Tick::ZERO)
            } else {
                let limit = retime_limit(v.have);
                if slot <= limit {
                    placed(slot, v.have, FillKind::Retime, Tick::ZERO)
                } else {
                    placed(limit, v.have, FillKind::Short, slot - limit)
                }
            }
        }
    }
}

fn place_beat(
    b: &BeatInput,
    s: &BeatState,
    start: Tick,
    hard: &mut Vec<HardIssue>,
) -> BeatPlacement {
    let body_len = body(b, s);
    let dur = body_len + s.pause;
    let mut issues = 0u32;
    let mut visuals = Vec::with_capacity(b.visuals.len());
    for (i, v) in b.visuals.iter().enumerate() {
        if let VisualKind::Fixed(len) = v.kind {
            if len > v.have {
                hard.push(HardIssue {
                    beat: b.id.clone(),
                    visual: i,
                    len,
                    have: v.have,
                });
                issues += 1;
            }
        }
        visuals.push(fill(v, scaled_offset(b, s.voice, v.offset), dur));
    }
    let retime = visuals
        .iter()
        .fold(Tick::ZERO, |acc, v| acc + v.retime_extra());
    let elastic_shrink = if elastic_only(b) {
        elastic_natural(b) - s.elastic
    } else {
        Tick::ZERO
    };
    let voice = match (b.voice, s.voice) {
        (Some(natural), Some(len)) => Some(VoicePlacement { natural, len }),
        _ => None,
    };
    let rate = voice.map_or(0, |v| {
        u64::try_from(v.speed_ppm() - UNIT_SPEED).unwrap_or(0)
    });
    let cost = Cost {
        hard: issues,
        rate,
        trim: to_cost(retime + elastic_shrink),
        pause: to_cost(b.pause.natural - s.pause),
        ..Cost::default()
    };
    BeatPlacement {
        id: b.id.clone(),
        start,
        dur,
        body: body_len,
        pause: s.pause,
        pause_natural: b.pause.natural,
        elastic_shrink,
        voice,
        visuals,
        cost,
    }
}

/// 惯性所需的“上次结果”通过 `prev` 显式传入（PR 3 尚不使用）。
pub fn solve(input: &SolveInput, _prev: Option<&SolveOutput>) -> SolveOutput {
    let n = input.zones.len();
    let mut states: Vec<Vec<BeatState>> = input
        .zones
        .iter()
        .map(|z| z.beats.iter().map(natural_state).collect())
        .collect();
    let naturals = zone_durs(input, &states);

    // 1. 区内消化：超出声明预算的段落先用自己的弹性
    for (zi, z) in input.zones.iter().enumerate() {
        if let Some(budget) = z.budget {
            if naturals[zi] > budget {
                let targets: Vec<(usize, usize)> = (0..z.beats.len()).map(|i| (zi, i)).collect();
                absorb(input, &mut states, &targets, naturals[zi] - budget);
            }
        }
    }

    // 2. 全片超出 L4 上限：在所有区剩余的弹性里消化
    let before = zone_durs(input, &states);
    let total = sum(&before);
    let (_, max) = input.window;
    let mut intent_over = Tick::ZERO;
    let mut l4 = vec![false; n];
    if total > max {
        let targets: Vec<(usize, usize)> = input
            .zones
            .iter()
            .enumerate()
            .flat_map(|(zi, z)| (0..z.beats.len()).map(move |i| (zi, i)))
            .collect();
        let need = total - max;
        intent_over = need - absorb(input, &mut states, &targets, need);
        let after = zone_durs(input, &states);
        for ((flag, b), a) in l4.iter_mut().zip(&before).zip(&after) {
            *flag = a < b;
        }
    }

    // 3. 借用相邻区未用完的声明预算；4. 剩余部分记为偏离声明预算
    let durs = zone_durs(input, &states);
    let mut slack: Vec<Tick> = input
        .zones
        .iter()
        .zip(&durs)
        .map(|(z, &d)| match z.budget {
            Some(b) if b > d => b - d,
            _ => Tick::ZERO,
        })
        .collect();
    let mut over: Vec<Tick> = input
        .zones
        .iter()
        .zip(&durs)
        .map(|(z, &d)| match z.budget {
            Some(b) if d > b => d - b,
            _ => Tick::ZERO,
        })
        .collect();
    let mut borrowed: Vec<Vec<(usize, Tick)>> = vec![Vec::new(); n];
    let mut lent: Vec<Vec<(usize, Tick)>> = vec![Vec::new(); n];
    let mut lenders = 0usize;
    for (zi, _) in input.zones.iter().enumerate() {
        // 相邻区：先前一个，再后一个
        let neighbours = [zi.checked_sub(1), (zi + 1 < n).then_some(zi + 1)];
        for nb in neighbours.into_iter().flatten() {
            if over[zi] <= Tick::ZERO {
                break;
            }
            let take = over[zi].min(slack[nb]);
            if take > Tick::ZERO {
                slack[nb] = slack[nb] - take;
                over[zi] = over[zi] - take;
                borrowed[zi].push((nb, take));
                lent[nb].push((zi, take));
                lenders += 1;
            }
        }
    }

    // 组装结果
    let l4_count = l4.iter().filter(|&&f| f).count();
    let mut cost = Cost {
        intent: to_cost(intent_over),
        declared_budget: to_cost(sum(&over)),
        cross_zone: u32::try_from(lenders + l4_count).unwrap_or(u32::MAX),
        ..Cost::default()
    };
    let mut hard = Vec::new();
    let mut zones = Vec::with_capacity(n);
    let mut cursor = Tick::ZERO;
    let name = |list: &[(usize, Tick)]| -> Vec<(SectionId, Tick)> {
        list.iter()
            .map(|&(j, t)| (input.zones[j].id.clone(), t))
            .collect()
    };
    for (zi, z) in input.zones.iter().enumerate() {
        let zone_start = cursor;
        let mut beats = Vec::with_capacity(z.beats.len());
        for (b, s) in z.beats.iter().zip(&states[zi]) {
            let p = place_beat(b, s, cursor, &mut hard);
            cursor = cursor + p.dur;
            cost = cost + p.cost;
            beats.push(p);
        }
        zones.push(ZonePlacement {
            id: z.id.clone(),
            start: zone_start,
            dur: cursor - zone_start,
            natural: naturals[zi],
            budget: z.budget,
            over: over[zi],
            borrowed: name(&borrowed[zi]),
            lent: name(&lent[zi]),
            l4: l4[zi],
            beats,
        });
    }
    SolveOutput {
        version: SOLVER_VERSION,
        zones,
        total: cursor,
        cost,
        intent_over,
        hard,
    }
}

/// 冲突集里的元素：节拍主体由谁决定，或停顿。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    Voice,
    Visual(usize),
    Elastic,
    Pause,
}

/// 每个节拍在所有弹性用尽时，各部分的最短长度。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MinElement {
    pub beat: BeatId,
    pub part: Part,
    pub min: Tick,
}

pub fn min_elements(input: &SolveInput) -> Vec<MinElement> {
    let mut out = Vec::new();
    for b in input.zones.iter().flat_map(|z| &z.beats) {
        let s = min_state(b);
        let len = body(b, &s);
        if len > Tick::ZERO {
            let part = if elastic_only(b) {
                Part::Elastic
            } else {
                match (s.voice, locked_end(b, s.voice)) {
                    (Some(v), Some((e, i))) if e > v => Part::Visual(i),
                    (Some(_), _) => Part::Voice,
                    (None, Some((_, i))) => Part::Visual(i),
                    (None, None) => Part::Elastic,
                }
            };
            out.push(MinElement {
                beat: b.id.clone(),
                part,
                min: len,
            });
        }
        if b.pause.min > Tick::ZERO {
            out.push(MinElement {
                beat: b.id.clone(),
                part: Part::Pause,
                min: b.pause.min,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(n: i64) -> Tick {
        Tick::from_seconds(n)
    }

    fn ms(n: i64) -> Tick {
        Tick(n * (FLICKS_PER_SECOND / 1000))
    }

    fn pause(nat: i64, min: i64) -> PauseInput {
        PauseInput {
            natural: ms(nat),
            min: ms(min),
            max: ms(nat.max(min) * 2),
        }
    }

    fn beat(id: &str, voice: Option<Tick>, visuals: Vec<VisualInput>, p: PauseInput) -> BeatInput {
        BeatInput {
            id: BeatId::parse(id).unwrap(),
            voice,
            visuals,
            pause: p,
        }
    }

    fn vis(offset: Tick, have: Tick, kind: VisualKind) -> VisualInput {
        VisualInput { offset, have, kind }
    }

    fn zone(id: &str, budget: Option<Tick>, beats: Vec<BeatInput>) -> ZoneInput {
        ZoneInput {
            id: SectionId::parse(id).unwrap(),
            budget,
            beats,
        }
    }

    fn wide(zones: Vec<ZoneInput>) -> SolveInput {
        SolveInput {
            zones,
            window: (Tick::ZERO, s(1000)),
        }
    }

    #[test]
    fn sequential_and_pure() {
        let input = wide(vec![zone(
            "S1",
            None,
            vec![
                beat("S1.b1", Some(s(2)), vec![], pause(1000, 1000)),
                beat("S1.b2", None, vec![], pause(1000, 1000)),
            ],
        )]);
        let a = solve(&input, None);
        assert_eq!(a.zones[0].beats[1].start, s(3));
        assert_eq!(a.total, s(4));
        assert_eq!(a.cost, Cost::default());
        assert_eq!(solve(&input, Some(&a)), a, "同样的输入必须得到同样的输出");
    }

    #[test]
    fn limits_are_exact() {
        assert_eq!(retime_limit(s(5)), s(6));
        assert_eq!(voice_min(ms(3600)), Tick(2_352_000_000));
        assert!(speed_ppm(ms(3600), voice_min(ms(3600))) <= UNIT_SPEED + 80_000);
    }

    #[test]
    fn speed_rounds_to_nearest() {
        assert_eq!(
            speed_ppm(ms(3200), voice_min(ms(3200))),
            UNIT_SPEED + 80_000
        );
        assert_eq!(
            speed_ppm(ms(3600), voice_min(ms(3600))),
            UNIT_SPEED + 80_000
        );
        assert_eq!(speed_ppm(ms(2500), ms(2640)), 946_970);
        assert_eq!(speed_ppm(s(1), s(1)), UNIT_SPEED);
        // 任何旁白在上限处都不超过 +8%
        for n in [1_i64, 7, 1234, 3_333_333, 705_600_001] {
            assert!(speed_ppm(Tick(n * 97), voice_min(Tick(n * 97))) <= UNIT_SPEED + 80_000);
        }
    }

    #[test]
    fn distribute_is_exact_and_ordered() {
        let caps = [Tick(3), Tick(3), Tick(3)];
        assert_eq!(distribute(Tick(10), &caps), caps.to_vec());
        assert_eq!(distribute(Tick(5), &caps), vec![Tick(2), Tick(2), Tick(1)]);
        assert_eq!(distribute(Tick(0), &caps), vec![Tick(0); 3]);
    }

    #[test]
    fn fill_kinds() {
        // 节拍 = 4s 旁白，无停顿
        let input = wide(vec![zone(
            "S1",
            None,
            vec![beat(
                "S1.b1",
                Some(s(4)),
                vec![
                    vis(Tick::ZERO, s(10), VisualKind::Elastic), // 槽位 4s：截取
                    vis(Tick::ZERO, s(4), VisualKind::Elastic),  // 槽位 4s：刚好
                    vis(s(1), ms(2600), VisualKind::Elastic),    // 槽位 3s：慢放
                    vis(s(1), s(2), VisualKind::Elastic),        // 槽位 3s：上限 2.4s，缺 0.6s
                ],
                pause(0, 0),
            )],
        )]);
        let out = solve(&input, None);
        let v = &out.zones[0].beats[0].visuals;
        let kinds: Vec<FillKind> = v.iter().map(|x| x.fill).collect();
        assert_eq!(
            kinds,
            vec![
                FillKind::Cut,
                FillKind::Exact,
                FillKind::Retime,
                FillKind::Short
            ]
        );
        assert_eq!(v[2].rec_len, s(3));
        assert_eq!(v[3].rec_len, ms(2400));
        assert_eq!(v[3].missing, ms(600));
        assert_eq!(out.cost.trim, to_cost(ms(800)));
    }

    #[test]
    fn small_overflow_uses_pauses_only() {
        let input = wide(vec![zone(
            "S1",
            Some(ms(4600)),
            vec![
                beat("S1.b1", Some(s(2)), vec![], pause(400, 200)),
                beat("S1.b2", Some(s(2)), vec![], pause(400, 200)),
            ],
        )]);
        let out = solve(&input, None);
        let z = &out.zones[0];
        assert_eq!(z.dur, ms(4600));
        assert_eq!(z.beats[0].pause, ms(300));
        assert_eq!(z.beats[1].pause, ms(300));
        assert_eq!(z.beats[0].voice.unwrap().len, s(2), "停顿够用时不动语速");
        assert_eq!(out.cost.pause, to_cost(ms(200)));
        assert_eq!(out.cost.rate, 0);
        assert_eq!(out.cost.declared_budget, 0);
    }

    #[test]
    fn large_overflow_then_rate_then_declared_budget() {
        let input = wide(vec![zone(
            "S1",
            Some(s(4)),
            vec![
                beat("S1.b1", Some(s(2)), vec![], pause(400, 200)),
                beat("S1.b2", Some(s(2)), vec![], pause(400, 200)),
            ],
        )]);
        let out = solve(&input, None);
        let z = &out.zones[0];
        for b in &z.beats {
            assert_eq!(b.pause, ms(200));
            let v = b.voice.unwrap();
            assert_eq!(v.len, voice_min(s(2)));
            assert!(v.speed_ppm() <= UNIT_SPEED + 80_000);
        }
        let expected_over = z.dur - s(4);
        assert!(expected_over > Tick::ZERO);
        assert_eq!(z.over, expected_over);
        assert_eq!(out.cost.declared_budget, to_cost(expected_over));
        assert!(!out.cost.needs_escalation(), "偏离声明预算不需要上报");
    }

    #[test]
    fn borrows_from_neighbour_slack() {
        let input = wide(vec![
            zone(
                "S1",
                Some(s(5)),
                vec![beat("S1.b1", Some(s(2)), vec![], pause(400, 400))],
            ),
            zone(
                "S2",
                Some(s(2)),
                vec![beat("S2.b1", Some(s(2)), vec![], pause(400, 400))],
            ),
        ]);
        let out = solve(&input, None);
        let z2 = &out.zones[1];
        let left = z2.dur - s(2);
        assert_eq!(z2.over, Tick::ZERO);
        assert_eq!(z2.borrowed, vec![(SectionId::parse("S1").unwrap(), left)]);
        assert_eq!(out.zones[0].lent.len(), 1);
        assert_eq!(out.cost.cross_zone, 1);
        assert_eq!(out.cost.declared_budget, 0);
    }

    #[test]
    fn rate_untouched_when_fixed_visual_dominates() {
        let input = wide(vec![zone(
            "S1",
            Some(s(5)),
            vec![beat(
                "S1.b1",
                Some(ms(3200)),
                vec![vis(Tick::ZERO, s(6), VisualKind::Fixed(s(6)))],
                pause(400, 200),
            )],
        )]);
        let out = solve(&input, None);
        let b = &out.zones[0].beats[0];
        assert_eq!(
            b.voice.unwrap().len,
            ms(3200),
            "主体由固定画面决定，加快语速没有意义"
        );
        assert_eq!(b.pause, ms(200));
        assert_eq!(out.cost.declared_budget, to_cost(ms(1200)));
    }

    #[test]
    fn intent_over_escalates() {
        let input = SolveInput {
            zones: vec![zone(
                "S1",
                None,
                vec![beat("S1.b1", Some(s(10)), vec![], pause(400, 400))],
            )],
            window: (Tick::ZERO, s(9)),
        };
        let out = solve(&input, None);
        assert!(out.intent_over > Tick::ZERO);
        assert!(out.zones[0].l4);
        assert!(out.cost.needs_escalation());
        let total_min: Tick = min_elements(&input)
            .iter()
            .fold(Tick::ZERO, |a, e| a + e.min);
        assert_eq!(total_min - s(9), out.intent_over);
    }

    #[test]
    fn fixed_longer_than_source_is_hard() {
        let input = wide(vec![zone(
            "S1",
            None,
            vec![beat(
                "S1.b1",
                None,
                vec![vis(Tick::ZERO, s(3), VisualKind::Fixed(s(5)))],
                pause(0, 0),
            )],
        )]);
        let out = solve(&input, None);
        assert_eq!(out.hard.len(), 1);
        assert_eq!(out.cost.hard, 1);
        assert_eq!(out.zones[0].beats[0].visuals[0].missing, s(2));
    }

    #[test]
    fn offsets_scale_with_rate() {
        let input = wide(vec![zone(
            "S1",
            Some(ms(3240)),
            vec![beat(
                "S1.b1",
                Some(ms(3240)),
                vec![vis(ms(1620), s(10), VisualKind::Elastic)],
                pause(240, 0),
            )],
        )]);
        // 超 0.24s：停顿正好吸收，语速不变
        let out = solve(&input, None);
        assert_eq!(out.zones[0].beats[0].visuals[0].offset, ms(1620));
        let tight = SolveInput {
            zones: vec![zone(
                "S1",
                Some(s(3)),
                vec![beat(
                    "S1.b1",
                    Some(ms(3240)),
                    vec![vis(ms(1620), s(10), VisualKind::Elastic)],
                    pause(240, 0),
                )],
            )],
            ..input
        };
        let out = solve(&tight, None);
        let b = &out.zones[0].beats[0];
        assert_eq!(b.voice.unwrap().len, s(3));
        assert_eq!(b.visuals[0].offset, ms(1500), "入画时间随语速等比缩放");
    }
}
