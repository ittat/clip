//! 时间刻度：flicks（1/705_600_000 秒）。
//!
//! 全程整数运算。能整除 24/25/30/50/60fps、23.976/29.97/59.94fps 的帧长，
//! 以及 44.1k/48k 的采样间隔。十进制秒最多 5 位小数可以精确表示
//! （705_600_000 = 2^9 · 3^2 · 5^5 · 7^2）。

use std::fmt;
use std::ops::{Add, Sub};

pub const FLICKS_PER_SECOND: i64 = 705_600_000;
pub const MAX_DECIMAL_PLACES: usize = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tick(pub i64);

impl Tick {
    pub const ZERO: Tick = Tick(0);

    pub const fn flicks(self) -> i64 {
        self.0
    }

    pub fn from_seconds(s: i64) -> Tick {
        Tick(s.checked_mul(FLICKS_PER_SECOND).expect("tick overflow"))
    }
}

impl Add for Tick {
    type Output = Tick;
    fn add(self, rhs: Tick) -> Tick {
        Tick(self.0.checked_add(rhs.0).expect("tick overflow"))
    }
}

impl Sub for Tick {
    type Output = Tick;
    fn sub(self, rhs: Tick) -> Tick {
        Tick(self.0.checked_sub(rhs.0).expect("tick overflow"))
    }
}

impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format_duration(*self))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TimeError {
    #[error("无法解析时长 `{0}`：应写成 `1.5s`、`+2f` 或 `123fl`")]
    Syntax(String),
    #[error("时长 `{0}` 的小数超过 5 位，无法精确表示")]
    TooPrecise(String),
    #[error("时长 `{0}` 用了帧单位，但没有帧率")]
    NeedsFps(String),
    #[error("帧率 `{0}` 无效：应为正整数（如 25）或有理数（如 30000/1001）")]
    BadFps(String),
    #[error("帧率 {num}/{den} 的帧长不是整数个 flick")]
    FpsNotExact { num: u32, den: u32 },
    #[error("采样率 {0} 的采样间隔不是整数个 flick")]
    RateNotExact(u32),
    #[error("百分比 `{0}` 无效：应写成 `10%` 或 `2.5%`")]
    BadPercent(String),
    #[error("时长溢出：`{0}`")]
    Overflow(String),
}

/// 帧率。帧长在构造时校验为整数个 flick。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fps {
    num: u32,
    den: u32,
    frame: Tick,
}

impl Fps {
    pub fn new(num: u32, den: u32) -> Result<Fps, TimeError> {
        if num == 0 || den == 0 {
            return Err(TimeError::BadFps(format!("{num}/{den}")));
        }
        let n = i128::from(FLICKS_PER_SECOND) * i128::from(den);
        let d = i128::from(num);
        if n % d != 0 {
            return Err(TimeError::FpsNotExact { num, den });
        }
        let frame = i64::try_from(n / d).map_err(|_| TimeError::BadFps(format!("{num}/{den}")))?;
        Ok(Fps {
            num,
            den,
            frame: Tick(frame),
        })
    }

    /// `25` 或 `30000/1001`。不接受 `29.97`，避免歧义。
    pub fn parse(s: &str) -> Result<Fps, TimeError> {
        let s = s.trim();
        let bad = || TimeError::BadFps(s.to_string());
        let (n, d) = match s.split_once('/') {
            Some((n, d)) => (
                n.trim().parse::<u32>().map_err(|_| bad())?,
                d.trim().parse::<u32>().map_err(|_| bad())?,
            ),
            None => (s.parse::<u32>().map_err(|_| bad())?, 1),
        };
        Fps::new(n, d)
    }

    pub fn num(&self) -> u32 {
        self.num
    }

    pub fn den(&self) -> u32 {
        self.den
    }

    pub fn frame(&self) -> Tick {
        self.frame
    }

    pub fn frames(&self, n: i64) -> Result<Tick, TimeError> {
        n.checked_mul(self.frame.0)
            .map(Tick)
            .ok_or_else(|| TimeError::Overflow(format!("{n}f")))
    }

    /// 向下取整到帧。
    pub fn floor_frames(&self, t: Tick) -> i64 {
        t.0.div_euclid(self.frame.0)
    }
}

/// 采样率。采样间隔在构造时校验为整数个 flick。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleRate {
    hz: u32,
    sample: Tick,
}

impl SampleRate {
    pub fn new(hz: u32) -> Result<SampleRate, TimeError> {
        if hz == 0 || FLICKS_PER_SECOND % i64::from(hz) != 0 {
            return Err(TimeError::RateNotExact(hz));
        }
        Ok(SampleRate {
            hz,
            sample: Tick(FLICKS_PER_SECOND / i64::from(hz)),
        })
    }

    pub fn hz(&self) -> u32 {
        self.hz
    }

    pub fn sample(&self) -> Tick {
        self.sample
    }
}

/// 比例（如容差 10%）。用整数分子分母表示。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ratio {
    pub num: i64,
    pub den: i64,
}

impl Ratio {
    /// t × ratio，向零取整（结果确定）。
    pub fn of(&self, t: Tick) -> Tick {
        let v = i128::from(t.0) * i128::from(self.num) / i128::from(self.den);
        Tick(i64::try_from(v).expect("tick overflow"))
    }
}

fn parse_uint(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<i64>().ok()
}

/// 十进制数 × unit，要求结果是整数。
fn parse_decimal(n: &str, unit: i64, orig: &str) -> Result<i64, TimeError> {
    let syntax = || TimeError::Syntax(orig.to_string());
    let (int_part, frac_part) = n.split_once('.').unwrap_or((n, ""));
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(syntax());
    }
    if n.contains('.') && frac_part.is_empty() {
        return Err(syntax());
    }
    let int = if int_part.is_empty() {
        0
    } else {
        parse_uint(int_part).ok_or_else(syntax)?
    };
    if frac_part.len() > MAX_DECIMAL_PLACES {
        return Err(TimeError::TooPrecise(orig.to_string()));
    }
    let frac = if frac_part.is_empty() {
        0
    } else {
        parse_uint(frac_part).ok_or_else(syntax)?
    };
    let scale = 10_i64.pow(u32::try_from(frac_part.len()).unwrap_or(u32::MAX));
    if unit % scale != 0 {
        return Err(TimeError::TooPrecise(orig.to_string()));
    }
    int.checked_mul(unit)
        .and_then(|v| v.checked_add(frac * (unit / scale)))
        .ok_or_else(|| TimeError::Overflow(orig.to_string()))
}

/// 解析时长：`1.5s`、`-0.04s`、`+2f`（需要帧率）、`123fl`（flicks 原值）。
pub fn parse_duration(s: &str, fps: Option<&Fps>) -> Result<Tick, TimeError> {
    let raw = s.trim();
    let syntax = || TimeError::Syntax(s.to_string());
    let (neg, body) = if let Some(b) = raw.strip_prefix('+') {
        (false, b)
    } else if let Some(b) = raw.strip_prefix('-') {
        (true, b)
    } else {
        (false, raw)
    };
    let value = if let Some(n) = body.strip_suffix("fl") {
        parse_uint(n).ok_or_else(syntax)?
    } else if let Some(n) = body.strip_suffix('f') {
        let fps = fps.ok_or_else(|| TimeError::NeedsFps(s.to_string()))?;
        let frames = parse_uint(n).ok_or_else(syntax)?;
        fps.frames(frames)?.0
    } else if let Some(n) = body.strip_suffix('s') {
        parse_decimal(n, FLICKS_PER_SECOND, s)?
    } else {
        return Err(syntax());
    };
    Ok(Tick(if neg { -value } else { value }))
}

/// 解析区间：`0.2s..0.8s`。
pub fn parse_range(s: &str, fps: Option<&Fps>) -> Result<(Tick, Tick), TimeError> {
    let (a, b) = s
        .split_once("..")
        .ok_or_else(|| TimeError::Syntax(s.to_string()))?;
    let lo = parse_duration(a, fps)?;
    let hi = parse_duration(b, fps)?;
    if lo > hi {
        return Err(TimeError::Syntax(s.to_string()));
    }
    Ok((lo, hi))
}

/// 解析百分比：`10%`、`2.5%`。
pub fn parse_percent(s: &str) -> Result<Ratio, TimeError> {
    let body = s
        .trim()
        .strip_suffix('%')
        .ok_or_else(|| TimeError::BadPercent(s.to_string()))?;
    let scaled =
        parse_decimal(body, 100_000, s).map_err(|_| TimeError::BadPercent(s.to_string()))?;
    Ok(Ratio {
        num: scaled,
        den: 10_000_000,
    })
}

/// 精确格式化：能用 ≤5 位小数秒表示时输出秒，否则输出 flicks 原值（`123fl`）。
pub fn format_duration(t: Tick) -> String {
    let unit = FLICKS_PER_SECOND / 100_000; // 10 微秒
    let v = t.0;
    if v % unit != 0 {
        return format!("{v}fl");
    }
    let sign = if v < 0 { "-" } else { "" };
    let a = v.unsigned_abs() / unit.unsigned_abs();
    let int = a / 100_000;
    let mut frac = a % 100_000;
    if frac == 0 {
        return format!("{sign}{int}s");
    }
    let mut digits = MAX_DECIMAL_PLACES;
    while frac % 10 == 0 {
        frac /= 10;
        digits -= 1;
    }
    format!("{sign}{int}.{frac:0digits$}s")
}

/// G-12：帧累加、格式化往返都不能漂移。返回第一个失败的说明。
pub fn roundtrip_selftest(iterations: u32) -> Result<(), String> {
    let rates = [
        (24, 1),
        (25, 1),
        (30, 1),
        (50, 1),
        (60, 1),
        (24000, 1001),
        (30000, 1001),
        (60000, 1001),
    ];
    for (n, d) in rates {
        let fps = Fps::new(n, d).map_err(|e| e.to_string())?;
        let mut acc = Tick::ZERO;
        for _ in 0..iterations {
            acc = acc + fps.frame();
        }
        if acc
            != fps
                .frames(i64::from(iterations))
                .map_err(|e| e.to_string())?
        {
            return Err(format!("{n}/{d} fps：累加 {iterations} 帧出现漂移"));
        }
        if fps.frames(i64::from(n)).map_err(|e| e.to_string())? != Tick::from_seconds(i64::from(d))
        {
            return Err(format!("{n}/{d} fps：{n} 帧不等于 {d} 秒"));
        }
    }
    for hz in [44_100, 48_000, 96_000] {
        SampleRate::new(hz).map_err(|e| e.to_string())?;
    }
    let step = Fps::new(30000, 1001).map_err(|e| e.to_string())?.frame();
    let mut t = Tick::ZERO;
    for i in 0..iterations {
        let s = format_duration(t);
        let back = parse_duration(&s, None).map_err(|e| e.to_string())?;
        if back != t {
            return Err(format!("第 {i} 次往返：{} → `{s}` → {}", t.0, back.0));
        }
        if format_duration(back) != s {
            return Err(format!("第 {i} 次往返：再次格式化结果不同"));
        }
        // 混入不是整 10 微秒的刻度，覆盖 `fl` 分支
        t = t + step + Tick(i64::from(i));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_lengths_are_exact() {
        assert_eq!(Fps::new(25, 1).unwrap().frame(), Tick(28_224_000));
        assert_eq!(Fps::new(24000, 1001).unwrap().frame(), Tick(29_429_400));
        assert_eq!(Fps::new(30000, 1001).unwrap().frame(), Tick(23_543_520));
        assert_eq!(Fps::new(60000, 1001).unwrap().frame(), Tick(11_771_760));
        assert_eq!(SampleRate::new(48_000).unwrap().sample(), Tick(14_700));
        assert_eq!(SampleRate::new(44_100).unwrap().sample(), Tick(16_000));
    }

    #[test]
    fn parse_examples() {
        let fps = Fps::new(25, 1).unwrap();
        assert_eq!(parse_duration("0.4s", None).unwrap(), Tick(282_240_000));
        assert_eq!(parse_duration(".5s", None).unwrap(), Tick(352_800_000));
        assert_eq!(parse_duration("12s", None).unwrap(), Tick::from_seconds(12));
        assert_eq!(parse_duration("+2f", Some(&fps)).unwrap(), Tick(56_448_000));
        assert_eq!(
            parse_duration("-2f", Some(&fps)).unwrap(),
            Tick(-56_448_000)
        );
        assert_eq!(parse_duration("123fl", None).unwrap(), Tick(123));
        assert!(matches!(
            parse_duration("0.123456s", None),
            Err(TimeError::TooPrecise(_))
        ));
        assert!(matches!(
            parse_duration("2f", None),
            Err(TimeError::NeedsFps(_))
        ));
        assert!(matches!(
            parse_duration("1.s", None),
            Err(TimeError::Syntax(_))
        ));
        assert!(matches!(
            parse_duration("1.5", None),
            Err(TimeError::Syntax(_))
        ));
    }

    #[test]
    fn range_and_percent() {
        let (lo, hi) = parse_range("0.2s..0.8s", None).unwrap();
        assert_eq!((lo, hi), (Tick(141_120_000), Tick(564_480_000)));
        let r = parse_percent("10%").unwrap();
        assert_eq!(r.of(Tick::from_seconds(12)), Tick(846_720_000));
    }

    #[test]
    fn format_examples() {
        assert_eq!(format_duration(Tick::from_seconds(3)), "3s");
        assert_eq!(format_duration(Tick(282_240_000)), "0.4s");
        assert_eq!(format_duration(Tick(-56_448_000)), "-0.08s");
        assert_eq!(format_duration(Tick(123)), "123fl");
    }

    #[test]
    fn roundtrip() {
        roundtrip_selftest(1000).unwrap();
    }
}
