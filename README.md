# 声学定位离线审查工具（acoustic-review）

面向工业现场多麦克风阵列的**离线**声源定位审查工具。设备只上传短时间声学特征
（本地时钙、峰值频带、SNR、若干互相关延迟），工具负责建立事件、生成并比较声源
位置候选、区分直达与墙面反射证据，并记录工程师的手工判断。

包含：Rust 服务 + 浏览器空间/时线视图 + SQLite 存储 + 合成数据生成器 + 自动化测试。

## 安装与演示

```bash
cargo fetch --locked
cargo test --locked && cargo run --locked -- --listen 127.0.0.1:5322
```

打开 <http://127.0.0.1:5322>。

首次启动会向 `acoustic-review.db` 写入三个可复现的演示事件（`--no-seed` 关闭）：

| 事件 | 场景 | 初始审查结论 |
| --- | --- | --- |
| `EV-A` | 四阵列、S4 缺席、S1 重复上报、含一条墙面反射 | 排除反射峰后点定位 `(24, 12) m`；含反射时给出多径区域候选 |
| `EV-B` | 仅两台站、只有本地时钙、时钟段未锁定 | 欠定区域，原因 **clock_freedom**；锁定两段后降为一条双曲线（sensor_geometry） |
| `EV-C` | 互相关延迟超过基线几何极限（超光速） | 欠定区域，原因 **unobservable_input**，优化器不夹紧 |

其他参数：`--db PATH` 指定数据库文件。

## 单位（全部显式）

- 坐标：米（m），二维 `(x, y)`
- 时间、延迟、时钟偏移：秒（s），界面同时显示毫秒残差
- 声速：米/秒（m/s），随事件存储（演示取 343）
- 频率：赫兹（Hz）

## 时钟模型

每台采集器一条独立时间轴，采用**分段常值偏移**模型：

```
本地读数 local(t) = 全局时间 t + offset        （在某一段内成立）
校正时间 corrected = local - offset
```

每个时钟段记录 `t_start`、`t_end`、`offset_sec`、`source`。段覆盖**半开区间**
`[t_start, t_end)`；`t_end = NULL` 表示延伸到无穷远。

- 观测校正时间是**派生列**：时钟段变化时只重算 `corrected_onset_sec`，
  原始 `local_onset_sec` 等字段永不修改。
- 某观测时刻命中 0 段（未建模）或多于 1 段（模型冲突）时，校正时间置空，
  界面显示“时钟未锁定”。
- 到达时刻差（onset TDOA）只有在两端台站的时钟段都被工程师**锁定**
  （`lock_clock` 证据）后才进入定位；CCF 站内互相关延迟由本地计算得到，
  不受时钟偏移影响，始终可用。

### 半开边界

区间统一为 `[start, end)`。两个**恰好相接**的校正段（前段 `end ==` 后段 `start`）
是允许的，但边界时刻**只属于后一段**，因此两段不会同时生效。写入新段时服务端
校验同站段不得重叠（相接合法），测试见
`half_open_segments_boundary_belongs_to_later_segment` 与
`overlapping_segments_rejected_adjacent_allowed`。

## 定位与欠定几何

两类 TDOA 测量，统一为 `(到 a 距离 − 到 b 距离) / c = tau`：

1. `ccf_lag`：上传的互相关峰值延迟（`hint = direct | reflection`）；
2. `onset_pair`：相对锚台（`station_id` 最小的上台站）的校正到达时间差。

求解器枚举“保留/排除哪些反射峰”的假设（直达优先，按 `lag_uid` 字典序，
上限 24 个），每个假设独立用多起点无约束高斯-牛顿（LM）求解，残差按
**测量逐条分解**（CCF / 时钙、实测、预测、残差、是否使用、未用原因）。

**超出可观测几何的输入不会被数值优化器硬拉回边界：**

- 优化器不加场地边界约束；解落在场地外时保留该点并标记
  `outside_observable_geometry`；
- `|tau| · c` 超过基线长度的“超光速”延迟标记 `superluminal_lag`，不参与拟合；
- 残差过大/发散 → 区域候选 `multipath` 或 `inconsistent_measurements`。

欠定结果可以是**点、一条双曲线、或一个区域形状**，并给出原因类别：

| 原因 | 含义 |
| --- | --- |
| `sensor_geometry` | 传感器几何不足（单 TDOA 为双曲线；交会退化给误差椭圆/场区） |
| `clock_freedom` | 时钟自由度：只有时钙差且校正段未锁定 |
| `multipath` | 多径：保留的测量互相冲突 |
| `unobservable_input` | 输入超光速等，几何上无解 |
| `outside_observable_geometry` | 解在场区外，未被夹紧 |
| `ambiguous_pair` / `kept_indistinguishable` | 两个不可分位置（自动检测或手工保留） |

## 派生证据与“只消耗新证据”的重算

工程师的手工判断写入**只追加**的 `evidence` 表，从不修改或删除：

- `exclude_reflection` `{lag_uid}`：排除某条反射路径；
- `lock_clock` `{segment_id}`：锁定一个时钟校正段；
- `keep_ambiguous` `{candidate_ids:[a,b]}`：保留两个不可分位置。

证据按事件内顺序编号（`ev-EVENT:0001…`）。每次重算基于“原始观测 + 时钟段 +
当前生效证据”计算 **SHA-256 输入指纹**（含求解器版本 `solver-1.0.0`）：

- 输入指纹相同 → 复用已有作业，不重算；
- 新观测/新证据改变指纹 → 新建作业；原观测与更早候选保留不动。

## 批次与作业指纹、崩溃恢复

- 上传批次以 `batch_uid` 幂等：重复提交只增加 `duplicate_count`，不重复添加观测；
  相同 `obs_uid` 的重传记录 `duplicate_of` 且不参与定位。
- 候选与批次都有指纹；候选 ID 形如 `cand-EVENT:j<job>:<枚举序>:<假设指纹>`。
- 作业状态机 `pending → running → completed | failed`：
  候选在单个事务内随作业一起提交，未完成候选不会出现在审查接口；
  启动时把残留 `running` 重置为 `pending` 并删除其半成品候选后重算。

## 排名稳定性（并行确定性）

- 假设集合按字典序枚举，候选枚举序在求解前固定；
- 工作线程（最多 4 个）只返回结果，排名在收集后统一按
  `(score = 加权/原始 RMS, 枚举序)` 排序，同分由枚举序决胜；
- 线程完成顺序、系统负载均不改变候选排名与证据编号。
  测试 `repeated_solve_has_stable_ranking_ids_and_fingerprint` 重复求解并比对
  指纹、候选 ID 与排名。

## HTTP API（节选）

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| GET | `/api/events` `/api/event?event_id=` | 事件列表 / 事件全量审查视图 |
| POST | `/api/stations` `/api/events` `/api/clock-segments` | 建站点/事件/时钟段（半开校验） |
| POST | `/api/batches` | 幂等上传观测批次（原始 JSON 同时存指纹） |
| POST | `/api/evidence` | 追加手工判断并自动入队重算 |
| POST | `/api/solve` | 以当前输入指纹入队求解（等价作业复用） |

## 目录

```
src/db.rs        SQLite 建表/幂等/证据追加/作业状态机
src/geom.rs      双曲线采样、LM 求解、可观测性、误差椭圆
src/solver.rs    测量推导、假设枚举、并行确定性求解、欠定分类
src/synthetic.rs 合成数据（三个演示事件）
src/server.rs    std::net HTTP 服务 + 后台 worker
static/          浏览器空间/时线视图（原生 JS，无构建步骤）
tests/           时钟边界与欠定几何等自动化测试
```
