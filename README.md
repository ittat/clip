# clip · 分层剪辑器内核（阶段 0 · PR 1）

PR 1 只做骨架：**先立起测试框架和确定性检查，求解器是朴素实现**（节拍按自然长度顺排，不伸缩、不计代价）。
后续 PR 在同样的接口下替换真实逻辑，黄金用例保证每一步的行为变化都看得见。

## 结构

```
crates/
├ core-model     Tick（flicks）、语义 ID、校验后的工程模型、Cost（纯数据）
├ core-store     读取源文件、引用校验（错误带位置和修复建议）、规范化 JSON
├ core-solver    求解器（纯函数）；PR 1 为朴素实现
├ core-compile   L2 → L1：只读源文件，只产出 build/
└ clip           CLI：compile、golden run、golden bless
golden/
├ fixtures/min_explainer/   最小样例工程
└ cases/                    G-01 G-02 G-03 G-12
```

依赖方向：`model ← solver ← compile ← clip`；`store` 只被 `compile` 和 `clip` 使用。

## 首次运行

```
cargo generate-lockfile                       # 生成并提交 Cargo.lock（CI 使用 --locked）
cargo test --workspace
cargo run -p clip -- compile golden/fixtures/min_explainer
cargo run -p clip -- golden bless --case G-01 # 生成基线，逐行审阅后提交
cargo run -p clip -- golden run
```

## 确定性由编译期规则保证

- workspace 级 clippy 规则：禁止浮点运算、禁止 `HashMap`/`HashSet`、禁止读时钟（`clippy.toml`）
- 时长字符串按十进制精确解析成 flicks，不经过浮点
- `build/` 输出是规范化 JSON：键排序、2 空格缩进、UTF-8、末尾换行、只有整数
- 编译 API 的输入是内存里的 `SourceSet`，不读文件系统、不读 `build/`
- CI 在 Linux、macOS、Windows 上跑同一套黄金用例；`.gitattributes` 统一 LF

## 当前进度：PR 2 节拍求解（`solver: beat-1`）

- 节拍主体 = 最长的硬元素；弹性画面填满到节拍结束：截取 / 慢放（≤20%）/ 报缺口
- 慢放多出的时长计入代价第 8 项（trim，单位 flicks）；每个节拍在报告里列出非零代价和原因
- 时间线片段带 `speed_ppm`（1,000,000 = 原速）
- 全局检查：同一素材区间被重复使用时报 `source_reuse`

## 已知限制（都会出现在 `solve_report.json` 的 `not_applied` 里）

- 音乐、补丁、规则、`must` 还没有执行
- `pause.flex`：停顿取自然长度，压缩/拉伸在 PR 3 的超时消化里做
- `visual.cut_points`：截取画面时直接取前段，还没有对齐可剪点
