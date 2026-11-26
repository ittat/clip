//! 语义 ID：带类型，不能混用。每种 ID 只接受自己的格式。
//!
//! 引用一律写完整前缀：`shot:A.1`，不写 `A.1`。

use std::fmt;

use crate::tick::{parse_duration, Fps, Tick, TimeError};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("`{id}` 不是合法的{kind}，应形如 `{example}`")]
    Format {
        id: String,
        kind: &'static str,
        example: &'static str,
    },
    #[error("锚点 `{0}` 无法解析：{1}")]
    Anchor(String, &'static str),
    #[error(transparent)]
    Time(#[from] TimeError),
}

fn is_num(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn is_word(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn section_body(s: &str) -> bool {
    s.strip_prefix('S').is_some_and(is_num)
}

fn shot_body(s: &str) -> bool {
    s.split_once('.')
        .is_some_and(|(a, b)| is_word(a) && is_num(b))
}

macro_rules! semantic_id {
    ($name:ident, $kind:literal, $example:literal, $check:expr) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            pub fn parse(s: &str) -> Result<Self, IdError> {
                let check: fn(&str) -> bool = $check;
                if check(s) {
                    Ok(Self(s.to_string()))
                } else {
                    Err(IdError::Format {
                        id: s.to_string(),
                        kind: $kind,
                        example: $example,
                    })
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

semantic_id!(SectionId, "段落 ID", "S2", section_body);
semantic_id!(BeatId, "节拍 ID", "S2.b1", |s| s
    .split_once(".b")
    .is_some_and(|(a, b)| section_body(a) && is_num(b)));
semantic_id!(NarrationId, "旁白句 ID", "n:S2.1", |s| s
    .strip_prefix("n:")
    .and_then(|r| r.split_once('.'))
    .is_some_and(|(a, b)| section_body(a) && is_num(b)));
semantic_id!(ShotId, "镜头 ID", "shot:B.12", |s| s
    .strip_prefix("shot:")
    .is_some_and(shot_body));
semantic_id!(EventId, "事件 ID", "ev:B.12.3", |s| s
    .strip_prefix("ev:")
    .and_then(|r| r.rsplit_once('.'))
    .is_some_and(|(a, b)| shot_body(a) && is_num(b)));
semantic_id!(MarkId, "标记 ID", "mk:B.12.goal", |s| s
    .strip_prefix("mk:")
    .and_then(|r| r.rsplit_once('.'))
    .is_some_and(|(a, b)| shot_body(a) && is_word(b)));
semantic_id!(MusicId, "音乐 ID", "M1", |s| s
    .strip_prefix('M')
    .is_some_and(is_num));
semantic_id!(OverrideId, "补丁 ID", "ov7", |s| s
    .strip_prefix("ov")
    .is_some_and(is_num));
semantic_id!(RuleId, "规则 ID", "r:5", |s| s
    .strip_prefix("r:")
    .is_some_and(is_num));

impl EventId {
    /// 事件所属的镜头（事件 ID 的前半部分）。
    pub fn shot(&self) -> ShotId {
        let body = &self.0["ev:".len()..];
        let (shot, _) = body.rsplit_once('.').expect("已校验");
        ShotId(format!("shot:{shot}"))
    }
}

impl MarkId {
    /// 标记所属的镜头（标记 ID 的前半部分）。
    pub fn shot(&self) -> ShotId {
        let body = &self.0["mk:".len()..];
        let (shot, _) = body.rsplit_once('.').expect("已校验");
        ShotId(format!("shot:{shot}"))
    }
}

/// 旁白里的一个词：`n:S2.1.w5`（词序号从 1 开始）。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WordRef {
    pub narration: NarrationId,
    pub index: u32,
}

impl WordRef {
    pub fn parse(s: &str) -> Result<WordRef, IdError> {
        let err = || IdError::Format {
            id: s.to_string(),
            kind: "词引用",
            example: "n:S2.1.w5",
        };
        let (n, w) = s.rsplit_once(".w").ok_or_else(err)?;
        let narration = NarrationId::parse(n).map_err(|_| err())?;
        if !is_num(w) {
            return Err(err());
        }
        let index: u32 = w.parse().map_err(|_| err())?;
        if index == 0 {
            return Err(err());
        }
        Ok(WordRef { narration, index })
    }
}

impl fmt::Display for WordRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.w{}", self.narration, self.index)
    }
}

/// 同一句旁白里的连续词：`n:S2.1.w1..w4`，也可以只写一个词。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WordRange {
    pub narration: NarrationId,
    pub first: u32,
    pub last: u32,
}

impl WordRange {
    pub fn parse(s: &str) -> Result<WordRange, IdError> {
        let err = || IdError::Format {
            id: s.to_string(),
            kind: "词区间",
            example: "n:S2.1.w1..w4",
        };
        let (a, last) = match s.split_once("..") {
            Some((a, b)) => {
                let last = b
                    .strip_prefix('w')
                    .filter(|x| is_num(x))
                    .and_then(|x| x.parse::<u32>().ok())
                    .ok_or_else(err)?;
                (a, Some(last))
            }
            None => (s, None),
        };
        let start = WordRef::parse(a).map_err(|_| err())?;
        let last = last.unwrap_or(start.index);
        if last < start.index {
            return Err(err());
        }
        Ok(WordRange {
            narration: start.narration,
            first: start.index,
            last,
        })
    }

    pub fn start(&self) -> WordRef {
        WordRef {
            narration: self.narration.clone(),
            index: self.first,
        }
    }

    pub fn end(&self) -> WordRef {
        WordRef {
            narration: self.narration.clone(),
            index: self.last,
        }
    }
}

/// 音乐刻度：`bar:M1.17.b3`（第 17 小节第 3 拍，均从 1 开始）。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BarRef {
    pub music: MusicId,
    pub bar: u32,
    pub beat: u32,
}

impl BarRef {
    pub fn parse(s: &str) -> Result<BarRef, IdError> {
        let err = || IdError::Format {
            id: s.to_string(),
            kind: "小节刻度",
            example: "bar:M1.17.b3",
        };
        let body = s.strip_prefix("bar:").ok_or_else(err)?;
        let mut it = body.split('.');
        let (Some(m), Some(bar), Some(beat), None) = (it.next(), it.next(), it.next(), it.next())
        else {
            return Err(err());
        };
        let music = MusicId::parse(m).map_err(|_| err())?;
        if !is_num(bar) {
            return Err(err());
        }
        let bar: u32 = bar.parse().map_err(|_| err())?;
        let beat: u32 = beat
            .strip_prefix('b')
            .filter(|x| is_num(x))
            .and_then(|x| x.parse().ok())
            .ok_or_else(err)?;
        if bar == 0 || beat == 0 {
            return Err(err());
        }
        Ok(BarRef { music, bar, beat })
    }
}

/// 锚点：一律是素材坐标，不是时间线时间码。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Anchor {
    Word(WordRef),
    Event(EventId),
    Bar(BarRef),
    Mark(MarkId),
    /// 兜底：素材内相对时间 `shot:B.12@+2.40s`
    Offset(ShotId, Tick),
}

impl Anchor {
    pub fn parse(s: &str, fps: &Fps) -> Result<Anchor, IdError> {
        if let Some((shot, off)) = s.split_once('@') {
            return Ok(Anchor::Offset(
                ShotId::parse(shot)?,
                parse_duration(off, Some(fps))?,
            ));
        }
        if s.starts_with("n:") {
            return Ok(Anchor::Word(WordRef::parse(s)?));
        }
        if s.starts_with("ev:") {
            return Ok(Anchor::Event(EventId::parse(s)?));
        }
        if s.starts_with("bar:") {
            return Ok(Anchor::Bar(BarRef::parse(s)?));
        }
        if s.starts_with("mk:") {
            return Ok(Anchor::Mark(MarkId::parse(s)?));
        }
        Err(IdError::Anchor(
            s.to_string(),
            "应以 n: / ev: / bar: / mk: 开头，或写成 shot:X.N@偏移",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_reject_each_other() {
        assert!(NarrationId::parse("n:S2.1").is_ok());
        assert!(NarrationId::parse("n:S2.1.w5").is_err());
        assert!(BeatId::parse("S2.b1").is_ok());
        assert!(BeatId::parse("S2").is_err());
        assert!(SectionId::parse("S2.b1").is_err());
        assert!(ShotId::parse("B.12").is_err(), "引用必须写完整前缀");
        assert_eq!(
            EventId::parse("ev:B.12.3").unwrap().shot().as_str(),
            "shot:B.12"
        );
    }

    #[test]
    fn words_and_bars() {
        let w = WordRef::parse("n:S2.1.w5").unwrap();
        assert_eq!((w.narration.as_str(), w.index), ("n:S2.1", 5));
        let r = WordRange::parse("n:S2.1.w1..w4").unwrap();
        assert_eq!((r.first, r.last), (1, 4));
        assert!(WordRange::parse("n:S2.1.w4..w1").is_err());
        let b = BarRef::parse("bar:M1.17.b3").unwrap();
        assert_eq!((b.bar, b.beat), (17, 3));
        assert!(BarRef::parse("bar:M1.0.b1").is_err());
    }

    #[test]
    fn anchors() {
        let fps = Fps::new(25, 1).unwrap();
        assert!(matches!(
            Anchor::parse("ev:B.2.1", &fps),
            Ok(Anchor::Event(_))
        ));
        assert!(matches!(
            Anchor::parse("shot:B.2@+2f", &fps),
            Ok(Anchor::Offset(_, Tick(56_448_000)))
        ));
        assert!(
            Anchor::parse("12.4s", &fps).is_err(),
            "时间线时间码不能做锚点"
        );
    }
}
