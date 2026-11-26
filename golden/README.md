# 黄金用例

- `fixtures/<名字>/`：样例工程（只放源文件，`build/` 不提交）
- `cases/<ID>/case.yaml`：用例定义；`compile` 类用例另有 `expected/`

| kind | 做什么 |
|---|---|
| `compile` | 编译样例工程，与 `expected/` 逐字节比对 |
| `idempotent` | 连续编译 3 次，并把 `build/` 产物混入源文件集合，结果必须一致 |
| `shuffle` | 用多个种子打乱 YAML 映射的键顺序并重新序列化，结果必须一致 |
| `tick_roundtrip` | 时间刻度往返与帧累加无漂移 |

更新期望输出：

```
cargo run -p clip -- golden bless --case G-01
git diff golden/cases/G-01/expected   # 必须逐行审阅
```

**期望文件的每一处变化都要在 PR 描述里说明原因。**
