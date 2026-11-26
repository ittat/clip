# 黄金用例

- `fixtures/<名字>/`：样例工程（只放源文件，`build/` 不提交）
- `cases/<ID>/case.yaml`：用例定义；`compile` 类用例另有 `expected/`

| kind | 做什么 |
|---|---|
| `compile` | 编译样例工程，与 `expected/` 逐字节比对 |
| `idempotent` | 连续编译 3 次，并把 `build/` 产物混入源文件集合，结果必须一致 |
| `shuffle` | 用多个种子打乱 YAML 映射的键顺序并重新序列化，结果必须一致 |
| `tick_roundtrip` | 时间刻度往返与帧累加无漂移 |
| `locality` | 对比 overlay 前后，按区统计变化（段落整体平移不算），不超过 `max_changed_zones` |

用例可以写 `overlay` 在样例工程上叠加修改（JSON merge-patch：映射递归合并，列表和标量整体替换，`null` 删除键），不用复制整个工程。
`compile` 用例还可以写 `expect_status`、`forbid_option_targets`（冲突选项里不能出现的目标）、`expect_zero_costs` / `expect_nonzero_costs`（代价项必须为 0 / 大于 0）。

更新期望输出：

```
cargo run -p clip -- golden bless --case G-01
git diff golden/cases/G-01/expected   # 必须逐行审阅
```

**期望文件的每一处变化都要在 PR 描述里说明原因。**
