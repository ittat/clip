//! 弹性求解器。纯函数：不读文件、不看时钟、不用随机数。
//!
//! PR 1 是朴素实现：节拍按自然长度顺排，不伸缩、不计代价。
//! 后续 PR 在同一个签名下替换实现，黄金用例保证行为可见。

use core_model::{BeatId, Cost, Tick};

pub const SOLVER_VERSION: &str = "naive-0";

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeatInput {
    pub id: BeatId,
    pub hard: Vec<Glue>,
    pub pause: Glue,
}

/// 求解输入。`beats` 按成片顺序排列。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolveInput {
    pub beats: Vec<BeatInput>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeatPlacement {
    pub id: BeatId,
    pub start: Tick,
    pub dur: Tick,
    pub hard: Tick,
    pub pause: Tick,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SolveOutput {
    pub version: &'static str,
    pub beats: Vec<BeatPlacement>,
    pub total: Tick,
    pub cost: Cost,
}

/// 惯性所需的“上次结果”通过 `prev` 显式传入（PR 1 不使用）。
pub fn solve(input: &SolveInput, _prev: Option<&SolveOutput>) -> SolveOutput {
    let mut cursor = Tick::ZERO;
    let mut beats = Vec::with_capacity(input.beats.len());
    for b in &input.beats {
        let hard = b.hard.iter().map(|g| g.natural).max().unwrap_or(Tick::ZERO);
        let pause = b.pause.natural;
        let dur = hard + pause;
        beats.push(BeatPlacement {
            id: b.id.clone(),
            start: cursor,
            dur,
            hard,
            pause,
        });
        cursor = cursor + dur;
    }
    SolveOutput {
        version: SOLVER_VERSION,
        beats,
        total: cursor,
        cost: Cost::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glue(s: i64, kind: GlueKind) -> Glue {
        Glue {
            natural: Tick::from_seconds(s),
            stretch: Tick::ZERO,
            shrink: Tick::ZERO,
            kind,
        }
    }

    #[test]
    fn naive_is_sequential_and_pure() {
        let input = SolveInput {
            beats: vec![
                BeatInput {
                    id: BeatId::parse("S1.b1").unwrap(),
                    hard: vec![glue(2, GlueKind::Rigid)],
                    pause: glue(1, GlueKind::Pause),
                },
                BeatInput {
                    id: BeatId::parse("S1.b2").unwrap(),
                    hard: vec![],
                    pause: glue(1, GlueKind::Pause),
                },
            ],
        };
        let a = solve(&input, None);
        assert_eq!(a.beats[1].start, Tick::from_seconds(3));
        assert_eq!(a.total, Tick::from_seconds(4));
        assert_eq!(solve(&input, Some(&a)), a, "同样的输入必须得到同样的输出");
    }
}
