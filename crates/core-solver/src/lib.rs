//! 弹性求解器。纯函数：不读文件、不看时钟、不用随机数。
//!
//! PR 2：节拍内求解。
//! - 节拍主体 = 最长的硬元素（旁白、rigid/fixed 画面）；没有硬元素时取画面的自然长度
//! - 停顿取自然长度；在 `flex` 内压缩或拉伸属于 PR 3 的超时消化
//! - 弹性画面填满到节拍结束：素材够长就截取，不够就慢放（最多 20%），仍不够就报缺口
//! - 代价：慢放多出来的时长计入第 8 项（trim），单位 flicks

use core_model::{BeatId, Cost, Tick};

pub const SOLVER_VERSION: &str = "beat-1";

/// 慢放上限：成片时长最多比素材长 20%。
pub const MAX_RETIME_PERCENT: i64 = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlueKind {
    Rigid,
    Elastic,
    Pause,
}

/// 盒子 + 胶水：自然长度、可伸、可缩。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glue {
    pub natural: Tick,
    pub stretch: Tick,
    pub shrink: Tick,
    pub kind: GlueKind,
}

/// 节拍里的一条画面。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualInput {
    /// 相对节拍开始的入画时间（由 `from` 锚点换算）
    pub offset: Tick,
    /// 素材可用长度
    pub have: Tick,
    /// rigid / fixed：按素材原长播放，并参与决定节拍长度
    pub rigid: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeatInput {
    pub id: BeatId,
    pub hard: Vec<Glue>,
    pub visuals: Vec<VisualInput>,
    pub pause: Glue,
}

/// 求解输入。`beats` 按成片顺序排列。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolveInput {
    pub beats: Vec<BeatInput>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FillKind {
    /// 素材长度刚好
    Exact,
    /// 素材够长，截取前段
    Trim,
    /// 慢放补足（不超过上限）
    Retime,
    /// 慢放到上限仍不够
    Short,
}

impl FillKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FillKind::Exact => "exact",
            FillKind::Trim => "trim",
            FillKind::Retime => "retime",
            FillKind::Short => "short",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualPlacement {
    /// 在成片里占的时长
    pub rec_len: Tick,
    /// 用掉的素材时长
    pub src_len: Tick,
    pub fill: FillKind,
    /// 慢放到上限后仍缺的时长
    pub missing: Tick,
}

impl VisualPlacement {
    /// 慢放多出来的时长（计入代价第 8 项）。
    pub fn retime_extra(&self) -> Tick {
        match self.fill {
            FillKind::Retime | FillKind::Short => self.rec_len - self.src_len,
            FillKind::Exact | FillKind::Trim => Tick::ZERO,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeatPlacement {
    pub id: BeatId,
    pub start: Tick,
    pub dur: Tick,
    /// 节拍主体（不含停顿）
    pub hard: Tick,
    pub pause: Tick,
    /// 与 `BeatInput::visuals` 一一对应
    pub visuals: Vec<VisualPlacement>,
    pub cost: Cost,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolveOutput {
    pub version: &'static str,
    pub beats: Vec<BeatPlacement>,
    pub total: Tick,
    pub cost: Cost,
}

/// 慢放上限下，素材最多能撑多长。
pub fn retime_limit(have: Tick) -> Tick {
    let extra = i128::from(have.flicks()) * i128::from(MAX_RETIME_PERCENT) / 100;
    have + Tick(i64::try_from(extra).expect("tick overflow"))
}

fn fill(v: &VisualInput, dur: Tick) -> VisualPlacement {
    let slot = if dur > v.offset {
        dur - v.offset
    } else {
        Tick::ZERO
    };
    if v.rigid {
        let len = v.have.min(slot);
        return VisualPlacement {
            rec_len: len,
            src_len: len,
            fill: FillKind::Exact,
            missing: Tick::ZERO,
        };
    }
    if v.have >= slot {
        let fill = if v.have == slot {
            FillKind::Exact
        } else {
            FillKind::Trim
        };
        return VisualPlacement {
            rec_len: slot,
            src_len: slot,
            fill,
            missing: Tick::ZERO,
        };
    }
    let limit = retime_limit(v.have);
    if slot <= limit {
        VisualPlacement {
            rec_len: slot,
            src_len: v.have,
            fill: FillKind::Retime,
            missing: Tick::ZERO,
        }
    } else {
        VisualPlacement {
            rec_len: limit,
            src_len: v.have,
            fill: FillKind::Short,
            missing: slot - limit,
        }
    }
}

/// 节拍主体长度：硬元素里最长的；没有硬元素时取画面的自然长度。
fn body(b: &BeatInput) -> Tick {
    let hard = b
        .hard
        .iter()
        .map(|g| g.natural)
        .chain(
            b.visuals
                .iter()
                .filter(|v| v.rigid)
                .map(|v| v.offset + v.have),
        )
        .max();
    hard.or_else(|| b.visuals.iter().map(|v| v.offset + v.have).max())
        .unwrap_or(Tick::ZERO)
}

fn to_cost(t: Tick) -> u64 {
    u64::try_from(t.flicks()).unwrap_or(0)
}

/// 惯性所需的“上次结果”通过 `prev` 显式传入（PR 2 尚不使用）。
pub fn solve(input: &SolveInput, _prev: Option<&SolveOutput>) -> SolveOutput {
    let mut cursor = Tick::ZERO;
    let mut total = Cost::default();
    let mut beats = Vec::with_capacity(input.beats.len());
    for b in &input.beats {
        let hard = body(b);
        let pause = b.pause.natural;
        let dur = hard + pause;
        let visuals: Vec<VisualPlacement> = b.visuals.iter().map(|v| fill(v, dur)).collect();
        let trim = visuals
            .iter()
            .fold(Tick::ZERO, |acc, v| acc + v.retime_extra());
        let cost = Cost {
            trim: to_cost(trim),
            ..Cost::default()
        };
        total = total + cost;
        beats.push(BeatPlacement {
            id: b.id.clone(),
            start: cursor,
            dur,
            hard,
            pause,
            visuals,
            cost,
        });
        cursor = cursor + dur;
    }
    SolveOutput {
        version: SOLVER_VERSION,
        beats,
        total: cursor,
        cost: total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_model::FLICKS_PER_SECOND;

    fn s(n: i64) -> Tick {
        Tick::from_seconds(n)
    }

    fn ms(n: i64) -> Tick {
        Tick(n * (FLICKS_PER_SECOND / 1000))
    }

    fn glue(t: Tick, kind: GlueKind) -> Glue {
        Glue {
            natural: t,
            stretch: Tick::ZERO,
            shrink: Tick::ZERO,
            kind,
        }
    }

    fn vis(offset: Tick, have: Tick) -> VisualInput {
        VisualInput {
            offset,
            have,
            rigid: false,
        }
    }

    fn beat(id: &str, hard: Vec<Glue>, visuals: Vec<VisualInput>, pause: Tick) -> BeatInput {
        BeatInput {
            id: BeatId::parse(id).unwrap(),
            hard,
            visuals,
            pause: glue(pause, GlueKind::Pause),
        }
    }

    #[test]
    fn sequential_and_pure() {
        let input = SolveInput {
            beats: vec![
                beat("S1.b1", vec![glue(s(2), GlueKind::Rigid)], vec![], s(1)),
                beat("S1.b2", vec![], vec![], s(1)),
            ],
        };
        let a = solve(&input, None);
        assert_eq!(a.beats[1].start, s(3));
        assert_eq!(a.total, s(4));
        assert_eq!(solve(&input, Some(&a)), a, "同样的输入必须得到同样的输出");
    }

    #[test]
    fn retime_limit_is_20_percent() {
        assert_eq!(retime_limit(s(5)), s(6));
    }

    #[test]
    fn trim_exact_retime_short() {
        // 节拍 = 4s 旁白，无停顿
        let input = SolveInput {
            beats: vec![beat(
                "S1.b1",
                vec![glue(s(4), GlueKind::Rigid)],
                vec![
                    vis(Tick::ZERO, s(10)), // 槽位 4s：截取
                    vis(Tick::ZERO, s(4)),  // 槽位 4s：刚好
                    vis(s(1), ms(2600)),    // 槽位 3s：慢放 2.6s → 3s
                    vis(s(1), s(2)),        // 槽位 3s：上限 2.4s，缺 0.6s
                ],
                Tick::ZERO,
            )],
        };
        let out = solve(&input, None);
        let v = &out.beats[0].visuals;
        let kinds: Vec<FillKind> = v.iter().map(|x| x.fill).collect();
        assert_eq!(
            kinds,
            vec![
                FillKind::Trim,
                FillKind::Exact,
                FillKind::Retime,
                FillKind::Short
            ]
        );
        assert_eq!(v[2].rec_len, s(3));
        assert_eq!(v[3].rec_len, ms(2400));
        assert_eq!(v[3].missing, ms(600));
        assert_eq!(out.cost.trim, to_cost(ms(800)));
        assert_eq!(out.beats[0].cost, out.cost);
    }

    #[test]
    fn rigid_visual_sets_length_without_voice() {
        let input = SolveInput {
            beats: vec![beat(
                "S1.b1",
                vec![],
                vec![
                    VisualInput {
                        offset: Tick::ZERO,
                        have: s(3),
                        rigid: true,
                    },
                    vis(Tick::ZERO, s(10)),
                ],
                ms(500),
            )],
        };
        let out = solve(&input, None);
        assert_eq!(out.beats[0].dur, ms(3500));
        assert_eq!(out.beats[0].visuals[0].rec_len, s(3));
        assert_eq!(out.beats[0].visuals[1].fill, FillKind::Trim);
        assert_eq!(out.beats[0].visuals[1].rec_len, ms(3500));
    }
}
