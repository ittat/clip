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

## 当前进度：PR 4 段落预算与偏短拉伸（`solver: zone-3`）

- **区内**：段落超出声明预算时，按代价从低到高消化：停顿（9）→ 纯画面节拍的主体（8）→ 语速 ±8%（7）
- **全片**：超出 L4 上限时，在所有区剩余的弹性里继续消化；仍超出记为第 3 项，`status: conflict`，退出码 1
- **借用**：仍超预算的区向相邻区未用完的声明预算借时间（6）；借不到的部分记为偏离声明预算（5），可接受
- **冲突**：给出最小冲突集和可选方案（删句 < 放宽 L4 < 删开场/结尾的句子 < 解锁人工固定画面），含 must 词的句子不会被提议删除。开场/结尾（`role: opening / closing`）的句子仍会列出，标 `protected_role`，但不会被推荐
- **速度**：`speed_ppm` 四舍五入，语速上限显示为整 1,080,000（+8.0%）
- **偏短拉伸**：全片短于 L4 下限时，停顿拉到 flex 上限（9）→ 纯画面节拍靠慢放变长（8）；**不放慢语速**。先在声明预算以内拉，再拉其余部分（产生第 5 项）；仍不够记为第 3 项，冲突选项为“加内容 / 放宽 L4”
- **语速**：时间线上的旁白用 `speed_ppm` 预览；报告列出 `tts_resynth_needed`，真正重新合成在阶段 1
- 画面填充方式：`exact` / `cut`（截取）/ `retime`（慢放 ≤20%）/ `short` / `fixed`；`fit: fixed` 需写 `len`
- 报告里节拍用 `offset`（相对所在段落），段落整体平移时节拍报告不变；G-04 据此统计局部性

## 已知限制（都会出现在 `solve_report.json` 的 `not_applied` 里）

- 音乐、补丁、规则还没有执行
- 规划模式（还没有内容时按 `role` 权重分配预算）推迟到阶段 1
- `visual.cut_points`：截取画面时直接取前段，还没有对齐可剪点
