# clip · 分层剪辑器内核（阶段 0 · PR 1）

PR 1 只做骨架：**先立起测试框架和确定性检查，求解器是朴素实现**（节拍按自然长度顺排，不伸缩、不计代价）。
后续 PR 在同样的接口下替换真实逻辑，黄金用例保证每一步的行为变化都看得见。

## 文档

- [设计文档 v0.2](docs/design.md)：架构、数据模型、求解规则、阶段 0 的全部决定
- [设计摘要](docs/summary.md)：2 页总览
- [上岗考试题库 v0.7](docs/exam.md)：阶段 1 的 AI 考试

**规则：改代码的行为时，同一个 PR 里同步改 `docs/design.md`。**

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

## 阶段 0 完成（`solver: zone-6`）

- **吸附**（D-7）：音乐覆盖范围内的段落切点吸附到强拍、片尾对齐小节线，靠调前一个节拍的停顿（±0.3s，受 flex、预算、L4 限制）；报告 `snaps`
- **可剪点**（D-8）：镜头的 `cut_points`；截取画面时平移素材窗口，出点对齐可剪点，时间线不变；报告 `slips`
- **标记**（D-9）：`media/index.yaml` 的 `marks`，`mk:` 锚点可用
- `clip init` 生成能直接编译的空工程

### CLI
| 命令 | 作用 |
|---|---|
| `clip init <dir>` | 生成能直接编译的空工程骨架 |
| `clip compile <dir>` | 编译：只读源文件，只写 `build/` |
| `clip validate <dir>` | 只加载和校验工程，不编译；错误带位置和修复建议 |
| `clip explain <dir> [--ref S2]` | 把求解报告整理成人能读的说明 |
| `clip diff <a> <b>` | 比较两个工程的编译结果（按区、代价项、片段、冲突） |
| `clip determinism <dir>` | 对任意工程跑幂等、顺序无关、局部性检查 |
| `clip golden run / bless` | 黄金用例 |
| `clip freeze / unfreeze <dir> <段落>` | 冻结 / 解冻段落 |
| `clip lock accept / check <dir>` | 求解器版本锁 |

所有命令都支持 `--json`（`explain` 除外），退出码：0 成功 / 1 冲突、失败或有差异 / 2 格式错误 / 3 内部错误。

区哈希包含补丁和规则之后的最终片段：补丁或规则的逻辑变了，锁也能察觉。

### 第 2 轮 规则、冻结、锁

- **规则**（`rules.yaml`）：阶段 0 支持 `do: {cut_on: beat.strong}`，范围可写 `section` / `beat` / `role`，具体优先于一般。补丁之后执行：画面入点吸附到最近的强拍（4/4 的第 1、3 拍），距离不超过半拍，只向后裁头且不能露出空白；**不碰挂了剪切补丁的片段和冻结段**。补丁的备选锚点分开时，唯一覆盖它的规则可以在已记录的几种解释里选一个
- **冻结**：`clip freeze <dir> S1` 把 S1 当前的片段存成 `frozen/S1.json`；编译时 S1 是固定长度的硬盒子，原样输出、整体平移；补丁和规则都不碰它。冲突方案里可能出现“请人解冻”。`clip unfreeze` 删除它。音乐不随段落冻结
- **锁**：`clip lock accept <dir>` 记录求解器版本和每个区的结果哈希（`locks/solver.lock.json`）；版本变了时报告给出 `solver_changed` 和变化的区。编译从不更新锁文件。`clip lock check` 检查一致性

### 第 1 轮 锚点与补丁

- **音乐**：从第一个用到它的节拍开始，从素材的 `first_bar` 起播；`music.start` 可以延后进入；`fit: bars` 时播放长度向下取整到整小节
- **锚点**：词、事件、素材偏移、拍子都换算成时间线位置（标记 `mk:` 还不支持）
- **补丁**：求解之后应用，不反过来影响求解。`cut: in / out` 裁头/裁尾，画面帧位置不变；其他属性原样传给渲染器
- **备选锚点**：主锚点和 `alt` 相差不到半帧视为重合；分开时报 `anchor_diverged`，按主锚点保留
- **拉伸**：停顿拉长不会让弹性画面撑不满

### PR 4 段落预算与偏短拉伸

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

- 冻结段里的音乐不随段落保存（D-10，按设计）
- 规划模式（还没有内容时按 `role` 权重分配预算）推迟到阶段 1
- `visual.cut_points`：截取画面时直接取前段，还没有对齐可剪点
