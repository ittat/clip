//! 代价：字段顺序就是设计文档 §7.3 的字典序。
//!
//! `derive(Ord)` 按字段声明顺序逐个比较，所以**调整字段顺序就是修改代价公式**，必须经过 review。

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cost {
    /// 1 违反硬约束数（must / forbid / 平台上限）
    pub hard: u32,
    /// 2 人工锁定偏离（含人设定或批准的规则）
    pub human_lock: u64,
    /// 3 L4 意图偏离
    pub intent: u64,
    /// 4 改意思的程度
    pub meaning: u64,
    // ───── 以上只能上报；以下编译器可自动处理 ─────
    /// 5 偏离已声明的段落预算
    pub declared_budget: u64,
    /// 6 跨区影响（被波及的区数）
    pub cross_zone: u32,
    /// 7 语速偏离
    pub rate: u64,
    /// 8 画面裁剪 / 变速量
    pub trim: u64,
    /// 9 停顿偏离
    pub pause: u64,
    /// 10 与上次结果的差异（只在平局时起作用）
    pub diff_from_prev: u64,
}

impl Cost {
    /// 按字典序列出各项。规范化 JSON 的对象会按键排序，所以报告里用数组保序。
    pub fn terms(&self) -> [(&'static str, u64); 10] {
        [
            ("hard", u64::from(self.hard)),
            ("human_lock", self.human_lock),
            ("intent", self.intent),
            ("meaning", self.meaning),
            ("declared_budget", self.declared_budget),
            ("cross_zone", u64::from(self.cross_zone)),
            ("rate", self.rate),
            ("trim", self.trim),
            ("pause", self.pause),
            ("diff_from_prev", self.diff_from_prev),
        ]
    }

    /// 第 1–4 项不为零，意味着必须上报。
    pub fn needs_escalation(&self) -> bool {
        self.hard != 0 || self.human_lock != 0 || self.intent != 0 || self.meaning != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexicographic_not_weighted() {
        let pause = Cost {
            pause: 1_000_000,
            ..Cost::default()
        };
        let trim = Cost {
            trim: 1,
            ..Cost::default()
        };
        assert!(pause < trim, "停顿偏离再大，也小于任何裁剪");
        let budget = Cost {
            declared_budget: u64::MAX,
            ..Cost::default()
        };
        let meaning = Cost {
            meaning: 1,
            ..Cost::default()
        };
        assert!(budget < meaning, "第 5 项永远小于第 4 项");
    }
}
