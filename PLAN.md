# Tachyon JS Implementation Plan

## 1. 文档目的

本文是从空仓库推进到可发布 Rust SDK 的执行计划。完成标准不是“能运行一段 JavaScript”，
而是同时满足以下结果：

- 固定版本 test262 的适用测试通过率不低于 98%。
- 在固定硬件、工具链和构建参数下，批准的 JavaScript benchmark 吞吐几何平均不弱于 Boa。
- Rust Host SDK 在类型安全、异步集成、模块加载、扩展组合和资源治理方面明显优于 QuickJS
  的 C API wrapper 体验。
- 完成 Escargot 同等级别的非 JIT 性能基础：紧凑寄存器字节码、专用 opcode、shape transition、
  属性/调用 inline cache、fast array、字符串多表示、分代 GC、builtin fast path 和反馈驱动优化。
- 默认提供锁定 revision 的 TC39 proposal-signals 原生实现；不能用
  默认关闭 feature 将其从产品、测试或性能门槛中移除。
- `cargo fmt`、`cargo clippy`、单元/集成/属性/Miri/sanitizer 检查通过。
- 首个商业版本只发布 Rust SDK。未来 C ABI 不在首发承诺内，但内部设计必须通过薄 FFI adapter
  smoke test，不能依赖无法跨 ABI 表达的隐藏状态。

本文不包括 JIT、长期 PGO、老年代压缩、并发 GC、可恢复 heap snapshot 和 wasm32。debugger
所需的流式诊断 snapshot 属于首发范围，但不是稳定序列化/恢复格式。profile 指导
的 opcode/数据结构选择属于正常实现，跨用户 workload 的长期 PGO 属于后期工作。

## 2. 不可违反的执行原则

1. `DESIGN.md` 是架构事实来源。实现偏离设计前先用基准、正确性或平台证据更新设计。
2. 每个 milestone 必须形成可运行的垂直切片，不能长期只堆抽象层。
3. test262 runner 和 benchmark runner 从第一阶段就存在，不能到引擎完成后才接入。
4. 普通解释器热路径不经过通用 trait object、channel、锁或原子引用计数。
5. Oxc 类型和 arena lifetime 不得离开 `tachyon-compiler`。
6. GC 不得依赖 VM 的具体 JavaScript 类型；VM 通过 descriptor/trace contract 描述对象。
7. 所有 heap field store 从第一次实现开始经过统一写入 API，即使首版 barrier 是空操作。
8. borrowed JS handle 不得跨 allocation、safepoint、thread 或 `await`。
9. `unsafe` 只允许存在于已列出安全不变量、边界测试和 Miri/sanitizer 覆盖的小模块。
10. 大于 20 行的函数必须有 doc comment 或函数前说明其算法、状态转换或安全边界的注释。
11. 不用大面积宏生成解释器或对象系统。少量 tuple arity adapter/重复 impl 宏必须说明原因，
    并用展开规模测试和代码审查控制。
12. 每个工作包单独提交；提交后重读 `AGENTS.md`，再开始下一工作包。
13. 每个热路径或长生命周期 collection 必须有基于精确计数、workload 分布或受限 high-water
    mark 的 capacity policy；不得把不可信 JavaScript/FFI length 直接用于预分配。
14. engine core 非测试代码不得执行 filesystem/network/process/stdio/ambient-env/thread-runtime I/O；
    所有宿主能力通过内存输入或 typed provider 注入，tools/adapters 只能单向依赖 engine。

## 3. 目标 Workspace

```text
crates/
  tachyon-value/       位级 Value 和 RawHeapRef
  tachyon-bytecode/    字节码模型、builder、verifier、disassembler
  tachyon-gc/          logical spans、collector、roots、handles
  tachyon-compiler/    Oxc、HIR、scope/binding、register allocation
  tachyon-vm/          isolate、fiber、interpreter、objects、builtins、jobs
  tachyon/             稳定 Rust SDK facade
adapters/
  tachyon-async-runtime/ 三种 executor adapter 与共用 contract tests
  tachyon-inspector/     executor/transport-neutral typed debugger -> CDP adapter
  tachyon-serde/       可选 typed conversion
  ffi-smoke/           不发布的未来 C ABI 可表达性验证
tools/
  tachyon-cli/         开发 CLI、bytecode dump、trace
  tachyon-inspector-server/ 可选 WebSocket/stdio transport
  test262-runner/      test262 metadata/harness/result runner
  benchmark-runner/    多引擎统一构建与测量
benches/
  scripts/             共享 JS benchmark corpus
  baselines/           固定机器的版本化结果
tests/
  js/                  Tachyon 专项 JS 回归
  fixtures/            compiler/bytecode/module fixtures
```

内部 crate 首版均为 `publish = false`；稳定产品入口是 `tachyon` 和可选
`tachyon-async-runtime`/`tachyon-inspector`。`tachyon-serde` 在 API 稳定后可以独立发布。参考仓库 `boa`、
`escargot` 和 `deno_ast-oxc-port` 不加入 workspace。

## 4. 全局完成定义

每个实现任务只有同时满足以下条件才可标记完成：

- 代码有正常路径、错误路径和资源边界测试。
- 新 `unsafe` 同文件包含 `SAFETY` 说明，测试覆盖最小/最大 offset、alignment、错误 tag、
  use-after-scope 或相应边界。
- 新公开 API 有 rustdoc 和最小可运行示例。
- 新 opcode 同时更新 encoder、decoder、verifier、disassembler、interpreter 和测试 fixture。
- 新 heap field 进入 trace，并通过 forced-GC 测试；涉及 young/old 引用时有 barrier 测试。
- 新 ECMAScript 功能包含对应 test262 子目录运行结果，不只包含手写 happy-path 测试。
- 新优化在提交中包含 before/after benchmark；无收益或明显扩大 `unsafe` 的优化删除。
- 新增或改变 collection 时记录 hint 来源、增长/回收策略、memory limit 行为，并在
  `capacity-stats` 下验证 growth count 与 unused capacity。
- `cargo fmt --all --check` 通过。
- `cargo clippy --workspace --all-targets --all-features -- -D warnings` 通过。
- `cargo test --workspace --all-features` 通过。
- 涉及可迁移状态的类型有 compile-time `Send`/`Sync` assertion。
- engine core 新代码通过 host-I/O architecture gate；测试使用的 `std::fs` 不进入 library target。
- `DESIGN.md`、本计划和实际结构一致。

## 5. 固定验证命令

Phase 0 必须提供 `justfile` 或 `cargo xtask`，稳定以下命令语义：

```text
cargo xtask check                 fmt + clippy + unit/integration + docs
cargo xtask test miri             Value/GC/handle 的 Miri 子集
cargo xtask test sanitizer        ASan/UBSan，支持的平台再运行 TSan
cargo xtask test runtimes         futures/smol/Tokio 单 feature 与 all-features contract matrix
cargo xtask test debugger         typed debugger、CDP protocol、root/snapshot stress
cargo xtask test signals          pinned proposal suite、graph/GC contract matrix
cargo xtask architecture check    core host-I/O bans、crate layers、dependency/build-script audit
cargo xtask test262 fetch         checkout 配置中固定 commit
cargo xtask test262 run <path>    运行目录、单文件或全套
cargo xtask test262 compare       对比两个 JSON 结果
cargo xtask bench build-engines   统一构建 Tachyon/Boa/QuickJS/Escargot
cargo xtask bench micro           Rust 与 JavaScript 微基准
cargo xtask bench suite           V8 scripts/js-engine-zoo 可比套件
cargo xtask bench compare         输出绝对值、ratio、geomean 和置信区间
```

命令名称可调整，但自动化、文档和本地必须调用同一个实现，不能维护多套 shell 流程。

## 6. 里程碑依赖图

```text
M0 Repository + harness
  -> M1 Value + bytecode contract
  -> M2 Oxc frontend + HIR
  -> M3 Minimal fiber VM
  -> M4 Precise non-moving generational GC
  -> M5 Objects/functions/environments
  -> M6 Language control + errors + classes
  -> M7 Rust Host SDK sync boundary
  -> M8 Builtins/modules/default native Signals/advanced semantics
  -> M9 Incremental major GC + barrier hardening
  -> M10 Promise/generator/async/actor
  -> M11 Host async + extension completion
  -> M11 Debugger + inspector completion
  -> M12 Full test262 convergence
  -> M13 Escargot-class performance foundation
  -> M14 Benchmark parity + release hardening
```

M0 的 test262/benchmark 工具此后持续运行。M13 的优化不会全部等到语义完成才开始：每个阶段
都保留基准，但只有语义与 GC 边界稳定后才冻结完整性能模型。

当前正式 Stage 指针（2026-07-18）：仍在 **S0 Foundation**，因为 benchmark runner、
Value/decoder 边界验证等 S0 gate 尚未闭合；test262 runner skeleton 已闭合。当前并行代码前沿是
**M2.3/M3.2 expression semantics 与 M5.1/global substrate**；下面的工作包台账
记录已经闭合的可审查增量，不代表其所属 Stage 已通过，也不替代各 M 项和 Stage Gate 的完整
验收条件。

- [x] M0 基础切片：六个 engine crate、CLI/xtask、optimized debug profiles、首版 architecture gate。
- [x] W0 首批模块所有权整理：将 `tachyon-vm`、`tachyon-gc::heap` 与 `tachyon-bytecode` 的大型
  inline test module 移到相邻测试文件，保持生产语义与公开 API 不变；Realm/builtin installer、
  compiler capacity analysis 和 bytecode verifier 的生产热点继续按独立可回滚提交拆分。
- [x] M0.2 test262 runner core：固定 checkout/release policy、YAML frontmatter、strict/module/raw/async
  variants、内存 harness + SHA-256、phase-aware result、deterministic parallel scan 与 stub adapter；固定
  commit 全量扫描得到 102,597 个独立 variants，全部 unsupported 仍保留在分母。
- [x] M0.2 test262 runner completion：edition/feature applicability classifier、按 edition 汇总、baseline
  fixed/broken diff，以及 `cargo xtask test262 fetch/run/compare` 统一入口。
- [x] M0.2 convergence output：xtask 与 standalone runner 支持 `--summary-only`，全量收敛运行可只输出
  稳定 `RunSummary`，避免每轮序列化数十 MB 的逐测试明细；完整 report 仍为默认并可用于 compare。
- [x] M0.2 per-file convergence diagnostics：xtask 与 standalone runner 的显式 `--progress` 在调用
  engine 前输出 started、完成后输出 variant 数与 elapsed，不改变默认 JSON/stdout 契约；串行诊断将
  `staging/sm/regress` 的首个长期 CPU 文件定位为 `regress-1507322-deep-weakmap.js`。
- [x] M0.2 Tachyon test262 execution adapter：每个 variant 使用独立 isolate，body-first parse 保留
  negative parse phase，harness/body 按内存 source unit 顺序执行，async/未实现 lowering/VM surface
  明确记为 unsupported，panic、fuel timeout 与 runtime throw 不折叠为 pass；固定 commit 的
  `language/directive-prologue` 首次真实运行 11/62 pass、51 unsupported、0 panic/crash。
- [x] 2026-07-27 全量 Test262 收敛基线：固定 102,597 variants 中 applicable 85,643，当前通过
  51,482（60.11%），semantic failure 16,432、parse mismatch 719、timeout 22、applicable unsupported
  16,988；全量扫描无 panic/crash。该项只记录真实进度，不代表 M14.3 的 98% release gate 已完成。
- [x] M0.3 benchmark measurement core：三条 Boa 基础脚本的 commit/path/SHA-256/license provenance、
  完整 micro category/mode schema、serial adapter contract，以及固定 sample/MAD/outlier/CI 统计基础。
- [x] M0.3 benchmark comparison/environment gate：matched-case baseline/candidate ratio、全局/category/suite
  几何平均、missing/invalid/classification drift 判定，以及 affinity/governor/background-noise validity；
  `cargo xtask bench verify/compare` 提供统一入口。
- [x] M0.3 external cold-start adapter：最初以 Boa/QuickJS/Escargot 验证共用 file-last argv protocol、无 shell
  command 拼接、进程 deadline/kill/wait、有界 stdout/stderr capture、binary-size identity，以及
  `cargo xtask bench run-external` 版本化 JSON 入口；fake process fixture 已覆盖 success/failure/timeout。
  公平 steady-state adapter 落地后已删除 Boa/QuickJS CLI kind/profile/入口，外部进程只保留 Escargot
  cold-start 维度，不能进入 linked-engine 吞吐 ratio。
- [x] M0.3 real-engine smoke：macOS aarch64 固定 Boa `0.21.0`、rquickjs `0.12.1` 与 Escargot
  `0ac9f5c...` identity；Boa/rquickjs linked adapter 与 Escargot release CLI 均对同一
  content-addressed `basic/call-loop` 生成 15-sample JSON，并通过 Markdown compare；当前无
  affinity/governor 的结果明确 invalid，不作为 parity evidence。
- [x] M0.3 Tachyon in-process adapter：新增 source-committed foundation arithmetic corpus，真实实现
  parse+compile+execute、precompiled execute 与 fixed-N steady-state；report schema v2 保存
  iterations/sample，comparison 拒绝 iteration drift，cold start 明确 unsupported。clean HEAD `d1e9697...`
  release smoke 的 median 分别为 30.5 us/1、542 ns/1、491.646 us/1000；macOS 环境 gate 仍标 invalid。
- [x] M0.3 linked reference adapter：benchmark tool 精确依赖 `boa_engine = 0.21.0` 与
  `rquickjs = 0.12.1`；runtime/context 与 setup 在 sample 外创建一次，sample 内预解析 global `main`
  后直接调用精确 N 次。两者只支持可诚实对齐的 `main-function + steady-state`，不启动 CLI/REPL，
  不把 context startup、I/O 或进程创建混入吞吐数据。
- [x] M1 基础切片：NaN-boxed `Value`、logical heap ref、word bytecode、builder/verifier/disassembler、
  immutable `CompiledModule` 与 property coverage。
- [x] M1.1 Value 边界证据：独立于生产 fast path 的 raw-bit classification oracle 覆盖任意 `u64`，
  f64/NaN/int32/logical heap ref 边界通过 property tests；strict-provenance、isolation-on Miri 专项验证
  bit conversion 与首尾合法 `RawHeapRef`。release assembly gate 仍作为独立缺口保留。
- [x] M2 基础切片：Oxc 0.140 内存 source boundary、owned diagnostics/HIR、算术/基础 local lowering。
- [x] M3 基础切片：显式 fiber/frame/register state、算术/分支 interpreter、
  `execute_batch::<1/2/4/8/16>` 结果与 fuel/quantum 对拍。
- [x] M3/M13 bytecode-only cursor baseline：每个 dispatch batch 只解析一次 active
  code/function，缓存不可变 `Arc<[u32]>` backing 的 pointer/length；每步核对 frame identity，跨
  function/code 立即 redispatch，普通 branch 留在 batch。唯一 unsafe slice reconstruction 由 owner
  move + loaded-code 容器扩容边界测试覆盖，call/throw/cross-module 在 N=1/2/4/8/16 下继续对拍。
  该 checkpoint 已证明 immutable backing lifetime，但每 opcode frame identity 核对、checked decode 与
  `pc` 回写现在明确属于迁移基线，将由 M13.1 verified execution kernel 取代，不能继续视为目标架构。
  clean HEAD `8424b0c...` 相对 `527f36f...` median：call-loop `7.183 -> 7.179 ms`，nested-loop
  `31.934 -> 30.984 ms`（`+2.98%`），closure `1.141 -> 1.102 s`（`+3.40%`）；macOS 环境 gate
  仍 invalid，因此只作为优化方向证据，不作为 release parity evidence。
- [x] M2/M3/M13 direct captured-environment opcode：删除 runtime-plan-index `LoadBinding/StoreBinding`，
  opcode 42/43 直接编码 `(register, depth, slot)`；module verifier 检查 register 与模块最大 slot，VM
  继续 checked traversal。`BindingPlan` 只保留 debugger/metadata 所需 name/mutability/location。
  `NoGcScope::borrow_reference{_mut}` 允许已由 traced owner 保活的 typed reference 在禁止 GC 期间跳过
  临时根发布，但仍验证 descriptor/liveness/owner；temporary-root capacity 不变测试覆盖该边界。
  clean HEAD `27e1957...` 相对 `8424b0c...` closure median `1.102 s -> 950.8 ms`（吞吐 `+15.9%`），
  call/nested 无可信回退；相对 Boa closure 快约 `1.45x`，相对 rquickjs 仍慢约 `5.08x`。真实
  directive-prologue 保持 `45/62`；macOS 环境 gate invalid，数据不作为 release parity evidence。
- [x] M3/M13 single-check callable resolution：`Call` 从 tagged `RawHeapRef` 进入
  `NoGcScope::borrow_raw_reference` 后只做一次 descriptor/liveness/owner validation，不再依次
  `checked_reference -> temporary root -> checked payload`。raw retype 不逃出 GC crate；错误 heap type/
  immediate callee 继续在 N=1/2/4/8/16 下转为 managed TypeError。clean HEAD `ebff7ed...` 相对
  `27e1957...` median：call-loop `7.233 -> 6.369 ms`（吞吐 `+13.6%`），closure
  `950.8 -> 894.4 ms`（`+6.3%`），nested `31.076 -> 30.506 ms`（无回退）。三项分别快 Boa 约
  `1.49x/1.55x/1.34x`，相对 rquickjs 仍慢约 `3.92x/4.78x/4.60x`；环境 gate invalid。
- [x] M3 function-call substrate：isolate-owned GC heap 与 typed FunctionObject descriptor、managed
  `CreateClosure`、contiguous actual-argument window verifier、显式 frame `Call/Return`、host heap/frame/
  register hard limits；N=1/2/4/8/16 均对拍，未使用 Rust recursion 或 reserved Value tag。
- [x] M2/M3 ordinary function vertical slice：owned function stencil 复制 simple parameters/body，顶层
  declaration hoist 到 entry `CreateClosure`，共享 constant pool，direct call 连续参数窗口，ordinary
  function explicit Return/implicit undefined Return；source 到 `addTwo(40) === 42` 端到端通过。
- [x] M2/M3 structured control vertical slice：owned `Block/If/Throw`、block lexical checkpoint、script
  completion result register、symbolic branch labels、递归 checked capacity count，以及未处理 throw 的显式
  `RunOutcome::Thrown`；callee throw 在 N=1/2/4/8/16 下对拍且不使用 Rust unwind。
- [x] M2/M3 logical expression vertical slice：owned `Logical` HIR、operand-valued `&&/||/??` 短路
  lowering、`JumpIfTrue/JumpIfNotNullish` verifier/disassembly/dispatch、IEEE `Negate` 与 primitive realm
  `undefined/NaN/Infinity` fallback；N=1/2/4/8/16 覆盖每种 branch 双路径。`08bb441` clean HEAD
  steady-state smoke 为 588.791 us/1000（环境 invalid，无明显回退）。真实 directive-prologue runner
  仍为 11/62，但共同失败从 assert.js span 390..428 前进到 switch statement span 589..975。
- [x] M2/M3 switch control vertical slice：owned source-ordered case table、strict-equality dispatch、
  中置 default、fallthrough、最近无标签 break target，以及 checked instruction/label/binding/literal/
  scope-name/switch-stack capacity；N=1/2/4/8/16 对拍 dispatch chain。`69ce6b8` clean HEAD
  steady-state smoke 为 531.791 us/1000（环境 invalid，无回退证据）。真实 directive-prologue 仍为
  11/62，但共同失败继续推进到 switch 内 member expression span 701..715；switch lexical TDZ 尚未闭合。
- [x] M2/M3 while/do-while control vertical slice：owned `Loop` HIR 以 `test_first` 保留前测/后测顺序，
  condition label 同时作为规范 continue 目标，break 仍使用最近 loop/switch end；script completion、
  function body、continue、break 和 N=1/2/4/8/16 backward-jump dispatch 均覆盖。真实 test262
  `language/statements/while` 从基线 31/72 提升至 37/72（unsupported 27 降至 19），
  `language/statements/do-while` 当前 41/70；其余 semantic failure/unsupported 仍保留，未宣称完整
  iteration statement 或 lexical-per-iteration semantics。
- [x] M2/M5 plain object literal vertical slice：owned `HirObjectProperty` 复制 identifier/string key 与
  value expression，lowering 按源码顺序发出 `CreateObject`/`SetById`，shape-backed read/update 与
  object spread/accessor/computed syntax 的显式 unsupported regression 均覆盖。`language/expressions/object`
  当前 499/2252（其中 300 个 parse-negative），仍不计作完整 object/descriptor/accessor 语义。
- [x] M2/M5 computed string object-key vertical slice：owned key 区分 `Static`/`Computed`，computed key
  先于 value 求值并走 `SetByValue`；VM 对 rooted string Value 做 exact UTF-16 copy/intern，同时保留
  int32 key fast path。object expression 从 317/2252 提升到 499/2252；computed numeric/f64、Symbol、
  method/accessor 仍保持明确 unsupported，避免伪造 ToPropertyKey 或 descriptor 语义。
- [x] M2/M5 default-parameter vertical slice：owned function stencil 保存 parameter initializer，函数入口
  用 `StrictEqual(undefined)` branch 仅在参数缺失/显式 undefined 时求值，支持左到右参数依赖；initializer
  的 instructions、labels、constants、scope names 纳入 checked capacity。statements/expressions/function
  的 default-parameters applicable 子集已运行，destructuring/rest/arguments parameter environment 仍未闭合。
- [x] M5/M10 parameter-environment TDZ closure：含 initializer 的函数在 activation environment 中预发布
  全部参数槽为 uninitialized，按源码顺序先执行当前 initializer、再用 `InitializeEnvironment` 发布绑定；
  self/later reference 命中 TDZ，prior reference 和非 undefined argument 保持快路径，named async function 的
  immutable self-binding 与参数槽共存。compiler 70/70、VM N=1/2/4/8/16 与 forced-major 回归通过；重建
  runner 后 async-function 目录从 137/161 提升到 151/161，剩余 semantic failure 属于 arrow lexical
  arguments/self-binding 和 direct eval，`with`/unscopables 仍 unsupported。
- [x] M5/M10 arrow lexical `arguments`/self-binding closure：Oxc unresolved-reference scope 在 HIR 冻结为
  最近 non-arrow owner，仅对真实被 nested arrow 捕获的 activation 追加 synthetic declarative slot；owner 在
  parameter initialization 后物化一次 arguments object，arrow 通过既有 closure environment 读取，不扩大
  `Frame` 或 56-byte `FunctionObject`。sloppy named-function self-binding identity 提升为 module-wide plan，跨
  nested arrow 写入仍静默忽略；mapped arguments 在 normal/throw frame exit 前同步成 owned snapshot，避免逃逸
  对象引用被截断 register window。compiler 71/71、VM N=1/2/4/8/16 与 forced-major 回归通过；async-function
  从 151/161 提升到 154/161，剩余 3 semantic failure 全属 direct eval，4 unsupported 全属 with/unscopables。
- [x] M2/M3/M6 call argument spread vertical slice：仅含 spread 的 call 使用 owned `CallSpread` HIR，
  compiler 复用 ArrayAccumulation 的 realm-rooted iterator CFG 保留多个 spread 与前后普通参数顺序；
  `CallSpread`/receiver/tail/direct-eval opcode 通过 GC-owned argument-list continuation 冻结 exact-size prefix，
  method `this`、callee getter、iterator abrupt 不错误执行 IteratorClose、direct-eval caller scope、
  N=1/2/4/8/16 与 forced-major 均覆盖。Promise applicable 从 1234/1274 提升至 1274/1274；construct/super-call
  spread 仍属于 M6 后续，因此 M6 总项不打勾。
- [x] M3/M5 cross-source code substrate：module-owned verified scope-name table、isolate-local
  `CodeId(NonZeroU32)` loaded-code table、frame/FunctionObject 双 code identity、显式 module/global limits，
  以及 rooted global-object function binding；`load_module/execute_loaded` 分离 embedding timing boundary，
  独立 harness/body module 在 N=1/2/4/8/16 下跨 code call 对拍。
- [x] M4.1：logical span table、Rust allocator backing、small/large typed allocation、managed
  collection/retry、hard-limit accounting 与 reference verifier。
- [x] M4.2 collector core：epoch tri-color strong mark、full sweep、weak/ephemeron fixed point、
  finalization enqueue、allocation/pressure trigger、forced-major stress。
- [x] M4.3 GC-internal roots：generative `RunningScope`、`NoGcScope` payload borrow、generation-protected
  persistent-root slab 与 compile-fail lifetime/thread boundary tests。
- [x] M4.4 young core：Eden/Survivor/Old cohorts、young-only minor、remembered cards、whole-span
  promotion、forced-minor stress、debug barrier verifier、bounded Eden pool/trim 与 GC accounting。
- [x] M4.4 policy instrumentation：age=2、80% occupancy early promotion、typed 8 MiB active-young cap、
  O(1) active-young accounting，以及 host-fed fixed-bucket minor/major pause histogram。
- [x] M4.2/M4.4 VM finalization scheduler substrate：入场快照 reserve/transfer、isolate-owned FIFO 精确根、
  callback throw/reentrancy，以及 callback-triggered GC/new-record deferral；Rust panic 直接 abort。
- [x] M5.1 string/atom foundation：Latin-1/UTF-16 owned backing、unpaired-surrogate code-unit 语义、
  lazy keyed hash、isolate-owned quota atom table、stable AtomId，以及 GC external backing accounting。
- [x] M5.1 canonical `typeof` object classification：Function 保持 callable 优先级，Array、boxed Number
  与 ordinary object 统一复用 object classifier；真实 `property-accessors` 从 16/42 提升到 18/42，
  原先两个 `UnsupportedTypeof` 变体清零。完整 String/Boolean exotic 与 primitive boxing 仍待闭合。
- [x] M5.1 StringToNumber grammar correctness：显式 ECMAScript WhiteSpace/LineTerminator 集合，拒绝
  Rust-only `inf`/`infinity` 与 malformed decimal，精确接受三种 Infinity spelling，radix literal 超过
  `u64` 后使用 top-53/guard/sticky 做一次 binary64 ties-to-even 舍入；unpaired UTF-16 surrogate 明确拒绝。Number 全目录
  保持 632/680；heap String 的 borrowed no-allocation scanner 与最终 rounding/perf corpus 仍待闭合。
- [x] M5.2 ordinary data-property substrate：isolate-owned append-only `ShapeTable`、稳定 `ShapeId`、
  共享 add-transition、host `max_shapes` hard limit、GC-managed `OrdinaryObject` 与精确 external-accounted
  fixed `Box<[Value]>` property storage；新增属性采用 traced exact-size replacement，已有 slot 原地更新并
  对真正的 storage owner 执行 value barrier。`CreateObject/GetById/SetById/CallWithReceiver` 已完成
  verifier/disassembly/dispatch，static member read/assignment/method call 保持 receiver 单次求值和 `this`；
  N=1/2/4/8/16 对拍。真实 directive-prologue 仍为 11/62，但共同失败已推进到 assert.js
  `try/catch` statement span 1090..1252。prototype/descriptor/accessor/delete/ownKeys/property order、
  object literal 与 IC 尚未完成，因此 M5.2 总项不打勾。
- [x] M5.2 PropertyKey/Symbol identity substrate：shape key 统一为 `Atom | Symbol`，每个 Symbol 使用
  isolate 单调且永不复用的 serial 与 logical heap ref 组成稳定 identity，避免 GC slot 复用后命中旧的
  append-only shape。shape metadata 不参与 tracing；实际拥有 Symbol property 的 fixed storage 保存稀疏
  traced key edge，删除最后一个 property 会清除 edge，活 Symbol 重新添加则恢复 edge。dynamic
  get/set/delete/has、ordinary descriptor/hasOwn 与 Object.assign 已接受 Symbol；string-only enumeration
  明确过滤 Symbol。object-level ordinary own-key provider 已统一 integer-index 数值升序、String chronology、
  Symbol chronology、函数 zero-backing metadata 与 delete/re-add 顺序；canonical `0..=2^32-2` 边界、
  N=1/2/4/8/16、forced-major 和 source-level 顺序均覆盖。Reflect.ownKeys/getOwnPropertySymbols、
  well-known/global Symbol、Proxy/exotic 与 observable accessor consumer 尚未完成，因此 M5.2 总项和完整
  property-order 项仍不打勾。
- [x] M5.2 ordinary accessor callback slice：ordinary `[[Get]]`/`[[Set]]` 保留原始 receiver，accessor
  callback 通过 typed traced continuation 与既有 JS frame trampoline 执行，不递归进入 Rust interpreter；
  inherited getter/setter、undefined getter/setter、strict assignment、callback throw/catch、compound
  getter→RHS→setter 顺序、nested accessor-valued ToPrimitive、forced-major 与 N=1/2/4/8/16 已覆盖。
  Proxy/exotic、完整 ToPropertyKey 与全部 descriptor/test262 前置语义仍未完成，因此 M5.2 总项不打勾。
- [x] M5.2 resumable ToPropertyDescriptor slice：六字段按规范顺序执行 observable inherited/own Get，
  首次 getter suspension 才分配 traced pending state，纯 data descriptor 保持同步零 pending allocation；
  partial values、target/source/Symbol key 全部精确 tracing/barrier。getOwnPropertyDescriptor、hasOwnProperty
  与 propertyIsEnumerable 已识别 accessor kind；6 fields × own/inherited × N=1/2/4/8/16 + forced-major
  共 60 组合通过。typed continuation 重排后仍为 32 bytes。真实 Object.defineProperty 从 1024/2250
  提升到 1456/2250；剩余 accessor consumer、Proxy/exotic 与完整 descriptor 语义仍未闭合。
- [x] M5.2 kind-neutral accessor enumeration slice：for-in、Object.keys 与
  Object.getOwnPropertyNames 从 shape attributes 与 slot tombstone 判断 property presence，不读取 accessor
  payload、也不调用 getter；own non-enumerable accessor 正确遮蔽 prototype enumerable key，删除 tombstone
  后 prototype key 可重新暴露。N=1/2/4/8/16、forced-major 与 getter zero-call 覆盖通过；真实
  Object.defineProperty 从 1456/2250 提升到 1690/2250，unsupported 从 264 降至 30。Object.values、
  Object.entries 与 Object.assign 仍需 resumable observable `[[Get]]`，不能复用 key-only fast path。
- [x] M5.2/M3 resumable `@@toPrimitive` slice：发布 immutable `Symbol.toPrimitive`，realm 精确 root
  `"default"`/`"string"`/`"number"` 三个 hint；当前 Add、relational、numeric unary/binary 与 String/Number
  call consumer 在 ordinary fallback 前执行一次 inherited/own observable GetMethod。getter 产生的 fresh
  callable 用 parent Conversion + child ConversionCallRoot 两个 32-byte typed entry 保根，hint 覆盖 caller
  destination 前不会丢失 callee；同步错误、JS throw 与 limit=1 的第二槽失败均精确回滚。getter/method
  this、exact one-argument hint、nullish fallback、non-callable/object-result TypeError、primitive short-circuit、
  left-to-right mutation/abrupt、N=1/2/4/8/16 和 forced-major 已覆盖。addition 从 59/95 提升到 65/95；
  Symbol.toPrimitive property descriptor 为 2/2，另 2 个 cross-realm variant 仍失败。Date/Symbol wrapper、
  String construct wrapper、Proxy 与 BigInt 尚未接入，M5 总项不打勾。
- [x] M2.3/M5.2 resumable ToPropertyKey slice：新增 verified `ToPropertyKey [dst, key, base]` 与
  `ToPropertyKeyForIn [dst, key, rhs]`，先执行 operation-specific guard，再以 string hint 复用 32-byte
  Conversion continuation；object literal、read/delete、simple/compound/logical assignment、update、`in`
  与 computed for-in target 均在 compiler 中显式 prepare 一次并复用结果。simple assignment 保持
  base/key expression -> RHS -> base guard -> ToPropertyKey -> Set；compound/update 在 Get/RHS 前 prepare；
  object literal 在 value 前 prepare；`in` 先验证 RHS Object，invalid RHS 不调用 key callback。primitive key
  暂存为内部 primitive normal form，直到既有 property operation 再同步 atomize，避免常见 numeric key 提前
  分配 String；builtin native CallSite 的 key consumer 暂不伪装成 opcode continuation。own/inherited fresh
  callable、string hint/this、forced-major、completion limit 1/2 与 N=1/2/4/8/16 已覆盖；test262 隔离
  clean-HEAD 对比为 assignment +2、compound-assignment +88、`in` +2、object +6、delete +/-0，合计
  98 fixed / 0 broken；computed method `CallWithReceiver` 接入后 property-accessors 另从 18/42 升至
  20/42。Proxy、BigInt 与 super/private key 仍未闭合。
- [x] M5.2 resumable builtin ToPropertyKey slice：`Object.defineProperty`、
  `Object.getOwnPropertyDescriptor`、`Object.hasOwn`、`Object.prototype.hasOwnProperty` 与
  `propertyIsEnumerable` 使用 closed `BuiltinPropertyKeyConsumer`，primitive key 保持同步零 pending
  allocation；仅 object key 分配 16-byte `PendingNativePropertyKey` 保存最多两个业务 Value，continuation
  仍用 pending ref + key object 两个 traced Value 且保持 32 bytes。callback primitive 先发布到 caller
  destination，保证 fresh Symbol 在 define storage allocation/forced-major 前有精确 root；defineProperty
  随后可继续挂起 descriptor getter。static hasOwn/descriptor/define 的 ToObject guard 在 key conversion 前，
  prototype query 则按规范先 ToPropertyKey 再检查 receiver。own/inherited fresh callable、Symbol result、
  abrupt/order、Conversion -> PropertyGet chaining、completion limit 1/2、forced-major 与 N=1/2/4/8/16 已
  覆盖；隔离 clean-HEAD 对比 hasOwn +6、hasOwnProperty +6、propertyIsEnumerable +6、
  getOwnPropertyDescriptor +16、defineProperty +16，合计 50 fixed / 0 broken。primitive String indexed
  descriptors、Proxy/exotic internal methods 与完整 ToObject boxing 仍未闭合。
- [x] M2.3/M5.2 resumable Abstract Equality slice：`==`/`!=` 仅在一侧 object、另一侧为规范 eligible
  primitive 时使用 `Equality(opcode)` default-hint consumer；object-object 与 object-nullish 不执行 getter。
  pending primitive 复用 continuation receiver 槽，callback 后按 same type、nullish、Boolean 单侧 ToNumber、
  Number/String 单侧 StringToNumber 的 redo 规则完成，Symbol 与异型 primitive 返回 false 而不错误 ToNumber。
  own/inherited fresh callable、this/default hint、左右方向、ordinary fallback、abrupt/object result、completion
  limit 1/2、forced-major 与 N=1/2/4/8/16 已覆盖；equals 35/93 -> 63/93，does-not-equals
  29/75 -> 53/75，净增 52 pass。剩余 unsupported 全为 BigInt；HTMLDDA 留给 Annex B 独立分支。
- [x] M5.3 environment owner metadata slice：`FunctionMetadata` 冻结独立 exact-size
  `EnvironmentSlotMetadata` dense slice 与 record kind，不把 owner declaration 伪装成可执行
  `BindingLocation`；verifier 校验 slot count、name、depth-0 binding identity/mutability，compiler 保存
  parameter/var/let/const/function declaration 的 activation initialization state。runtime 按 metadata
  创建 state-bearing record、block/catch 独立 record 与完整 lexical parent graph 仍未闭合。
- [x] M3/M6 try/catch vertical slice：owned HIR 保存 try/catch/finally 全部 body，simple catch identifier
  lowering 生成 outer-first immutable half-open `HandlerEntry` table、exact handler count 与最大嵌套深度；
  `LoadException` 从 fiber pending slot 显式取值，frame 保存 caller call-site PC，`Throw` 沿显式 frame
  查找 innermost handler、截断 callee checkpoints 并跨函数传播，不使用 Rust unwind。normal/caught/
  nested rethrow/callee throw 以及 N=1/2/4/8/16 均通过。真实 directive-prologue 仍为 11/62，但共同
  失败已推进到 assert.js `new Test262Error(message)` span 1455..1480。finally、return/break/continue completion
  replay、catch destructuring 与规范 Error object 尚未完成，因此 M6 try/finally 总项不打勾。
- [x] M6 finally bytecode/compiler substrate：新增 verified `EnterFinally`/`ResumeCompletion` 与
  break/continue-through-finally target，`HandlerEntry` 保存 finalizer exclusive end；verifier 拒绝缺失
  Resume、伪 abrupt target、crossing finalizer range 与 understated nested depth。compiler 不复制 finalizer
  body，使用 disposable completion register、outer-first finally/catch table 与 exact capacity lowering。
  VM 使用显式 active-finalizer + traced completion record 迭代 replay，finalizer abrupt 正确覆盖旧
  completion，callback throw、nested catch/finally、跨一/多层 break/continue、stale record、forced-major、
  host limit 与 N=1/2/4/8/16 已覆盖；无 finally 的 Frame 缓存 bit 保留 Return 直接热路径且仍为 104 bytes。
  catch destructuring、labelled control、iterator close 与完整 Error 语义未完成，因此 M6 总项仍不打勾。
- [x] M2/M5 function expression vertical slice：anonymous ordinary function expression lowering 为 owned
  stencil，nested child-before-parent ID 与 module `FunctionId(stencil + 1)` 对拍，运行时复用 `CreateClosure`；
  `FunctionObject` 内嵌同一 ordinary shape/storage base，函数属性不另建 map，add/update/get 与普通对象
  共用 property path。scalar property 在 N=1/2/4/8/16 下通过，callable→storage→heap value 在 forced-major
  下保持 trace/barrier。`outer()()` 与 `assert._isSameValue = function...` 源码端到端通过。named function
  expression self-binding、真实 lexical capture、arrow/generator/async 与 construct/new.target 尚未完成。
- [x] M2/M5 ordinary construct vertical slice：owned `New`/`This`/`NewTarget` HIR，verified contiguous
  `Construct` argument window 与 `LoadThis/LoadNewTarget`；VM 在验证 constructor 后分配 receiver，frame
  精确保存 `this/new.target/construct_receiver`，Return 按 object replacement/primitive fallback 选择结果，
  constructor throw 复用显式 frame propagation。receiver initialization、primitive/object return、普通 call
  的 undefined new.target、N=1/2/4/8/16 与 forced-major rooting 均通过。真实 directive-prologue 仍为
  11/62；compound assignment 已通用支持并保持 identifier/member reference 的旧值读取顺序，
  classic `for`、identifier/static update、numeric less-than、break/continue 与 int32 computed member
  已接入；GC string Value、loaded literal cache、canonical typeof 与 string equality/falsy 已接入，
  共同失败推进到 assert.js `var` declaration kind span 1021..1059。constructor
  prototype lookup、class/derived semantics、规范 TypeError 和非 ordinary callable 仍未完成。
- [x] M2/M5 ordinary prototype/instanceof vertical slice：`OrdinaryObject` traceable prototype edge、
  default `function.prototype`/`prototype.constructor`、construct 读取当前 prototype、ordinary property
  traversal 与 verified `InstanceOf`；默认/替换 prototype、primitive LHS、N=1/2/4/8/16、forced-major
  均覆盖。真实 directive-prologue 从 11/62 提升到 18/62；`%Object.prototype%` fallback、descriptor、
  prototype mutation/cycle guard、`@@hasInstance`、bound/native/Proxy 与规范 TypeError 仍未完成。
- [x] M5.3 native-callable/`Function.prototype.call` slice：`FunctionExecutable` 显式区分 bytecode/native，
  Realm 精确 root `%Function.prototype%` 与 native `call`；ordinary closure 共享 prototype chain，CallSite
  保存独立 argument base，native forwarding 迭代消费 thisArg 且不复制参数、不用 Rust recursion。
  N=1/2/4/8/16、forced-GC 既有对象/closure fixtures 与 this/多参数转发通过；directive-prologue 从
  18/62 提升到 28/62。host callback ABI 未完成；apply、bind/name/length 由后续独立切片闭合。
- [x] M5.3 `CreateListFromArrayLike` forwarding slice：`Function.prototype.apply`、`Reflect.apply` 与
  `Reflect.construct` 在 target validation 后共享 typed GC continuation，完整走 `Get(length)` 与按索引的
  ordinary `Get`，缺失/undefined getter 产出 undefined，getter throw 原样跨显式 frame 传播；length 已知后用
  exact-capacity external-backed state 收集参数并通过 immutable bound prefix 启动一次 call/construct。定向
  `Reflect.apply` 为 16/18、`Reflect.construct` 为 16/20（剩余分别由动态 Function 与既有 Error 语义阻塞），
  `Function.prototype.apply` 的 ES6 accessor abrupt cases 通过；object-length ToPrimitive、Proxy/exotic 与
  full `ToLength` 上限仍待相应 substrate，故不将完整 Function/Reflect 目录标为闭合。
- [x] M5.3 Function identity/Object extensibility slice：Realm 精确 root 并发布全局 `%Function%`，
  `%Function.prototype%.constructor`、Function constructor 的 internal `[[Prototype]]` 与 own
  `prototype` identity 通过普通 property path 建立；动态 Function call/construct 在 source compilation
  接入前保持显式 unsupported，不伪装成规范异常。`Object.isExtensible` 读取 ordinary/function 共享的
  object state、对 primitive 返回 false，真实 test262 目录达到 54/76。Function body 参数解析仍未完成。
- [x] M5.2 `[[Extensible]]`/preventExtensions slice：`OrdinaryObject` 在 ShapeId 后的既有 padding 中保存
  object-local bool，64-bit payload 保持 24 bytes；function object 复用内嵌 ordinary base，不建 side table。
  `Object.preventExtensions` 对 object/function 原地关停扩展并返回原值，对 primitive 原样返回；已有 own
  data slot 仍可更新，新属性的 sloppy assignment 静默失败、strict assignment 与 defineProperty throwing
  path 抛 managed TypeError。真实 preventExtensions 目录达到 18/78；accessor/exotic internal method、
  seal/freeze 与 Proxy trap 仍未完成。
- [x] M5.2 ordinary data descriptor slice：Shape 将 zero-based slot 与 property_count 分离，attribute
  reconfigure 以 immutable overlay transition 复用原 slot，不增长 storage、不改变插入顺序；own-key
  materialization 用 exact-capacity `Option<PropertyKey>` snapshot 在 O(shape depth + property count) 内折叠
  overlay，并由后续 Symbol substrate 分组输出 Atom/Symbol。
  `DataPropertyDescriptor` 区分 absent/present-undefined，Object.defineProperty 支持 data value 与
  writable/enumerable/configurable 默认、兼容性检查、deleted-slot 复用和 non-extensible/readonly TypeError；
  Object.getOwnPropertyDescriptor 物化普通属性、Function prototype 与 native name/length flags，keys/assign/
  entries 跳过 non-enumerable。真实 defineProperty 达到 254/2250、getOwnPropertyDescriptor 达到 126/620。
  accessor storage/call、array/string exotic descriptor、完整 ToPropertyKey 与 propertyHelper 所需 array helpers
  仍未完成。
- [x] M3/M5 `Function.prototype.bind`/bound exotic slice：GC-managed `BoundFunctionData` 同时保存规范 immediate
  bound-target chain 与 ultimate call target、first bound-this、exact `Box<[Value]>` argument prefix 和缓存
  name/length；nested bind 仅将 call 参数视图扁平化，call/
  construct 通过 `CallSite`/`Frame` 的 traced prefix view 转发，不逐调用复制 `Vec` 或使用 Rust recursion。
  direct construct 替换 bound newTarget、忽略 bound-this，instanceof 委托 ultimate target，bound callable 无 own
  prototype；native callable 另以 `is_constructor` 限制 prototype 暴露。FunctionLayout 冻结 function length 与
  verified name scope index，未使用 parameter 仍保留 frame register。虚拟 name/length 支持普通 descriptor
  override、delete tombstone 与 non-extensible 语义。N=1/2/4/8/16、forced-major、nested bind、call.bind、
  construct/instanceof、nested newTarget substitution 与 metadata descriptor 均覆盖；真实 bind 目录从实现前
  baseline 推进到 68/200 variants。
  剩余失败主要依赖 accessor、Array/propertyHelper、Reflect/newTarget、cross-realm、Symbol、primitive boxing 与
  Number constants；这些能力未完成前不把整个 bind 目录标为闭合。
- [x] M2/M3/M5 `for-in` 首个纵向切片：owned HIR 保存 declaration/assignment head，verified
  `CreateForInIterator/ForInNext` 使用 GC-managed `Box<[AtomId]>` 快照；原型链上任意 present own key
  屏蔽同名原型键，只有 enumerable key 输出，null/undefined 为空、primitive string 输出 index。
  key/seen storage 由预扫描 upper bound 一次性分配，N=1/2/4/8/16 与 forced-major 已覆盖；同时将
  Object/Array/Function/Error prototype 已发布 builtin 改为 non-enumerable descriptor。定向
  `test/language/statements/for-in` 为 114/198；integer-index 排序、删除后的动态跳过、per-iteration
  lexical environment/head TDZ、destructuring 与 Array push/compareArray 等 harness 依赖仍未完成。
- [x] M2/M5 strictness/sloppy-this slice：compiler 从 Oxc scope flags 将 directive 与继承 strictness 冻结进
  immutable function metadata，删除 VM 重复推断；Realm 精确 root managed global object，script entry
  `this` 指向 global object、module entry `this` 为 undefined，ordinary strict call 原样保留 thisArgument，
  sloppy call 仅将 undefined/null 替换为 global object。N=1/2/4/8/16 与源码端到端覆盖，真实
  directive-prologue 从 28/62 提升到 35/62。primitive this boxing、global binding/property 统一、
  arrow lexical this 与完整 `OrdinaryCallBindThis` 尚未完成。
- [x] M5/M6/M8 native Error abrupt-completion slice：Realm 用独立、不消耗 user-global quota 的
  atom-indexed stable intrinsic slots 统一 `undefined/NaN/Infinity` 与 Error constructors，精确 reserve
  mandatory binding/atom index backing；managed `Error/ReferenceError/SyntaxError/TypeError/RangeError` constructor 与
  prototype hierarchy 支持 call/construct、constructor identity、instanceof 和 forced-major。dispatch 只将
  明确规范错误映射为 JS abrupt completion，资源/GC/verifier 错误仍为 host `ExecutionError`；strict
  unresolved assignment 抛 ReferenceError，sloppy 分支创建 global，N=1/2/4/8/16 对拍；公开只读
  `Isolate::native_error_kind` 让 embedding/test262 runner 不接触 shape/heap ID 即可分类 runtime negative。
  真实 `identifier-resolution/assign-to-global-undefined.js` negative-runtime 1/1 pass，
  directive-prologue 从 35/62 提升到 45/62；完整 name/message ToString/cause/stack、Error attributes、
  AggregateError/SuppressedError 与 `%Object.prototype%` 链仍未完成。
- [x] M5/M8 branded Error object slice：对照 QuickJS `JS_CLASS_ERROR` 与 Escargot `ErrorObject`，Error
  instance 改用独立 GC descriptor 保存 unforgeable `NativeErrorKind` 和 ordinary property base；
  `Error.isError` 不再沿 prototype chain 猜测品牌，伪造 `Error.prototype` 的普通对象返回 false。
  prototype `name/message`、实例 non-enumerable message descriptor、constructor `message` 的 string-hint
  ToPrimitive、Proxy/accessor-aware `InstallErrorCause` 与 `Error.prototype.toString` 的 ordered Get/ToString
  全部通过 typed continuation 接入，不递归进入 Rust interpreter。Error constructor/toString state 使用
  GC-managed fixed traced slots；任何 continuation 已弹出但同步操作仍可能分配的窗口都会临时重新发布 typed
  root，constructor/message/cause/toString 的拆分与组合 forced-major，以及 N=1/2/4/8/16 均覆盖。
  Error Test262 后续 stack 与 constructor-Realm 纵切已提升到 184/186、release-applicable 92/92；剩余 2 个
  non-applicable `Error.isError` cross-realm variants 由 runner 标为 unsupported，不进入当前 release denominator。
  `Reflect.construct(Error, [], foreignNewTarget)` 在 `newTarget.prototype` 非对象时从 newTarget Realm 选择
  `%Error.prototype%`，不再错误回退 active Realm。NativeErrors 的独立 cross-realm 长尾仍按其目录追踪。proposal
  `Error.prototype.stack` 不伪造字符串，留给 debugger/stack-capture substrate；cross-realm、
  AggregateError 与 SuppressedError 由后续独立切片闭合。
- [x] M8 SuppressedError constructor slice：将 SuppressedError 纳入 closed native Error kind、Realm-local
  constructor/prototype hierarchy 与全局 intrinsic table，constructor length=3；复用 Error 的 resumable
  string-hint message conversion 和 fixed five-Value traced state，严格按 message、error、suppressed 顺序发布
  writable/configurable/non-enumerable own data property。call/construct、undefined message、descriptor/order、
  N=1/2/4/8/16 与 forced-major 均覆盖；固定 Test262 从 0/44 提升到 40/44。剩余 4 个 unsupported 分别属于
  shared Proxy GetPrototypeFromConstructor continuation 与 captured-binding frontend 缺口；disposal protocol
  尚未闭合，因此 Error release-target 总项不打勾。
- [x] M6/M11 proposal `Error.prototype.stack` accessor slice：`Error.prototype` 发布 realm-local
  `get stack`/`set stack` native accessor，descriptor 为 non-enumerable/configurable，getter 只接受独立
  `ErrorObject` 的 unforgeable `[[ErrorData]]` brand，Proxy 与 prototype-chain 伪造均返回 undefined；在完整
  debugger frame capture 到位前返回稳定的 native error-kind String，不读取可观察的 name/message 属性。
  setter 精确实现 `SetterThatIgnoresPrototypeProperties`：defining-Realm home identity、String-only value、
  `[[GetOwnProperty]] -> CreateDataPropertyOrThrow/Set(..., true)`，并复用 Proxy get-own/define/set continuation
  保留 trap 顺序、拒绝、异常 identity 与 cross-realm TypeError identity。N=1/2/4/8/16、forced-major 和
  Test262 `test/built-ins/Error/prototype/stack` 70/70 通过；源码位置/async stack 的真实展示仍归 M11 debugger。
- [x] M5.1/M5.2 Object valueOf and Boolean wrapper slice：`Object.prototype.valueOf` 按 ToObject
  语义返回对象 identity，并复用真实 Number/String/Symbol wrapper；新增 GC-managed `BooleanObject`
  保存 traced `[[BooleanData]]` 和 ordinary property base，`Boolean` construct/call 分流、prototype
  `toString/valueOf`、primitive Boolean prototype lookup、Object tag、name/length/non-constructor 与
  forced-major/N=1/2/4/8/16 均覆盖。`Object.prototype/valueOf` 从 8/40 提升到 40/40，Boolean 从
  51/101 提升到 87/101；剩余为 dynamic Function、cross-realm/legacy global 和错误对象映射前置缺口，
  不把 Boolean payload 伪装成 Number/String。
  clean HEAD Apple M5 同一 runner 的 `basic/call-loop` median 为 Tachyon 4.233 ms、Boa 9.434 ms、
  rquickjs 1.555 ms；本机 affinity/governor probe 不可用，故该轮仅作方向性性能回归，不计 release gate。
- [x] M5.1 Object.prototype.toLocaleString observable-call slice：按规范执行
  `Get(receiver, "toString") -> IsCallable -> Call(method, receiver, [])`，不直接调用内部
  `ObjectToString`。独立两阶段 typed continuation 保存原始 receiver，复用 Proxy-aware `[[Get]]` 与
  既有 callback trampoline，getter、Proxy trap、method call 和 abrupt completion 均不递归进入 Rust
  interpreter，也不扩大 104-byte `Frame` 或 ordinary property hot path。普通对象覆盖方法、primitive
  Boolean getter、receiver identity、Proxy `get`、non-callable TypeError、getter throw identity、
  forced-major 与 N=1/2/4/8/16 均覆盖；真实 Test262 从 4/22 提升到 22/22，18 fixed / 0 broken。
- [x] M5.1/M8 Object.prototype.toString `@@toStringTag` slice：先计算 Array/Arguments/Callable/Error 与
  Boolean/Number/String/Date/RegExp 的 compact builtin fallback，再以 typed native continuation 保存 boxed
  receiver/fallback，执行 Proxy/accessor-aware `Get(O, @@toStringTag)`；仅 primitive String 覆盖 fallback，
  最终结果直接按 UTF-16 code unit 精确预留和拼接，不经 Rust UTF-8。Realm 发布真实、可配置的 Iterator、
  Array/String/Map/Set Iterator、Generator/GeneratorFunction、AsyncFunction/AsyncGenerator(Function)、JSON tags，
  async closure 使用独立 `%AsyncFunction.prototype%`。N=1/2/4/8/16、forced-major、Proxy/getter abrupt 与
  non-String tag 覆盖；Test262 `Object/prototype/toString` 从 68/82 提升到 82/82。
- [x] M5.1/M5.2 Object.prototype.isPrototypeOf internal-method slice：严格保留“V非Object先返回false，
  再ToObject(this)”的历史顺序；ordinary prototype chain同步迭代，Proxy节点通过既有observable
  `[[GetPrototypeOf]]` dispatcher和typed parent continuation挂起/恢复，不递归进入Rust interpreter。
  同期修正惰性ordinary function prototype错误使用null原型，改为精确root并继承realm
  `%Object.prototype%`。两层constructor chain、primitive/nullish receiver、Proxy trap getter/call、
  forced-major与N=1/2/4/8/16均覆盖；真实Test262从12/20提升到20/20，8 fixed / 0 broken。
- [x] M5.1 legacy accessor definition slice：安装 `%Object.prototype%.__defineGetter__` 与
  `__defineSetter__`，按 `ToObject(this) -> IsCallable(callback) -> ToPropertyKey(P) ->
  DefinePropertyOrThrow` 顺序构造 enumerable/configurable accessor descriptor。ordinary target 直接复用
  ValidateAndApplyPropertyDescriptor，Proxy target 复用 `[[DefineOwnProperty]]` continuation 并以
  `LegacyAccessor` result mode 映射成功为 `undefined`；key `@@toPrimitive`、callback、trap getter/call 和
  abrupt completion 均不借 Rust unwind。N=1/2/4/8/16、forced-major、getter/setter 合并与 descriptor
  identity 均覆盖；`__defineGetter__` 与 `__defineSetter__` 各从 0/22 提升到 22/22，44 fixed / 0 broken。
- [x] M5.1/M5.2 legacy accessor lookup slice：安装 `__lookupGetter__`/`__lookupSetter__`，共享
  `ToObject -> ToPropertyKey -> ([[GetOwnProperty]], [[GetPrototypeOf]])*` 状态机。ordinary chain同步迭代；
  Proxy get-own mode用内部Hole区分“descriptor absent”与“data/缺失accessor”，只有Hole经typed parent
  continuation继续observable `[[GetPrototypeOf]]`，其余立即返回目标accessor或undefined。Hole在builtin
  返回前必须消费，不进入用户Value；Proxy trap getter/call、nested prototype、data shadowing与abrupt
  completion均复用统一internal-method invariant。N=1/2/4/8/16和forced-major覆盖；两个Test262目录各从
  0/32提升到32/32，64 fixed / 0 broken。
- [x] M5.1/M5.2 legacy `__proto__` accessor consumer slice：在 `%Object.prototype%` 安装
  configurable/non-enumerable getter/setter pair，getter执行ToObject后复用Proxy-aware
  `[[GetPrototypeOf]]`，setter保留RequireObjectCoercible、invalid proto/primitive receiver no-op与
  `[[SetPrototypeOf]]` false-throw顺序。Proxy consumer增加LegacyAccessor result mode，成功统一返回
  undefined；ordinary cycle walk遇到Proxy exotic即停止，并将realm `%Object.prototype%` 固定为
  immutable-prototype exotic。N=1/2/4/8/16、forced-major、direct Proxy traps、cycle/non-extensible和
  metadata覆盖；Proxy `[[Set]]` 接入后Test262从0/30提升到30/30。完整M5.2仍不打勾。
- [x] M5.2 Proxy `[[Set]]` assignment surface slice：新增独立 `ProxySet` typed continuation/state，
  覆盖四参数 `set` trap、trap getter/call、缺省 trap 的 ordinary target forwarding、assignment 与
  `Reflect.set` result mapping，以及 strict false rejection。普通 receiver 继续走原有同步热路径；
  N=1/2/4/8/16、forced-major 与 `__proto__` inherited-set consumer 已覆盖。target descriptor
  invariant、nested Proxy forwarding 与完整 exotic receiver descriptor 检查仍属于后续闭合，M5.2 总项不打勾。
- [x] M5.2 Proxy `[[Set]]` invariant/prototype-boundary slice：ordinary assignment resolver像read
  resolver一样返回`Write | Proxy`，在prototype链的exotic边界保留原始receiver进入同一dispatcher；
  truthy trap复用own descriptor与SameValue检查non-configurable/non-writable data及setter-less accessor。
  对receiver Proxy且descriptor traps均为missing/nullish的缺省转发使用等价直接写入，observable trap不被吞掉。
  N=1/2/4/8/16和forced-major覆盖；Proxy/set从26/54提升到40/54，剩余缺口转入下一切片。
- [x] M5.2 Proxy receiver/exotic forwarding slice：新增 `ReceiverGetOwn -> ReceiverDefine` typed parent
  stages，复用既有 Proxy `[[GetOwnProperty]]`/`[[DefineOwnProperty]]` invariant dispatcher；Reflect.set
  也在 ordinary prototype exotic boundary 停止并保留 receiver。补齐 ArraySetLength shrink、RegExp virtual
  read-only flags、String wrapper own-index read/descriptor与Function virtual prototype value-only define。
  internal get-own `SetReceiver` mode只发布absent/writable/blocking三态，避免为OrdinarySet物化无用descriptor；
  ProxySet state、get-own trap state和descriptor result edges在任何intern/materialize allocation前发布到
  register/continuation。N=1/2/4/8/16与完整nested forced-major覆盖。
  Test262 `Proxy/set` 从40/54提升到50/54；剩余2个indexed accessor descriptor前置缺口和2个cross-realm
  failure，完整M5.2仍不打勾。
- [x] M5.2 Array ordinary-prototype mutation prerequisite：`ordinary_set_prototype_of` 对明确使用ordinary
  internal methods的Array exotic更新其ordinary base prototype并发布write barrier；不把所有内嵌
  `OrdinaryObject`的exotic自动视为ordinary。N=1/2/4/8/16与forced-major覆盖Array prototype Proxy的indexed
  `[[Set]]` receiver forwarding，Test262 `Proxy/set` 从50/54提升到52/54；剩余2个cross-realm failure属于
  `$262.createRealm`宿主能力缺口，完整M5.2仍不打勾。
- [x] M5/M8 boxed Number/valueOf/radix-toString slice：独立 GC-managed `NumberObject` 保存 traced
  `[[NumberData]]` 与 ordinary property base，`Number.prototype` 自身为 `+0` wrapper，`new Number` 保留
  newTarget prototype；numeric primitive property lookup 直接从 Number prototype 起步而不分配临时 wrapper，
  `thisNumberValue` 对 primitive/wrapper 共用严格 brand check。`valueOf`、十进制与 radix 2..36 shortest
  round-trip `toString`、非法 radix RangeError、descriptor、prototype/instanceof/Object tag 与 forced-major
  均覆盖；non-decimal formatter 使用集中 tuning 的 2200-byte stack scratch、checked cursor/digit 和
  adjacent-double/ties-to-even 算法，不引入 BigInt allocation。同期修正 compiler 将 `!==` 错降为
  LooseEqual+Not 及 `!=` count pass 少算一条指令的问题，使 cross-source test262 assert harness 恢复严格语义。
  真实 `toString` 为 168/180；随后复用 pinned `ryu-js` 的 ECMAScript fixed formatter 实现精度 0..100
  的 `toFixed`，覆盖 exactness/1e21/NaN/Infinity/RangeError，并对两个 primitive conversion method 均
  不可调用的对象完成确定 TypeError fast path，可调用方法仍等待 reentry continuation。`toFixed` 为 22/32，
  随后以 32-limb 固定栈 bigint 表示精确 binary64 rational，实现 `toExponential` 最短形式、显式
  0..100 位精度、十进制 ties-up/carry、NaN/Infinity/负零与 RangeError 顺序，不调用 Rust float
  formatting、不引入 heap bigint；集中 tuning 的 scratch 上限覆盖最大/最小 binary64。为避免缺失方法
  造成 `undefined.call` 假通过，首个 GC-managed Symbol primitive identity/typeof/不可构造/ToNumber-TypeError
  substrate 同批落地，但不宣称完整 Symbol builtin。`toExponential` 为 20/30，整个
  `test/built-ins/Number` 从 478/680 推进到 512/680；相对前一 slice 增加 34 fixed、0 broken。随后
  抽取共享 exact significant-digit generator，实现 `toPrecision` 1..100 位、舍入后 fixed/exponential
  branch、undefined ToString 与非有限值顺序；ryu shortest display exponent 另经 exact ratio 校正为
  `floor(log10(binary64))`，覆盖 `1e-21` 实际略小于十进制边界的 case。`toPrecision` 为 20/34，
  整个 Number 达到 530/680；相对 toExponential slice 增加 18 fixed、0 broken。随后以 traced
  `Completion::Native` 建立不依赖 Rust recursion/unwind 的 callback trampoline，Number 四个带可选 numeric
  参数的方法按 number hint 执行 `valueOf`/`toString`、primitive short-circuit、object fallback、用户 throw
  和 TypeError；callback 自身 catch 的 completion base 位于 native continuation 之后，避免异常截断误删
  suspended state。N=1/2/4/8/16、forced-major、内部/外部 catch 与源码端到端均覆盖，Frame 保持 104 bytes。
  `toFixed` 为 24/32、`toExponential` 为 24/30、`toPrecision` 为 28/34，整个 Number 达到 544/680；
  相对 continuation 前增加 14 fixed、0 broken。随后把同一 trampoline 提升为多 consumer：String call 按
  string hint 执行 `toString`/`valueOf`、object fallback、内部 catch 与用户 throw，continuation 在任何 callback
  environment allocation 前进入 traced completion 栈；同时两个 delete opcode 共享 active-frame strictness，
  对 non-configurable property 实现 sloppy false / strict TypeError。String consumer 的 N=1/2/4/8/16、
  forced-major 与源码顺序测试覆盖，test262 propertyHelper 因其 eager `String(desc)` 得以运行。Number 达到
  604/680，`toFixed` 30/32、`toExponential` 30/30、`toPrecision` 34/34；相对前一批 60 fixed、0 broken、
  2 条由 unsupported 推进为 semantic-failure。随后 Number call/construct 复用 number-hint consumer：construct
  在 conversion 成功后才读取 `newTarget.prototype` 并分配 wrapper，call 返回 primitive；construct bit 使用
  continuation 既有 padding，Frame 104 bytes、Continuation/Completion 32 bytes 均不增长。object fallback、
  call/construct abrupt 与 wrapper brand 源码测试覆盖，Number 达到 614/680；相对前一批 10 fixed、0 broken、
  2 条由 unsupported 推进为 semantic-failure。随后以 2-byte `ConversionConsumer` 把 continuation 从
  NativeFunction identity 解耦，并接入 Opcode::ToNumber；primitive fast path 保留在 hot dispatch，object
  branch 单独 cold/noinline。unary `+` 的 valueOf-first、toString fallback、内部 catch、用户 throw以及
  N=1/2/4/8/16/forced-major 均覆盖，Frame 104 bytes、Continuation/Completion 32 bytes 仍不增长；真实
  unary-plus 目录当前 18/34，剩余主要由独立 ReferenceError/legacy-global 与 BigInt 缺口造成。随后复用同一
  cold builder 接入 Negate/BitwiseNot，callback 返回后由 consumer 唯一一次完成 ToNumber 与最终操作；primitive
  fast path 不增加动态分派。两者的 fallback/throw、N=1/2/4/8/16/forced-major 均覆盖，unary-minus 从
  10/28 提升为 14/28，bitwise-not 从 18/32 提升为 26/32，合计 12 fixed、0 broken。其他通用 opcode
  随后以 `BinaryLeft`/`BinaryRight` 两阶段 consumer 接入 Sub/Mul/Div、bitwise、shift、remainder 与
  exponentiation；复用 continuation 的 receiver 槽依次 trace pending right/converted left，保持 32 bytes，
  左右 callback 顺序、右 method mutation、左 abrupt、内部 catch 和 N=1/2/4/8/16/forced-major 均覆盖。
  当前 subtraction 43/75、multiplication 45/79、division 47/89、bitwise and/or/xor 各 43/59、left-shift
  73/89、right-shift 57/73、unsigned-right-shift 73/89、exponentiation 72/88。随后以 AddLeft/AddRight 独立
  consumer 完成 default-hint、string concatenation、Symbol TypeError 与 numeric fast path；sentinel 前移到
  method lookup 之前，左临时 String 跨右 callback 保持 traced。拼接一次精确 reserve，ASCII/Latin-1 结果
  压缩为单字节 backing，宽结果接管 owned UTF-16；N=1/2/4/8/16/forced-major 与顺序/abrupt/fallback 均覆盖，
  addition 从 25/95 提升到 59/95，34 fixed、0 broken。relational string comparison 使用独立 consumer。
  随后以 RelationalLeft/RelationalRight 保留原始左到右 callback 顺序与 primitive kind；双 String 用 root 后的
  JsStringView 直接按 UTF-16 code unit 无分配比较，其他组合唯一一次 ToNumber，number/number 保留 dispatch
  fast path。四运算符 string/number/object、mutation/abrupt、N=1/2/4/8/16/forced-major 均覆盖；`<` 从
  53 到 71/89、`>` 从 47 到 79/97、`<=` 从 45 到 79/93、`>=` 从 45 到 71/85，净增 110 pass；剩余
  semantic failure 均为 direct eval whitespace tests，unsupported 均为 BigInt，不能以假 global shim 代替
  M5 direct-eval scope/strictness。其他通用 opcode ToPrimitive、
  `@@toPrimitive`、accessor/Proxy、Symbol
  prototype/well-known registry/property keys、BigInt 与 cross-realm 尚未完成。随后将 `SymbolObject` 作为独立
  GC payload 接入，`%Symbol.prototype%` 使用 absent `[[SymbolData]]`，`Object(symbol)` 生成保留 traced
  Symbol data 的 ordinary wrapper；属性 storage、shape mutation、write barrier 与 `thisSymbolValue` brand
  check 全部走共同 object contract。完整 descriptor/cross-realm 仍待 M8。
- [x] M5.3 global intrinsic object-environment read slice：loaded-code 仍缓存 stable `IntrinsicSlotId` 做名称
  分类，但 intrinsic 标识符值改从当前 Realm global object 的同名 data property 读取，不再从
  `IntrinsicBinding.value` 暴露第二份状态；`global.Object = replacement` 后 `Object` 立即观察 replacement。
  N=1/2/4/8/16 与真实 `Object.getOwnPropertyDescriptors/tamper-with-global-object` 覆盖。direct assignment
  同样写入 canonical data property，并按 strictness 处理 non-writable rejection。global
  accessor Get/Set、普通 var/function property publication 和最终删除 value storage 仍归后续
  object-environment continuation，M5.3 总项不打勾。
- [x] M5.3 immutable global descriptor flags：global object publication 对 `undefined`、`NaN`、`Infinity`
  明确使用 non-writable/non-enumerable/non-configurable，不再把所有 intrinsic globals 统一标成
  configurable。N=1/2/4/8/16 覆盖 descriptor flags；该修复同时服务 descriptor、delete 与 integrity-level
  consumers，不以 Date/function descriptor 假壳掩盖尚未实现的 builtin。
- [x] M5.2 getOwnPropertyDescriptor primitive ToObject：non-nullish primitive target 在 resumable key conversion
  前进入 truthful wrapper allocator，String primitive 因而暴露 index/length exotic descriptor；wrapper 由
  pending key operand跨 callback/GC 精确保持。N=1/2/4/8/16 与 Test262 primitive-string 覆盖。
- [x] M5.2 primitive own-query ToObject：`Object.hasOwn` 在 key conversion 前 boxing target；prototype
  `hasOwnProperty/propertyIsEnumerable` 在 key completion 后 boxing receiver，以保留各自 observable ordering。
  三者共享 String exotic descriptor core，N=1/2/4/8/16 覆盖 primitive index presence/enumerability。
  受影响目录分别为 `hasOwn 124/124`、`hasOwnProperty 126/126`、`propertyIsEnumerable 32/32`；完整
  Object 子树当前 `6120/6802`，剩余失败主要转为 Date/RegExp/TypedArray/legacy harness 缺口。
- [x] M5.2 TestIntegrityLevel builtin slice：注册 `Object.isSealed`/`Object.isFrozen`，复用完整 own descriptor
  与 extensible state；ordinary object/function/array、primitive return 和 N=1/2/4/8/16 已覆盖。真实目录
  `isSealed 60/66`、`isFrozen 112/118`；完整 Object 子树推进到 `6120/6802`，剩余主要为 Date/Proxy
  ownKeys/unsupported syntax，Proxy 不以同步
  shortcut 冒充完成。
- [x] M5.2 Proxy TestIntegrityLevel continuation：新增 integrity ownKeys modes，先可恢复执行
  `[[IsExtensible]]`，再复用 `PendingProxyOwnKeys` 按 key 顺序调用 Proxy `[[GetOwnProperty]]`；无需新 GC
  payload 或 Frame/Continuation 字段。missing-ownKeys ordering 的 isSealed/isFrozen Test262 strict/sloppy
  全过，N=1/2/4/8/16 覆盖 descriptor callback suspension；目录提升为 isSealed 62/66、isFrozen 114/118。
- [x] M5.2 Object.create descriptor-map reuse slice：删除仅支持 ordinary data descriptor 的同步旁路；新对象
  在 destination root 发布后直接进入与 `Object.defineProperties` 相同的 `PendingDefineProperties` 状态机，
  因而复用 exact-capacity key/descriptor storage、getter suspension、Proxy ownKeys/GetOwn/Get 与收集后原子
  mutation。N=1/2/4/8/16 覆盖 nested descriptor getter 和 Proxy trap order；真实 Object/create 从约
  468/640 提升到 604/640；随后补齐 Properties primitive ToObject 的 zero-own-key allocation-free fast path，
  Boolean/Number/Symbol/empty String 直接返回 target，非空 String 确定在首个 descriptor 转换抛 TypeError。
  完整 M5.2 仍不打勾。
- [x] M5.1 URI global encoding/decoding slice：独立 `GlobalUriFunction` 元数据和 `builtins/uri.rs` 实现
  `encodeURI`/`encodeURIComponent`/`decodeURI`/`decodeURIComponent`；encode 对 UTF-16 代理对执行严格
  Unicode scalar 校验并预扫描得到 exact output capacity 后一次 reserve，decode 按输入长度 reserve、校验 `%XX`
  UTF-8 canonical form、拒绝 overlong/surrogate/out-of-range，并按规范保留 `decodeURI` reserved escapes。
  对象参数复用 string-hint `ConversionConsumer` continuation，格式错误统一映射 managed `URIError`，无
  新 continuation payload。N=1/2/4/8/16 源码回归覆盖；定向 Test262：encodeURI 50/62（其余为缺失
  `String.prototype.toUpperCase`/前端 syntax）、decodeURI 106/110、encodeURIComponent 50/62、
  decodeURIComponent 110/112。完整 M5 仍不打勾。
- [x] M5.1 String default case conversion slice：注册 `toUpperCase`/`toLowerCase`，新增不扩张
  Frame/Completion 的 receiver-ToString native continuation，object receiver 按 string hint 可恢复执行
  `@@toPrimitive -> toString -> valueOf`。有效 UTF-16 交给 Rust Unicode Default Case Conversion，覆盖
  Final Sigma 与多 code-point expansion；unpaired surrogate 分隔有效段并原样保留。通用 ToString 现在对
  Symbol 抛 TypeError，只有显式 `String(symbol)` 走独立 canonical formatter。N=1/2/4/8/16、String
  wrapper/object callback/Symbol/error/Unicode 边界覆盖；directive completion 修复后 Test262
  `toUpperCase 52/52`、`toLowerCase 60/60`；URI encode 两目录同步提升至 58/62，
  semantic failure 清零，余项均为前端 unsupported syntax。`%String.prototype%` 同步从错误的 ordinary
  object 修正为带空 `[[StringData]]` 的老年代 String exotic object，直接作为 generic method receiver
  时按空字符串工作；补齐 `string_prototype` 与前一切片 `global_uri_functions` 的 Realm trace，初始化期和
  forced-major callback continuation 均有精确 root 覆盖。完整 M5 仍不打勾。
- [x] M5.1 locale-insensitive String case aliases：注册 `toLocaleUpperCase`/`toLocaleLowerCase`，复用同一
  receiver-ToString continuation 和 Unicode default case kernel；当前 release 的定向 Test262 不含 locale
  tag-specific behavior，故不引入伪造的 ICU/locale provider。directive completion 修复后目录分别
  `52/52`、`56/56`；后续 locale-aware Turkish/Azeri/Lithuanian 数据接入时保留独立 native identity。
- [x] M5.3 script directive completion/eval bridge slice：Oxc `Program.directives` 不再从 owned HIR 丢失，
  directive string 按源码顺序作为 expression completion lower，因而单字符串 `eval('"bj"')` 返回真实
  String 而非 undefined。Nested same-realm `execute_in_realm` 的 `RunOutcome::Thrown` 通过显式
  `ExecutionError::HostThrown(Value)` 回到外层 interpreter 的 `throw_value`/handler 路径；VM 覆盖
  nested completion、outer catch、N=8，大小写目录 direct-eval strict/sloppy 两个剩余失败清零。完整
  direct-eval lexical scope/var binding、with、Annex B 仍未完成，M5.3 总项不打勾。
- [x] M2/M5.3 direct-eval caller binding slice：新增 verified `DirectEval`，仅 syntactic direct call 且
  callee identity仍为当前Realm eval intrinsic时携带caller lexical environment；alias/comma/cross-realm路径
  保持indirect。含direct eval的activation将parameter/var/lexical binding按exact estimate全部提升到named
  environment slot，runtime environment只保存冷 `(CodeId, FunctionId)` owner并回查immutable metadata，
  不扩大104-byte Frame、不永久atomize local name。动态load/store、write barrier、nested throw、forced-major与
  N=1/2/4/8/16已覆盖；eval-code从111/454提升至118/454。sloppy eval新增var/function binding仍需sparse
  variable-environment overlay，strict eval独立declarative record/strictness继承、with与Annex B继续不打勾。
- [x] M5.3 eval non-String argument slice：direct/indirect eval在host compile callback前检查primitive String
  identity，undefined/null/Boolean/Number/Symbol/Object均原样返回且不执行ToString；N=1/2/4/8/16与
  forced-major覆盖。
- [x] M5.3 direct eval caller-strictness slice：`EvalKind`携带caller strictness，host compile contract以
  `"use strict"; void 0;` prologue触发Oxc strict early errors且不污染empty/declaration-only completion；
  compile diagnostics经专用`InvalidEvalSource`进入managed SyntaxError，而结构化unsupported不伪装成
  syntax failure。strict reserved-word与unresolvable assignment、N=1/2/4/8/16、forced-major已覆盖；direct
  eval分区达99/336，完整eval-code因正确分类strict early-error从118/454升至185/454（104 semantic、165
  unsupported）。strict eval var/function/lexical声明的独立record仍由下一纵切闭合。
- [x] M5.3 direct eval var-environment overlay slice：对照QuickJS var object，将verified entry bytecode的
  `DeclareScope`双遍扫描后按exact declaration count分配GC-managed `EvalVar` record；sloppy function eval
  overlay以activation depth为owner并跨eval持久化，strict eval overlay只root于child fiber，全局sloppy eval
  继续使用global record。已有caller slot不复制，新var/function、重复eval更新、nested closure读取、nested
  owner shadowing、frame/tail-call清理、write barrier、N=1/2/4/8/16与forced-major均覆盖；function declaration
  显式`DeclareScope`后不再从StoreScope猜声明。完整eval-code达203/454，direct ES5除`with` unsupported外
  全部通过；lexical declaration record、delete/configurability、parameter-expression varEnv与Annex B仍待闭合。
- [x] M5/M10 non-simple parameter eval-var conflict slice：immutable environment-slot metadata 显式标记
  parameter binding，sloppy direct eval 声明实例化若跨越参数表达式环境并与参数同名，则在执行 eval body
  前产生 managed SyntaxError；simple parameter list 的同名 var 仍复用既有 binding，strict eval 保持隔离。
  不扩大 Frame/Environment 热布局，VM N=1/2/4/8/16 与 forced-major 回归通过；async-function 目录从
  130/133 提升到 131/133，剩余 2 项均为 `with`/`Symbol.unscopables` unsupported。完整 parameter-expression
  varEnv 的 nested lexical/Annex B 交互仍由总项追踪。
- [x] M5/M8 FinalizationRegistry JS binding：新增 40-byte registry header 与 32-byte GC-managed linked
  registration cell，registry 内不保存可扩张 Rust `Vec`；cell 弱持 target/token、强持 registry/held value/next，
  collector pending record 保存真实 cell owner。constructor/register/unregister、object 与 ordinary Symbol、
  repeated-token 全注销、brand/metadata/`@@toStringTag`、N=1/2/4/8/16、forced-major 已覆盖。cleanup callback
  复用 Promise job checkpoint 和 typed native continuation，不递归进入解释器；normal/throw 都消费当前 job，
  held identity 与 callback rooting 经真实 major-GC 测试。FinalizationRegistry 为 88/94，剩余 2 cross-realm
  与 4 observable `GetPrototypeFromConstructor` variants 是共享 construct continuation 缺口。
- [x] M8 Map/Set iterable-constructor vertical slice：`PendingCollectionInitializer` 通过 typed native
  continuation 保存 target/iterator/cached adder 与当前 entry；`new Map(iterable)`/`new Set(iterable)` 按
  `adder -> @@iterator -> next -> done/value -> adder` 的 observable 顺序恢复，支持 Array 与用户 iterator、
  被覆盖的 `set`/`add`、bytecode callback frame 和 forced-GC roots。Map 目录从 199/405 到 241/405，Set 从
  261/764 到 345/764；通用 `IteratorClose`、forEach、Weak collections 和 hash specialization 仍属于后续工作。
- [x] M8 Map/Set `forEach` live-cursor slice：独立 `collection_for_each` 模块以 traced callback state 和
  continuation 逐项调用用户 callback；callback mutation 后重新读取 collection backing，涵盖删除、追加、
  重插、thisArg、参数顺序和 callback throw。Map `forEach` 为 34/36、Set 为 60/64；残余失败是 Weak collection
  brand 与 arrow lexical-this，不归入该 builtin 实现。
- [x] M8 Map `getOrInsertComputed` continuation slice：callback 在 typed native continuation 外执行，
  traced pending state 保留 Map/canonical key/callback；present key 仍先验证 callback，absent key 将 callback
  的 normal completion 无条件写回（覆盖 callback 内对同 key 的 mutation），throw 原样传播。proposal 目录
  通过 30/37；其余由 Function constructor、for-of binding/Error harness 或 parser 前置限制导致。
- [x] M8 WeakMap/WeakSet ephemeron core slice：弱集合采用独立、精确 external-memory 计费的
  fixed-capacity `WeakCollection`，slot 保存 GC `Ephemeron` 而非强 `Value` 对；扩容发布替换 backing，
  ordinary property receiver、realm/global intrinsic、brand check 及 resumable iterable constructor 一次接通。
  `WeakMap` applicable 从 0/204 到 162/204，`WeakSet` 从 0/170 到 144/170；剩余项集中在现有 well-known
  Symbol 静态属性、`@@toStringTag`、Reflect/cross-realm 及 proposal upsert，不能把这些错误归为弱边语义。
- [x] M8 WeakMap/WeakSet stable-address hash slice：private backing 保留 insertion-ordered ephemeron entries，
  另以 `RawHeapRef` logical address 建 power-of-two open-addressing index，单次 no-GC borrow 完成 expected O(1)
  lookup；delete 用 tombstone，GC clear 后原地重建 index/free-list，growth 仅复制 live ephemeron。
  collision/delete/collector-clear/growth 与跨 span hash avalanche 回归覆盖，修复
  staging deep-weakmap 99,999-entry chain 的 O(n^2) slot scan，同时保持 external-memory 精确计费。
- [x] M8 fixed WeakRef JS binding slice：新增 32-byte `WeakRefObject`，只保存 collector-cleared
  `WeakGcRef<()>` 与 ordinary base；constructor/deref 复用 `CanBeHeldWeakly`，支持 object、ordinary Symbol
  与 well-known Symbol，拒绝 primitive 和 registered Symbol。Heap 公开受验证的 `add_to_kept_objects`
  contract，constructor/deref 保活到显式 job boundary，wrapper 存活但 target 无强 root 时 major GC 清除
  weak edge。Realm constructor/prototype、brand、metadata、`@@toStringTag`、N=1/2/4/8/16、forced-major 和
  payload layout 已覆盖；FinalizationRegistry 发布后 `test/built-ins/WeakRef` 从 0/58 到 52/58。其余 6
  variants 分属 cross-realm fallback 与 observable accessor `GetPrototypeFromConstructor`，故完整 M8.1 weak
  builtins 总项不打勾。
- [x] M5/M8 ProxyCreate substrate：对照 QuickJS `JSProxyData` 与 Escargot `ProxyObject::createProxy`，
  新增无 ordinary base 的独立 GC-managed Proxy payload，target/handler 精确 trace，构造前按顺序验证
  两者均为 Object；`%Proxy%` 全局 binding、name/length、Function prototype identity 与“constructible
  但无默认 prototype property”契约接入，forced-major identity 测试覆盖。Proxy 根级 constructor/create
  文件中可独立归因的 44 variants 通过；全目录显示 164/607，但其中 invariant tests 可能因尚未接 trap
  的统一 TypeError 早抛而假阳性，禁止把该数字当作 Proxy internal methods 完成度。call/construct
  capability、revocable 与完整 typed exotic dispatch/continuation 仍未完成，M5.2 总项不打勾。
- [x] M5.2 Proxy prototype/extensibility internal-method slice：对照 QuickJS
  `js_proxy_getPrototypeOf/js_proxy_isExtensible/js_proxy_preventExtensions` 与 Escargot 对应 virtual
  internal methods，接入 `[[GetPrototypeOf]]`、`[[IsExtensible]]`、`[[PreventExtensions]]` 的 trap lookup、
  target forwarding、trap result coercion 与 non-extensible invariant。accessor-backed trap getter 和随后
  bytecode trap call 均通过 32-byte typed native continuation 在 Rust 栈外恢复，N=1/2/4/8/16 与
  forced-major 覆盖；Object/Reflect 六个入口共用单一 cold Proxy dispatch helper。过程中修复 Realm atom-slot
  索引表在发布较低 AtomId 时错误截断较高映射的问题并加入独立回归。固定 test262 checkout 当前为
  getPrototypeOf 26/38、isExtensible 14/24、preventExtensions 16/23，Reflect 总目录 286/306；本切片提交时
  nested Proxy forwarding、revocable、instanceof exotic traversal 与 cross-realm 仍未完成，因此不勾选
  完整 Proxy/M5.2，nested forwarding 的后续闭合见下一项。
- [x] M5.2 nested Proxy forwarding slice：absent/nullish trap 的 Proxy target 不再进入 ordinary snapshot，
  同步 absent 链以迭代 dispatch 前进，accessor getter 返回 nullish 后重新进入同一 typed exotic boundary。
  `Object.preventExtensions` 使用 traced parent continuation保存最外层 Proxy，将嵌套 target 的布尔
  `[[PreventExtensions]]` completion映射为 throw-on-false/return-original-object，且不扩大32-byte entry。
  源码回归同时覆盖 accessor-undefined、bytecode traps、三种 internal methods与 outer identity；test262
  getPrototypeOf 由26提升到32/38、isExtensible由14提升到20/24、preventExtensions由16提升到18/23。
  Object.preventExtensions目录为70/78。其余失败集中在revocable、instanceof/cross-realm与一条module
  namespace前端限制，完整Proxy仍不打勾。
- [x] M5.2 `Proxy.revocable` slice：对照 QuickJS function-data revoker与Escargot
  `ExtendedNativeFunctionObject` private slot，新增不暴露公开属性/side table的
  `FunctionExecutable::ProxyRevoker(Value)`；首次调用先将私有slot置null，再无分配地清空Proxy target/handler，
  后续调用幂等返回undefined并及时断开三条强GC edge。`Proxy.revocable`一次构造`proxy,revoke`两槽
  shape/storage结果，避免内部Vec增长；forced-major覆盖创建与edge清空，且FunctionExecutable保持16 bytes。
  revocable目录从0提升到30/35；getPrototypeOf提升到34/38、isExtensible到22/24、preventExtensions到
  20/23。余下revocable失败是Proxy `[[Get]]`、callable Proxy与cross-realm缺口，完整Proxy仍不打勾。
- [x] M5.2 ordinary `Object.setPrototypeOf` prerequisite：发布name/length为`setPrototypeOf`/2的
  non-constructor builtin，按规范顺序执行RequireObjectCoercible、prototype Object/null校验、primitive原值
  返回与object throw-on-false；mutation复用`ordinary_set_prototype_of`的identity、cycle、extensibility与
  write-barrier契约。源码回归覆盖identity/inheritance、cycle、non-extensible与primitive，test262目录从
  8提升到20/24；余下2个semantic failure属于Proxy `[[SetPrototypeOf]]`，2个unsupported属于前端限制。
- [x] M5.2 Proxy `[[SetPrototypeOf]]` slice：对照QuickJS `js_proxy_setPrototypeOf`与Escargot
  `ProxyObject::setPrototype`，以GC-managed fixed-capacity `NativeCallState`保存`(target, prototype)`及
  invariant identity，并由activation-aligned side table向bytecode trap提供参数而不扩大104-byte `Frame`。
  accessor GetMethod、dynamic trap、nested Proxy forwarding、Reflect boolean/Object throw-on-false与
  不可扩展target的IsExtensible/GetPrototypeOf invariant均使用typed continuation；只在首次observable
  callback分配状态。forced-major回归验证handler `this`与两个参数，并覆盖dispatch batch 1/2/4/8/16；
  Proxy/setPrototypeOf为30/34，剩余2个cross-realm failure属于`$262.createRealm`缺口，2个unsupported属于
  accessor descriptor前端缺口。完整Proxy/M5.2仍不打勾。
- [x] M5.2 Proxy `[[HasProperty]]` slice：对照QuickJS `js_proxy_has`与Escargot
  `ProxyObject::hasProperty`，Opcode `in`和`Reflect.has`共用prototype-aware dispatcher，ordinary chain遇到
  Proxy时转入typed slow path；GetMethod accessor/data trap、handler `this`、canonical `(target,key)`参数、
  nullish/missing nested forwarding、revoked/non-callable/abrupt与ordinary target的non-configurable/
  non-extensible false invariant均已覆盖。NativeCallState复用固定参数源，Frame/NativeContinuation不增大；
  forced-major及batch 1/2/4/8/16回归通过。Proxy/has从8/43提升到26/43，Reflect/has从18/20提升到
  20/20；余下9个with前端unsupported、2个cross-realm、2个Array exotic setPrototypeOf及4个
  String/RegExp/callable-Proxy依赖。outer trap=false且target为Proxy的完整invariant已由后续resumable Proxy
  `[[GetOwnProperty]]`切片闭合；完整Proxy/M5.2仍不打勾。
- [x] M5.2 Proxy `[[GetOwnProperty]]` slice：对照QuickJS `js_proxy_get_own_property`并补齐其源码标注
  incomplete的compatibility check，同时以Escargot/规范覆盖non-configurable+writable强化约束。Object/Reflect
  `getOwnPropertyDescriptor`、`Object.hasOwn`、`hasOwnProperty`、`propertyIsEnumerable`共用typed internal
  method；GetMethod accessor/data trap、handler `this`、canonical `(target,key)`、undefined/object result、
  target descriptor、conditional extensibility、六字段resumable ToPropertyDescriptor、
  CompletePropertyDescriptor与IsCompatiblePropertyDescriptor严格按observable顺序执行。nested Proxy target的
  `TargetGetOwn/TargetIsExtensible`使用parent continuation，target descriptor/extensibility在GC-managed五槽
  state中跨callback保存；同步child错误清理parent，且Proxy Has的nested false invariant不再保守拒绝。
  descriptor-field accessor、nested target两种bytecode trap、forced-major与batch 1/2/4/8/16回归通过；
  Proxy/getOwnPropertyDescriptor从16/42提升到30/42，Reflect目录24/26提升到26/26，Object.hasOwn为
  124/124、propertyIsEnumerable为32/32。余下12个Proxy目录失败分别依赖cross-realm、Proxy `[[Get]]`/
  `[[Delete]]`或String/function exotic own descriptors，完整Proxy/M5.2仍不打勾。
- [x] M5.2 Proxy `[[Get]]` opcode/Reflect纵向切片：对照QuickJS `js_proxy_get`、Escargot
  `ProxyObject::get`与ECMA-262保留原始Receiver，bytecode属性读取和`Reflect.get`共享Proxy-aware dispatcher；
  ordinary chain使用不物化descriptor的静态resolver，首次遇到Proxy才进入cold typed continuation。
  GetMethod accessor/data trap、handler `this`、canonical `(target,key,Receiver)`、missing/nullish nested转发、
  revoked/non-callable/abrupt及non-configurable data/accessor invariant均已接入；data invariant使用SameValue，
  保留NaN相等和正负零区分。同步nested target链迭代前进，nested target descriptor通过parent continuation
  挂起，child同步错误清理parent。NativeCallState第五槽先root待调用callee、trap完成后复用为result，修复
  forced-major在state分配点移动callee后的陈旧引用；batch 1/2/4/8/16均覆盖handler与三参数identity。
  Proxy/get从8/38提升到36/38，剩余2项仅为cross-realm基础设施；Reflect/get为22/22。转换、descriptor、
  iterator、collection、argument-list和其他builtin consumer仍需迁移到统一resumable `[[Get]]`边界，因此
  完整Proxy/M5.2仍不打勾。
- [x] M5.2 Proxy `[[Delete]]` opcode/Reflect纵向切片：对照QuickJS `js_proxy_delete_property`、Escargot
  `ProxyObject::deleteOwnProperty`与ECMA-262，以`Reflect/Sloppy/Strict` mode统一Reflect布尔返回和
  DeletePropertyOrThrow边界；ordinary target保持无状态直达，Proxy data/accessor trap才分配固定五槽
  NativeCallState并向bytecode trap提供canonical `(target,key)`。handler `this`、GetMethod nullish/callable、
  revoked/abrupt、false短路、missing/nullish nested Proxy迭代转发、target `[[GetOwnProperty]]`与
  `[[IsExtensible]]` parent continuation均已接入；同步child错误清理parent。true结果拒绝隐藏
  non-configurable own property，也实现QuickJS源码仍标记`proxy-missing-checks`的“configurable own property
  位于不可扩展target”约束。state第四槽在分配safepoint先root callee，accessor getter发布前重新读取callee与
  handler，forced-major及batch 1/2/4/8/16覆盖handler和两参数identity。Proxy/deleteProperty从6/30提升到
  24/30，剩余2个cross-realm、2个String/RegExp exotic delete依赖及2个在`Object.defineProperties` setup
  即失败的前置builtin缺口；Reflect/deleteProperty从20/22提升到22/22。完整Proxy/M5.2仍不打勾。
- [x] M5.2 Proxy `[[DefineOwnProperty]]` Object/Reflect纵向切片：复用既有六字段resumable
  ToPropertyDescriptor consumer，不重复读取descriptor getter；missing/nullish trap保持不物化descObj且对nested
  Proxy迭代转发。首次真实trap分配GC-managed `PendingProxyDefine`保存presence-aware Desc、outer result identity、
  active Proxy与callee；FromPropertyDescriptor逐字段从managed state重新读取，forced-major后不使用Rust局部旧引用，
  再压缩为既有五槽NativeCallState向trap提供`(target,key,descObj)`及captured handler。Object模式false抛错/
  success返回原始Proxy，Reflect模式返回boolean；handler `this`、revoked/non-callable/abrupt、descriptor/trap
  accessor、nested target `[[GetOwnProperty]]`/`[[IsExtensible]]` parent continuation及同步child错误清理均已覆盖。
  invariant复用统一IsCompatiblePropertyDescriptor core，并补齐QuickJS仍标记missing-check的non-configurable
  writable target不能报告writable=false强化约束。forced-major覆盖descriptor value heap edge与nested target，
  batch 1/2/4/8/16覆盖完整双getter和三参数identity。Proxy/defineProperty从14/48提升到28/48，剩余16个
  cross-realm及4个Array/String/function exotic define前置依赖；Reflect/defineProperty从22/24提升到24/24。
  完整Proxy/M5.2仍不打勾。
- [x] M5.2/M12 Proxy `[[OwnPropertyKeys]]` linear synchronous-drain slice：SpiderMonkey staging
  `ownkeys-linear.js` 暴露 trap-result 普通 data element 经 `advance -> begin Get -> resume` 每项递归一次，
  15,000 keys 在默认 8 MiB 主线程栈形成约 28,000 Rust frames并 abort。普通 data/missing element 现由显式
  loop drain，只有 accessor/Proxy Get 发布 typed continuation；accessor 不再被错误近似为 undefined。
  重复与 target-inclusion 检查使用 exact-capacity、external-accounted 开放寻址表，key 只编码稳定 Atom index/
  Symbol serial，不保存需随 GC 更新的引用，最坏逆序输入保持线性。默认栈 staging strict/sloppy 2/2、4,096-key
  本地栈回归通过；完整 Proxy 仍有 cross-realm/exotic 前置缺口，M5.2 总项不打勾。
- [ ] M4.4 promotion/young-cap policy freeze：建立完整 JS allocation/survival corpus，联合吞吐、
  fragmentation、retained bytes 与 pause distribution 冻结默认值。

因此当前代码前沿跨 M2.3/M3.2、M5.2/M5.3 function-object property/function-expression substrate 与
M6 try/catch vertical slice，
但正式执行 Stage 仍是 S0；**S0、S1、S2
均未通过 Stage Gate**：
benchmark runner、完整 M1 opcode VM 语义、M3 execution control contract、M5-M8 与 Signals suite
都尚未闭合。跨线程 `Persistent<T>` actor wrapper 也属于后续 host/actor 工作。后续工作必须
优先闭合 S0 gate，不能再把跨 Stage 的实现增量表述为 Stage 前移。

## 7. M0: Repository、质量门与测量基础

### M0.1 Workspace 初始化

- [x] 将根 `Cargo.toml` 改为 workspace，创建第 3 节列出的核心 crate 与工具 crate。
- [ ] 固定 Rust edition、MSRV、resolver、workspace dependencies 和 lint policy。
- [x] 所有 library 启用 `#![deny(unsafe_op_in_unsafe_fn)]`。
- [ ] 支持目标只允许 little-endian `x86_64`、`aarch64`、`riscv64`；其他目标给出明确
  compile error。
- [x] 配置 optimized dev/test 与 release/bench profile，并保留 debug symbols，避免开发和测试默认
  `-O0` 妨碍真实性能测量。
- [x] 基准结果记录 `panic=abort`、LTO、codegen-units 和 target-cpu，确保构建参数可复现。
- [x] 建立 `cargo xtask`，避免平台相关逻辑散落在 shell script。
- [ ] 建立 `capacity-stats` 诊断 feature 和 `cargo xtask capacity-audit`：按 subsystem 输出初始
  hint、growth count、peak length/capacity、unused bytes 和 allocation failure；release 默认移除。
- [ ] 建立各 crate 的指定 `tuning` 模块与根 `TUNING.md` registry；`cargo xtask tuning list/check`
  检查每个 knob 的 owner、单位、范围、benchmark evidence 和最近调优 metadata。
- [ ] 建立 tuning/layout/config 分类检查：性能 threshold 只在 `tuning` 模块，表示不变量只在
  `encoding.rs`/`layout.rs`，宿主资源限制只在 typed config；调用点不出现无主 magic threshold。
- [ ] 定义 machine-readable crate layers：六个 engine crates、adapters、tools/tests；architecture gate 拒绝
  engine -> adapter/tool 依赖和未登记的新 workspace member。
- [x] engine crate 开启 `clippy::disallowed_types/methods`，禁止 fs/net/process/env/current-dir、stdio、
  thread spawn/sleep；同 crate 的 unit test module 与独立 integration harness 才可局部 allow。
- [ ] `cargo xtask architecture check` 解析 Cargo metadata/source imports，审计 transitive dependency
  features 与 build scripts；第三方 crate 若在 engine path 隐式启动 I/O/thread 即拒绝或关闭 feature。
- [ ] compile-fail architecture fixtures 分别尝试 `std::fs::read`、`TcpStream`、`Command`、`var/current_dir`、
  `println!`、`thread::spawn/sleep`，证明 engine library target 会被 architecture gate 拒绝，test/tool
  fixture 可通过。
- [ ] core build script 禁止网络与运行时资源发现；必须生成的表由可复现 xtask 生成并 checked in，
  或只使用 Cargo 显式输入且输出 hash 可验证。
- [ ] async adapter contract 分别运行 `--no-default-features --features futures`、`smol`、`tokio` 和
  `--all-features`；用 `cargo tree` gate 保证 core crates 不反向依赖任何 executor。
- [x] 给每个 crate 写职责、不变量和禁止依赖的 crate-level docs。
- [ ] `tachyon-inspector` dependency gate 禁止网络/executor crate；server tool 才选择 transport/runtime。
- [x] 固定 `tc39/proposal-signals` proto-spec commit/API hash、Stage 状态，以及
  `proposal-signals/signal-polyfill` tests/benchmark commit；upgrade bot 只能提显式 compatibility PR，
  不能浮动跟随 main。
- [x] 建立 `cargo xtask test signals` 的内容寻址 proposal fixture 门禁：固定来源与 API hash，首批
  state/computed、custom equality/error cache、cycle/pruning、dynamic graph、Watcher/liveness、
  receiver/frozen、untrack/introspection/brands 七组语料执行 7/7；VM 侧同一语料覆盖 N=1/2/4/8/16
  与 forced-major。完整 upstream suite、GC liveness、differential trace 仍未完成。

验收：空实现 workspace 可在所有目标 `cargo check`，依赖图中不存在 VM -> compiler 或
GC -> VM 反向边。

### M0.2 Test262 runner 骨架

- [x] 复用 Boa tester 的 metadata、edition、strict/noStrict、negative phase 和 JSON 结果思想，
  不复制 Boa engine adapter。
- [x] 在配置中固定 test262 repository URL、commit、release target 和 feature policy。
- [x] 解析 YAML frontmatter：description、esid、features、flags、includes、negative phase/type。
- [x] 支持 `raw`、`onlyStrict`、`noStrict`、`module`、`async`、`CanBlockIsFalse` 等 flags。
- [x] 拼接 `sta.js`、`assert.js` 和 test-specific includes，保存最终 source hash。
- [x] 定义 parse、resolution、runtime 三种 negative phase，不能把所有抛错都算通过。
- [x] 结果区分 pass、semantic failure、parse mismatch、timeout、panic、crash、unsupported、
  harness failure；保留 stdout/stderr 和精简 backtrace。
- [x] 支持目录/单文件/filter、并行 isolate、串行复现、随机顺序与固定 seed。
- [x] 输出总量、按 feature/path/edition/phase 分类结果和与 baseline 的增退列表。
- [x] 在 VM 尚未实现时接入 stub adapter，证明 runner 可完整读取 suite 并正确报告失败。
- [x] 接入真实 Tachyon in-process adapter；body-first parse、ordered in-memory source units、独立 isolate、
  fuel timeout 和 unsupported 分类均有测试，并真实跑通 parse-negative test262；Rust panic 直接 abort runner。

验收：能扫描固定 test262 commit，全套 metadata 解析无 panic；用 fake engine 的 fixture 验证
positive/negative/strict/async/module 判定。

### M0.3 Benchmark runner 骨架

- [x] 复用 `boa/benches/scripts` 中适用且许可证允许的脚本，记录来源和 hash。
- [x] 建立微基准分类：parse、compile、dispatch、arithmetic、call、closure、property、prototype、
  array、string、regexp、JSON、Promise、host sync/async、allocation 和 GC。
- [ ] 建立套件分类：Boa V8 scripts、js-engine-zoo 可比脚本及后续批准 corpus。
- [x] 实现 Tachyon、Boa 0.21.0 与 rquickjs 0.12.1 linked in-process adapter；reference engine 的
  runtime/context 与 setup 在计时外创建一次，sample 内直接调用预解析的 global `main` 精确 N 次。
- [x] 外部 file adapter 统一 script-last argv 与结果协议；只诚实实现 cold start，具备 timeout、
  bounded stdout/stderr、exit/crash 分类和 binary size 采集。Boa/QuickJS CLI 路径已删除，当前仅
  Escargot 使用该维度，且其 report 不与 linked steady-state throughput 混算。
- [x] 用三个真实 release executable 各跑通至少一个 content-addressed script，并固定其构建 provenance；
  `bench build-profile/run-profile` 验证 platform、checkout HEAD、tracked cleanliness 与结构化 build argv。
- [x] timing contract 区分 cold start、parse+compile+execute、precompiled execute、steady-state；adapter
  无法准确实现的 mode 返回 `UnsupportedMode`。
- [x] benchmark entry contract 区分 `script`/`main-function`：Boa corpus setup 只执行一次，Tachyon
  precompiled/steady 使用独立 `main();` invocation code，external cold-start 合成 setup+一次 main；entry
  写入 report schema v3，comparison 拒绝 entry drift，禁止只计时顶层定义冒充 workload。
- [x] 为 Tachyon in-process adapter 实现 parse+compile+execute、precompiled execute 与 steady-state
  的真实边界。
- [x] steady-state work count 由每个 content-addressed corpus script 独立声明，不保留 adapter 全局默认值；
  request 固定并校验 warmup/sample 次数，report schema v4 保存 iterations/sample，comparison 拒绝 drift。
  内含大循环的 Boa `main-function` 初始为 1，foundation arithmetic 为 1000，后续只在 config 中调优。
- [x] report 保存 CPU、OS、compiler、commit、features、binary size 和 build flags。
- [ ] 对支持的平台/adapter 保存每个 case 的 peak RSS，并明确不可用平台的 reason。
- [x] 默认至少 10 个 sample，报告 median、MAD/置信区间、ratio 与几何平均；异常值规则固定。
- [x] 支持 CPU affinity、performance governor 检查和背景噪声预检；不满足条件则标记结果无效。

验收：在 Tachyon 尚为空时，也能统一运行并比较 Boa、QuickJS、Escargot 的至少一个脚本，
产出版本化 JSON 和 Markdown summary。

## 8. M1: Value 与 Bytecode 契约

### M1.1 `tachyon-value`

- [x] 实现 `#[repr(transparent)] Value(u64)`，外部 crate 不访问内部 bits。
- [x] 实现 canonical numeric NaN、int32、heap ref、undefined、null、bool、hole、uninitialized。
- [x] 实现 `RawHeapRef(NonZeroU32)`；offset 0 永远无效。
- [x] 所有 reserved tag decode 返回错误或 debug trap，不能产生伪造对象。
- [ ] 提供 `is_*`/`as_*` fast path，检查 release assembly 是否没有多余 branch/函数调用。
- [x] 添加 `size_of::<Value>() == 8`、`size_of::<RawHeapRef>() == 4` 静态断言。
- [x] property test 覆盖全部 `u64` 分类、任意 f64 roundtrip、NaN canonicalization、int32 边界。
- [x] Miri 测试 bit conversion 和 raw ref construction。

### M1.2 `tachyon-bytecode`

- [x] 定义 newtype：`RegisterId`、`ConstantId`、`FunctionId`、`FeedbackSlot`、`WordOffset`。
- [x] 定义 32-bit word-coded compact/normal/wide 编码，不依赖 Rust enum layout。
- [x] 第一批 opcode：load constants/immediate、move、arithmetic、comparison、jump、branch、return、
  throw、call、create closure、scope load/store。
- [x] builder 支持 symbolic label、forward/back edge patch、register high-water mark 和 source span。
- [ ] builder 在 lowering/count pass 后为 words、constants、functions、handlers、feedback 和 source
  map 预留容量；冻结后转换为 boxed slice/`Arc<[T]>`，避免保留 builder slack。
- [x] verifier 验证 opcode、operand word 数、register/constant/function 范围、jump boundary、handler
  nesting、fallthrough 和 terminal instruction。
- [ ] decoder 对不可信 bytes 只返回结构化错误，唯一 unsafe fast decoder 只接受 verified module。
- [x] disassembler 输出 word offset、source span、logical operands 和 feedback slot。
- [x] `CompiledModule` 使用 `Arc<[T]>`/boxed slices 保存 immutable code、constant、function metadata、
  handler table、source map 和 `Arc<str>` source。
- [x] `CompiledModule` scope-name table 不保存 isolate-local atom；`LoadScope/StoreScope` operand 由 verifier
  检查，module load 一次解析为 bounded isolate-local AtomId table。
- [x] 常量池不保存 runtime `Value` 或 heap ref；字符串保留源码拥有的字节/UTF-16 数据。
- [x] roundtrip/property tests 覆盖 compact/normal/wide 边界和最大 `u32` logical index。

验收：可手写 `1 + 2` 字节码并反汇编、验证；错误 opcode、截断 operand 和跳入 operand word
均被拒绝。

## 9. M2: Oxc Frontend、HIR 与 Lowering

### M2.1 Source 与诊断

- [x] 定义 `SourceId`、`SourceName`、`MediaType`、`SourceText` 和 compile options。
- [x] compiler 只接受调用方提供的内存 source；`SourceName` 是 opaque diagnostic label，任何 core API
  都不能将其转换为 path/URL 后自行读取，path convenience 只允许存在于 CLI/tool crate。
- [x] 按 Deno 行为映射 JS/JSX/TS/TSX/MJS/CJS/MTS/CTS 到 Oxc `SourceType`。
- [ ] 锁定最小 Oxc crates 的精确版本；升级必须单独提交并跑 compiler fixtures/test262 parse。
- [x] 将 Oxc diagnostics 转为 Tachyon owned diagnostic，保存 primary/secondary span 和 source name。
- [x] parse/transform/semantic 完成后立即把需要的信息复制到 owned HIR 并 drop allocator。
- [x] Oxc 0.140 string literal UTF-16 ownership slice：owned HIR String 改为 `Arc<[u16]>`，在 arena
  存活期按 `lone_surrogates`/`U+FFFDxxxx` contract 解码，不从 lossy Rust `str` 猜回 code unit；普通
  escape、lone lead/trail、有效 surrogate pair、真实 U+FFFD 与 lone-surrogate 混合、directive 均有
  compiler fixture。含 lone surrogate 的 object/class/pattern static literal key 改走常量 computed-key
  lowering，避免污染只接受 well-formed Rust `str` 的 scope-name/atom table；所有临时 UTF-16 backing 使用
  `try_reserve_exact`，分配失败返回结构化 compile error。
- [ ] compile result 不含 Oxc ID、AST node 或 arena lifetime 的 compile-fail test。

### M2.2 HIR 与作用域计划

- [x] HIR 明确表达 expression/statement completion，不复用 Oxc AST enum。
- [x] 建立稳定 `ScopeId`、`BindingId`、`ReferenceId`、`FunctionStencilId`：Oxc semantic ID/parent/flags/
  resolved binding/read-write mode 在 arena 内复制为 Tachyon-owned graph；bytecode local lookup 使用
  BindingId，不再按同名字符串决定 shadowing。
- [x] captured environment substrate：semantic resolved references 标记跨 function capture；compiler 只提升
  captured binding，按 exact slot 生成 `BindingPlan::Environment` 与 plan-index `LoadBinding/StoreBinding`；
  traced GC `Environment` 使用 exact boxed slots/external accounting，空 activation 继承捕获链。可变 closure、
  两层 chain、forced-major 与 N=1/2/4/8/16 对拍已通过；TDZ/loop cloning/direct eval 等仍属后续条目。
- [ ] `BindingPlan` 区分 register、frame slot、captured environment、module cell、global lexical、
  global property、dynamic lookup。
- [x] `CompiledFunction` 冻结 verifier-owned `BindingPlanEntry`：完整 location enum 覆盖 frame register、
  environment(depth/slot)、module cell、global lexical/property、dynamic lookup；binding name 独立于 runtime
  atom/scope table，空 name、register/environment slot 越界在 module freeze 拒绝。compiler 已为当前实际
  frame/global-property/captured-environment binding 生成 plan；captured references 已使用 plan-index operand，
  global lexical/module/dynamic 与旧 global name operand 删除仍由上项闭合。
- [x] Realm global substrate 删除 binding-order `Vec` scan，使用稳定 `GlobalSlotId` storage 与
  atom-indexed resolution table；发布前分别按 AtomId upper bound/global count 精确 reserve，为
  BindingPlan/loaded-code slot cache 提供 identity。loaded scope operand 首次解析后缓存 stable slot，
  unresolved operand 可在后续 binding 发布后自愈；delete/reuse 前必须加入 generation/version guard。
  descriptor 分层仍由上项闭合。
- [x] declarative global lexical substrate：独立 stable `GlobalLexicalSlotId`/atom resolution 保存
  initialized/mutable state，`DeclareGlobalLexical` 在 statement 前预声明，`InitializeGlobalLexical` 只初始化
  一次，lookup lexical-first；TDZ、const、跨 source/redeclaration、预加载 self-heal、N=1/2/4/8/16 与
  benchmark setup→main 已覆盖。完整 collision/property attributes/Error objects/block env 仍未闭合。
- [x] `StoreResolvedScope` 首个慢路径覆盖 current/nested-function/prior-source global 的 simple、compound、
  prefix/postfix 写回，并区分 sloppy read-only intrinsic no-op 与 strict module error；缺失 binding 不错误
  创建。script/function strict metadata、sloppy unresolvable create 与 slot/version fast path由上项闭合。
- [ ] 分析 var/let/const/function/class、hoisting、TDZ、parameter environment、arguments object。
- [x] 首个 `var` 切片递归收集 script/function `VarDeclaredNames`，function frame 入口去重初始化、参数
  同名复用、block 外溢和 source-order initializer；script `DeclareScope` 保证跨 source unit 重声明不覆盖。
  完整 global descriptor/lexical collision、Annex B、eval 与 BindingPlan 仍由上项闭合。
- [ ] 处理 closure capture、named function expression、arrow lexical this/super/new.target。
- [x] synchronous arrow expression/block 的 owned stencil、参数和普通调用路径已接入；lexical this/super/new.target 仍待闭合。
- [x] object literal numeric keys 使用 ECMAScript number formatting canonicalize；hex/指数源码不保留原 spelling。
- [x] sequence/comma expression 已复制为 owned HIR，并按左到右求值后返回最后一项。
- [x] Array.prototype.sort 完整 resumable stable sort 已接入：skip-holes Has/Get snapshot、user comparator
  Call/ToNumber、默认 UTF-16 comparison、undefined 后置、严格 Set/DeletePropertyOrThrow writeback 均由
  typed continuation 或显式同步 loop 执行；Test262 97/107，余 10 为共享 primitive-call frontend、dynamic
  Function/RAB 缺口。
- [x] Number.isNaN/isFinite/isInteger 作为纯 numeric predicates 发布在 Number constructor 上，不进行隐式 ToNumber。
- [x] Number.isSafeInteger 复用 numeric predicate path，并以统一 MAX_SAFE_INTEGER 常量检查边界。
- [x] Number 的 EPSILON/MAX_VALUE/MIN_VALUE/MAX_SAFE_INTEGER/MIN_SAFE_INTEGER/NaN/±INFINITY 以只读 descriptor 发布。
- [x] generic `fill` 已接入并遵循相对边界、materialize holes；旧同步 `lastIndexOf` 已由后续 resumable
  bidirectional search slice 取代。
- [x] generic `copyWithin` 已接入；重叠区间按方向复制，并保留 source holes 的删除语义。
- [x] `Array.prototype.flat` 已由独立 resumable owner 取代旧同步 work stack；任意深度使用 GC-managed
  显式 frame backing，typed continuation 覆盖 Proxy/species/转换与 CreateDataPropertyOrThrow。
- [x] function-body direct declaration instantiation：HIR 使用 ScriptBody/ScriptNested/FunctionBody/
  FunctionNested 四态上下文；
  direct declaration 在 activation statement execution 前创建 closure 并发布到 register/environment，
  覆盖声明前调用和多层 capture。block declaration/Annex B、named expression self-binding/arrow 仍未完成。
- [ ] 标记 direct eval、with、Annex B block function 导致的动态 scope/optimization 禁用。
- [x] 首个 direct-eval analysis 切片复制 Oxc `DirectEval` scope capability 并有 owned-HIR regression；
  dynamic environment allocation、专用 eval call、with 与 Annex B invalidation 仍由上项闭合。
- [ ] 为 script/module、strictness、top-level await 和 import/export 建立 module stencil。
- [ ] source map 能从 HIR/bytecode span 回到原始 TypeScript/JSX 输入。

### M2.3 Bytecode lowering

- [x] 首个 ordinary function 切片覆盖顶层与 function-body direct declaration hoist、simple parameters、
  direct call、显式/隐式 return、function expression 与 selective capture；arrow/construct 完整语义/
  default/rest 仍由后续条目统一闭合。
- [x] 首个结构化 statement 切片覆盖 script/function 的 block、if 和未处理 throw；while/for、labelled
  control 与 handler dispatch 仍由下列完整条目统一闭合。
- [x] logical expression 切片覆盖 operand-valued `&&`、`||`、`??` 短路与 checked label/instruction/
  literal/scope-name capacity；完整 expression 条目仍缺其他 unary/binary、sequence 与 coercion。
- [x] switch control 切片覆盖 case-test 顺序、default 任意位置、fallthrough 和最近无标签 break；
  switch-scope lexical declaration instantiation/TDZ、labelled break 与循环 continue 仍由完整条目闭合。
- [ ] 先覆盖 literal、identifier、unary/binary、assignment、sequence 和 conditional expression。
- [ ] 覆盖 block、if、while、do/while、for、break、continue 和 labelled statement。
- [ ] 覆盖 function declaration/expression、arrow、call、construct、return。
- [ ] 覆盖 object/array literal、member get/set/delete、computed key 和 spread 的基础 lowering。
- [ ] completion/handler table 先设计，再实现 try/catch/finally，避免依赖 Rust unwind。
- [ ] register allocator 先用 linear lifetime/free-list；保存 peak register 与 debug mapping。
- [ ] 生成 stable `DebugSiteId`、source span、scope/binding location、call/return/throw 与
  async-parent metadata；优化/lowering 后每个不可观察 binding 记录明确 unavailable reason。
- [ ] HIR/scoping pass 记录 node、binding、reference、scope、literal 和 function count；所有 owned
  tables 使用 checked hint，禁止根据未验证 source/AST count 造成无界 reserve。
- [ ] constant folding 只做规范安全操作；不能跨 observable conversion/getter/throw。
- [ ] compiler snapshot test 同时检查 diagnostics、HIR、binding plan 和 disassembly。

验收：source -> owned compiled module；drop Oxc allocator 后仍可反汇编和显示诊断；property tests
对任意生成的 UTF-8 输入无 panic。

## 10. M3: Minimal Fiber VM

### M3.1 Isolate 与执行状态

- [ ] `Engine: Send + Sync` 持有 immutable config、atom/code cache 和 extension template。
- [ ] `Isolate: Send + !Sync` 持有 heap、realm、fibers、jobs、feedback、host state 和 budget。
- [ ] 使用 compile-time assertions 固化类型边界，不通过持久 TLS 隐式绑定 worker。
- [ ] `Fiber` 持有 frame/register/handler/completion stack，所有 index 使用 checked newtype。
- [x] frame 保存 function、pc、base、environment、return target、this/new.target 和 strictness。
- [x] `CompiledFunction` 保存 register count、max handler/completion depth、argument/temp window 和
  feedback count；function entry 在 opcode dispatch 前一次 reserve，opcode loop 内不允许 realloc。
- [ ] `ExecutionBudget` 同时支持 hard fuel 和 poll quantum；builtin 也能查询 safepoint。

### M3.2 Interpreter loop

- [x] 初版使用稳定 Rust `match` dispatch；`execute_batch<const N: usize>` 每次从当前 `pc` fetch，
  jump/branch 在 batch 内继续，N=1/2/4/8/16 的结果、fuel 边界和跨 batch branch 已对拍。
- [ ] 增加每 opcode count 和 branch outcome 的诊断 feature；release 热路径默认完全移除。
- [ ] 固定 `Continue/Redispatch/Safepoint/Exit` 单步 control contract；call/return/throw/suspend 接入后
  调整 batch size 不重写 opcode handler。
- [ ] verified module 使用小型 unsafe fast decoder；debug build 可切换 checked decoder 对拍。
- [ ] 实现 M1 第一批 opcode 和显式 `RunOutcome`。
- [x] JS call push frame，不用 Rust recursion 表示 JS 调用栈。
- [x] frame 与 FunctionObject 使用 `CodeId + FunctionId`，跨 source closure call 不把 module-local
  FunctionId 误解析到当前 caller module；N=1/2/4/8/16 均覆盖 code switch。
- [x] 无匹配 handler 的 `Throw` 返回显式 `RunOutcome::Thrown`，active fiber 保持 payload rooted；
  N=1/2/4/8/16 的 callee throw 结果一致。
- [ ] exception 通过 completion/handler stack 展开，不用 Rust panic。
- [ ] quantum exhausted 保存完整 pc/register/frame 并返回，不改变 JavaScript job 顺序。
- [ ] stack trace 从显式 frame/source map 生成。

### M3.3 Debug execution substrate

- [ ] 定义 `Detached/Attached/Paused/Terminating` 状态机、单调 attach/pause generation 和
  high-priority debug command queue；非法状态转换返回 typed error，不 panic。
- [ ] interpreter 提供 `execute_batch::<N, false/true>` 两个单态化路径；detached 路径只在 batch
  边界发现 generation 变化，attached 路径才逐 opcode 查询 debug site。
- [ ] `CompiledModule` debug metadata 保持 immutable/Sync；isolate-local breakpoint bitmap 以
  `CodeId + DebugSiteId` 寻址，attach/set/remove breakpoint 不修改共享 bytecode。
- [ ] breakpoint location resolver 将 URL/source position 映射到最近可停 site，并处理同一模块被
  多 isolate 共享、script 重载、source-map 映射失败和尚未解析脚本。
- [ ] pause 捕获 active fiber、frame depth、scope locations、exception completion 和 async parent ID；
  resume 清除 pause-only state，不能留下 register/environment root。
- [ ] 单元测试覆盖 batch 中途 attach/pause、jump/call/return/throw site、重复 attach/detach、stale
  generation、共享 module 两 isolate 不互相污染和 detached disassembly bits 不变。

验收：CLI 可执行 arithmetic、loop、recursive function 和 throw/catch fixture；hard fuel 能终止
无限循环；深 JS recursion 不耗尽 native stack；基础 pause/resume 不修改程序结果或共享 bytecode。

## 11. M4: Precise Non-moving Generational GC

当前 checkpoint（2026-07-18）：`e68d672` 已建立 rewrite-capable `Trace`/`Tracer` 与静态
`TypeDescriptor`，`37ee766` 已把 fiber execution state 纳入精确 root tracing，`4bfd5fb` 已固定
32-bit logical span address，`b4356bc` 已建立 `CollectionEpoch`、checked `SlotIndex`、固定容量
allocation/mark/card bitmap 与 non-moving cohort enum，`61866ad` 已建立 Rust-allocator-backed aligned
span storage、渐进 span table/free ranges 与完整 small-span side metadata，`0e14572` 已闭合 epoch
overflow 的全 live-span bitmap reset，`ec34a6d` 已建立 immutable typed descriptor registry、typed
small-object initialization、Eden bump、Survivor allocation rejection、Old free-list、fixed active-size-class
slots、small-span resource error 与 small-reference verifier。`8b54a18` 已增加 large owner/continuation
logical ranges、独立连续 backing allocation、整段回收复用、统一
small/large verifier 与 external backing hard-limit accounting。`f802e65` 已加入 32-bit iterative gray
queue、mark-before-enqueue 去重、descriptor-driven strong fixed point、10,000-depth non-recursive graph
test 与 capacity high-water 统计；`c0d3dc6` 已实现 span-level full sweep、descriptor drop、Old free-list
重建、empty/large span 回收、析构前 unpublish 与完整 accounting。`79c9320`、`5f18348`、
`fa13a37` 依次完成 generative temporary roots、token-validated `NoGcScope` payload borrow 和
generation-protected persistent-root slab。`337c28e` 已完成
intrusive remembered-source chain、old-to-young barrier、young-only marker 与 conservative rebuild。
`3de20d6` 已完成 intrusive young-span chain、strong-only `collect_minor`、young sweep、空 backing
release、cohort aging、in-place promotion、promotion remembered cards 与 active allocator cache repair。
`d67eb43` 已完成 typed weak/ephemeron trace contract、受配额 weak-owner worklist、major/minor
ephemeron fixed point、dead weak/ephemeron clearing 与 weak-phase work/capacity stats。
`c9e4940` 已完成 job-scoped kept roots、finalization registration、sweep 前 FIFO cleanup enqueue、
pending registry/held roots、显式 safepoint transfer contract 与 immutable descriptor `OldOnly` policy。
当前主要缺口是 promotion/young-cap 完整 corpus tuning。FinalizationRegistry JS binding 已完成 registry
object、GC-managed registration cell、snapshot transfer、FIFO precise roots、throw/reentrancy、callback 内
GC/new-record deferral，以及通过普通 call trampoline 执行实际 ECMAScript cleanup callback；collector 仍不
执行 JS。

### M4.1 Logical span table 与 allocator

- [x] `GcRef` 32-bit logical offset 固定为 high 16-bit `SpanId` + low 16-bit 64 KiB span offset；
  offset 0 保留，最大 65,536 logical spans，checked encode/decode 覆盖全部边界。
- [x] 默认 storage 只使用 Rust allocator 按需创建 16-byte-aligned span，不直接调用 mmap/VirtualAlloc、
  reserve/commit/decommit/protect；未来 Wasm backend 可复用 logical contract，但 wasm32 不是 1.0 target。
- [x] span table 按历史峰值渐进 `try_reserve`，entry index稳定并用 free-index ranges复用；不得预分配
  65,536 entries，table reallocation 不影响 `GcRef`，所有 object borrow 每次重新 resolve。
- [x] small span metadata 维护 size class、space kind/cohort age、allocation/mark bitmap、mark epoch、
  bump/free-list、512-byte-granularity cards、sweep state、reuse generation 与 accounting。
- [x] 定义最小 16-byte slot、8-byte header；large object 使用连续 logical SpanId range 与独立 owner/
  continuation metadata，external memory 单独计入 isolate limit。
- [x] Eden bump 与 old free-list allocation fast path 使用 active size-class span 并针对性内联；table/span
  growth 仅在 slow path，heap limit 与 allocation failure 返回 structured resource-limit error。
- [x] allocator slow path 接入 collection trigger；分配失败时按策略执行 collection/retry，而不是只增长
  span 或直接返回 resource-limit error。
- [x] debug verifier 检查 SpanId/table entry、logical offset、slot boundary/alignment、allocation bit、
  type ID、large continuation 和 live state；不依赖 page fault。

### M4.2 Epoch tri-color 与 full major collector

- [x] 定义 `Trace`/`Tracer`，从第一阶段使用 `&mut Value`/`&mut GcRef` visitor；1.0 collector 不移动或
  重写引用，但 contract 不冻结未来内部表示。
- [x] immutable type descriptor 保存 trace/drop/size/alignment/name；GC crate 不依赖 VM/async host API，
  callback contract 禁止执行 JS、poll host future 或重新进入 GC allocator。
- [x] 当前 root composition 精确包含 scope temporary roots 与 persistent-root slab；VM `Fiber::trace_roots`
  覆盖 frame/register/handler/completion 中的 heap value，不扫描 native stack。
- [ ] 将 root composition 扩展到全部 running/suspended fiber registry、realm、module、host promise table、
  debugger pause roots 和 collection phase temporary roots。
- [x] `CollectionEpoch(NonZeroU32)` 使 epoch 不匹配的 span bitmap 逻辑全白；首次 mark 该 span 才清
  bitmap/update epoch，overflow 执行全 bitmap reset 并有 forced-wrap test。
- [x] 三色只由 current-epoch mark bit 与 32-bit-offset iterative gray queue 表示；禁止对象内 Color、
  allgc/gclist/root count、black bitmap、atomic bitmap、锁、channel、background marker 和 spin。
- [x] gray queue 使用受配额 high-water policy，记录 initial/growth/peak/retained/slack；mark bit 只在
  reserve 成功后发布，steady-state strong mark 不因逐 edge push 反复 realloc。
- [x] span-level sweep worklist 使用受配额 high-water policy，记录
  initial/growth/peak/retained/slack；worklist 只保存 owner `SpanId`，不为每个 object 建 entry。
- [x] temporary roots 使用同类受配额 high-water policy，记录
  initial/growth/peak/retained/slack；正常 scope 退出后不得保留 stale root。
- [x] 实现 descriptor-driven strong mark fixed point；mark-before-enqueue、iterative gray queue 和 deep graph
  test 保证 cycle 去重且不使用 Rust recursion。
- [x] `Tracer` 明确表达 nullable weak slot 与 ephemeron pair；strong trace 把 weak owners 收入受配额
  high-water worklist，ephemeron 迭代到 fixed point 后在 sweep 前原地 clear dead weak/key/value，major/minor
  共用 phase contract 且不扫描全 heap、不缓存 payload pointer。
- [x] kept-object set 在显式 job boundary 前作为精确强根；dead finalization target 在 weak clearing 后、
  sweep 前 reserve/enqueue FIFO cleanup record，pending registry/held value 持续作为 roots，collector
  trace/drop/sweep 不执行 callback 或重新进入 allocator。
- [x] VM job safepoint scheduler 消费 rooted pending finalization record、建立精确 rooted FIFO cleanup job；
  callback throw/reentrancy、callback-triggered GC 与下一轮 record deferral 已由 VM tests
  覆盖。M5/M8 已接入实际 ECMAScript registry/callback 调用，normal/throw 均通过 typed continuation
  显式恢复而不使用 Rust unwind。
- [x] full sweep 使用 allocation bitmap 与 current epoch mark 批量 drop、rebuild free lists 和回收空 spans；
  large objects、external bytes 和 fragmentation/slack 进入统计。
- [x] trigger 使用 allocation-byte debt、young/old growth、heap limit、显式 force 或 safepoint pressure
  command；禁止后台线程、时钟轮询和每-opcode atomic flag。
- [x] forced-major mode 在每个可分配点 collection，用于暴露漏 root；memory-pressure/low-limit tests
  验证 high-water retained memory 不能绕过 accounting。

### M4.3 Scope 与 handle

- [x] `RunningScope` 通过 generative callback lifetime 管理 heap-owned 临时 root stack；
  `Local<'scope, T>` 不实现 Send/Sync，nested scope 按 checkpoint 回滚；panic 直接 abort，不承诺恢复。
- [x] `NoGcScope` 通过当前 heap 的 `GcType<T>` token、header 与 layout 验证后借用内部对象；
  shared/mutable payload pointer 在内部类型上分离，scope API 不暴露分配或 collection。
- [x] GC 内部 `PersistentRootId<T>` 使用 generation-protected slab/free-list；release 后复用 slot
  必须递增 generation，溢出永久 retire，stale ID 不能 resolve/release 新 occupant；ID 可作为 actor
  command data 传输但不携带 isolate owner capability。
- [ ] facade `Persistent<T>: Send + Sync` 组合 isolate handle 与 root ID，不公开 isolate-relative ID。
- [ ] persistent clone/drop 通过 isolate 所有权或 actor command 更新 root，不直接跨线程访问 heap。
- [x] compile-fail doctests 覆盖 Local 逃逸、跨线程和跨 worker await。
- [x] compile-fail doctests 覆盖 `NoGcScope` 中无法调用分配/collection API、payload borrow 逃逸和
  重叠 mutable borrow。

### M4.4 Non-moving young cohorts 与 minor GC

- [x] small span metadata 已定义 `Eden`、`Survivor { age }`、`Old`；Eden 按 size class bump allocate，
  Survivor 在 promotion 前不接纳新对象，large allocation 直接进入 Old。
- [x] immutable descriptor `OldOnly` policy 使 pinned/finalizer payload 即使被请求为 Young 也直接进入 Old；
  同一 Rust type 以冲突 policy 重复注册返回 typed error。
- [x] minor roots = precise roots + dirty old cards；gray queue 只 enqueue young target，trace 遇到 old edge
  不递归扫描 old graph，minor 不遍历全部 old spans。
- [x] young sweep 前执行 ephemeron fixed point 与 weak clearing；Old key 在 minor 视为 live，dead young key/
  weak target 在 sweep 前清除，old-to-young weak owner 通过 remembered card 被发现。
- [x] minor root composition 包含 job-scoped kept objects；dead young finalization target 在 sweep 前进入
  bounded FIFO pending queue，registry/held value 在 cleanup scheduler 消费前继续被精确 trace。
- [x] VM cleanup scheduler 可在 minor/major 返回后的 safepoint消费 finalization queue，并在执行期间把
  registry/held value 纳入 `Isolate` roots；sweep/drop callback 仍禁止运行 JS 或重新进入 allocator。
- [x] `collect_minor` 只沿 intrusive young-span chain sweep Eden/Survivor；空 span 优先进入 fixed-capacity
  per-size-class Eden pool，overflow/major/pressure trim 才释放 backing；存活 Eden 变 Survivor，达到
  centralized cohort age 后整 span晋升 Old，`GcRef` 与 native address 均不改变，allocator active cache
  在 phase 后修复。
- [x] 为 empty young span 增加受 heap accounting/pressure 管理的 per-size-class Eden pool 与 trim policy；
  benchmark 证明 retained storage 优于当前立即 release 策略后再替换。
- [x] whole-span promotion 只扫描本轮 marked live objects 并建立 remembered cards；dead holes 在 promotion
  后一次性重建为 Old free list，promoted span 可直接进入 active Old cache。
- [ ] promotion age、occupancy early-promotion 和 young storage cap 完成 tuning registry 与 corpus/benchmark
  验证；当前初始 age 只在 `tachyon-gc::tuning` 定义且不暴露 API。
- [x] Phase 1B heap-field post-write barrier 对 old-to-young store 标记 512-byte card；large owner 使用
  owner-level remembered bit，stable `SpanEntry` intrusive chain 使 clean-to-dirty transition O(1)、无扩容，
  minor 只访问 remembered owners，成功后按实际 direct young edge 收紧状态，错误路径保持 conservative。
- [ ] 将后续 object/array/environment/root setter 全部接入统一 barrier，并为 Phase 2 incremental shading
  保留同一 API；禁止调用方直接写 heap edge 后遗漏 barrier。
- [x] debug barrier verifier 全扫描 Old graph 发现遗漏 card/large remembered owner，并提供 forced-minor
  stress fixture。
- [x] forced-minor mode 在每个 young allocation collection；stress 随机 minor/full major 并覆盖 cycle、
  weak/ephemeron/finalizer、span reuse、promotion 与 low-memory failure。
- [x] minor 统计 marked/traced/scanned/live/reclaimed object/bytes、dirty cards、Old/large remembered scan、
  Eden/Survivor processed/aged、whole-span promotions、released storage 和 fragmentation/slack；不存在
  copied/forwarded bytes。
- [x] 补齐 allocated bytes、young/old live span count、card false-positive、Eden-pool retained bytes 和
  pause P50/P95/P99 聚合，并接入 capacity/benchmark report。

验收：循环对象可回收，rooted 对象存活，unroot 后回收；M3 fixture 在 forced-minor/major 下通过；
对象 offset/address 在 minor、promotion 和 major 前后不变；Miri/sanitizer 无错误，普通 young
allocation 不逐对象调用系统 allocator。

## 12. M5: Object、Function 与 Environment

### M5.1 String 与 atom 基础

- [x] 定义 ECMAScript UTF-16 code unit 语义，不能把 Rust `str` 当作所有 JS string 的表示。
- [x] 支持 Latin-1/8-bit 与 UTF-16 owned string、atom/interned string 和 lazy hash。
- [x] 短字符串、concat/rope、slice 先保留表示 tag，具体阈值由 M13 基准确定。
- [x] atom table 属于 isolate 或 engine immutable/shared 边界，明确内存配额与清理策略。
- [ ] 实现 ToString/ToNumber 所需解析、比较和 hash，不依赖 locale。

### M5.2 Shape/object/property

- [ ] object/Shape contract 保存 prototype identity、property key、slot、attributes、transition 和
  version/watchpoint；当前 prototype 在 traced object payload，未来若移入 shape 必须先 GC-manage shape。
- [ ] 普通对象使用连续 property storage；初始是否 inline slots 由 layout microbench 决定。
- [ ] object/array literal 使用 compiler 的精确 property/element count；dynamic property/elements
  growth 采用分类型策略，记录 packed-to-holey/dictionary 转换前后的 capacity slack。
- [ ] shape transition 支持 add property、attribute change、prototype change；复杂 mutation 转 dictionary。
- [ ] 实现 property descriptor、ordinary get/set/define/delete/has/ownKeys 和 prototype traversal。
- [x] 首个 ordinary read traversal 与 constructor/instanceof prototype 链切片完成；完整 descriptor、
  setPrototypeOf/cycle、accessor/exotic/Proxy 与缓存失效仍由上项闭合。
- [x] 对照 Escargot 将 ordinary function prototype 改为 inline lazy slot：plain `CreateClosure` 不物化，
  首次 read/construct/instanceof 才分配 default prototype 与单槽 constructor backing；direct replacement
  不产生默认对象，forced-major 与 constructor back-reference 覆盖。
- [ ] property order 符合 integer index、string、symbol 顺序。
- [ ] 建立闭合 object internal-method 分派：ordinary 静态 fast path 与 Array/String/TypedArray/Proxy/
  Module Namespace cold exotic path 共用 getOwn/define/delete/ownKeys/get/set/has/prototype 契约，builtin
  不直接识别 concrete GC payload。
- [ ] 函数 `length`/`name`/适用的 `prototype` 从创建起进入共享初始 shape；shape 将 chronology ordinal
  与 `PropertyLocation` 分离，derived/lazy metadata 不分配普通 storage，descriptor override 原位迁移到
  storage，delete/re-add 才获得新 ordinal。删除临时 virtual-key merge 与来源 attribute bit。
- [x] 普通 configurable property 删除对照 Escargot 执行 structural remove：从 empty shape 重放 retained
  descriptors、精确压缩 slots/Symbol edges，最后一个属性恢复无 backing；delete/re-add 在 String/Symbol
  分区移至末尾。N=1/2/4/8/16、forced-major 与 Symbol edge 回收覆盖。integer-index numeric order、函数
  reserved metadata shape、exotic internal method 和 dictionary mutation mode 仍由未完成总项闭合。
- [ ] 从第一天把 Proxy/accessor/exotic 分支隔离到 slow path，普通数据属性保持短 fast path。

### M5.3 Function/environment

- [x] 首个 global-object binding substrate 使连续 script source units 共享顶层 function declaration；
  binding/root/module 数量有 host hard limit。global lexical/TDZ/var/property attributes 仍未完成。
- [ ] 支持 lexical/declarative/object/function/module/global environment record。
- [ ] closure 只捕获需要的 binding；未捕获 local 保留 register/frame slot。
- [ ] function object 区分 bytecode/native/bound/class constructor 和 generator/async kind tag；derived class
  constructor 已由 immutable bytecode `FunctionKind` 区分，generator/async kind 仍待完成。
- [x] bytecode/native/bound executable kind 不允许无效字段组合；native intrinsic 仍是 engine 内部 closed enum，
  不能冒充 M7 typed host callback ABI，class/generator/async kind 仍待完成。
- [ ] 完成 this binding、arguments、rest/default parameter、caller restrictions 和 construct/new.target；
  strict exact-this 与 sloppy nullish→global 已完成，primitive boxing、arrow lexical this 等仍待闭合。
- [ ] native function descriptor 预留 M7 typed callback，不让宿主 ABI 进入 bytecode format。

### M5.4 Array/elements 基础

- [x] ArrayObject 已成为独立 GC identity；Array.isArray 不再依赖原型链猜测，Array prototype 自身为数组。
- [x] length 初始 descriptor、generic push/join/toString、holes/null/undefined/self-reference 语义已有覆盖。
- [x] generic `at`/`indexOf`/`lastIndexOf`/`includes` 已接入；`indexOf` 两方向使用 resumable
  HasProperty/Get 与 strict equality，`includes` 使用 SameValueZero，缺失项遵循 array-like length scan。
- [x] generic `pop`/`slice` 已接入；slice 保留 holes，pop 更新 length 并拒绝不可删除末项。
- [x] generic `shift`/`unshift`/`reverse` 已接入；移动和交换路径显式处理 holes、length 与删除失败。
- [x] Math 36 intrinsic methods、全局 `isFinite`/`isNaN`/`parseFloat`/`parseInt` 及 EvalError/URIError
  constructors 已接入；Math 对象参数 continuation、Math.sumPrecise 与其余 global substrate 仍待后续批次。
- [x] realm-local `Symbol.iterator`、`%IteratorPrototype%`、`%ArrayIteratorPrototype%` 及 Array
  `keys`/`values`/`entries`/`@@iterator`/`next` 已接入；Array iterator payload 由 GC tracing 管理。
- [x] `Object.getOwnPropertyNames` 补齐 callable 的虚拟 `length`/`name`/constructor `prototype` own keys，尊重 shape tombstone。
- [x] 默认整数索引已从 named-property shape/storage 分离到精确 GC 计费的 `ArrayElements`；hole count
  区分 packed/holey，远端首写和超阈值 gap 回退 ordinary dictionary-style property，不为稀疏索引分配巨型 backing。
- [x] dense growth 使用 fixed `Box<[Value]>` allocate-copy-swap、4/3 容量增长和显式 array/value roots；
  覆盖 hole fill、delete、length truncate、descriptor migration、ownKeys、prototype indexed lookup 与
  N=1/2/4/8/16/forced-major。20,000 sparse unshift 在 12-span heap 内通过。
- [x] ArrayBuffer/TypedArray fixed backing 与 detach contract 已建立；RAB/SAB/transfer 仍在 M8 追踪。
- [x] fixed non-shared `ArrayBuffer.prototype.slice` 完整纵切：start/end 使用 resumable
  ToIntegerOrInfinity，constructor/`@@species` Get 与 Construct 经过 typed continuation；constructor
  结果按 brand、detach、source identity、最小 byteLength 顺序验证，随后再次检查 source detach。
  byte copy 使用受调优的 bounded stack chunk 和分离 no-GC borrow，不分配未计费临时 Vec、不使用
  unsafe alias；N=1/2/4/8/16、forced-major、cross-Realm、Proxy `@@species`、own constructor getter、
  conversion detach 和 source/result 双向 copy independence 均覆盖。定向 test262 为64/66，两个
  semantic failure均是此切片明确不实现的 SharedArrayBuffer receiver；fixed non-shared applicable子集
  62/62。RAB/SAB/transfer 不在此切片。
- [x] fixed non-shared `ArrayBuffer.prototype.transfer`/`transferToFixedLength` 纵切：两个length=0
  native identity共用 ArrayBufferCopyAndDetach 内核；显式 newLength 先走 resumable ToIndex，再重新检查
  detach，result allocation/zero-fill/copy 全部成功后才清 source edge。external backing 继续使用精确
  GC charge与allocate-copy-detach，不使用mmap/atomic/unsafe或未计费临时Vec；低heap-limit证明OOM时
  source仍attached。N=1/2/4/8/16与forced-major通过。定向test262：transfer 34/48、
  transferToFixedLength 42/48；剩余仅RAB preservation、SAB/immutable及共享BigInt前端，不属于本fixed切片。

验收：对象、原型、descriptor、closure、constructor、dense/sparse array 专项测试通过；test262
对应 language 子树可持续运行并输出分类结果。

## 13. M6: 控制流、异常、类与完整调用语义

- [ ] 实现 try/catch/finally、return/break/continue 穿越 finally 的 completion 恢复。
- [ ] 实现 iterator protocol、IteratorClose 和 abrupt completion 顺序。同步 `for...of` 已复用 shared iterator record 支持 `var`/assignment head、continue、break/return/throw close，并在 throw completion 时保留原异常而抑制 close error；per-iteration lexical environment、rest、iterator-result object 校验与 async iteration 仍待闭合。
- [ ] 实现 destructuring、spread/rest、template literal、optional chaining、nullish coalescing。
- [x] owned recursive binding/assignment pattern HIR 已从 Oxc arena 解耦，并接入 BoundNames/capture/capacity；复杂 pattern bytecode lowering 与 IteratorClose 仍待完成。
- [x] synchronous object/array pattern bytecode 已覆盖 declaration/assignment、nested、computed key、default 与 elision；array pattern 通过通用 `@@iterator` 缓存 `next` 并在 normal early completion 执行 `return`；同步 `for...of` 复用同一 iterator record。array/object rest、abrupt `IteratorClose`、iterator-result object 校验与 per-iteration lexical environment 仍待闭合。
- [x] 实现 generator fiber、yield/yield*、return/throw injection；普通与 delegated suspension、abrupt
  injection、iterator close/error precedence 均已完成；async generator request queue 独立留到 M10。
- [x] derived class constructor/method 纵切：owned HIR/bytecode/verifier 区分 class、ordinary function 与
  non-constructible class method，支持
  `extends`、唯一显式 instance constructor、动态 superclass `super(...)`、`this` TDZ/单次初始化、
  derived return restriction、class 普通 call TypeError、`new.target` 转发及 constructor/prototype wiring。
  sparse derived-activation side vector不扩大普通 Frame；N=1/2/4/8/16、forced-major 与 Promise subclass
  trampoline 均通过。新增 `DefineClassMethodById` 以非枚举 descriptor 发布静态名称的 instance/static
  methods；method strict、name、无 own prototype、不可 `new` 与 forced-major 均通过。无显式 constructor 的
  derived class 生成 `SuperConstructForwardAll -> InitializeThis -> ReturnUndefined`，直接复用 Frame 的完整
  argument source/prefix/count；N=1/2/4/8/16、native-owned Promise executor 与 forced-major 均覆盖，
  `subclass` 从 40/217 提升到 86/217，完整 `statements/class` 达到 1398/8662。
- [x] base class constructor 纵切：无 heritage 的 class 生成独立 `BaseClassConstructor` metadata 与
  `CreateBaseClass`，显式/默认 constructor 均为 strict 且普通 call 抛 TypeError；construct 在 body 前建立
  receiver，object return 替换、primitive/undefined 回退，并发布 `%Function.prototype%`/`%Object.prototype%`
  标准原型对和 non-writable `prototype` descriptor。N=1/2/4/8/16、forced-major 与 facade return/descriptor
  fixtures 均通过；完整 `statements/class` 从 1398/8662 提升到 1906/8662，unsupported 从 7132 降到
  6308，新增暴露的 deeper semantic failures 不重分类为 unsupported。
- [x] class accessor 纵切：getter/setter 使用独立 `DefineClassGetterById`/`DefineClassSetterById`，函数名按
  `get name`/`set name` 推导，instance/static accessor descriptor 为 non-enumerable/configurable，复用既有
  accessor merge、receiver 与 GC barrier；N=1/2/4/8/16、forced-major、facade descriptor/name 与 Test262
  `class/definition/accessors.js` 2/2 通过。完整 class 目录达到 2000/8662，applicable 2000/8635，
  unsupported 6212；仍未把其余 accessor-name/computed/private 目录误报为完成。
- [x] computed public class element 纵切：HIR 保存 `Static | Computed(HirExpression)` key，lowering 按 class
  element 源码顺序执行 key expression、`ToPropertyKey`、closure、runtime method/accessor name 与安装；新增
  `SetFunctionNameByValue`、`DefineClassMethodByValue`、`DefineClassGetterByValue`、`DefineClassSetterByValue`。
  Symbol key 使用 `[description]` name，instance/static 交错和 accessor merge 均覆盖；N=1/2/4/8/16、
  forced-major 及前后 Test262 report comparison 均通过，class 目录达到 2132/8662，applicable 2132/8635，
  unsupported 6074，fixed 132、broken 0。
- [x] `super.property` 纵切：`super.foo`/`super[key]` 及 call 形式按规范区分动态 superclass 与当前
  receiver，class method、base constructor、derived constructor 均通过 `[[HomeObject]]` 解析；constructor
  activation 使用独立 sparse side vector，不扩大 104-byte `Frame`，并在环境分配回滚、正常 return、异常
  unwind 与 forced-major root tracing 中保持对称。N=1/2/4/8/16、动态 `Object.setPrototypeOf`、提取方法
  自定义 `this` 与 Test262 `statements/class/super` 16/16 通过；相对 computed baseline 为 20 fixed、0 broken。
- [x] named class expression lexical environment：HIR 分离 BindingIdentifier identity 与 `class.scope_id`
  owner；`EnterClassEnvironment -> InitializeClassEnvironment -> LeaveClassEnvironment` 建立单槽 immutable
  TDZ binding，constructor/method/nested closure 捕获该 environment，离开后不泄漏外层名称。binding plan 用
  `ClassEnvironment` 与 function-owned slot 分型，handler metadata 保存 class environment depth，正常完成、
  same-frame throw/catch、frame unwind 与 forced-major 均恢复/trace 精确；verifier 拒绝未平衡环境和跨 depth
  jump。N=1/2/4/8/16、depth=0/1 capture、heritage TDZ、outer shadow、Promise `then` 146/146 均通过；
  `expressions/class` 达 1866/8027，相对上一 baseline 6 fixed、0 broken、0 changed。
  `statements/class` 达 2160/8662，相对 super baseline 8 fixed、0 broken；18 changed 均为移除 class-expression
  guard 后继续暴露的既有 semantic failure。
- [x] public static fields 纵切：HIR 改为保留 method/field 源顺序的 `HirClassElement`，每个 initializer 编译为
  strict、non-constructible 的 `ClassFieldInitializer` hidden stencil；computed keys 全部先求值一次，再初始化
  inner class-name binding，最后按 static source order 以 class constructor 作为 `this`/`[[HomeObject]]` 调用
  initializer。`DefineFieldById/Value` 创建 W/E/C=true own data property，不走 assignment/setter；anonymous
  initializer result 在 define 前推导 field name。class declaration 与 named expression 统一拥有 inner class
  environment，environment resolution 改为沿 scope ancestor 选择最近 owner；synthetic initializer boundary 会把
  Oxc 未标记的 outer parameter/let/var 精确提升为 capture，同时不误提升 class-name slot。N=1/2/4/8/16、
  forced-major、computed-key TDZ、outer capture、this/super/name/descriptor 均覆盖；`statements/class` 达
  2194/8662（34 fixed、0 broken），`expressions/class` 达 1884/8027（18 fixed、0 broken）。
- [x] public instance fields 纵切：constructor stencil 显式标记 instance-element 初始化点，base constructor 在
  parameter/default/body 前执行，derived constructor 在首次成功 `InitializeThis` 后执行；第二次 `super()` 先按
  重复 BindThis 抛错，`super()` 前直接返回 object 不运行 fields。computed keys 在 class evaluation 按源码顺序
  只求值一次，key/initializer/name-inference 三元组通过 verified contiguous register window 冻结为 exact-size、
  traced `ClassFieldPlan`；rare `ClassBytecode` payload 不扩大 16-byte `FunctionExecutable`、56-byte
  `FunctionObject` 或 104-byte `Frame`。initializer return、anonymous name inference 与 Proxy
  `[[DefineOwnProperty]]` 统一使用 traced resumable continuation，forced-major 下 scratch register 保持返回值；
  own data descriptor 固定 W/E/C=true，不触发 inherited setter，throw 后保留已完成字段。N=1/2/4/8/16、
  forced-major、Symbol/computed key、outer capture、instance `super`、Proxy/non-extensible receiver 与 partial
  initialization 均覆盖；`statements/class` 达 2503/8662（相对 static baseline 309 unsupported->pass、
  82 unsupported->semantic-failure、0 broken），`expressions/class` 达 2172/8027（288、82、0）。
- [x] class static blocks 纵切：HIR 保留独立 `StaticBlock` element 与 strict hidden stencil，但 runtime 复用
  non-constructible `ClassFieldInitializer` callable contract；static fields/blocks 在全部 computed keys 求值、inner
  class-name binding 初始化后通过 exact-capacity ordered queue 按源码顺序交错调用。block 以 constructor 作为
  `this`/`[[HomeObject]]`，支持动态 `super.property`、`new.target === undefined`、var/function hoisting、独立 lexical
  scope、outer/class-name capture 与 abrupt completion；Oxc `ClassStaticBlock` scope 纳入 synthetic function owner
  判定，使 block-local -> nested closure 与 outer -> block 两个 capture 方向均拥有精确 environment slot。没有新增
  opcode、Frame字段或 Rust递归；N=1/2/4/8/16 与 forced-major 均覆盖。`statements/class` 达 2529/8662
  （26 fixed、0 broken），`expressions/class` 达 2174/8027（2 fixed、0 broken）；剩余6个带
  `class-static-block` 标签的 unsupported 均交叉依赖 generator/async 或 private names。
- [x] instance private data field 纵切：HIR 使用稳定 module-local `{class, element}` identity，class evaluation
  为每个 private declaration 分配 fresh Symbol payload，并写入“可选 class-name slot + private slots”的
  exact-size lexical environment；`EnterClassEnvironment`/`InitializeClassEnvironment` 携带并验证 slot count/index。
  shape key 新增不可公开伪造的 `Private(SymbolId)` 域，ownKeys 过滤且 storage 精确 trace key/value；private
  define/get/set 不走 prototype、descriptor 或 Proxy trap，错误 brand/重复初始化转 TypeError，ordinary
  non-extensible receiver 拒绝新增 private slot。Proxy 使用惰性 private-only ordinary sidecar，不把字段写入 target，也不扩大
  ordinary object 热布局；sidecar allocation 显式 root receiver 并 barrier 发布。instance-element record 使用
  key/payload/infer-name/kind 四元组，capacity/verifier 同步按 `count * 4`。已覆盖 default/initializer order、
  read、simple/compound assignment、prefix/postfix update、nested closure、同名 nested class identity、outer private
  capture、hidden ownKeys、wrong receiver、non-extensible rejection、Proxy bypass、N=1/2/4/8/16 与 forced-major。
  static private fields、private accessors、`#x in object` 仍待后续纵切。`statements/class` 达 2703/8662
  （174 fixed、120 unsupported->semantic-failure、0 broken），`expressions/class` 达 2294/8027
  （120 fixed、110 unsupported->semantic-failure、0 broken）；因此完整 class 总项不打勾。
- [x] synchronous instance private methods 纵切：HIR 区分 `PrivateMethod`，class evaluation 只创建一次带
  `C.prototype` `[[HomeObject]]` 与 `#name` 的 shared closure；统一 `ClassInstanceElementPlan` 使用
  `PublicField/PrivateField/PrivateMethod` kind，按“全部 private methods、再按源码顺序 fields”初始化，并保持
  exact-capacity 四槽 record window。private method 以 non-writable hidden slot 挂到每个 instance，错误 receiver、
  assignment/update 均转 TypeError；private-member call 使用 receiver-preserving `CallWithReceiver`，不能把
  `this.#method()` 降为丢失 base reference 的普通 `Call`。普通 non-extensible receiver 拒绝 method stamping，
  Proxy sidecar 仍绕过 traps。shared identity、initializer visibility、dynamic `super`、private field access、
  N=1/2/4/8/16 与 forced-major 均覆盖；`statements/class` 达 2865/8662（相对 private-field baseline
  162 fixed、148 unsupported->semantic-failure、0 broken），`expressions/class` 达 2436/8027（142、144、0）。
  private accessors、static private elements、`#x in object` 与 lexical arrow-`this` 仍待后续纵切。
- [x] synchronous instance private accessors 纵切：HIR 将同名 private getter/setter 合并为一个
  `PrivateAccessor`，class evaluation 创建带 prototype `[[HomeObject]]` 与 `get #name`/`set #name` 的 closure，
  再用 cold `CreateAccessorPair` opcode 分配一次 shared pair；实例计划新增闭合的 `PrivateAccessor` kind，并把
  shared pair 作为 non-writable hidden accessor slot stamping 到每个 receiver。`GetPrivate`/`SetPrivate` 复用
  既有可恢复 `PropertyGet`/`PropertySet` native continuation，保持 receiver、assignment result、abrupt completion
  和 compound/update 单次求值顺序；缺失 getter/setter、错误 brand 与 ordinary non-extensible receiver 转
  TypeError，Proxy sidecar 继续绕过全部 traps。没有扩大 104-byte `Frame` 或 native-continuation enum；已覆盖
  getter/setter pair、getter-only/setter-only、dynamic `super`、多 accessor exact-capacity、N=1/2/4/8/16 与
  forced-major。`statements/class` 达 3038/8662（相对 private-method baseline 173 fixed、0 broken），
  `expressions/class` 达 2568/8027（132 fixed、0 broken）；剩余 accessor 组合失败依赖 lexical arrow-`this`、
  legacy `__lookupGetter__`/`__lookupSetter__`、static private elements、generator/async 或既有 descriptor harness。
- [x] synchronous static private elements 纵切：`HirPrivateField/Method/Accessor` 显式携带 `is_static`，不复制
  static/instance element 类型；class evaluation 先把所有 static private method/accessor shared payload 通过
  `DefinePrivateMethod/DefinePrivateAccessor` cold opcode 装到 defining constructor，再初始化 class-name binding，
  最后让 static private fields 通过 `DefinePrivateField` 与 public static fields/static blocks 按源码顺序交错执行。
  private data/method/accessor 沿用 hidden `PropertyKey::Private` storage、writable/non-writable/accessor kind 和现有
  resumable Get/Set continuation，不新增 class object、Frame、native continuation 或热对象字段；纯 static class
  不创建 prototype register 或 `ClassInstanceElementPlan`。已覆盖 uninitialized/initialized field、method-before-field、
  shared method identity、getter/setter、dynamic `super`、field/block order、nested lexical identity、method overwrite、
  subclass/instance/Proxy wrong receiver、N=1/2/4/8/16 与 forced-major。`statements/class` 达 3383/8662
  （相对 instance-accessor baseline 345 fixed、166 unsupported->semantic-failure、0 broken），`expressions/class`
  达 2916/8027（348、158、0）；剩余 static-private failure 交叉依赖 direct eval、lexical arrow-`this`、async/generator
  或既有 public descriptor/propertyHelper 语义。
- [x] private brand check expression 纵切：owned `HirExpressionKind::PrivateIn` 保留 lexical private identity 与 RHS，
  dedicated `HasPrivate` cold opcode 直接查询 receiver own hidden shape/Proxy private sidecar；不执行 getter/method、
  不做 ToPropertyKey、prototype walk、Proxy `has` trap 或 continuation。missing brand 返回 false，primitive RHS 转
  TypeError，RHS abrupt completion/单次求值保持原序。已覆盖 field/method/accessor、static/instance、nested同名
  identity、unresolved/primitive RHS、stamped/unstamped Proxy trap bypass、N=1/2/4/8/16 与 forced-major；
  `language/expressions/in` 的 `class-fields-private-in` 34/38 variants 通过，剩余4个仅依赖 await/yield。
  `statements/class` 保持3383/8662、`expressions/class` 保持2916/8027，两个完整分区均0 broken。
- [x] 完成 synchronous class fields、static blocks 与 private names substrate：class method/constructor/super/name
  environment、public/static/instance fields、static blocks、private data/method/accessor、static private elements 与
  private brand check 已形成闭合纵向路径；async/generator、direct eval 与 lexical arrow-`this` 由其所属后续纵切
  闭合，不能把本项外推为完整 class/test262 semantics。
- [x] 实现 strict proper tail call 语义路径并启用可证明 bytecode frame reuse；100,000 层 Test262
  helper 与 N=1/2/4/8/16 已验证，普通 call-loop clean-HEAD 复测为 4.240/4.298 ms，对照同轮 Boa
  8.993 ms，仍快约 2.09x。macOS host gate 无 affinity/governor，数字只作为无灾难性回归证据。
- [ ] 完成 Error subclasses、cause、stack capture；首批 Error/ReferenceError/SyntaxError/TypeError
  constructor/prototype identity 与 VM abrupt conversion 已完成，name/message ToString/attributes 等仍待闭合。
- [ ] 实现 direct eval/indirect eval、with 和 dynamic lookup；标记并绕过不安全优化。
- [ ] 实现 script/realm/global declaration instantiation 和 Annex B 配置。

验收：专项测试覆盖 await 之前全部 completion 组合；test262 language/control-abstraction、
statements、expressions 和 classes 分类结果可复现，无 native stack overflow。

## 14. M7: Rust Host SDK 同步边界

这是产品差异化里程碑，不允许推迟到语义完成后临时包装。

### M7.1 Typed conversion 与 handle API

- [ ] 定义 `FromJs<'scope>`、`FromJsOwned`、`IntoJs`、`IntoJsError`。
- [ ] sync conversion 可借用 `JsStr<'scope>`/buffer view；async conversion 只能产生 owned 值。
- [ ] 覆盖 primitive、Option、Result、tuple、Vec、map、string、bytes、Local、Persistent。
- [ ] tuple arity 0..=8 可用小型 macro 生成重复 impl；文档说明 Rust 无 variadic generics 是原因。
- [ ] conversion error 保存参数位置、expected/actual 和 JS cause，不用字符串拼接丢失结构。
- [ ] facade 不公开 `Value` bits、raw heap ref 或 GC address。

### M7.2 Native function 与受控重入

- [ ] builder 注册 typed sync function、method、getter/setter、constructor 和 variadic raw callback。
- [ ] callback 接收 `CallScope`、this、new.target 和参数 view；普通 typed path 自动转换参数。
- [ ] `CallScope::call/construct` 支持 JS -> Rust -> JS 同步重入，建立 recursion limit 和 root frame。
- [ ] host panic 直接 abort；不得捕获、恢复或转换为 JS throw/poisoned isolate。
- [ ] host callback 可返回 JS throw、Rust host error 或正常 value；三者不能混为 panic。
- [ ] benchmark host call overhead、argument conversion 和 reentry，对比 QuickJS C callback + Rust wrapper。

### M7.3 Extension composition

- [ ] `Extension` 只在 engine/isolate template 构建时安装 immutable descriptors。
- [ ] builder 注册 globals、native classes、synthetic modules、realm initializer 和 hooks。
- [ ] extension name/version/dependency/conflict 检查稳定且有确定顺序。
- [ ] duplicate global/module/class registration 默认报错，不由安装顺序静默覆盖。
- [ ] extension 不支持 live unload；动态状态通过 `Arc<T>` resource 更新。
- [ ] type-safe engine/isolate/realm resource table 不使用全局变量或 TLS。

### M7.4 Host object 与 external memory

- [ ] native class payload 支持 isolate-local resource ID 和 `Arc<T: Send + Sync>`。
- [ ] payload finalizer 不执行 JS；复杂关闭进入 cleanup job。
- [ ] ordinary wrapper properties仍使用 VM shape，native method/accessor 才进入 host callback。
- [ ] ArrayBuffer 支持 copy、owned transfer、shared backing 和显式 detach。
- [ ] 自定义 external buffer 需要小型 audited unsafe trait；safe constructors 覆盖 Vec/Box/Arc 常见场景。
- [ ] external bytes 进入 isolate/global memory accounting，shared allocation不能逃过配额。

### M7.5 可插拔宿主能力

- [ ] clock、timezone provider、entropy source 分离，test262/benchmark 可注入 deterministic 实现。
- [ ] locale/ICU data、module source、deadline 与 tracing subscriber 均由宿主提供；provider 缺失使用
  typed error/明确默认语义，不能 fallback 到 env、当前目录、系统 locale 文件或系统 clock。
- [ ] promise rejection、uncaught exception、resource limit、tracing hook 分离。
- [ ] 不允许扩展替换 ECMAScript microtask ordering、GC barrier 或普通 property semantics。
- [ ] 每个 hook 标明调用线程、是否可重入、是否可分配和延迟预算；panic 后果固定为 abort。

验收：示例 extension 同时提供 typed function、native class、synthetic module、zero-copy buffer 和
custom clock；无 `unsafe` 的普通用户代码可完成嵌入。host microbench 建立 QuickJS 对照基线。

## 15. M8: Builtins、Module 与高级语义

### M8.1 核心 builtin 分组

按依赖顺序实现，每组完成后运行对应 test262：

- [ ] Object、Function、Boolean、Number、String、Symbol。
- [ ] Array、Iterator helpers 所属 release target、TypedArray、ArrayBuffer、DataView。
- [ ] Map、Set、WeakMap、WeakSet、WeakRef、FinalizationRegistry。
- [ ] Math、BigInt、Date、JSON、Reflect、Proxy。
- [ ] RegExp 和 String regexp integration；后端必须满足 ECMAScript 语义，不以宿主 regex 语义替代。
- [ ] 完成 Error、AggregateError、SuppressedError 及 release target 中的 disposal protocol；首批四类 native
  Error hierarchy/call/construct 已完成，不把该纵切等同于完整 Error builtin。
- [ ] Atomics、SharedArrayBuffer 和 test262 agent 所需同步原语；SAB 是唯一 shared JS memory，
  `Atomics.wait` 使用 provider parking/事件唤醒而非 spin，runner thread/sleep 不进入 engine core。
- [ ] Intl 使用 ICU4X provider 抽象并纳入 all-features release conformance；provider 数据可由
  embedder 裁剪，但标准 ECMA-402 测试不能通过关闭 feature 从 98% 分母移除。
- [ ] Temporal 及其他 proposal 以固定 test262 commit 对应的标准状态决定；进入 release target
  后纳入主通过率，尚未标准化时单独统计。

每个 builtin 抽象操作放在语义模块，opcode 只保留 verified fast path 和 slow-path 调用。

### M8.2 RegExp backend 与 ECMAScript integration

- [ ] 精确锁定 `regress` 版本并启用 UTF-16/UCS-2；先跑其完整 upstream tests 和 Boa 已知
  regression tests，dependency upgrade 必须单独提交。
- [ ] 在 `tachyon-vm::regexp::backend` 建立静态封装：owned compiled program、Latin1/Utf16 borrowed
  input、code-unit start index、match/no-match/interrupted/resource-limit outcome，不泄漏 regress type。
- [ ] flags parser 支持 `d/g/i/m/s/u/v/y`、canonical order、duplicate error 和 `u`/`v` conflict；
  pattern syntax error 对 literal 是 early error，对 constructor 是 runtime `SyntaxError`。
- [ ] 匹配与 capture range 全部使用 UTF-16 code-unit offset；覆盖 unpaired surrogate、surrogate pair、
  non-unicode UCS-2、Unicode code point、Unicode properties 和 `v` string/set operations。
- [ ] 为 regress 增加或 upstream Latin-1 input indexer，避免 Boa 当前 Latin-1 `to_vec` 扩宽；临时
  fallback 只能使用复用、受配额的 scratch buffer，并由 benchmark 决定删除期限。
- [ ] 为 classical backtracking 增加 step budget 与 interrupt/cancel checkpoint。`max_regex_steps`
  属于 typed resource config；checkpoint interval 属于 tuning。所有 backend stack/buffer 使用
  checked limit/`try_reserve`，复杂 pattern 不能 panic、OOM abort 或绕过 VM hard fuel。
- [ ] `CompiledRegExp` immutable + Send + Sync；engine cache key 只含 source 和 `i/m/s/u/v`，不含
  `d/g/y`。cache 同时限制 entry 与 compiled bytes，缓存 compile error并记录 hit/miss/eviction。
- [ ] RegExp literal 每次求值创建独立对象但共享 compiled program；dynamic constructor、Annex B
  compile 和跨 realm prototype 保留规范 identity/observable behavior。
- [ ] 实现 `RegExpBuiltinExec`：lastIndex get/set/failure reset、global/sticky、empty match、captures、
  named/duplicate named groups、`d` indices/groups 和 exact result-array layout。
- [ ] 实现 `RegExpExec` custom exec path、species 与 Proxy/getter observable ordering；builtin fast path
  只在 receiver shape、exec/prototype watchpoint 和 ordinary lastIndex 全部命中时启用。
- [ ] 实现 `@@match`、`@@matchAll`/RegExp String Iterator、`@@replace`/GetSubstitution、`@@search`、
  `@@split`，并接入 String.prototype.match/matchAll/replace/replaceAll/search/split。
- [ ] 实现 `source` escaping、flags getters、toString、test、exec、RegExp.escape 和 release target
  要求的 Annex B/legacy behavior。
- [ ] compiled metadata 保存 capture/name count 与优化 hint；test-only 模式避免 JS result allocation，
  capture/indices buffer 按精确计数预留并进入 capacity stats。
- [ ] test262 运行 language/literals/regexp、built-ins/RegExp、RegExpStringIterator、String regex methods、
  staging regress；不得把 backend bug 放入 ignore 来维持 98%。

验收：RegExp 相关适用 test262 >= 98%，panic/crash/非 allowlist timeout 为零；adversarial pattern
能被 resource budget 中断；Latin-1/UTF-16 steady-state match 无每次输入转换 allocation。

### M8.3 默认原生 TC39 Signals

- [x] M8.3 第一纵切：所有正常 Realm 默认安装原生 `Signal.State`、`Signal.Computed` 与
  `Signal.subtle.Watcher`；GC payload 支持 State get/set、Computed lazy callback/cache/依赖失效，及
  Watcher ordered watch/unwatch/getPending。覆盖 dispatch N=1/2/4/8/16 与 forced-major；同步 notify、
  custom equals/hooks、Checked cleanup、完整 proposal suite 仍由下列条目追踪。
- [x] M8.3 constructor/Realm API contract slice：State/Computed/Watcher constructor、prototype 和方法的
  descriptor/name/length/new-only/brand 已覆盖，subclass/newTarget 选择派生 prototype，Computed callback
  receiver 保持实例本身；main/child Realm namespace identity 分离且 branded method 可跨 Realm 使用。
  dispatch N=1/2/4/8/16、forced-major 与 child Realm 回归通过。proposal well-known symbols、options/hooks、
  notify/frozen/untrack/introspection 和完整 pinned suite 仍由下列未完成项追踪，不能据此勾完整 M8.3。
- [x] M8.3 cross-Realm options/subclass exception slice：foreign State/Computed constructor 的 options
  `watched/unwatched` Get 使用 constructor defining Realm 的 proposal symbols，而实例 prototype 仍按 newTarget
  Realm/derived prototype 选择；构造器 Realm symbols 发布在现有 traced options record，不增加 continuation
  kind 或常驻 payload。local subclass extends foreign State/Computed、foreign Watcher subclass、hook receiver/
  order、跨 Realm Computed cycle 的异常 identity/cache/dependency-change recovery、N=1/2/4/8/16 与
  forced-major 已覆盖，并在 default/no-default/all-features 构建验证。
- [x] 在 release manifest 固定 proposal commit（初始研究 revision
  `9124ed91b24bb02ff7408b2fcf5abb6e18b095d7`）、API hash、Stage，以及 reference polyfill/tests/
  benchmark commit（初始研究 revision `1c33f914806f0872229cba05a1c882a38c0def4f`）；升级必须单独
  提交并列出 observable API/algorithm 差异。
- [x] M8.3 pinned public guard slice：从固定 reference polyfill 的 `guards.test.ts` 与
  `public-api-types.ts` 补齐 realm-local `Signal.isState/isComputed/isWatcher`，对对应 native brand 与 subclass
  返回 true，跨 Realm 可识别同 isolate native brand，其他 Signal brand、普通值和 Proxy wrapper 返回 false，
  且不触发 Proxy trap。release manifest 的 Stage 1/API hash 与第八组 fixture 已更新，N=1/2/4/8/16、
  forced-major、descriptor/name/length/non-constructor、cross-Realm 与 default/no-default/all-features build
  已覆盖；完整 upstream graph/Watcher suite 仍由后续未完成项追踪。
- [ ] `Signal` 和 `Signal.subtle` 在所有正常 realm 默认安装；不提供默认关闭 Cargo feature、
  extension opt-in 或 runtime config，`--no-default-features` 也不能移除该全局 API。
- [ ] realm-local 创建 State/Computed/Watcher constructors、prototypes、well-known-for-proposal symbols；
  覆盖 descriptor attributes、name/length、new-only、subclass/newTarget、brand 与 cross-realm identity。
- [ ] 定义 GC native layouts：State 保存 value/equals/hooks/live sinks，Computed 保存 callback/cached
  completion/state/ordered sources/live sinks/generation，Watcher 保存 notify/state/ordered watched signals。
- [ ] isolate 保存 agent-wide `computing`、`frozen`、monotonic generation 和 reusable graph worklists；
  不使用 TLS，worker migration 后 currentComputed/frozen 状态必须完全一致。
- [x] State constructor/options 按 observable property access 顺序读取 equals/watched/unwatched；默认 equals
  使用规范 `Object.is`，callback `this` 是 signal，异常不能留下半初始化 graph node。
- [ ] State.set 实现 equals、unchanged fast path、direct dirty、transitive checked 与 Watcher pending；
  使用 iterative ordered DFS，深度/fanout 不耗尽 Rust stack。
- [ ] graph coloring 完成后按 depth-first observable 顺序同步运行所有原 watching Watcher notify；
  notify 期间 frozen，单异常原样抛出，多异常按顺序构造 `AggregateError`，其余 notify 仍全部运行。
- [ ] Computed 初始 Dirty，get 实现 deepest-left-most lazy pull、Checked cleanup、Dirty propagation、
  value/throw completion cache、default/custom equals 和 Computing cycle detection。
- [ ] callback throw、equals throw、termination 和 allocation/resource error 使用 restoration guard 恢复
  `computing`/`frozen`；错误缓存/重抛与下次 dependency change 后重算符合 pinned proto-spec。
- [x] M8.3 Signals callback-dispatch resource slice：以 host completion quota 确定性覆盖 Computed callback、
  custom-equals Computed、State custom equals、State/Computed `watched`/`unwatched` hooks、nested `untrack`、
  Watcher notify 的 continuation 发布失败；失败后 agent-wide
  `computing`/`frozen` 恢复，随后 fresh job 可重新计算、re-arm Watcher 并保持 graph 可用。N=1/2/4/8/16
  与 forced-major 已覆盖。terminal host error 与 fresh execution 丢弃 suspended Fiber 前均由 execution-level
  cancellation 逆序恢复 native continuation：Computed 从 traced operation record 恢复 old sources/Dirty，
  Watcher 解冻并落到合法 waiting 状态，`untrack` 恢复 previous owner；hook 前已提交的 attach/detach 不回滚，
  但 graph membership 保持合法且 fresh job 可继续 re-arm/set/notify。pinned suite 11/11 fixtures、19 files、
  70 definitions、114 assertions 通过。其余 native allocator failpoint 尚未 exhaustive，不能据此勾完整
  termination/resource matrix。
- [ ] 每次 recompute 用 old/new 双 ordered source buffer 动态更新依赖，去重但保留首次 read 顺序；
  分支切换、重复 read、nested computed、computed write 与 untracked read 有专项 trace assertions。
- [ ] 只有递归连接 Watcher 的 live graph 建立 source→sink 反向强边；Computed→source 与
  Watcher→watched signal 被 GC 精确 trace，不使用 host persistent root 伪造 graph ownership。
- [ ] watch/unwatch 先完整验证参数再按左到右更新 ordered set/live edges；first/last live sink 递归触发
  watched/unwatched，callback throw 后 graph 与 Watcher state 仍满足不变量。
- [x] M8.3 State options/live-hook slice：options 按 equals→watched→unwatched observable Get，默认
  Object.is/custom equals、direct State 与静态 Computed source 的 first/last live hook、异常后 graph invariant、
  N=1/2/4/8/16 与 forced-major 已覆盖；dynamic dependency diff、完整 Checked coloring/notify 仍未完成。
- [x] M8.3 synchronous notify/dynamic dependency lifecycle slice：State change 使用 ordered iterative DFS 将
  immediate Computed 标 Dirty、transitive clean Computed 标 Checked，并只将原 Watching Watcher 入队一次；
  graph coloring 后在 set 返回前同步 notify，callback this/frozen、单异常 identity、多异常 ordered
  AggregateError、其余 callback 继续执行均覆盖。Computed recompute 的 old sources 保存在 GC-traced operation
  record，新 sources 保持首次 read 顺序，成功后仅对 added/removed edges 递归 attach/detach 并执行
  watched/unwatched；N=1/2/4/8/16、forced-major、branch switch 与 temporary Aggregate roots 已覆盖。
  deepest-left-most Checked pull、callback/equals throw cache、完整 diamond unchanged cleanup 仍由未完成项追踪。
- [x] M8.3 Computed checked-pull/callback-completion slice：单次 GC-traced operation 保存 iterative ordered
  DFS frame 与 old sources，按 deepest-left-most 顺序拉取 Dirty source；unchanged source 将 nested Checked
  chain 清回 Clean，changed source 才提升下游并重算，diamond shared source 每轮最多执行一次。callback throw
  以原 identity 缓存并在 dependency change 前重放，cycle 同样形成稳定 abrupt completion；normal/abrupt 都
  恢复 agent-wide computing，且不增加常驻 Computed payload size。N=1/2/4/8/16、forced-major、diamond、
  nested pruning、throw identity 与 dependency-change recompute 已覆盖。custom Computed equals/equals throw、
  termination/resource restoration 和完整 pinned suite 仍由未完成项追踪，不能据此勾完整 M8.3。
- [x] M8.3 Computed custom-equals slice：constructor 先校验 computation callable，再以 resumable
  Proxy-aware Get 读取 `options.equals` 并执行 IsCallable；仅 custom-equals 实例分配 GC-traced cold sidecar，
  不增加常驻 Computed payload。首次计算不调用 equals，后续重算以 signal receiver 和 old/new 参数调用；
  equals 内 Signal read 归属 inner Computed，truthy 结果剪枝 Checked diamond，throw completion 按 identity
  缓存并在 dependency change 后失效。callback/getter/Proxy abrupt、non-callable equals、N=1/2/4/8/16、
  forced-major、diamond 与 pinned custom-equality tracking case 已覆盖。termination/resource exhaustive
  matrix 和完整 pinned suite 仍未完成，不能据此勾完整 M8.3。
- [x] M8.3 callable Proxy/prohibited-context slice：State custom equals、Computed callback/custom equals 与
  Watcher notify 全部使用规范 IsCallable，而不是只接受直接 FunctionObject；callable Proxy 保持 receiver、
  参数和同步 notify 语义，non-callable Proxy 在对应入口抛 TypeError。Computed callback 内允许 State 读写，
  自写依赖在 normal/nested pull 后回到 Clean 且缓存不重复执行。N=1/2/4/8/16 与 forced-major 已覆盖；
  revoked Proxy、allocator failpoint 和完整 pinned suite 仍由后续条目追踪。
- [x] M8.3 Computed watched/unwatched slice：constructor 按
  `equals -> watched -> unwatched` observable Get 顺序读取 options，equals IsCallable 在读取 hooks 前完成；
  custom callback 复用 GC-traced cold sidecar且不扩大默认 Computed payload。first/last recursive live
  transition 按 Computed-before-ordered-sources 顺序同步调用 hooks，receiver、重复 Watcher 去重、动态依赖、
  hook/getter abrupt 后 graph invariant、N=1/2/4/8/16 与 forced-major 已覆盖。termination/resource exhaustive
  matrix 和完整 pinned suite 仍未完成，不能据此勾完整 M8.3。
- [x] M8.3 Watcher state-machine slice：Waiting/Watching/Pending、zero-argument 与 duplicate `watch`
  re-arm、`unwatch` 不隐式 re-arm、ordered dirty-Computed `getPending`、initial Dirty 与 unchanged Checked
  cleanup 已按 pinned polyfill 对齐；State-only notify 的 pending 为空，Waiting 期间再次 dirty 仍保留 pending，
  callback throw 后进入 Waiting 且 identity/graph 不变量不变。notify frozen 期间 State/Computed get/set 与
  Watcher watch/unwatch/getPending 均拒绝；watched/pending edge 写屏障、N=1/2/4/8/16、forced-major、duplicate
  hook 去重与 ordered snapshot 已覆盖。继续复用 `OrderedSignals` 和 `tuning::signals` capacity，不增加
  `WatcherSignal` 常驻字段或 size class；完整 untrack/introspection/pinned suite 仍由后续条目追踪。
- [ ] Watcher 剩余 contract：补齐 termination/resource-error exhaustive matrix，并导入完整 pinned
  Watcher suite；completion quota dispatch failure 已由上方 resource slice 覆盖，已完成的
  state/pending/frozen 部分不得在此重复计数。
- [x] M8.3 Watcher fanout AggregateError/resource-root slice：7-way ordered notify fanout 在单次 set 与
  explicit re-arm 后再次触发，偶数位 callback errors 按 watcher DFS 顺序组成 AggregateError，奇数位仍
  执行；所有 Watcher 进入 Waiting、pending 清空、error identity 保持，temporary Aggregate roots 在
  forced-major 下存活。N=1/2/4/8/16、forced-major 与 no-default build 已覆盖；allocator failpoint/terminal
  cancellation exhaustive matrix 仍由剩余 contract 条目追踪。
- [x] M8.3 `untrack` slice：realm-local `Signal.subtle.untrack` 按 pinned polyfill 暂停 outer dependency
  owner，nested normal/throw 按 continuation 栈恢复 agent-wide `computing`；回调内触发的 Computed 仍追踪
  自身 sources，但不把该 Computed/直接 State read 泄漏给 outer owner。previous owner 保存在 GC-traced
  32-byte native continuation，不依赖 Rust unwind/TLS/栈值；completion reserve 与同步 call failure 均在
  owner 清空前失败或对称恢复。notify frozen 入口先于 callback dispatch 拒绝，non-callable 后 owner 不受损；
  descriptor/name/length/non-constructor、Proxy callback、cross-Realm、N=1/2/4/8/16 与 forced-major 已覆盖。
- [ ] `untrack` 剩余 termination/resource-error exhaustive matrix；不得据当前 normal/throw 覆盖误勾完整
  Signals 或完整 GC liveness。同步 continuation quota failure与异步 callback 内 host terminal error 已复用
  execution-level cancellation 覆盖；其余 allocator failpoint 仍需补齐。
- [x] M8.3 `currentComputed` slice：realm-local `Signal.subtle.currentComputed` 直接读取 agent-wide
  dependency owner；Computed callback/custom equals/nested Computed 返回当前 owner，top-level、Watcher notify、
  live hook 与 `untrack` 返回 undefined。该只读查询在 frozen notify 内允许调用，不分配、不追踪依赖且不暴露
  graph payload；descriptor/name/length/non-constructor、cross-Realm、normal/throw owner restoration、
  N=1/2/4/8/16 与 forced-major 已覆盖。
- [x] ordered `introspectSources/Sinks`、`hasSources/Sinks` 返回精确容量、GC-traced packed Array
  snapshot，不暴露 internal buffer/cold reverse index/tombstone；sources 保留首次 read/watch 顺序，sinks
  只投影递归 live consumers。descriptor/name/length/non-constructor、domain brand、dynamic dependency、fresh
  snapshot、frozen rejection、cross-Realm、N=1/2/4/8/16 与 forced-major 已覆盖。
- [ ] minor/major/forced GC 精确 trace 全部 Signal edge/callback/value并保持 identity；forced-GC 在
  constructor、get、set、recompute、notify、watch/unwatch 和 introspection 每个可分配点运行。
- [x] M8.3 Signals forced-minor allocation matrix：Signals 专用高容量测试 isolate 在
  `ForcedCollectionMode::Minor` 下逐个覆盖 constructor、lazy get、set propagation、recompute、notify、
  watch/unwatch 与 ordered introspection；N=1/2/4/8/16 全部通过。该项验证了每次 young allocation
  的 edge/root 正确性，major/minor/forced 的全分配点组合与 resource-error exhaustive matrix 仍未闭合。
- [x] GC liveness tests 覆盖 unobserved Computed 独立回收、rooted source 保活 active Watcher、unwatch
  解除保活、unreachable watched cycle 回收和 watched/unwatched hooks 不被 GC 私自调用。
- [x] M8.3 Signals graph ownership/major-GC slice：以跨 job `WeakRef` oracle 验证 rooted State 的 live
  reverse edge 保活 active Watcher，`unwatch` 后解除保活，不可达 State/Computed/Watcher watched cycle
  整体回收，cold 无 source Computed 独立回收，且 major collection 本身不调用 watched/unwatched hook；
  N=1/2/4/8/16 均覆盖 forced-major allocation stress 与显式 major collection。rooted source 对 cold
  dependent Computed 的弱回收由下一项闭合；minor/forced allocation-point exhaustive matrix 仍由上方
  GC 总项追踪，不能据此勾完整 M8.3。
- [x] M8.3 cold reverse-edge weak-liveness slice：State/Computed reverse sink index 改为逐 edge 保存
  collector-cleared `WeakGcRef` identity，只有递归连接 active Watcher 的 edge 同时保存强 `Value`；first/last
  live transition 按 ordered source 逐边 promote/demote并执行 generational barrier。major-GC oracle 在同一
  rooted source 上同时保留 active Watcher、回收已求值但 cold 的 dependent Computed，证明不是节点级
  `live_sinks != 0` 粗略强 trace；清除后的 weak tombstone 在 snapshot/insert 时过滤或复用。N=`1/2/4/8/16`、
  forced-minor/major allocation policy、跨 job 显式 major collection与完整 Signals fixture matrix 均通过。
  allocator/resource failpoint exhaustive matrix仍由上方未完成条目追踪，不据此勾完整 M8.3。
- [ ] sources/sinks 使用 benchmark 选择的 inline ordered storage，propagation/recompute scratch 使用受限
  high-water buffer；所有 reserve checked/accounted，稳态 set/get/不变 dependency recompute 零 realloc。
- [ ] debugger object preview 识别三种 Signal node；额外 graph inspector 输出 state/generation/source/sink
  metadata，不运行 computed、equals、hook 或 proxy/getter。当前 workspace 尚无 typed debugger/object
  preview substrate，因此该项继续归 M11；M8.3 不创建无消费方的 inspector 空架子。
- [ ] 从 pinned `proposal-signals/signal-polyfill` tests 提取 test262-style suite，补齐 descriptor、
  ordering、exception、cycle、GC、subclass 和 cross-realm tests；未来 upstream test262 出现同
  feature 时立即并入相同 runner。固定 revision 当前有 19 个非 benchmark 测试文件、70 个
  `it/test` 定义；11 组内容寻址 fixture 已映射全部 19 个文件，并补入 Preact/Vue ported graph、
  graph convergence/order 与 Watcher 动态依赖生命周期回归，但尚未逐项移植全部 70 个定义，
  因此本项保持未完成。
- [x] M8.3 Signals pinned-suite breadth/refactor slice：runner 从 8/8 扩展至 11/11 fixture，新增
  caught dependency error、flag/diamond pruning、stale tracker 与 convergence ordering、pull 中写入、
  nested Computed，以及 Watcher `s1 -> s2 -> plain getter` dependency detach。全部 checked-in fixture
  同时进入 VM 的 N=1/2/4/8/16 与 forced-major 矩阵。生产实现按 runtime/state/computed/watcher/graph
  拆为真实子模块，测试按 API fixture/graph fixture/resource/cases/helpers 拆分；没有使用 `include!`
  拼接 Rust 源码，也没有改变 GC payload、continuation、默认 feature 或 observable semantics。
- [x] M8.3 Signals definition-coverage/assertion-oracle slice：`signals_suite.toml` schema v2 将固定
  reference revision 的 19 个 `*.test.ts`、70 个 `it/test` 定义逐项记录 upstream path/line/name，runner
  拒绝重复、遗漏、错误 case ownership，并报告每个 fixture 的映射定义数。11 个 fixture 以动态计数
  oracle 验证总计 114 次预期 assertion 确实执行；`cycles-pruning` 原被 `if (false)` 屏蔽的 2 条 live
  pruning assertion 已恢复并通过。所有 fixture 覆盖 N=1/2/4/8/16、forced-minor 与 forced-major；该
  映射是精确 coverage ledger，不虚报为 70 个上游定义已经逐句完整移植，因此上方完整 suite 项仍未完成。
- [ ] differential runner 对比 pinned reference polyfill 的可比 observable trace；native-only GC、frozen
  reentrancy、resource limit 和 debugger behavior 使用 Tachyon oracle/property tests，不能拿 Boa 缺失当失败豁免。

验收：Signal API 在默认、`--no-default-features` 与 all-features build 均存在；pinned proposal suite
100% 通过且无 ignore/timeout/crash，forced-minor/major/incremental stress 通过，steady-state 基础路径无 realloc。

### M8.4 Module system

- [x] module record、requested modules、named import/export entries、TDZ-aware live binding cell；star/
  namespace/ambiguous export 仍由下方完整状态机项闭合。
- [x] 首个纯内存 record/link 垂直切片：owned canonical specifier/binding name、稳定
  `NonZeroU32` module/cell ID、append-only live binding cell、named local/indirect export，以及带显式
  work/capacity limit 的确定性 iterative Tarjan cyclic linking。失败只回滚仍处于 `Linking` 的不完整
  record，已完成 dependency SCC 保持 `Linked`；测试覆盖 live alias/TDZ、三节点 cycle、共享依赖顺序、
  indirect/circular export、transaction rollback、limit/duplicate rejection 和 1000 节点非递归深图。
- [ ] parse、link、instantiate、evaluate 状态机和 cyclic module graph。
- [x] Isolate-owned 同步 lifecycle 纵切：host `ModuleLoader` 分离 canonical resolve/load，只接收 owned
  synthetic/precompiled module；bounded iterative dependency load、失败 transaction rollback、sync
  dependency-postorder evaluate-once/completion cache、TLA 显式 async boundary，以及 module graph 在全部
  allocation-triggered `VmRoots` 中的精确 tracing。N=1/2/4/8/16、forced-major live-cell、identity
  substitution、missing-dependency retry 均覆盖；parser/declaration instantiation、async evaluate 未完成。
- [ ] dynamic import、import.meta、top-level await 的 async 部分与 M10 对接。
- [ ] `ModuleLoader` 分离 resolve/load，返回 source、precompiled module 或 synthetic module。
- [ ] loader future 不访问 isolate；完成后在 isolate 验证 module identity 和状态。
- [ ] cache key、referrer、attributes、media type、redirect 和 source name 有明确 owned 表示。
- [ ] filesystem/network 不是 core 行为，由 extension 提供；test262 使用 deterministic loader。

### M8.5 Structured clone

- [ ] graph serializer 保留 cycle 和 shared identity。
- [ ] primitive、Object、Array、Map/Set、Date、RegExp、Error、BigInt。
- [ ] ArrayBuffer/TypedArray/DataView clone 与 transfer，detach 顺序符合 Web 语义。
- [ ] Function、Promise、Weak collection 返回 `DataCloneError`。
- [ ] parser/decoder 对不可信 bytes 有 size/depth/reference limit 和 property tests。

验收：模块 cycle/live binding、Proxy invariants、weak semantics、typed array detach 和 structured
clone 专项测试通过；test262 built-ins/module 分类达到阶段 baseline。

## 16. M9: Incremental Major GC 与 Barrier Hardening

- [ ] major phase 状态机为 `Idle -> MarkRoots -> Mark -> WeakClosure -> Sweep -> Idle`，只由 isolate
  单 mutator 在 safepoint 推进；incremental 不等于 concurrent，phase/bitmap/queue 全部非原子。
- [ ] root snapshot/start marking、gray work 和 span sweep 分别按 bytes、edges、objects、spans 与 quantum
  的有界 work units 分片；可选 time cap 仅按稀疏 checkpoint 读取宿主 clock，不每对象读取时间。
- [ ] marking active 时 baseline insertion barrier 对每个 heap/root pointer store shade 尚未标记的新
  target；新分配对象 born-black，不查询 source color、不扫描 gray queue、不增加 atomic/black bitmap。
- [ ] minor 与 incremental major 不交错并共享一张 epoch bitmap；major active 时 Eden allocation 继续、
  但不启动 minor，allocation debt 偿还 marker work；major reserve exhaustion fallback 完成 mark/weak
  closure 后才能 minor，并单独记录 pause，禁止用并发 marker/第二套 atomic bitmap 掩盖落后。
- [ ] mutator 恢复前保存完整 gray/ephemeron/weak phase state；final remark/weak closure 的暂停与 work
  量单独统计，不能把长 ephemeron fixpoint 隐藏在总 GC 时间中。
- [ ] major mark 完成后按 span incremental/lazy sweep；allocation slow path 可优先 sweep 匹配 size class
  的 unswept Old span，显式 full GC/heap pressure 必须完成全部 sweep 和 cleanup。
- [ ] allocation debt 根据 allocated/reclaimed bytes、old growth、survival 和 recent pause 自适应；不得
  在 old allocation/full-major 路径形成重复 collection loop 或无进展 retry。
- [ ] card scanning 跳过 clean cards 并记录 false-positive；whole-span promotion、object/array/
  environment/promise/Signal edge 统一使用同一 store barrier 入口。
- [ ] debug barrier verifier 同时发现漏 old-to-young card 与 incremental shade；forced 模式在每个 store/
  allocation safepoint 推进最小 slice，stress 随机切换 minor/major phase。
- [ ] memory-pressure 可回收 gray/sweep/high-water scratch，但只在 safepoint/idle transition 触发；禁止
  background timer、atomic polling 和历史峰值永久绕过 isolate limit。
- [ ] 记录 mark/sweep work units、debt、mutator utilization、dirty/scanned cards、young/old fragmentation、
  reclaimed bytes 和 pause P50/P95/P99。

验收：M0-M8 在 forced-minor、forced-major、incremental-every-safepoint 和 random-GC 模式全部通过；
Miri/sanitizer 无 stale pointer；同一对象 logical offset/address 跨所有 phase 稳定；无 atomic/lock/spin
进入 direct isolate、GC metadata 或普通 heap access path。

## 17. M10: Promise、Generator、Async 与 Actor

### M10.1 Promise/job semantics

- [x] Promise allocation/static substrate slice：对照 QuickJS Promise records/job list 与 Escargot
  PromiseObject/Job，新增 GC-managed Promise state、固定 reaction node、isolate-local FIFO queued/active
  job roots及统一 ObjectReceiver 分派；默认 realm 安装 `%Promise%`、prototype、resolve/reject，intrinsic
  resolve identity 与 branded prototype 通过源码测试。`built-ins/Promise/resolve` 达到 12/60；constructor
  executor、resolving-function shared cell、thenable assimilation、reaction checkpoint 和 subclass capability
  仍由下项闭合，因此不勾选完整 Promise semantics。
- [x] Promise constructor/resolving-function slice：新增 GC-managed shared already-resolved cell，resolve/reject
  callable 通过同一 cell 实现 first-call-wins；constructor 同步 trampoline 调 executor，bytecode/native abrupt
  在显式 frame unwind 边界转 rejection而不用 Rust unwind，正常返回保留原 Promise。custom newTarget data
  prototype、resolver name/length/non-constructor、strict this、两参数、forced-major 与重复 settle 已覆盖；
  Promise 全目录 applicable 从 0/1274 提升到 122/1274。observable prototype accessor/Proxy、thenable
  assimilation、generic NewPromiseCapability 与 reactions 仍待闭合。
- [x] Promise reaction/checkpoint/thenable slice：`then`/`catch` 使用固定 GC reaction node，settlement 按注册
  顺序发布 FIFO jobs，顶层 completion drain-to-empty；handler return 重新进入 Promise Resolution Procedure，
  observable `Get(resolution, "then")` 支持 accessor/Proxy 的 typed continuation，thenable job 使用 fresh shared
  resolving pair，并在 bytecode return/throw 边界维持 traced active job。sibling chain、nested enqueue、poisoned
  getter、self-resolution、first-call-wins 与 returned-thenable assimilation 已由源码测试覆盖；
  `Promise/prototype/then` 达到 116/146，Promise 全目录 applicable 达到 318/1274。剩余 14 个 legacy sequence
  failure 已定位为缺失 `Array.prototype.forEach`，Species/generic capability 与 8 个 compiler unsupported 仍未闭合。
- [x] Promise helper/Array forEach continuation slice：安装可恢复的 `Array.prototype.forEach`，固定 5-slot
  managed state 保存 receiver/callback/thisArg/length/index，`length` Get、HasProperty、element Get 与 callback
  均可穿越 accessor/Proxy/bytecode frame，holes 与 length snapshot 由源码测试覆盖。修复通用 native continuation
  parent drain 不得越过当前 JavaScript frame `completion_base` 的边界，否则 reaction handler 内嵌 native callback
  会提前消费外层 Promise continuation。`Array/prototype/forEach` 达到 288/376，原 14 个 Promise helper
  sequence failure 全部转 pass，`Promise/prototype/then` 达到 130/146。
- [x] Promise Species/generic capability slice：`Array`/`Promise` 安装标准 `@@species` getter；`then` 以固定
  5-slot managed state 执行 observable constructor/species Get，custom constructor 使用固定 24-byte managed
  capability record 与 length 2 capture executor，验证 object result 及 callable resolve/reject。reaction settlement
  保留 intrinsic direct-Promise fast path，generic 路径调用捕获函数；缺失 fulfillment handler 也重新进入
  Promise Resolution Procedure。修复同步 child native 连续 drain parent 后 depth 小于 baseline 仍 double-pop
  的通用 Promise trampoline 问题；4 个 thenable 文件 8/8 转 pass，`Promise/prototype/then` 达到 138/146，
  剩余 8 个均为 class/statement compiler unsupported；Promise 全目录 applicable 达到 354/1274。
- [x] generic `Promise.resolve`/`Promise.reject` derived class capability slice：非 intrinsic `this` 走固定 managed
  `NewPromiseCapability` state，调用 custom constructor 捕获 resolve/reject，再以原 resolution 调 resolve；
  continuation 在 bound-prefix allocation 前发布，所有中间 capability/executor/state 均进入精确 roots。
  配合 derived class constructor 后 `Promise/prototype/then` 达到 146/146；class 创建、static resolve 与完整
  then trampoline 均有 forced-major 测试。Promise input 的 observable `constructor` identity fast path、
  `Promise.reject` 达到 28/30，`Promise.resolve` 达到 58/60；各剩余 2 个 variant 都依赖尚未实现的 global
  `eval`，ordinary own-data constructor override 已覆盖；accessor/Proxy constructor Get 顺序仍未闭合。
- [x] `Promise.prototype.finally`/observable `catch` slice：`finally` 完成 SpeciesConstructor、PromiseResolve
  identity、callback result mapping、rejection restoration 与 observable `then` 次序；`catch` 通过可恢复 Get/Call
  continuation 执行 `Invoke(this, "then", ...)`，同步 intrinsic 路径不再以内部 state 覆盖 result Promise。
  同步补齐 `Array.prototype.filter` 基础 continuation 与 derived Error 的 `newTarget.prototype` 分配语义；
  `Promise/prototype/finally` 达到 58/58。
- [x] generic `Promise.all` combinator slice：对照 QuickJS `js_promise_all`、Escargot Promise combinator
  callbacks 与 Boa `PerformPromiseAll`，新增独立 traced aggregate state 和每元素 shared once-cell；按
  `NewPromiseCapability(C) -> GetPromiseResolve(C) -> GetIterator -> IteratorStepValue -> Call(resolve) ->
  Invoke(then)` 可恢复执行，支持 custom constructor/capability、observable resolve/then、IteratorClose、
  原始 throw 优先级和最终 Promise Resolution Procedure。非空 Array 不再走缺少 watchpoint 证明的伪 fast
  path；仅 guarded empty intrinsic Array 保留直达路径。构造 receiver/prototype safepoint 额外以 Fiber 稀疏
  `CallSite` root 修复 bound-prefix 搬移，N=1/2/4/8/16 与 forced-major custom capability 回归通过。
  `Promise/all` 达到 188/196，余 8 个均为 spread/BigInt expression 前端 unsupported，已解析项 0 semantic
  failure；完整组合器项仍不勾选，`race`/`allSettled`/`any` 与 `AggregateError` 留在下一纵切。
- [x] `Promise.race` shared-driver slice：aggregate state 加入紧凑 `PromiseCombinatorKind`，复用完整
  NewPromiseCapability/GetPromiseResolve/iterator/then/IteratorClose driver；race 只替换 element settlement
  policy，直接把 capability resolve/reject 传给每个 `then`，空 iterable 保持 pending，不复制第二套协议状态机。
  intrinsic fulfill/reject、empty、custom constructor/capability、N=1/2/4/8/16 与 forced-major 均有源码回归；
  Test262 从 8/188 提升到 180/188，余 8 个全为现有 spread/class 前端 unsupported，0 semantic failure。
- [x] `Promise.allSettled` shared-driver slice：继续复用同一 capability/iterator/close driver，并让 fulfilled/
  rejected handler 共享元素 once-cell；结果策略按输入序创建 `{ status, value }` 或 `{ status, reason }`，
  个体 rejection 不调用 aggregate reject。settled record 先发布进 traced values Array，再跨每个分配点从
  native destination root 刷新 aggregate/result；handler 参数先保存到 managed temporary，避免移动 GC 后读取
  Rust 栈上的旧 `CallSite` argument source。同步校正 IteratorStep 分类：`NextGet`、`NextCall`、`DoneGet`、
  `ValueGet` abrupt 均直接 reject、不执行 IteratorClose。N=1/2/4/8/16 与 forced-major 回归通过；Test262
  达到 200/208，余 8 个均为 spread/BigInt 前端 unsupported，0 semantic failure。组合器总项仍不勾选，
  下一纵切为 `Promise.any` 与完整 `AggregateError`。
- [x] `Promise.any`/`AggregateError` slice：combinator 增加 `Any` result policy；fulfilled 直接安装 capability
  resolve，保持规范允许同一 thenable 多次调用 resolve，只有 reject-element 使用 once-cell。rejection 按输入
  序写 errors Array，最后一个 rejection 与空 iterable 创建 branded AggregateError，并以 W=true/E=false/
  C=true 定义 `errors`。public AggregateError 纳入 Error intrinsic hierarchy，constructor length=2；message、
  cause 后复用 Array.from 的 resumable iterator core，但启用 required-iterable 模式禁止 array-like fallback，
  不复制 iterator/IteratorClose 状态机。N=1/2/4/8/16、forced-major、empty/order/descriptor 回归通过；
  `Promise/any` 从 4/188 提升到 166/188，余 22 个全为 spread/destructuring/class/BigInt frontend
  unsupported，0 semantic failure；AggregateError 从 0/50 提升到 44/50，余 4 frontend unsupported 和
  2 个共享 `GetFunctionRealm(newTarget)` cross-realm fallback 缺口。完整 Promise/Errors 总项仍不勾选。
- [x] Promise combinator ownership refactor：原 1076 行组合器文件按 capability/入口、可恢复 iterator
  driver、元素 settlement policy 与 GC storage/barrier 拆成独立子模块；不改变共享 continuation enum、job
  queue 或显式 completion 语义。新增 `then` getter abrupt 后 IteratorClose 的回归，固定 close getter 再次
  abrupt 时仍以原始 reason reject，并覆盖 N=1/8/16 与 forced-major。固定 Test262 四个组合器目录中已解析
  项继续保持 0 semantic failure；剩余项属于既有 spread/class/BigInt frontend unsupported，因此不虚报
  builtin 通过率增量。
- [x] `Promise.try`/async Test262 harness slice：对照 QuickJS、Escargot 与 Boa，intrinsic `%Promise%` 直接
  复用 Promise Resolution Procedure，custom constructor 复用 `NewPromiseCapability`，callback normal/throw
  分别进入 resolve/reject；variadic 参数只复制一次到精确容量的 GC-managed bound prefix，任何 suspend/GC
  边界都不依赖 Rust 栈值。独立 `promise_try.rs` 保存四阶段 typed continuation；N=1/2/4/8/16、forced-major、
  thenable/object return identity、object throw identity、arguments 与 custom capability 均覆盖。Test262 runner
  的专用 `$DONE` 注入同时建立跨 source-unit binding 与 `globalThis` own property，不影响 non-async raw harness；
  `built-ins/Promise/try` 从 4/24 提升到 24/24。
- [x] Array `forEach`/`filter` synchronous trampoline slice：同步 HasProperty/Get/native callback 统一由显式
  Rust loop 推进，getter、Proxy trap 与 bytecode callback 发布 typed continuation 后退出，恢复时重入同一 loop，
  不再按元素增长 Rust stack。20,000 长度稀疏数组回归通过。
- [x] Array `filter` SpeciesCreate/order slice：length conversion 先于 callback callable validation；Array receiver
  通过 traced constructor/`@@species`/Construct 三阶段创建结果，custom bytecode constructor、accessor 与
  Proxy 均可暂停，null/undefined 回退 intrinsic Array。species 聚类达到 12/12；`forEach` 达到 324/376，
  `filter` 达到 432/480。共享 `coerce_to_object` 已接入两个方法，string/number/boolean primitive receiver
  用例通过；filter ordinary species result 以标准 CreateDataProperty descriptor 写入，可覆盖 configurable
  non-writable slot，并拒绝 non-configurable/non-extensible target。Proxy define trap、cross-realm Array
  constructor、Math/Date/Arguments brand、TypedArray/RAB 与其他共享转换缺口仍待闭合。
- [x] Array `filter` Proxy result define/GC slice：species 结果为 Proxy 时复用 `ProxyDefineMode::Object` 的
  resumable `[[DefineOwnProperty]]` dispatcher，新增 `FilterDefine` parent continuation；trap 返回 false 转
  `TypeError`，throw 保留原始 identity，成功后才递增 dense output index。选中元素暂存于 traced filter
  side-state，atom/shape/Proxy trap 分配前重新从 VM root 取得移动后的 state；SpeciesCreate 的 intrinsic/custom
  allocation 也在 state 发布后执行。N=1/2/4/8/16、nested Proxy、bytecode trap、forced-major 回归通过。
- [x] Array `every`/`some` shared predicate slice：复用 `forEach` 的 Get length、ToLength、IsCallable、
  HasProperty/Get/callback continuation 与同步 loop，只增加两个 traced side-state 槽保存 thisArg 和“继续迭代”
  truthiness。empty default、稀疏/继承索引、length snapshot、参数/thisArg、短路、Proxy has/get 顺序、callback
  abrupt identity、N=1/2/4/8/16 与 forced-major 均有独立测试。`every` 达到 403/433，`some` 达到
  404/434；剩余项属于共享大整数属性键、TypedArray/RAB 与 dynamic Function 缺口。完整 Array applicable
  从 2183/5929 提升到 2956/5929，净增 773 且既有通过项 0 回退。
- [x] Array `reduce`/`reduceRight` direction-parameterized slice：独立子模块复用 Array typed continuation、
  Proxy-aware HasProperty/Get 和显式同步 loop；固定 5-slot state 保存 receiver/callback/accumulator/length/
  logical cursor，方向与 accumulator 是否初始化编码在 scalar mode，故显式 initialValue `undefined` 不会被误判
  为缺失。callback 固定 `(accumulator, value, index, receiver)`/undefined this，首个存在元素、empty sparse
  TypeError、N=1/2/4/8/16、Proxy 顺序、abrupt identity、forced-major 与 20,000 次同步 native callback 已覆盖。
  ordinary non-Proxy prototype chain 在剩余 hole run 超过 `tuning::arrays::ARRAY_ITERATION_SPARSE_SKIP_THRESHOLD` 时重新
  枚举 numeric candidate，抵达后仍执行正式 Has/Get；Proxy chain 逐项观察。safe-integer 极限回归通过。
  length object 使用共享 resumable `ArrayLength` conversion consumer，不在 reducer 内同步近似 ToLength。
  `reduce`/`reduceRight` 各达到 509/517（98.45%），仅余 TypedArray/RAB 与 dynamic Function 共享缺口；
  共享 ToLength 同时修复其他 Array 方法的对象 length variant，完整 Array 从 2956/5929 提升到
  3972/5929，净增 1016 且既有通过项 0 回退。
- [x] Array `map` resumable output slice：新增独立 `array_for_each/map.rs` 入口，并将 filter/map 共用的
  ArraySpeciesCreate/Construct 边界拆入 `array_for_each/output.rs`；主迭代文件保持 881 行。map 以捕获的
  source length 构造 species result，逐项严格执行 HasProperty/Get/Call/CreateDataPropertyOrThrow，holes
  保留原索引，filter 继续使用 dense output cursor。custom species 的 length argument prefix 保存于
  `OutputConstruct` continuation retained slot，带形参 bytecode constructor 在 forced-major 下不会丢 root；
  source/result Proxy、false define、abrupt identity、cross-Realm、N=1/2/4/8/16 均由 Test262/专项测试覆盖。
  普通对象及原型链共享 Escargot 风格 forward numeric candidate scan，长 hole run 不创建逐索引 atom；任一
  Proxy 出现在链上即禁用快进并逐项观察 has。Array constructor 单 Number 参数补齐 0..2^32-1 边界，并将
  `InvalidArrayLength` RangeError 与 push/unshift 的 safe-integer `ArrayLengthOverflow` TypeError 分离。
  `map` 达到 421/429（98.14%），ES6 421/421；仅余 2 个 TypedArray/RAB semantic 与 6 个 dynamic Function/RAB
  unsupported。完整 Array 从 3972/5929 提升到 4413/5929，fixed 441、broken 0。
- [x] Array `indexOf`/`lastIndexOf` bidirectional search slice：新增独立 `array_for_each/search.rs`，固定
  5-slot traced state 在 Get length、对象 fromIndex conversion、Proxy/accessor HasProperty/Get 间恢复；反向
  cursor 保存 `index + 1`，省略 fromIndex 与显式 undefined 由 scalar mode 区分。ordinary prototype chain
  使用 Escargot 风格双向 numeric candidate scan，Proxy chain 逐项观察 has；strict equality、holes、继承索引、
  abrupt identity、N=1/2/4/8/16、forced-major 与 MAX_SAFE_INTEGER 稀疏对象均覆盖。同步修正普通对象字面量
  data property lowering 为 CreateDataProperty，避免误触 inherited accessor。`indexOf` 达到 393/401（98.00%，
  ES6 393/393），`lastIndexOf` 达到 389/395（98.48%，ES6 389/389）；剩余仅 dynamic Function/RAB unsupported。
  完整 Array 从 4413/5929 提升到 4711/5929，fixed 298、broken 0。
- [x] Array find-family resumable slice：`find`/`findIndex`/`findLast`/`findLastIndex` 共享
  `array_for_each/find.rs` 的 direction/result scalar mode、length conversion、Proxy-aware Get 与 callback
  dispatcher，但不执行 HasProperty、不跳 holes；每个索引都以 `(value, index, boxed receiver)` 调 predicate。
  forward next-index/backward remaining cursor 覆盖 MAX_SAFE_INTEGER-1，callback element 由 typed continuation
  retained root 保活。四模式的 holes、mutation、Proxy get-only 顺序、N=1/2/4/8/16、forced-major 均有专项
  覆盖。`find`/`findIndex` 各 34/44，`findLast`/`findLastIndex` 各 36/46；每个目录余 6 个 RAB/dynamic
  Function unsupported 与 2 个依赖尚未闭合 splice 的 mutation variant。完整 Array 从 4711/5929 提升到
  4827/5929，fixed 116、broken 0。
- [x] Array splice resumable slice：新增独立 GC-managed `PendingArraySplice`，一次
  `try_reserve_exact` 后冻结插入参数并精确计入 external memory；length/start/deleteCount、
  ArraySpeciesCreate、deleted-result Has/Get/CreateDataProperty、双向 move、tail delete、item Set 与 final
  length Set 全部通过 typed continuation 恢复。argc 0/1/2、holes/inherited index、generic receiver、
  Proxy trap order、cross-Realm/species、N=1/2/4/8/16 与 forced-major 均覆盖；同时补齐 Proxy
  `defineProperty`/`set` 在 handler 自身为 Proxy 时的 observable trap Get。`splice` Test262 达到
  162/162，完整 Array 从 4827/5929 提升到 4981/5929，fixed 154、broken 0。
- [x] Array concat resumable slice：删除 `i32` cursor/direct data lookup 的同步近似实现，新增独立
  GC-managed `PendingArrayConcat`；receiver 后的参数一次 `try_reserve_exact` 并冻结为 `Box<[Value]>`，
  精确计入 external memory。ArraySpeciesCreate、`@@isConcatSpreadable`、LengthOfArrayLike、
  HasProperty/Get/CreateDataPropertyOrThrow 与 final length Set 全部经 typed continuation 恢复；ordinary
  sparse source 使用 numeric candidate scan，Proxy chain 仍逐索引观察。补齐 revoked Proxy 的通用 IsArray
  handler-null 检查，N=1/2/4/8/16、Proxy order、String exotic 与 forced-major 均覆盖。`concat` Test262
  从 44/137 提升到 131/137；剩余 4 个 TypedArray variant 与 2 个 unpaired-surrogate literal variant 属于
  共享 M8/前端缺口。完整 Array 从 4981/5929 提升到 5070/5929，fixed 89、broken 0、另有 2 个
  unsupported。后续 TypedArray substrate 已使 4 个 typed-array variants 通过；同步 Has/Get/ordinary Define
  已收敛到单一 driver loop，12,000 元素回归和单 Rayon worker 顺序执行 small/large typed-array 的 4/4
  结果证明 native stack 不再随 source length 增长。
  `methods-called-as-functions` variant 因执行越过 concat 后抵达后续未支持语法而从 semantic reclassify 为
  unsupported。
- [x] ArrayAccumulation/String iterator closure slice：删除数组 spread 对可观察
  `Array.prototype.concat` 的旧 lowering，owned HIR 显式保存 element/elision/spread；element/spread 使用
  CreateDataProperty，只有 elision 单独 Set length。共享 iterator lowering 通过 verified
  `LoadIteratorSymbol` 读取 realm well-known identity，并以 `CheckObject` 校验 iterator 与每次 next result；
  替换全局 Symbol 不改变协议。String iterator 复用 branded indexed-iterator payload，以固定两 code-unit
  scratch 逐 Unicode code point 推进，安装独立 prototype/next/@@toStringTag，且 receiver ToString 可经 typed
  conversion continuation 恢复。N=1/2/4/8/16、forced-major、sparse own-undefined、primitive result TypeError、
  concat override 与 surrogate pair 均覆盖；`String/prototype/Symbol.iterator` Test262 12/12，
  `StringIteratorPrototype` 12/14，余 2 个为共享 unpaired-surrogate literal frontend 缺口；ES6 Array expression
  目录 40/44，余 4 个为 generator frontend 缺口。
- [x] Array.of resumable static-constructor slice：新增可供后续 Array.from 复用的 GC-managed
  `PendingArrayStatic`，参数一次 `try_reserve_exact` 后冻结并计入 external memory；IsConstructor 分支以
  单个 length 参数执行通用 Construct，逐项使用 CreateDataPropertyOrThrow，最后以 throw-on-false Set
  写 length。custom/Proxy constructor、Proxy define/set、跨 Realm、abrupt completion、N=1/2/4/8/16 与
  forced-major 均覆盖；`Array/of` Test262 达到 32/32。
- [x] Array.from resumable iterable/array-like slice：扩展 `PendingArrayStatic` 保存 source、mapper/thisArg、
  iterator/next/result、kind、safe-integer cursor/length 与 abrupt-close marker；GetMethod、两种 Construct
  参数形态、LengthOfArrayLike、逐项 Get、mapper Call、CreateDataPropertyOrThrow 和 final length Set 均通过
  typed continuation 恢复。IteratorClose 共享执行器改为接受 rooted iterator identity，仅 mapper/define
  abrupt 关闭，next/done/value abrupt 不关闭；N=1/2/4/8/16、forced-major、mutation、number boundary 与
  close ordering 均覆盖。`Array/from` 达到 82/90，余 2 generator、2 ArrayBuffer 与 4 global var/globalThis
  alias assertion 属于共享 M2/M5/M8 缺口；完整 Array 从 5070/5929 提升到 5174/5929，fixed 104、broken 0。
- [x] Change-array-by-copy `toReversed`/`with`/`toSpliced` slice：新增 GC-managed `PendingArrayCopy`，以 typed
  continuation 执行 observable LengthOfArrayLike、index ToIntegerOrInfinity 与逐项 Get；结果始终使用当前 Realm
  intrinsic Array，忽略 `@@species`，read-through-holes 并创建 dense own data properties。`toReversed` 保持
  descending Get 顺序，`with` 在 replacement index 不读取 source；ArrayCreate/relative-index 越界均映射
  RangeError。N=1/2/4/8/16、forced-major、getter order、holes、species 与负索引覆盖；Test262 分别达到
  `toReversed` 34/34、`with` 42/42。`with` 的 frozen receiver 与 getter shrink 回归同时闭合
  dense-present descriptor 迁移：省略的 value/WEC 从旧 element 保留，不套用新属性默认值。
  `toSpliced` 以 exact-capacity、external-accounted backing 在 length Get
  前冻结 items，按实参数量区分 omitted 与 explicit undefined deleteCount，prefix/items/suffix 共用 output
  cursor 且 deleted range 不执行 Get；Test262 达到 60/60。`toSorted` 使用独立 GC-managed 双 buffer
  bottom-up stable merge state，先完整 read-through-holes 收集再比较；user comparator Call/结果 ToNumber 与
  default comparator 双侧 string-hint ToString 均可恢复，undefined 特判不调用 comparator。N=1/2/4/8/16、
  forced-major、stable equal、abrupt、object conversion 均覆盖；Test262 40/42，余 2 为共享 BigInt literal
  frontend 缺口。`sort` 复用同一 stable merge core但以 Has/Get 收集 present items、按调优常量和 replacement
  GC state扩容，比较结束后执行observable Set/Delete；N=1/2/4/8/16、forced-major、2048-element同步
  no-Rust-recursion与超过初始容量的扩容均覆盖。sort从35/107提升到97/107，fixed 62、broken 0；完整
  Array复测达到5404/5929，fixed 62、broken 0。
- [x] Array.prototype.slice resumable species/copy slice：删除 ordinary-only 同步旁路，新增独立
  `PendingArraySlice`，按 ToObject、LengthOfArrayLike、start/end ToIntegerOrInfinity、ArraySpeciesCreate、
  HasProperty/Get/CreateDataPropertyOrThrow、最终 throw-on-false Set length 的规范顺序推进。custom/Proxy
  species、cross-Realm intrinsic Array fallback、primitive receiver、洞位与 abrupt completion 全部复用统一
  property/construct continuation；单个 species 参数显式 exact reserve，返回元素用 traced `retained` edge
  跨 atom/descriptor allocation。N=1/2/4/8/16、forced-major、Proxy define/set order 与 512-element
  no-Rust-recursion 扫描均覆盖；slice Test262 达到 136/142，semantic failure 为 0，余 6 个为共享 Dynamic
  Function/RAB unsupported。完整 Array 从 5404/5929 提升到 5448/5929，fixed 44、broken 0。
- [x] Array.prototype.flatMap resumable depth-one flatten slice：新增独立固定大小
  `PendingArrayFlatMap`，严格按 ToObject、LengthOfArrayLike、mapper callable validation、
  ArraySpeciesCreate(O,0)、outer Has/Get/Call、mapped Array IsArray/LengthOfArrayLike/inner Has/Get 与
  CreateDataPropertyOrThrow 顺序推进。outer/inner cursor 和 target cursor 均在 typed continuation 中恢复，
  callback 返回非 Array 直接定义，Array 只展开一层；ordinary 长洞位按统一调优阈值扫描 numeric candidate，
  Proxy 链保持逐索引观察。custom/Proxy species、bound mapper 参数前缀合并、N=1/2/4/8/16、forced-major、
  Proxy order 与 10001-length sparse no-Rust-recursion 均覆盖。flatMap Test262 达到 43/47，余 4 个均为
  Int32Array 共享缺口；完整 Array 从 5448/5929 提升到 5487/5929，fixed 39、broken 0。
- [x] Array.prototype.flat resumable arbitrary-depth slice：删除同步 `Vec<FlatWork>` 路径，新增独立
  `PendingArrayFlat`，严格按 ToObject、LengthOfArrayLike、ToIntegerOrInfinity(depth)、
  ArraySpeciesCreate(O,0)、逐层 Has/Get/IsArray/LengthOfArrayLike 与 CreateDataPropertyOrThrow 推进。
  `Infinity` 使用独立 tag，不与有限饱和值混同；遍历使用 externally-accounted 固定 `Box<[Frame]>`，常见
  深度按 `INITIAL_ARRAY_FLAT_FRAME_CAPACITY` 预留，满载后 allocate-copy-swap replacement state，绝不在
  已发布 GC payload 内扩 Vec 容量。N=1/2/4/8/16、Proxy access order、12层 backing 扩容与 forced-major
  均覆盖；flat Test262 达到 38/38，完整 Array 从 5487/5929 提升到 5503/5929，fixed 16、broken 0。
- [x] Array iterator creator surface closure：发布 realm-local `Array.prototype.keys` 与 `entries`，与既有
  `values`/`@@iterator` 共用 branded `ArrayIteratorObject`、live LengthOfArrayLike 与 resumable element Get。
  共享 creator 入口补齐 ToObject；Key 模式不读取元素，KeyAndValue 对 data/hole/accessor completion 都创建
  `[index,value]` intrinsic Array。N=1/2/4/8/16、array-like getter order 与 forced-major entry allocation
  均覆盖；keys/entries 各达到 18/24，剩余各6个均为 Dynamic Function/RAB共享缺口。完整 Array从
  5503/5929提升到5535/5929，fixed 32、broken 0。
- [x] Array.prototype pop/shift resumable removal slice：删除 direct-data-only 同步旁路，新增固定大小、
  无内部 Vec 的 GC-managed `PendingArrayRemove`。两者严格执行 ToObject、LengthOfArrayLike、indexed Get、
  DeletePropertyOrThrow 与最终 throw-on-false Set length；shift 额外逐索引执行 HasProperty，再按结果执行
  Get/Set 或 Delete。零长度仍观察 `Set(O, "length", 0, true)`，返回的首/尾元素由 traced edge 跨全部
  setter/Proxy trap 保活，cursor 只在目标 mutation 成功后提交。N=1/2/4/8/16、稀疏数组、primitive
  receiver、Proxy 精确顺序与 forced-major 均覆盖；同步修正两者被误报为 1 的 native arity，pop 达到
  46/46、shift 达到 40/40。完整 Array 从 5535/5929 提升到 5581/5929，fixed 46、broken 0；semantic
  failure 从 164 降到 134，unsupported 从 416 降到 400。packed array 专用 fast path 保留到 M13
  profile 驱动实现。
- [x] Array.prototype push/unshift resumable insertion slice：删除 direct-data-only 同步旁路，新增独立
  GC-managed `PendingArrayInsert`。调用参数按已知 argc 一次 `try_reserve_exact` 后冻结为 externally-accounted
  `Box<[Value]>`，不会在已发布 payload 内扩容；LengthOfArrayLike、unshift 反向 Has/Get/Set/Delete move、
  左到右 item Set 与最终 length Set 全部通过 typed continuation 恢复。argc=0 仍观察 final length Set，
  safe-integer overflow 在任何 indexed mutation 前抛 TypeError，cursor 只在 mutation 成功后提交，moved value
  使用 traced edge 和 generational barrier 保活。N=1/2/4/8/16、primitive receiver、稀疏数组、Proxy 精确
  trap order、对象 length conversion、overflow 与 forced-major 均覆盖；push 从 34/48 提升到 48/48，
  unshift 从 24/44 提升到 44/44；20,000 长度同步稀疏 move 使用显式 loop，Rust 栈深度不随 length
  增长。完整 Array 从 removal 后的 5581/5929 提升到 5615/5929，fixed 34、broken 0；相对记录于
  `/tmp/tachyon-array-after-remove.json` 的 arity 修复前报告为 fixed 38、broken 0。packed array 专用原地
  append/move fast path仍由 M13 eligibility/profile 闭合，不能恢复对 generic/exotic receiver 直接写 own
  data property 的旧旁路。
- [x] Array.prototype copyWithin resumable in-place copy slice：删除 direct-data-only 同步旁路，新增固定
  大小、无内部 Vec 的 `PendingArrayCopyWithin`。LengthOfArrayLike、target/start/end 的
  ToIntegerOrInfinity 严格从左到右转换，重叠区间按 QuickJS `JS_CopySubArray` 与 Escargot indexed-property
  顺序选择方向；每项执行 HasProperty 后再 Get/Set 或 DeletePropertyOrThrow，source value 由 traced edge
  和 generational barrier 跨 setter/Proxy trap 保活，from/to/count 只在 mutation 成功后提交。同步完成路径
  使用显式 loop，N=1/2/4/8/16、primitive receiver、稀疏 holes、Proxy 精确顺序、forced-major 与 20,000
  长度 no-Rust-recursion 均覆盖；同步修正 `copyWithin.length` 为 2。Test262 从 46/78 提升到 70/78，
  剩余 4 个 shape quota、2 个 Dynamic Function/RAB 与 2 个共享 Proxy trap throw 展开缺口；本切片净修复
  24、已知 broken 0。packed/holey 原地批量 fast path 保留到 M13 profile 驱动闭合。
- [x] Array.prototype reverse resumable pair-mutation slice：删除 direct-data-only 同步旁路，新增固定大小、
  无内部 Vec 的 `PendingArrayReverse`，同时 trace lower/upper 两个 observed Value。每个 pair 严格执行 lower
  Has/Get、upper Has/Get，再按 present/present、hole/present、present/hole、hole/hole 四分支执行规范
  Set/DeletePropertyOrThrow 顺序；两个 mutation 全部成功后才提交 lower cursor。同步 pair 用显式 loop，
  accessor/Proxy 才发布 typed continuation；N=1/2/4/8/16、primitive receiver、四种 holes、Proxy 精确
  顺序、forced-major 与 20,000 长度 no-Rust-recursion 均覆盖，并修正 `reverse.length` 为 0。reverse
  Test262 从 20/36 提升到 32/36，剩余 2 个 Dynamic Function/RAB 与 2 个超大 Proxy fixture；完整 Array
  从 5639/5929 提升到 5651/5929，fixed 12、broken 0。QuickJS fast-array swap 保留到 M13 由 elements
  kind eligibility 与 profile 闭合，generic path 不使用 direct storage swap。
- [x] Array.prototype fill resumable indexed-write slice：删除旧同步 builtin，新增固定大小、无内部 Vec 的
  `PendingArrayFill`，严格按 ToObject、Get/ToLength(length)、ToIntegerOrInfinity(start)、
  ToIntegerOrInfinity(end) 后逐索引执行 throw-on-false Set。receiver、填充值及原始 start/end 参数全部由
  traced state 保活，cursor 只在 Set 成功后提交；同步完成路径使用显式 loop，accessor/Proxy 才发布 typed
  continuation。N=1/2/4/8/16、primitive receiver、hole materialization、length/start/end 转换顺序、Proxy
  trap order、forced-major 下对象填充值保活与 20,000 长度 no-Rust-recursion 均覆盖。fill Test262 从
  28/44 提升到 40/44，剩余 4 个均为 RAB/TypedArray 共享 Dynamic Function 缺口；完整 Array 从
  5651/5929 提升到 5663/5929，fixed 12、broken 0。packed fill fast path 留到 M13，在 genuine packed
  Array、writable length、extensible receiver 与 prototype indexed-property eligibility 闭合后实现。
- [x] Array.prototype includes resumable direct-Get search slice：删除旧同步 direct-data-property loop，在
  既有固定五槽 Array search state 中加入独立 `SEARCH_INCLUDES` mode，复用 resumable length/fromIndex
  conversion 与 forward cursor，但每个索引直接执行 Proxy/accessor-aware Get，绝不进入 indexOf 的
  HasProperty/hole-skip 分支；holes 因而与 undefined 匹配，比较使用 SameValueZero，命中/结束发布 boolean。
  length 为零跳过 fromIndex conversion，同步 Get 使用显式 loop。N=1/2/4/8/16、Proxy 仅 get trap、getter
  values-not-cached、对象 fromIndex、NaN/正负零、forced-major 与 20,000 长度 no-Rust-recursion 均覆盖。
  includes Test262 从 36/60 提升到 54/60，剩余 6 个均为 RAB/Dynamic Function；完整 Array 从
  5663/5929 提升到 5681/5929，fixed 18、broken 0。packed/proven-hole search fast path 留到 M13，并必须
  保留 searchElement 为 undefined 时首个 hole 命中的契约。
- [x] Array.prototype join resumable string-assembly slice：删除旧同步 `join_array_like`，新增独立
  `PendingArrayJoin`，以 externally-accounted `Box<[u16]>` 保存 separator/output；ToObject、Get/ToLength、
  separator ToString、逐索引 direct Get、nullish/self 空串、element ToString 与最终 UTF-16 allocation 全部
  通过 typed continuation 恢复。初始 backing 使用 `JOIN_INITIAL_UNITS_PER_ELEMENT` /
  `JOIN_MAX_INITIAL_UNITS`，扩容采用 allocate-copy-swap，已发布 payload 不发生 Vec growth；cursor 在 Get
  成功后提交，普通路径显式 loop 不增长 Rust 栈。N=1/2/4/8/16、Proxy/accessor order、对象 separator 与
  element conversion、direct self-cycle、forced-major、3,000 元素 replacement growth 均覆盖；join Test262
  从 34/46 提升到 40/46，剩余 6 个均为 RAB/Dynamic Function；完整 Array 从 5681/5929 提升到
  5695/5929，fixed 14、broken 0。`Array.prototype.toString` 默认 join 路径同步迁移；zero-copy StringBuilder、
  packed fast path 与 ordinary proven-hole skip 留到 M13 profile。
- [x] Array.prototype toLocaleString resumable element-call slice：复用 join 的 UTF-16 backing 与长度/索引
  状态，新增 `ArrayToLocaleString` intrinsic；每个非 nullish 元素按 direct Get、Get `toLocaleString`、零参数
  Invoke、结果 ToString 的顺序恢复执行，primitive element 的调用 receiver 保留原值，N=1/2/4/8/16 与
  forced-major 覆盖。定向 Test262 当前 12/22 通过，剩余 2 项是继承 `Object.prototype.toLocaleString` 后
  嵌套 native continuation 缺口，另 8 项属于 spread/Dynamic Function/RAB 前置缺口；该边界已记录，不能把
  本切片标记为完整 ES semantics。
- [x] M8 fixed ArrayBuffer substrate slice：对照 QuickJS `JSArrayBuffer`、Escargot
  `ArrayBufferObject`/`BackingStore` 与 Boa ArrayBuffer 实现，将普通对象 payload 和独立 GC-managed
  `ArrayBufferData` backing 分离；backing 使用 externally-accounted fixed `Box<[u8]>`，普通 ArrayBuffer
  路径不引入 atomic、mmap 或 host I/O。Realm 安装 constructor、`isView`、`byteLength`、`maxByteLength`、
  `resizable`、`detached` 与 `@@toStringTag`，对象 MOP、N=1/2/4/8/16、forced-major 覆盖。重建 runner 后
  ArrayBuffer 定向 Test262 从 0/384 applicable 提升到 114/384；剩余集中在 TypedArray/DataView view、
  `slice`、RAB resize 和 transfer/detach，不能把 fixed substrate 标记为完整 ArrayBuffer。
- [x] M8 fixed DataView Number-element slice：对照 QuickJS `JSTypedArray` DataView class、Escargot
  `DataViewObject` 与 Boa view witness，新增独立 traced `DataViewObject` 保存原始 ArrayBuffer edge 和
  `u32` offset/length，不复制 backing、不缓存裸指针。Realm 安装 constructor、buffer/byteLength/byteOffset、
  Number-backed 8/16/32-bit integer 与 Float32/Float64 get/set，默认 big-endian、显式 little-endian、
  `ToIndex` 截断和 checked bounds 均覆盖；`ArrayBuffer.isView` 同步识别 DataView。N=1/2/4/8/16、
  forced-major、brand、metadata 和 endian roundtrip 回归通过；定向 DataView applicable 从 0/1100 提升到
  496/1100。剩余集中在 TypedArray observation、BigInt64/Float16、detach/RAB/SAB 与共享 object conversion
  continuation，因此 M8.1 DataView 总项不打勾。
- [x] M8 DataView Float16 element slice：在既有参数化 DataView get/set 路径加入 `getFloat16`/
  `setFloat16`，使用无依赖整数位算法完成 IEEE 754 binary16 与 Number 的双向转换；编码固定
  round-to-nearest-ties-even，保留符号零/无穷并 canonicalize NaN，不依赖宿主 endian、unaligned load 或
  当前浮点舍入模式。全部非 NaN 16-bit pattern roundtrip、规范向量、subnormal/midpoint/overflow、
  N=1/2/4/8/16 与 forced-major 均覆盖。固定 Test262 commit 上 `getFloat16` 为 `36/42`，`setFloat16`
  applicable 为 `34/46`，合计 `70/88`；完整 DataView applicable 从本轮基线 `676/1100` 提升到
  `724/1100`（原有 Float16 元数据/共享前置项已通过，因此净增 48）。剩余项属于 RAB/immutable backing 与共享对象
  `ToIndex`/`ToNumber` continuation，DataView 总项仍不打勾。
- [x] M8 fixed Number TypedArray substrate slice：对照 QuickJS `typed_array_init`/
  `js_typed_array_constructor`、Escargot `TypedArrayObject`/`installTypedArray` 与 Boa integer-indexed exotic，
  新增单一 48-byte `TypedArrayObject` 保存原始 ArrayBuffer edge、`u32` offset/length、element kind 和 ordinary
  base，不缓存 backing 裸指针、不为元素创建 shape slot。Realm 建立不可构造但拥有 `.prototype` 的
  `%TypedArray%`、九种 Number concrete constructor/prototype、共享 length/buffer/byteLength/byteOffset/
  `@@toStringTag` accessor 和 `BYTES_PER_ELEMENT`；integer-indexed Get/Set/GetOwnProperty/DefineOwnProperty/
  Delete/OwnKeys 按 canonical numeric index 直接映射 backing。length 与 fixed ArrayBuffer construction、显式
  little-endian storage、整数 wrap、Uint8Clamp ties-to-even、N=1/2/4/8/16、forced-major 和 payload layout
  回归通过。TypedArrayConstructors applicable 从 0/1442 提升到 370/1442；随后 `%TypedArray%` prototype
  metadata 修正使 concrete prototype `proto.js` 2/2 通过。object/iterable/TypedArray source construction、
  resumable object ToNumber、全部 shared methods、BigInt64/BigUint64/Float16、detach/RAB/SAB 仍待后续，
  因此 M8.1 TypedArray 总项不打勾。
- [x] M8 BigInt primitive substrate Slice A：参考 QuickJS `JS_TAG_SHORT_BIG_INT`/`JS_NewBigInt64` 与
  Escargot `BigInt` sign-magnitude payload，新增 signed 48-bit `SmallBigInt` NaN-box tag 和 GC-managed canonical
  `BigIntValue { sign, Box<[u64]> }`；OXC literal 经 owned HIR、`BytecodeConstant::BigInt` 和 `CodeLoadRoots`
  精确解析并在 forced-major 下保持 rooted。十进制 parse/format、strict equality、`typeof`、truthiness、
  primitive string conversion、unary negation 与 modulo 2^64 全部不经过 Number，heap limbs 纳入 exact external
  memory accounting。小值边界、multi-limb/huge decimal、负值、独立 heap constant equality、N=1/2/4/8/16 和
  forced-major 回归通过。BigInt constructor/wrapper、完整 arithmetic 和 BigInt64Array/BigUint64Array kinds 明确
  留给后续 slices，M8.1 TypedArray 总项仍不打勾。
- [x] M8 BigInt constructor/conversion Slice C：默认 Realm 安装 `BigInt` function，并按规范保留
  `[[Construct]]` 识别但在任何 argument coercion 前拒绝 NewTarget；call path 以 number hint 运行一次 resumable
  `ToPrimitive`。`NumberToBigInt` 仅在 constructor path 接受 integral Number，以 IEEE-754 significand/exponent
  直接生成 canonical limbs，NaN/Infinity/fraction 映射 RangeError；共享 `ToBigInt` 对 BigInt identity、Boolean、
  Unicode-trimmed decimal/binary/octal/hex String 完整转换，并对 Number/null/undefined/Symbol 映射 TypeError，非法
  StringIntegerLiteral 映射 SyntaxError。BigInt TypedArray constructor element write 复用同一 primitive finish，
  String/Boolean/object source 与 Number mixing 回归覆盖。N=1/2/4/8/16、forced-major、getter/@@toPrimitive、异常
  identity 和 construct-no-coercion 均通过；Test262 BigInt 顶层 42/44，剩余 2 个仅为明确后续的 wrapper object，
  整个 `built-ins/BigInt` 为 64/154，余项集中在 wrapper/prototype/asIntN/asUintN。BigInt 总项仍不打勾。
- [x] M8 BigInt arithmetic Slice D：在 canonical signed-48-bit immediate/GC sign-magnitude substrate 上实现
  Add/Subtract/Multiply/Divide/Remainder/Exponentiate、And/Or/Xor/Not、signed left/right shift 与 BigInt `>>>`
  rejection；SmallBigInt checked fast path 保持 allocation-free，heap fallback 不经 f64，bitwise 使用无限
  two's-complement 语义，negative shift 反转方向，negative exponent 与 zero divisor 映射 RangeError，mixed
  Number/BigInt 与 unsigned shift 映射 TypeError。对象 operand 复用既有 resumable ToNumeric continuation；
  N=1/2/4/8/16、forced-major、multi-limb/negative/boundary/resource-limit 回归通过。Test262 arithmetic 本体
  Add/Sub/Mul/Div/Mod/Exp 各 2/2 variants；13 个 BigInt operator 目录合计 86/146，剩余 60 项集中在共享
  `Object(BigInt)` wrapper/boxing（及其 harness assertion），因此 BigInt 总项仍不打勾。
- [x] M8 BigInt wrapper/prototype/fixed-width Slice E：对照 QuickJS `JS_CLASS_BIG_INT`/`js_thisBigIntValue` 与
  Escargot `BigIntObject`/ordinary `m_bigIntPrototype`，新增独立 GC-managed `BigIntObject` private slot，且保持
  `%BigInt.prototype%` 为无 `[[BigIntData]]` 的 ordinary object。`Object(bigint)`、primitive prototype routing、
  `valueOf`、radix 2..=36 `toString`、无 Intl fallback `toLocaleString`、`@@toStringTag`、`asIntN/asUintN` 与
  `Number(bigint)` exception 已接入；asN 以两阶段 typed continuation 串联 observable ToIndex/ToBigInt，固定宽度
  two's-complement 截断不经 Number。N=1/2/4/8/16 与 forced-major wrapper/heap-BigInt matrix 通过；Test262
  `asIntN` 28/28、`asUintN` 28/28、`prototype/valueOf` 16/16、`prototype/toLocaleString` 2/2、
  `prototype/toString` 22/26，整个 `built-ins/BigInt` 从 64/154 提升到 148/154。剩余 6 个 unsupported
  均是 closure environment frontend 前置缺口，因此 BigInt built-in 本 slice 打勾，但完整 M8 BigInt 总项仍不打勾。
- [x] M8 BigInt64Array/BigUint64Array substrate Slice B：`TypedArrayKind` 扩为十一种并显式引入
  `ContentType::{Number,BigInt}`，两种 BigInt kind 继续复用单一 48-byte TypedArray payload、Realm installer、
  integer-indexed MOP 与 fixed ArrayBuffer backing。read 在 no-GC scope 内仅复制八字节，退出后按 signed/unsigned
  分配 canonical BigInt；write 复用 modulo 2^64 helper 并以 little-endian two's complement 存储。primitive/
  iterable/array-like/同 ContentType typed-source constructor、indexed Get/Set、metadata 与底层 byte layout 已闭合，
  Number/BigInt constructor source 和 indexed write 混用映射为 JS TypeError。signed/unsigned boundary、wrap、
  cross-kind source、N=1/2/4/8/16、forced-major 与既有 Number TypedArray 回归通过。map/filter/sort 等 shared
  methods、DataView BigInt accessor、RAB/SAB 不在本 slice，M8.1 TypedArray 总项仍不打勾。
- [x] M8 fixed Number TypedArray source-construction slice：对照 QuickJS `typed_array_init` 与 Escargot
  `builtinTypedArrayConstructor`，以 GC-managed `PendingTypedArrayConstruction` 串联 observable
  `GetPrototypeFromConstructor`、ArrayBuffer offset/length conversion、`@@iterator` Get、IteratorToList、
  array-like length/index Get 和逐元素 ToNumber。iterable 先完整收集再转换，array-like 固化 length 后按
  `Get(k) -> ToNumber -> write` 交错；TypedArray source 分配独立 backing，同 kind byte-copy 保留 NaN payload
  与 signed zero，异 kind 逐元素转换。primitive length 分配后立即完成并保持 zero-filled backing；所有跨
  callback edge 精确 trace/write barrier，不缓存 no-GC borrow。N=1/2/4/8/16、forced-major、derived/cross-Realm
  prototype、abrupt conversion、descriptor MOP 与 source mutation 回归通过。TypedArrayConstructors applicable
  从 400/1442 提升到 572/1442，fixed 172、broken 0；剩余集中在 shared prototype/static methods、BigInt64/
  BigUint64、Float16、detach/RAB/SAB 和 generator syntax，因此 M8.1 TypedArray 总项仍不打勾。
- [x] M8 TypedArray bulk-construction stack-stability slice：共享 IteratorToList 对内部/原生 Array result 的
  intrinsic Array iterator 使用 iterative drain，TypedArray iterable-list 与 ordinary array-like primitive
  element conversion 同样在同步 data path 中循环；Accessor、Proxy 与对象转换继续经 typed continuation
  挂起。10,000 元素 `Array.from`、iterable TypedArray、array-like TypedArray 在高配额 isolate 中共同回归，
  不再逐元素增长 Rust stack。原 detach/copyWithin harness 不再在已定位的三条路径立即 abort，但完整矩阵仍因
  indexed shape throughput 超过 3 分钟而终止，性能/profile 与 detach/RAB 语义继续作为未完成项。
- [x] M8 fixed Number `TypedArray.prototype.at` slice：默认安装共享 `at`，严格 brand/backing 检查，支持
  `ToIntegerOrInfinity` 的 object callback、NaN、正负 Infinity、正负零及相对索引；callback 后重新 snapshot
  fixed backing。N=1/2/4/8/16 与 forced-major 回归通过，定向 Test262 从 0/30 提升到 22/30，fixed 22、
  broken 0；剩余 8 项全部依赖 RAB/OOB/resize，因此 TypedArray 总项仍不打勾。
- [x] M8 fixed Number `TypedArray.prototype.includes` slice：严格 TypedArray brand/backing，空 view 在
  fromIndex 转换前返回 false，primitive fromIndex 零分配，对象 `ToIntegerOrInfinity` 使用固定 native state
  精确 root receiver/search/初始 length；扫描在单次 checked no-GC backing borrow 中执行 SameValueZero，
  searchElement 不做 coercion。N=1/2/4/8/16、forced-major 回归通过，定向 Test262 从 6/90 提升到 40/90，
  fixed 34、broken 0；剩余 18 个 semantic failure 属于 detach/RAB/OOB，32 个 unsupported 属于 BigInt、
  RAB 与共享前置缺口，因此 TypedArray 总项仍不打勾。
- [x] M8 fixed Number TypedArray `indexOf`/`lastIndexOf` slice：两个方法共享方向参数化 native identity、
  conversion state 与 no-GC backing scan；初始 ValidateTypedArray/length snapshot 先于空 view 短路，空 view
  不转换 fromIndex，反向省略参数与显式 undefined 精确区分。对象 fromIndex 经 traced continuation 执行
  ToIntegerOrInfinity，之后重新验证 view/backing；转换期间 detach 返回 -1，未来 RAB resize 使用 initial/current
  length 交集。比较为 Strict Equality，NaN 永不匹配、正负零匹配，searchElement 不做 coercion，逐元素无分配或
  Vec。N=1/2/4/8/16 与 forced-major 回归通过；`indexOf` 从 6/86 提升到 46/86（fixed 40、broken 0），
  `lastIndexOf` 从 6/84 提升到 44/84（fixed 38、broken 0），Number ES6 variants 全部通过，剩余项属于
  BigInt、RAB/OOB 与共享前置缺口，因此 TypedArray 总项仍不打勾。
- [x] M8 fixed Number TypedArray callback-family slice：`every`/`some`/`find`/`findIndex`/`findLast`/
  `findLastIndex` 共用一个五槽 GC-managed `NativeCallState` 与六模式 resumable driver；初始
  ValidateTypedArray/attached backing 和 callable validation 顺序固定，初始 length 只保存为数值快照，
  每次 callback 前重新通过 receiver 解析当前 backing，continuation 第二槽保留当前 element，不缓存 witness/
  backing pointer。callback 中 detach 后剩余 initial-length 索引按 IntegerIndexedElementGet 观察 undefined，
  正反 cursor 在调用前提交，throw 由 fiber abrupt path 原样传播。N=1/2/4/8/16、detach 与 forced-major
  回归通过；Test262 `every`/`some` 各从 8/88 到 44/88，`find`/`findIndex`/`findLast`/`findLastIndex`
  各从 8/72 到 38/72，合计 fixed 192、broken 0。剩余集中在 BigInt、RAB/OOB、Dynamic Function 与
  Reflect.set 共享边界，因此 TypedArray 总项仍不打勾。
- [x] M8 fixed TypedArray `forEach`/`reduce`/`reduceRight` slice：扩展既有五槽 callback state，
  `forEach` 丢弃 callback result，reducer 复用 thisArg 槽保存 accumulator，以 mode 精确区分省略初值与显式
  `undefined`，正反方向均在 callback 前提交 cursor。四参数 reducer callback 使用 undefined this，返回对象
  经 write barrier 发布；初始 length snapshot、callback 中 detach 后的 undefined element、动态 element read、
  abrupt identity、N=1/2/4/8/16、forced-major 与 20,000 次 bytecode callback stack stability 均覆盖。
  BigInt64/BigUint64 复用同一 driver，element 由 `ContentType` 分流直接传递 BigInt primitive，不经 Number
  conversion；signed/unsigned modulo-64、heap BigInt、默认 accumulator、显式 undefined initialValue 和
  forced-major 均回归。Test262 `forEach` 为 52/84（BigInt 10 pass），`reduce`/`reduceRight` 各为 74/100
  （BigInt 各 22 pass）；剩余集中在 BigInt source-construction/Reflect.set、RAB/OOB、Dynamic Function 与
  harness error mapping 共享边界，TypedArray 总项仍不打勾。
- [x] M8 TypedArray integer-indexed `[[Set]]`/`Reflect.set` slice：direct receiver 写入 backing 而不创建
  ordinary shadow slot；ordinary/TypedArray alternate receiver、short receiver false、canonical invalid
  numeric index、detached/OOB success mapping 与 Number/BigInt ContentType mismatch 已接入。对象 value 使用
  GC-traced continuation 完成 number-hint ToPrimitive 后再写入，N=1/2/4/8/16、forced-major 与 callback live
  mutation 回归通过。`TypedArrayConstructors/internals/Set` 从 78/106 提升到 92/106，forEach/reduce/
  reduceRight 的 Reflect.set mutation 各 2/2；剩余 14 项属于 Proxy prototype-chain、BigInt wrapper/prototype
  和 RAB，integer-indexed MOP 总项仍不打勾。
- [x] M8 fixed Number `TypedArray.prototype.fill` slice：primitive value/start/end 走零状态分配路径，按
  ValidateTypedArray、value ToNumber、start/end ToIntegerOrInfinity、最终 backing revalidation 顺序执行；任一
  object 参数才使用五槽 traced `NativeCallState` 与三个 typed conversion consumer，detach/throw identity 跨
  callback 保持。fill value 只编码一次，并在单个 checked no-GC backing borrow 内以同步 chunk loop 写入，不缓存
  裸指针、不按元素分配或增长 Rust 栈。N=1/2/4/8/16、forced-major、三阶段 conversion order/detach 与 20,000
  element bulk fill 均覆盖；Test262 从 18/104 提升到 58/104，fixed 40、broken 0。剩余 semantic 文件仅为
  BigInt 两项、RAB initial-length/OOB 两项与 immutable backing 一项，TypedArray 总项仍不打勾。
- [x] M8 BigInt TypedArray shared-method slice：`fill` 按 receiver `ContentType` 复用既有
  `primitive_to_bigint`，在 start/end observable conversion 前只转换一次 value，并在单次 checked no-GC borrow
  内复用 modulo-2^64 编码；`includes`/`indexOf`/`lastIndexOf` 共享一次 needle normalization，BigInt 先验证
  signed/unsigned 64-bit 可表示性，再按原始 little-endian bits 扫描，不逐元素分配 BigInt。对象 value/fromIndex
  仍由 traced native state 保持，Number/BigInt mismatch 返回 false/-1，N=1/2/4/8/16、forced-major、对象
  conversion 与跨 signed/unsigned boundary 回归通过。定向 Test262：fill/BigInt 32/36、includes/BigInt 26/28、
  indexOf/BigInt 28/30、lastIndexOf/BigInt 26/28；余项仅为 RAB/OOB/ES2024 detach，共享方法不宣称 RAB 支持。
- [x] M8 DataView BigInt accessor slice：Realm 安装 `getBigInt64`/`getBigUint64`/`setBigInt64`/`setBigUint64`，
  直接按 endian 编解码 64-bit bits 并复用 BigInt modulo/分配路径，不经过 Number。N=1/2/4/8/16 与 forced-major
  本地 fixture 通过；定向 Test262 基础路径分别为 getBigInt64 30/42、getBigUint64 30/42、setBigInt64 34/46、
  setBigUint64 2/4 applicable，剩余为 RAB/immutable backing、Symbol.toPrimitive 与 ES2024 长尾。
- [x] M8.2 RegExp `Symbol.match` + `String.prototype.match` branded slice：安装 well-known `Symbol.match`，
  global RegExp 返回完整匹配数组、non-global 复用 exec 结果，String primitive fallback 创建 RegExp；基础
  String.prototype.match 定向 Test262 74/102。自定义/proxy `Symbol.match` observable continuation 与 v-flag
  backend 仍待后续 M8.2 收敛，不宣称完整 RegExp 总项。
- [x] M8.2 RegExp `Symbol.replace` + `String.prototype.replace` branded slice：支持普通字符串首匹配、RegExp
  global/sticky 扫描及 `$&`、`$``、`$'`、`$$`、`$1..$99`、`$<name>` replacement token，复用现有
  UTF-16 backend capture range，不创建中间 match Array。数字 token 保持最长有效编号与两位回退一位规则，
  未参与/不存在的命名捕获展开为空；VM N=1/2/4/8/16 与 forced-major fixture 覆盖。定向
  String.prototype.replace 由 56/108 提升至 66/108，RegExp Symbol.replace 为 34/138；剩余主要是 callback
  replacement、自定义 observable Symbol.replace/exec/result、resumable receiver/replacement conversion 与
  v-flag backend，故不宣称完整 replace 总项。
- [x] M8.2 branded functional replacement slice：新增专用、external-memory-accounted、可 trace 的
  `PendingRegExpReplace`，保存 receiver/input/replacer、预计算 global UTF-16 match ranges、
  `nextSourcePosition`、output backing 与按 capture count 精确定容的 callback 参数 backing；任意 capture 数、
  未参与 capture、named groups null-prototype object、严格/非严格 callback receiver、global Unicode empty
  match advancement、callback abrupt identity，以及 callback result 的 resumable string-hint ToPrimitive/ToString
  均通过 iterative trampoline，不使用 Rust unwind 或跨 safepoint 未 rooted Value。`NativeContinuationKind` 仅新增
  无 payload discriminant，既有 4-byte kind/32-byte continuation 编译期断言保持；N=1/2/4/8/16、forced-major
  与 12 captures fixture 通过。定向 Test262：String.prototype.replace 84/108，RegExp `@@replace` 44/138。
  generic custom `@@replace`/`exec`/result property observable state、receiver/input conversion 与完整 lastIndex
  protocol 尚未闭合，因此完整 replace/M8.2 总项不打勾。
- [x] M8.2 RegExp `Symbol.matchAll` + `String.prototype.matchAll` iterator substrate slice：安装
  `Symbol.matchAll`、`%RegExpStringIteratorPrototype%` 与 GC-managed iterator payload；genuine RegExp 路径
  复制匹配状态，global/non-global 迭代、Unicode empty-match advancement 与 done sticky state 不修改原对象。
  RegExp 分配将 source/flags/prototype 全部纳入 traced allocation roots，N=1/2/4/8/16 与 forced-major
  回归通过。定向 Test262 为 String matchAll 28/50、RegExp `@@matchAll` 18/52、RegExpStringIterator
  20/34；剩余 generic species/custom exec/observable Get-Set 与完整 `u/v` backend 继续由 M8.2 总项追踪。
- [x] M8.2 RegExp String Iterator observable `RegExpExec` slice：`next()` 不再绕过 `exec` 直接进入
  backend；固定五槽 `NativeCallState` 与独立 typed continuation 覆盖 Proxy/accessor-aware `exec` Get、custom
  Call、返回值 object/null 校验、result `"0"` Get/ToString、empty-match `lastIndex` Get/ToLength/strict Set，
  abrupt completion 保留原始 JS throw identity。builtin fallback 的临时 exec state 写入 VM root，outer iterator
  state 由 completion stack 保活，不让 moving GC 依赖 Rust local `Value`。N=1/2/4/8/16、forced-minor/
  forced-major 回归已加入；定向 RegExpStringIterator Test262 从 20/34 提升到 32/34，剩余 2 个 variant 是
  `lastIndex.valueOf()` 抛出时的共享 conversion-unwind catch identity 缺口。`@@matchAll` 创建阶段 species、flags
  与 receiver/input observable conversion 仍未闭合，因此 M8.2 总项不打勾。
- [x] M8.2 RegExp prototype accessor slice：删除把 `source`/`flags`/boolean flags 伪装成实例虚拟数据属性的
  捷径，改为 realm-local `%RegExp.prototype%` accessor descriptors；source 执行 slash/line-terminator
  `EscapeRegExpPattern`，boolean getters 做 branded slot check，generic flags 按 `d/g/i/m/s/u/v/y` 顺序读取
  ordinary data properties并为 genuine RegExp 保留 slot fast path。N=1/2/4/8/16 与 forced-major 通过；
  source 从 6/24 提升到 22/24、global 为 18/20。后续 flags continuation 以固定五槽 state 保存 receiver 与
  8-bit result mask，按 `d/g/i/m/s/u/v/y` 逐项执行 Proxy/accessor-aware Get，使用固定 8-byte stack buffer
  输出且不建立增长 Vec；同步 native accessor 已消费 continuation 后以 completion depth 停止二次 pop。
  flags 最终从 2/32 提升到 32/32，N=1/2/4/8/16、forced-major、getter/Proxy 顺序与 abrupt identity 均覆盖。
  剩余 source/global 各 2 项属于共享 cross-realm Error identity，M8.2 总项不打勾。
- [x] M8 String trim receiver conversion slice：`trim`/`trimStart`/`trimEnd` 的 primitive receiver 保留直接快路径，
  Number/Boolean/boxed/object receiver 统一接入 resumable ToString（`Symbol.toPrimitive`、`valueOf`、`toString`）
  顺序，不在 native 入口错误地做 String brand 检查。定向 trimStart 与 trimEnd 均由 22/46 提升至 40/46；
  剩余 6 项为既有异常 identity/边界，不宣称完整 String trimming 总项。
- [x] M8 fixed Number `TypedArray.prototype.copyWithin` slice：primitive target/start/end 零状态分配，object
  index 才使用五槽 traced state 与三个 typed conversion consumer，严格保持 target→start→end observable
  coercion 顺序。initial/current length 双重 count 截断、conversion detach、zero-count detach、throw identity、
  非零 byteOffset、正反重叠与 receiver identity 均覆盖；字节移动在单个 checked no-GC borrow 内使用 safe
  `slice::copy_within`，保留 bit-level encoding并让后端生成 memmove-quality 重叠复制，不使用 unsafe/裸指针。
  N=1/2/4/8/16、forced-major 与 20,000 element stack stability 通过。RAB 暴露后已原子回滚未完成 capability，
  Test262 目录从实现前 18/130 提升到 70/130；剩余 22 个 semantic variant 集中在 BigInt、immutable backing、
  RAB/OOB 与共享 error/harness 能力，另有 38 个 unsupported variant，故不勾 TypedArray 总项。
- [x] M8 fixed Number `TypedArray.prototype.reverse` slice：入口只建立一次 fixed view/backing witness，在单个
  checked no-GC mutable borrow 内按 1/2/4/8 byte element width 使用 const-generic safe block swap；不做
  element decode/re-encode，因此 Float64 NaN payload 与全部 raw bits 原样交换，且无 unsafe、Vec、逐元素分配或
  Rust recursion。九种 Number TypedArray、odd/even、非零 byteOffset、ordinary non-index property、receiver
  identity、N=1/2/4/8/16、forced-major detach 与 20,000 element stack stability 均覆盖；Test262 从 6/44
  提升到 26/44，ES6 fixed Number 26/26。剩余 12 个 semantic 与 6 个 unsupported variant 属于 BigInt、
  immutable backing、RAB/OOB 与 Dynamic Function 前置能力，未发布 partial RAB，TypedArray 总项仍不打勾。
- [x] M8 fixed Number `TypedArray.prototype.set` slice：TypedArray source 加 primitive offset 走零状态分配
  bulk path；array-like 或 observable offset 使用五槽 traced `NativeCallState`，严格保持 offset ToInteger、
  target backing validation、ToObject(source)、Get/ToLength(length)、逐项 Get/ToNumber/write 的可观察顺序。
  同 kind/same backing 使用 safe `slice::copy_within`；其他 typed source 以 `try_reserve_exact` 建立精确 byte
  snapshot，保留同 kind NaN payload 并为 cross-kind 执行 Number conversion，不缓存 backing pointer。
  callback 中 detach 后写入成为 no-op，但后续 source Get 继续；入口前已 detach 仍先执行 offset conversion，
  随后抛 TypeError 且不读取 source.length。九种 Number TypedArray、双向 overlap、cross-kind、非零 byteOffset、
  observable/abrupt conversion、N=1/2/4/8/16、forced-major 与 20,000 array-like stack stability 均覆盖。
  本切片将 Test262 从 14/220 提升到 88/220；join 前置闭合后达到 90/220，ES6 为 90 pass、0 semantic、
  2 unsupported。剩余依赖 BigInt、SAB、RAB/OOB 与 immutable backing，未发布 partial capability，
  TypedArray 总项仍不打勾。
- [x] M8 fixed Number `TypedArray.prototype.join` slice：入口先 ValidateTypedArray/attached backing 并冻结
  internal length，separator undefined 直接使用 comma，primitive 走零状态路径，object separator 仅使用二槽
  traced `NativeCallState` 与 string-hint continuation，严格保证初始 detached 在 ToString 前抛错、conversion
  detach 后仍按 initial length 输出 separators。输出复用统一 ECMAScript Number formatter，先同步预扫精确计算
  UTF-16 units，再 `try_reserve_exact` 一次并以第二遍显式 loop 填充；无 Rust Display、递归或 Vec 扩容猜测。
  九种 Number TypedArray、空/单元素、NaN/Infinity/-0、非零 byteOffset、abrupt identity、N=1/2/4/8/16、
  forced-major 与 20,000 element exact-capacity 回归通过。Test262 从 8/64 提升到 36/64，ES6 fixed Number
  34/34；同时清除 set 的 join 前置失败，使 set 从 88/220 到 90/220、ES6 90 pass/0 semantic/2 unsupported。
  剩余 join variant 属于 BigInt、RAB/OOB 与 immutable backing，TypedArray 总项仍不打勾。
- [x] M8 fixed TypedArray `TypedArray.prototype.slice` slice：五槽 traced `NativeCallState` 保存 source、归一化
  start、count、constructor scratch 与 result，start/end ToIntegerOrInfinity、constructor Get、`@@species`
  Get 和 custom Construct 全部经 typed continuation 恢复；species 结果重新执行 TypedArray brand/backing
  验证，长度小于 count 通过专用 engine error 映射为 TypeError，count 非零时才重新验证 source detach。
  同 kind/different backing 使用 exact-capacity raw-byte snapshot，保留 Float NaN payload；同 backing overlap
  严格按新版规范逐字节前向读写，使先前 target write 可影响后续 source read；cross-kind 在相同 ContentType
  内逐 element 转换，BigInt64/BigUint64 之间保留 modulo-2^64 语义，Number/BigInt species 即使 count 为零也
  抛 TypeError。十一种 TypedArray、cross-kind/custom species、same-buffer offset overlap、detach/zero-count、
  abrupt identity、N=1/2/4/8/16、forced-major 与 20,000 element stack stability 均覆盖；删除了 cross-kind
  Number-only `expect`，全量运行不再因 BigInt species 触发 panic=abort。Test262 目录达到 160/184，ES6
  78/78；剩余 16 semantic 与 8 unsupported variants 集中在 RAB/OOB、immutable backing 和共享前置缺口，
  TypedArray 总项仍不打勾。
- [x] M8 fixed Number `TypedArray.prototype.subarray` slice：五槽 traced `NativeCallState` 保存 source、原始
  ArrayBuffer identity、begin/end 与 initial length/constructor scratch；入口 RequireInternalSlot 后将 detached
  fixed source 的 witness length 视为 0，不提前抛错，随后严格执行 begin、end ToIntegerOrInfinity、constructor
  Get、`@@species` Get、Construct。fixed view 始终传递精确三参数 `(buffer, originalByteOffset + start *
  sourceElementWidth, newLength)`，result 只验证 TypedArray brand 与 attached backing，允许 custom species 返回
  cross-kind/different-length/different-buffer 实例；default/foreign species 仍共享原 backing。九种 Number
  TypedArray、共享 mutation、三参数 observation、初始 detached conversion order、conversion detach 后 custom
  species 成功、abrupt/species、N=1/2/4/8/16、forced-major 与 cross-Realm prototype 均覆盖。Test262 从
  14/134 提升到 72/134，ES6 72/72；剩余 20 semantic/34 unsupported 属于 BigInt，2 semantic/6 unsupported
  属于 RAB auto-length/OOB 与共享 syntax，TypedArray 总项仍不打勾。
- [x] M8 TypedArray iterator projection slice：在共享 `%TypedArray%.prototype` 安装 `keys`、`values`、`entries`
  与 `@@iterator = values`，创建现有 GC-managed ArrayIterator payload，并在入口执行 brand/backing 校验。
  三种 Number TypedArray 的 value/key/entry projection、descriptor、N=1/2/4/8/16 与 forced-major fixture 均通过；
  observable custom iterator/proxy、BigInt/RAB/SAB 长尾仍由 TypedArray 总项追踪。
- [x] TypedArray large iterable regression oracle：10,000-element `Array.from`、array-like TypedArray constructor
  与 intrinsic-iterator constructor 夹具返回 boolean；恢复被 iterator projection 提交误改为 integer 511 的
  assertion。大输入 stack-stability test 与完整 TypedArray test group 重新通过，不把 test-only 修复计为
  ECMAScript semantic 增量。
- [x] M8 fixed TypedArray default-sort slice：十一种 fixed TypedArray 先建立 exact-capacity element snapshot，
  以 stable sort 完成 Number/BigInt 数值顺序、NaN-last 与 `-0` before `+0`，再通过一次 checked backing
  writeback 提交；不缓存裸指针、不依赖 Rust unwind。N=1/2/4/8/16 与 forced-major 回归通过，定向
  Test262 从 12/70 applicable 提升到 34/70。
- [x] M8 fixed TypedArray callable-sort slice：入口先读取 exact-length GC-traced Value snapshot，随后复用
  `Array.prototype.sort` 的 GC-managed bottom-up stable merge owner；comparefn 通过 iterative frame
  trampoline 调用，返回 object 经 resumable ToNumber，NaN 作为相等，不在 Rust sort closure 内重入 JS。
  comparator detach 后继续使用已收集 values，writeback 重新走 integer-indexed Set；callback 与 conversion
  abrupt completion 保持原始 identity。Number/BigInt、稳定性、N=1/2/4/8/16 全矩阵 forced-major 均通过，
  定向 Test262 从 34/70 applicable 提升到 58/70；剩余 applicable failure/unsupported 均依赖 RAB/OOB。
  RAB/OOB、immutable backing 与共享 syntax 仍待后续切片，因此 TypedArray 总项不打勾。
- [x] M8 fixed TypedArray `toSorted` slice：ValidateTypedArray 后按 internal length 创建 active-Realm same-kind
  fixed target，不读取 receiver `length`/`constructor`/`@@species`；source 先发布到 destination register，跨
  ArrayBuffer/view 分配后重新读取并 raw-copy 全部 bytes，随后 target 接管 root 并复用 default/callable sort
  机器。原 view/backing 保持独立，NaN payload、signed zero、Number/BigInt、callback abrupt identity 与
  comparator detach source 均覆盖；N=1/2/4/8/16 全矩阵 forced-major 通过，定向 Test262 24/24。
  RAB auto-length/OOB、immutable backing 与 future cross-Realm扩展仍由 TypedArray 总项追踪。
- [x] M8 fixed TypedArray `TypedArray.prototype.with` slice：独立五槽 traced state 严格按 ValidateTypedArray、
  initial length、`ToIntegerOrInfinity(index)`、按 `ContentType` 执行 `ToNumber`/`ToBigInt`、当前
  IsValidIntegerIndex revalidation 的 observable 顺序恢复；结果使用 active-Realm same-kind intrinsic，完全忽略
  `constructor`/`@@species`，先复制 value conversion 后的源位模式再替换单个元素。Number/BigInt、负索引、异常
  identity、conversion 中 detach、N=1/2/4/8/16 与 forced-major 均覆盖。对当前 pinned Test262 checkout
  重新从提交 `8b4e771` 复测后的真实基线为 `14/44`；旧 `36/44` 计数来自较早 harness/variant 口径，不能继续
  作为当前基线。完整 RAB 与 harness source-construction 能力仍由后续纵切追踪，因此不勾 TypedArray 总项。
- [x] M8 RAB auto-length view tracking slice：TypedArray/DataView payload 使用显式
  `ViewLengthMode::{Fixed,Tracking}`，不以 `u32::MAX` magic sentinel 与合法最大显式长度冲突；TypedArray mode
  利用既有 alignment padding 保持 48-byte payload，DataView 为正确表示从 40 bytes 增至 48 bytes。省略
  length/byteLength 的 RAB view 每次 snapshot 从当前 backing byte length 派生范围，shrink 后进入 OOB/零长度，
  grow 后恢复；fixed view 保留显式长度并在当前 backing 不足时进入 OOB。新增 TypedArray/DataView shrink/grow/
  OOB/restoration 回归，ArrayBuffer 14/14、TypedArray 51/51 定向通过。`TypedArray.prototype.with` 对 value
  conversion 中 grow 的 source 只复制 result 初始长度、并只在 result index 可表示时写 replacement，消除
  raw-slice 越界 panic；当前 pinned Test262 从 `14/44` 提升到 `18/44`，4 个 RAB variants 全部修复、broken 0。
  完整 RAB method witness、DataView conversion 中 resize、SAB 与 immutable backing 仍未闭合。
- [x] M8 RAB constructor capability contract：`ArrayBuffer(length, { maxByteLength })` 按非 undefined option
  设置 resizable bit，即使 `maxByteLength === length` 也允许 shrink；这闭合 Test262 TypedArray constructor
  的 shrunk-RAB source factory，使 `TypedArray.prototype.with` 的共享 constructor/source matrix 不再在方法入口
  前误抛 RangeError。新增 equal-limit shrink fixture，以及十一种 TypedArray 的 N=1/2/4/8/16、forced-major
  length-tracking copy fixture；独立 clean worktree 的 pinned Test262 结果：`with` 从 18/44 到 44/44，
  `subarray` 从 20/134 到 122/134，`slice` 从 26/184 到 162/184，ArrayBuffer 全目录从 328/442 到
  338/442（applicable 324/384 到 334/384）。
- [x] M8 RAB TypedArray species/OOB witness closure：TypedArray substrate 新增显式 current witness，区分
  detached、attached-but-OOB 与合法零长度 view；`slice` 入口用 ValidateTypedArray witness 拒绝 OOB receiver。
  tracking RAB 的省略-length constructor 对非整除 byte range 按 element width 向下取整，fixed view 继续要求
  整除；`subarray` 按 immutable `ViewLengthMode` 和 end 是否 undefined 在 species Construct 时选择两参数
  `(buffer, byteOffset)` 或三参数 fixed view，callback 后重新 snapshot source。N=1/2/4/8/16、forced-minor/
  forced-major fixtures 覆盖 tracking species args、OOB grow recovery、非整除 RAB 与 OOB ValidateTypedArray。
  pinned Test262：subarray 从 122/134 到 128/134（semantic 0，剩余 6 unsupported Dynamic Function fixture），
  slice 从 162/184 到 174/184（applicable semantic 0；剩余 8 unsupported Dynamic Function 和 2 个非发布
  immutable-buffer semantic）。
- [x] M8/M12 fixed ArrayBuffer detach substrate slice：ArrayBuffer object 的 backing edge 可幂等清除，
  既有 TypedArray/DataView 每次经原 buffer identity 重新解析，因此无需 observer list、atomic、mmap 或
  per-view mutation；新增独立 `DetachedArrayBuffer` TypeError，统一 fixed ArrayBuffer/TypedArray/DataView
  accessor、integer-indexed MOP、constructor 和 element access 的 detach 结果与转换/bounds 顺序。
  embedding Realm hooks 默认安装 `$262.detachArrayBuffer` 并复用同一 VM 原语；N=1/2/4/8/16、重复 detach、
  detach-during-conversion 和 forced-major 回归覆盖。RAB、transfer、SAB 仍由各自未完成项追踪，不能据此
  勾完整 M8.1 或 M12.2 Host API。固定 Test262 detached 结果：ArrayBuffer `18/22`（剩余 4 项为
  SharedArrayBuffer），DataView `byteLength` detached `2/2`、`getUint8` detach 顺序 `6/6`，TypedArray
  `length` 的 fixed detached 用例通过。
- [x] M5/M8 Number/Math surface closure：`Number.parseFloat`/`Number.parseInt` 复用 global intrinsic
  function identity，安装 `Number.prototype.toLocaleString` 的 number-brand fallback，补齐
  `Math[Symbol.toStringTag]` descriptor，并修正 `Math.round` 在 `0.5 - Number.EPSILON / 4` 上的二次舍入。
  Number 定向 Test262 达到 674/680（剩余项属于 BigInt 或 cross-realm），Math 达到 624/634；
  `Math.sumPrecise` 仍保留为需要 iterator/number semantics 的独立切片。
- [x] NativeFunction identity width closure：标准 builtin surface 加入 flatMap 后超过 256 个 identity，
  `NativeFunction` 从 `repr(u8)` 升为 `repr(u16)`；既有 compile-time layout gate 证明
  `FunctionExecutable` 仍为 16 bytes、`FunctionObject` 仍为 56 bytes，不增加每函数常驻成本。同步补齐
  generic bound Call 对既有 argument prefix/source 的 exact-capacity 合并，不再把合法 bound callback
  错分为 unsupported，并以 continuation root 覆盖 forced-major 下的旧/new prefix 生命周期。
- [x] Array brand/IsArray closure slice：`Object.prototype.toString` 识别现有 Realm `%Math%`、`%JSON%` 和
  `ArgumentsObject` payload；`IsArray` 保持直接 Array fast path，并沿 nested Proxy target 递归、对 revoked
  Proxy 传播异常。`Array/prototype/filter` 提升到 440/480，`create-proxy.js` 达到 2/2。
- [x] M5/M10 `Object.prototype.toString` observable-tag closure：对照 QuickJS `js_object_toString`、Escargot
  `builtinObjectToString` 与 Test262，把 fallback brand 在 observable lookup 前压缩保存，并通过 typed native
  continuation 执行 Proxy/accessor-aware `Get(O, @@toStringTag)`；只有 primitive String 覆盖 fallback，结果直接
  以 UTF-16 拼接。Realm graph 补齐 `%IteratorPrototype%`、Array/String/Map/Set iterator、Generator/AsyncGenerator、
  GeneratorFunction/AsyncFunction/AsyncGeneratorFunction 的标准 tag，以及三类隐藏 constructor/prototype 的
  `constructor` 回边；dynamic specialized Function constructor 调用仍由独立 host compiler contract 切片追踪。
  N=1/2/4/8/16、forced-major 和 abrupt getter/Proxy 回归通过，固定 Test262 目录从 44/82 提升到 82/82。
- [x] Array literal CreateDataProperty slice：对照 QuickJS `OP_define_array_el` 与 Escargot Array
  `defineOwnProperty`，新增 verified `CreateDataPropertyById/ByValue`，数组字面量元素直接创建
  writable/enumerable/configurable own data property，不再由 `SetById` 误触发继承 accessor；HIR 为尾部
  elision 合成的 `length` 仍走 Array `[[Set]]`，避免用全 true descriptor 重定义不可配置 length。
  opcode 编解码/verifier/disassembly、N=1/2/4/8/16 回归通过；`Array/prototype/filter` 提升到 442/480。
- [x] ArraySpeciesCreate cross-Realm intrinsic slice：对照 QuickJS `JS_GetFunctionRealm` 与 Escargot
  `getFunctionRealm`，constructor 为异 Realm 原生 `%Array%` 时在读取 `@@species` 前回退当前 Realm Array；
  复用 Proxy/Bound-aware `realm_for_callable`，新增不切换 execution context 的 realm-local Array constructor
  identity lookup。两个 Realm 的 observable species getter 均保持零调用，N=1/2/4/8/16 回归通过；
  `Array/prototype/filter` 提升到 444/480。
- [x] Date branded numeric foundation slice：对照 QuickJS `JS_CLASS_DATE`/`js_date_constructor` 与 Escargot
  `DateObject`/`builtinDateConstructor`，引入独立 traced `DateObject { [[DateValue]], ordinary }`，Realm-local
  `%Date%`/`%Date.prototype%`、cross-Realm `GetPrototypeFromConstructor` fallback，以及带 brand check 的
  `getTime`/`valueOf`。单参数 numeric/boolean/Date-copy 构造使用规范 `TimeClip`，包括 `8.64e15` 边界、
  truncation 和负零归一化；N=1/2/4/8/16 与 forced-major 回归通过。`Date/prototype/getTime` 为 16/16、
  `valueOf` 为 12/12、prototype constructor 为 2/2；`Array/prototype/filter` 因真实 Date brand 提升到
  450/480。完整 Date 仍未完成：函数调用、字符串解析、多参数本地时间构造、UTC/local getters/setters 与
  格式化等待 Date package，不允许 engine core 回退读取系统时间。
- [x] Date UTC arithmetic slice：以单一 `DateUtcField` descriptor 安装 8 个 UTC getter，clipped integral
  milliseconds 通过 Euclidean day/time decomposition 与 proleptic Gregorian civil conversion 解码，负 epoch、
  leap day、上下 TimeClip 边界均不依赖 timezone provider。`Date.UTC` 按 MakeDay/MakeTime/MakeDate 指定顺序
  计算并覆盖 field overflow、year 0..99 offset、floating-point evaluation-order 与 `8.64e15` clip；
  `setTime` 直接更新 branded payload。N=1/2/4/8/16、forced-major 和负毫秒回归通过；8 个 UTC getter
  各 16/16、`Date.UTC` 30/34、`setTime` 18/22，Date 全目录从 152/1172 applicable 提升到 348/1172。
  剩余 8 个 UTC/setTime variant 依赖通用 object ToPrimitive/ToNumber continuation，不在 Date 内复制近似路径。
- [x] Date UTC setter family slice：以单一 `DateUtcSetter` descriptor 安装 `setUTCFullYear`/`Month`/`Date`/
  `Hours`/`Minutes`/`Seconds`/`Milliseconds`，先 brand-read `[[DateValue]]`、再按参数顺序 ToNumber，只覆盖
  supplied optional fields并复用共享 MakeDate/TimeClip；invalid Date 仅 `setUTCFullYear` 从 +0 恢复，其余
  setter 在完成参数转换后保持 NaN。七个方法全部进入 N=1/2/4/8/16 与 forced-major 回归；分别达到
  8/12、12/18、8/14、16/22、10/16、12/18、10/16，Date 全目录从 348/1172 applicable 提升到
  438/1172。剩余 40 个 variant 全为 observable object ToPrimitive 顺序用例，归共享 conversion continuation。
- [x] Date UTC formatting slice：新增固定 40-byte stack buffer 的 `toISOString` 与 `toUTCString`，直接消费
  `UtcDateParts`，不使用 Rust formatting、heap 中间 Vec 或 timezone provider；ISO 覆盖 4-digit/带符号
  6-digit extended year、millisecond padding、invalid Date RangeError，UTC string 覆盖 weekday/month table、
  negative year 和 `Invalid Date`。`toGMTString` 与 `toUTCString` 共享同一 function identity。N=1/2/4/8/16、
  forced-major 和最大 extended-year Test262 通过；`toISOString` 达到 22/34、`toUTCString` 10/18，Date
  全目录从 438/1172 applicable 提升到 466/1172。剩余 formatter failures 依赖 local multi-argument Date、
  timezone offset 或 string Date parsing，不由 UTC formatter伪造宿主默认值。
- [x] Date resumable numeric arguments slice：`Date.UTC`、`setTime` 与七个 UTC setter 复用通用
  `@@toPrimitive` trampoline 的 number-hint consumer；primitive 参数保持零分配，首次 object 参数才发布
  128-byte traced `PendingDateNumericArguments`，固化最多七个参数、已转换字段、receiver、operation 与原始
  invalid-Date 提交条件。转换严格从左至右，brand/`[[DateValue]]` snapshot 先于任何 callback；非 FullYear
  setter 的原值为 NaN 时完成全部 conversion 后返回 NaN，但不覆盖 callback 对 receiver 的可观察修改。
  连续七个 object 参数、abrupt completion、N=1/2/4/8/16 与 forced-major 回归通过；Date 全目录从
  466/1172 applicable 提升到 514/1172，相对基线 fixed 48、broken 0。
- [x] Date `@@toPrimitive` slice：发布 non-writable/non-enumerable/configurable 的
  `%Date.prototype%[Symbol.toPrimitive]`，native name/length 为 `[Symbol.toPrimitive]`/1；generic object
  receiver 依据 `string`/`default` 从 ToString stage、依据 `number` 从 ValueOf stage 直接进入共享 conversion
  trampoline，等价于 QuickJS `HINT_FORCE_ORDINARY` 与 Escargot `ordinaryToPrimitive`，不会递归查询自身
  `@@toPrimitive`。invalid hint 和 non-object this 映射 TypeError；N=1/2/4/8/16、forced-major、getter/
  non-callable/fallback/abrupt 顺序通过，专项目录 36/36；Date 全目录从 514 提升到 542/1172，fixed 28、
  broken 0。
- [x] Date generic `toJSON` slice：对照 QuickJS `js_date_toJSON` 与 Escargot `builtinDateToJSON`，先对
  receiver 执行共享 `ToObject` 和 number-hint `ToPrimitive`；仅 primitive Number 的非有限结果短路为 null，
  其他 primitive 继续对原 boxed receiver 执行 Proxy/accessor-aware `Get("toISOString")`，再以原 receiver、
  零参数 Call。conversion 后复用 32-byte 两阶段 `DateToJson(Get/Call)` continuation，不新增 heap state、
  Frame 字段或 Rust 递归。同步/bytecode getter、abrupt conversion、Number/Symbol primitive boxing、
  N=1/2/4/8/16 与 forced-major 回归通过；同时修正不可构造 `%Symbol%` 的 `prototype` 为规范 constant own
  data property，而非仅 constructor 可见的虚拟 prototype 槽。`Date/prototype/toJSON` 为 26/26；Date 全目录
  从 542 提升到 566/1172，fixed 24、broken 0。
- [x] Date ISO parse UTC/offset slice：安装 non-constructible `Date.parse` 并复用
  `ConversionNativeFunction` 的 string-hint ToPrimitive/ToString trampoline；独立 `builtins/date/parse.rs`
  直接解析 UTF-16 code units，覆盖 4-digit/带符号 6-digit year、负零 year rejection、partial date defaults、
  optional seconds/fraction、24:00、`Z` 与 `+/-HH:mm`。parser 结果显式区分 `Utc(value)` 与
  `Local(fields)`；date-only 和显式 offset 在减 offset 后才最终 TimeClip，offsetless date-time 在 M7 timezone
  provider 接入前保持 NaN，不伪装为 UTC。N=1/2/4/8/16、forced-major、对象 string conversion 与边界回归
  通过；`Date/parse` 从 0 提升到 12/16，完整 Date 从 566 提升到 580/1172，fixed 14、broken 0。剩余
  4 variants 分别依赖 local timezone/getTimezoneOffset 与 local `Date.prototype.toString`。
- [x] M7/M8 Date host-provider boundary slice：新增互相独立的 `WallClockProvider` 与 `TimeZoneProvider`，
  由 isolate 独占 `Box<dyn ... + Send>`，不使用 `Arc`/atomic，也不污染保持 `Copy + Eq` 的纯数据
  `IsolateConfig`。`HostProviderError` 使用固定 failure code，不在 FFI 错误路径分配字符串；
  `Isolate::new_with_host_providers` 显式注入，旧 `new` 表示没有宿主能力且返回结构化 missing-provider error，
  绝不读取 `SystemTime`、libc、环境变量或时区文件。`Date.now` 与零参数 `new Date()` 共用 wall-clock +
  `TimeClip` 路径；Test262 runner 注入 deterministic clock。N=1/2/4/8/16、forced-major、provider failure
  传播已覆盖，Date applicable 从 580 提升到 592/1172。timezone trait 已冻结，但 local Date consumer 尚未
  接入，因此 M7.5 与完整 Date 总项不打勾。
- [x] M7/M8 Date local-time consumer slice：`TimeZoneProvider` 正式接入 UTC instant→local offset 与 local
  wall-time→UTC 两个方向；local getter、`getTimezoneOffset`、七个 local setter、offsetless date-time parse、
  多参数构造、函数形式 `Date()`、`toString`/`toDateString`/`toTimeString` 及 ECMA-262 locale fallback 共用
  fixed calendar arithmetic 与 40-byte format buffer。单参数构造新增 default-hint resumable ToPrimitive，
  Date-copy、String parse、`@@toPrimitive`、valueOf fallback 与 abrupt completion 不再走同步近似路径；多参数
  构造复用既有 128-byte traced numeric state，newTarget 在 callback/forced-major 期间保持精确 root。
  `%Date.prototype%` 修正为无 `[[DateValue]]` 的 ordinary object。runner 注入 deterministic UTC timezone，
  固定 +01:30 provider 的双向转换、N=1/2/4/8/16、forced-major、missing/failure code 与 required
  `Date.parse(x.toString()/toUTCString())` round-trip 均覆盖。Date applicable 从 592 提升到 1166/1172
  （99.49%），fixed 574、broken 0；剩余 6 个均为三个 negative-year fixture 对共享
  `String.prototype.split` 的依赖，不是 Date calendar/format failure。
- [x] M8 String.prototype.split + genuine RegExp fast-path slice：新增独立 `string_split.rs`，primitive
  receiver/separator/limit 保持同步快路；只有 `@@split` Get/Call 或对象 ToPrimitive 才分配固定五槽 traced
  state。GetMethod 先于 receiver ToString，自定义 splitter 收到原 receiver/limit；fallback 严格执行 receiver
  ToString、limit ToUint32、separator ToString，并在 limit=0 前保留 separator conversion。字符串扫描直接按
  UTF-16 code unit 写 intrinsic Array，不建立 match-position Vec。`RegExp.prototype[Symbol.split]` 的 genuine
  RegExp path 复用 regress backend，按 sticky-at-q、Unicode AdvanceStringIndex、empty match、capture insertion
  与 limit 执行，不修改原 regexp lastIndex；substring property key 在 value allocation 前准备，forced-major
  同时修复 RegExp literal source/flags 跨分配 rooting。N=1/2/4/8/16、forced-major、custom splitter/order 与
  captures 已覆盖；String split applicable 从 4/240 提升到 237/240（98.75%），Date applicable 随之达到
  1172/1172。剩余 String 项为 2 个 BigInt literal 前端缺口和 1 个 strict eval/global-binding 缺口；完整
  RegExp `@@split` generic species/custom exec/observable lastIndex 路径仍为 30/88，因此 M8.2 总项不打勾。
- [x] M8.2 RegExpExec/test + exec input-conversion slice：新增独立 `regexp_exec.rs`，
  `RegExp.prototype.test` 先完成 resumable ToString，再执行 Proxy/accessor-aware `Get(R, "exec")`；custom
  callable 保留 receiver 与单个已转换 String 参数，null/object result 分流且 primitive result 抛 TypeError，
  non-callable exec 仅在 receiver 拥有 `[[RegExpMatcher]]` 时进入 builtin fallback。genuine test 使用无 match
  Array/capture allocation 的 backend test-only path并保留 global/sticky lastIndex。`RegExp.prototype.exec`
  同步接入相同 ToString substrate；固定 traced state 在任意分配前发布 receiver、input、result Array、groups 与
  capture temporary，数字/命名 capture 均 key-before-value。N=1/2/4/8/16、getter/call abrupt、Proxy、
  ordinary/non-callable brand、object toString/valueOf、function input、named/positional captures 与 forced-major
  已覆盖；`RegExp/prototype/test` 从 72/90 提升到 90/90。后续 observable
  lastIndex 纵切补齐 mandatory Get、number-hint resumable ToLength、global/sticky success/failure strict Set、
  non-global/non-sticky read-without-write、readonly abrupt 与超长索引归零；N=1/2/4/8/16 和 forced-major 均纳入
  专项矩阵。
- [x] M8.2 RegExp `d` indices + full-Unicode exec slice：backend pattern compile 从 UTF-16 code unit
  iterator 按 `u/v` 合并 surrogate pair，执行时非 Unicode 使用 UCS-2 matcher、`u/v` 使用 UTF-16 matcher，
  两路结果都保持 ECMAScript code-unit offset；闭合 Unicode property escape 和 `v` sets backend。exec result
  始终发布 `groups`，命名 groups 使用 null prototype；`d` 创建完整 match/capture pair Array、`indices.groups`
  和 duplicate named alternative 结果，并从已 root 的 result graph 逐层发布 indices/groups/pair，不扩大全局
  `NativeCallState`。compiled backend 额外保存精确 decoded name→numeric capture index，不用相同 span 猜测
  identity；N=1/2/4/8/16 与各自 forced-major 覆盖 unmatched capture、nested equal-span pair identity、astral offset、
  `u/v` property escape；`RegExp/prototype/exec` 从 148/158 提升到 158/158。完整 M8.2 仍受 compiled cache、
  borrowed Latin-1 input、matcher resource budget 及其余 String regexp integration 项约束，不打总项勾。
- [x] M8.2 RegExp Unicode named-capture atom slice：decoded capture name 不再把 Rust UTF-8 `as_bytes()`
  错当 Latin-1 property name，而是统一按 ECMAScript UTF-16 string identity intern，并复用于 exec `groups`、
  `indices.groups` 与 functional replacement groups。N=1/2/4/8/16、forced-major 和 RegExp 单测 17/17
  通过；Test262 `match-indices` 从 20/28 到 22/28、`named-groups` 从 50/72 到 52/72，两个非 ASCII
  property-name 文件均 2/2。RegExpStringIterator 剩余 conversion-unwind 缺口仍由共享异常 continuation
  追踪，M8.2 总项不打勾。
- [x] M8 JSON.stringify primitive String `space` slice：对照 QuickJS、Escargot 与 Boa，将 String
- [x] M8.2 `RegExp.prototype[@@search]` + `String.prototype.search` slice：新增独立
  `regexp_search.rs` 与固定五槽 traced state，闭合 String GetMethod/RegExpCreate/Invoke、RegExp input
  ToString、observable lastIndex Get/SameValue/strict Set/restore、custom exec、builtin exec、result.index Get
  及 Proxy/accessor suspension。primitive searchValue 按现行规范跳过 `@@search` GetMethod 后进入 RegExpCreate；
  三层 `String.search -> @@search -> exec` native trampoline 在 ExecCall 完成处保留 frame-owned parent，避免
  同步 drain 跨 frame 提前消费 continuation。N=1/2/4/8/16、forced-major、custom method/exec、lastIndex
  `-0`、abrupt identity 和 primitive prototype non-observation 均覆盖；String search 从 2/86 到 86/86，
  RegExp @@search 从 4/46 到 44/46，合计从 6/132 到 130/132。剩余 2 个为共享 Test262Error constructor
  identity/harness failure，search 自身的对象 abrupt identity 已由定向测试闭合；完整 M8.2 总项仍不打勾。
- [x] M8.2 branded `@@matchAll` + RegExp String Iterator slice：新增专用 traced iterator payload，保存
  cloned matcher、input、global/unicode/done internal slots；`next` 按需执行 builtin exec，non-global 首次
  success 即完成，global empty match 使用 UTF-16 `AdvanceStringIndex`，`u/v` 模式保留 surrogate pair。
  matcher 与 exec state 在任意后续分配前发布到当前 native destination register，N=1/2/4/8/16 与
  forced-major 均覆盖；原 RegExp `lastIndex` 不被迭代消费，clone 从观测 cursor 起步。定向 applicable：
  String matchAll 28/50、RegExp @@matchAll 18/52、RegExpStringIteratorPrototype 20/34。custom exec、species、
  Proxy/getter 与 object ToString 仍需独立 typed continuation，故完整 `@@matchAll`/M8.2 总项不打勾。
- [x] M8.2 `String.prototype.replaceAll` protocol/substitution slice：固定五槽 traced `NativeCallState` 保存
  receiver、replacement、searchValue 及已转换 input/search String；按 IsRegExp `Symbol.match`、flags ToString/
  global 校验、GetMethod `Symbol.replace`、receiver/search/replacement ToString 的规范顺序，通过 typed
  continuation 覆盖 Proxy/accessor/callback suspension 与 abrupt identity。ordinary String path 执行非重叠
  UTF-16 全局搜索（含 empty search），静态 GetSubstitution 支持 `$$`、`$&`、``$` ``、`$'`，functional
  replacer 复用 external-memory-accounted `PendingRegExpReplace`。intrinsic RegExp flags fast path 仅在 own flags
  缺失、prototype/flags getter 都保持当前 Realm intrinsic 时启用。静态 output 的初始估算使用 checked add，
  unmatched slice 与每个 replacement token 在写入前执行 fallible `try_reserve`，`$``/`$'` 扩张不经过隐式
  `Vec` growth。N=`1/2/4/8/16`、forced-minor/major、64-match capacity 与协议顺序均覆盖；focused Test262
  `String/prototype/replaceAll` 为 86/90，剩余 4 个均是共享 destructuring bytecode unsupported，完整 M8.2
  总项不打勾。
- [x] M8 JSON.stringify primitive String `space` slice：对照 QuickJS、Escargot 与 Boa，将 String
  primitive 按 UTF-16 code unit 截取前 10 个；
  Array/Object 共用 fixed 10-unit gap 与显式 depth 输出换行、缩进和 pretty-mode colon space，不建立随深度
  扩张的 indent String。N=1/2/4/8/16 与 forced-major 回归覆盖嵌套容器、数值边界和字符串截断；
  `JSON/stringify` 从 44/132 applicable 提升到 46/132，fixed 2、broken 0。Number/boxed `space` 由下一共享
  resumable conversion slice 闭合；replacer/reviver/`toJSON`/Proxy-aware Get 仍未完成，因此 JSON 总项不打勾。
- [x] M8 JSON.stringify Number/boxed `space` slice：primitive Number 无回调实现 ToIntegerOrInfinity、0..10 clamp
  与固定 ASCII-space gap；boxed Number/String 分别走共享 number-hint/string-hint conversion continuation，观察
  `@@toPrimitive`、`valueOf`、`toString` 与 abrupt identity，且原始 stringify value 在 callback/GC 期间保持
  traced。覆盖 NaN、正负 Infinity、负数、浮点截断、上限及 overridden wrapper methods；N=`1/2/4/8/16` 与
  forced-major 共用 indentation fixture。`JSON/stringify` 从 46/132 提升到 52/132，fixed 6、broken 0；
  replacer/reviver/`toJSON`/Proxy-aware Get 仍未完成，因此 JSON 总项不打勾。
- [x] M8 JSON.stringify resumable serialization/property-list slice：删除递归 Rust serializer，改为单一
  GC-traced `PendingJsonStringify` + iterative frame；完整流水线覆盖 accessor/Proxy-aware Get、`toJSON`、replacer
  function、Array/Proxy replacer property list（observable length/index Get、boxed String/Number string-hint ToString、
  stable dedup）、Proxy ownKeys/getOwnPropertyDescriptor/Get 与 abrupt identity。output/frame 容量按
  `tuning::json` 在扩容前 replacement-state，key list 改 GC-managed Array edge，跨 allocation 后从 destination
  刷新 movable state，不保留 Rust local Value。N=`1/2/4/8/16`、forced-minor/major、Proxy/replacer、boxed list、
  duplicate 与 >initial output/property-list growth 有内部覆盖；`JSON/stringify` 从 52/132 提升到 130/132，
  fixed 78、broken 0（98.48%）。剩余 2 个仅为 `value-bigint-cross-realm.js` 的共享 TypeError Realm identity；
  JSON.parse reviver 尚未完成，因此 JSON 总项不打勾。
- [x] M8/M12 JSON.stringify large property-list stack-stability slice：对照 QuickJS `for` 与 Escargot `while`
  的 replacer 扫描，把 synchronous property Get 明确拆成 `Returned`/`Suspended`，普通 replacer entry 与无
  replacer function 的 missing Object member 均在 Rust loop 内 drain；accessor/Proxy/user callback 仍发布 typed
  continuation 后退出。Test262 `staging/sm/JSON/stringify-large-replacer-array.js` strict/sloppy 2/2 通过，
  4,096-entry sparse 回归覆盖 N=`1/2/4/8/16`、forced-minor/major，native stack 不再随 missing lookup 数增长。
- [x] M8/M12 JSON.stringify dense primitive stack-stability slice：无 replacer function 时，Object/Array 同步
  Get 得到 null/boolean/finite 或 non-finite Number/String/undefined/Symbol 后直接在 container loop 提交
  prefix/value 并推进 cursor，不再经 `finish -> complete -> advance` 递归；Object/BigInt 的可观察 `toJSON`、
  getter/Proxy 与 replacer callback 继续挂起。4,096 个实际 Object 数值成员及 4,096 个混合 primitive Array
  元素回归覆盖 N=`1/2/4/8/16`、forced-minor/major，native stack 保持常量。
- [ ] Promise state/reaction/capability、thenable assimilation 和 self-resolution。
- [ ] FIFO microtask queue、job realm、unhandled rejection timing。
- [ ] 顶层 job 完成/规范暂停后执行 checkpoint；quantum yield 不形成 JS job boundary。
- [x] `Promise.all` intrinsic Array-iterator fast-path slice：为 intrinsic `%Promise%` 建立固定五槽 aggregate
  state、indexed fulfillment/rejection reaction function 与结果顺序提交；非 Promise 输入进入已 fulfilled 的
  native Promise，pending/rejected 输入复用 reaction queue。aggregate、result Array、input/child/handlers 在
  每个 allocation safepoint 前进入 VM register 或 traced attachment，N=1/2/4/8/16、empty/immediate reject、
  pending input 与 forced-major 均覆盖。定向 `Promise/all` 为 44/196；generic constructor、observable
  `GetPromiseResolve`、thenable、通用 iterator/IteratorClose 与 per-element once guard 尚未完成，因此总项不打勾。
- [x] Promise combinators、finally、withResolvers、Promise.try 等 release target API；各独立纵切与 Test262
  数字见本节前述记录。完整 Promise state/job realm/unhandled rejection 仍由上方未勾选总项追踪。

### M10.2 Async fiber

- [x] generator suspended-start、普通 `yield` 与 `next(value)` vertical slice：对照 QuickJS generator data、Escargot
  `GeneratorObject`/`ExecutionPauser` 与 Boa `GeneratorContext`，compiler 将 `function*` 冻结为独立
  `FunctionKind::Generator`；调用分配 GC-managed `SuspendedStart` 对象，并把 retained arguments 一次冻结进
  immutable argument-prefix backing；第一次 `.next()` 不再复制参数，只通过现有显式 frame 和 typed
  `GeneratorResume` continuation 执行 body，frame 发布后清除 generator 的 creation-time roots。
  `Completed`/`Executing` 重复 next 为固定 header 快路径。normal return/
  throw 均先转 `Completed`，重复 next 返回 `{ undefined, true }`；per-function prototype、
  `%GeneratorPrototype%`/`%GeneratorFunction.prototype%` 链、brand/new/Executing TypeError 和 prototype
  descriptor 已覆盖。普通 `Yield` 使用 immutable wide-aware suspend metadata；GeneratorObject 在 caller 与
  paused generator 两条完整 traced Fiber 间做 allocation-free ownership swap，后续 `next(value)` 注入 yield
  expression destination，首次 next 参数忽略。N=1/2/4/8/16、forced-major、try/finally、large argc 与 512 次
  恒定 native stack 恢复通过。Test262 `language/expressions/yield` 从 6/123 提升至 42/123，余 81 项为
  structured unsupported（主要是 delegated `yield*`，另有 `with`），无 semantic failure；
  `built-ins/GeneratorPrototype/next` 从 22/28 提升至 28/28。`Generator.prototype.return/throw` 已按
  QuickJS resume magic 与 Escargot `generatorResumeAbrupt` 语义接入：SuspendedStart/Completed/Executing 四态、
  suspended-yield `CompletionRecord::Return/Throw` 注入、内部 catch、finally 再次 yield、completion override、
  caller throw propagation、N=1/2/4/8/16、forced-major 与 512 轮 constant-native-stack stress 均覆盖；Test262
  `built-ins/GeneratorPrototype/return` 46/46、`throw` 44/44。
- [x] delegated `yield*` vertical slice：对照 QuickJS yield-star compiler CFG、Escargot ResumeState 和 Boa
  generator compiler，新增 verified `YieldDelegate(result, resume_base, suspend_id)`；compiler 展开
  next/return/throw protocol loop，done=false 原样转发 delegate result identity，missing throw 先 close 且
  close getter/call/non-object error 覆盖最终 TypeError。Generator Fiber 用相邻 value/kind register 接收
  Normal/Return/Throw，不增加 delegate heap object或 Rust recursion；paused Fiber 精确 trace iterator/method/result
  roots。N=1/2/4/8/16、forced-major、catch/finally、nested delegation、return/throw done false/true、所有
  iterator-result Object validation、missing-throw close precedence 与 512 轮 constant-native-stack stress 已覆盖。
  Test262 `language/expressions/yield` 从 42/123 提升到 122/123，唯一 unsupported 为独立 `with` statement
  缺口，semantic failure 为 0。generator expression/method breadth 与 async generator request queue 由后续项闭合。
- [ ] `Await` 保存 pc、destination 和 completion；settled promise 也只通过 microtask 恢复。
- [ ] async function 调用同步推进到首次暂停/完成，再返回结果 Promise。
- [ ] active-fiber trampoline 保证 JS 调用不递归进入 Rust interpreter loop。
- [x] M10.2 ordinary async function/`Await` 第一纵切：compiler 将普通 async function/arrow 冻结为
  `FunctionKind::Async` 并生成 verified suspend metadata；GC-managed `AsyncFunctionState` 独占结果 Promise、
  caller/paused Fiber 与 resume destination/instruction。调用同步 trampoline 到首次 await/return/throw，settled
  Promise、primitive 与 thenable 均只经 Promise job 恢复，rejection 从 Await origin 重入 catch/finally abrupt
  dispatcher。N=1/2/4/8/16、forced-major、return/rejection/try-finally/async-arrow 矩阵通过；Test262
  `language/expressions/await` 为 40/44，ES8 普通 await 22/22，余 4 项仅为 async-generator interleaving 与
  `for-await-of`。完整 async-generator Await/module execution 仍未闭合，因此上方总项保持未勾。
- [x] M10.2 async generator request-queue 第一纵切：compiler 区分 `AsyncGenerator` function kind，调用只创建
  suspended-start object，首次 `.next()` 才启动；generator 自持 traced FIFO request queue 与 active request，
  `.next/.return/.throw` 在 Executing 状态只入队。primitive `yield`/body `return` 经 Promise job settlement，
  completed `next/throw` 按规范同步 settle capability，suspended-start invalid receiver 返回 rejected Promise；
  checkpoint 内 ResumeNext 将 caller PC 固定回有效 Return instruction，不能恢复到 bytecode-end 或复用异 code
  request offset。N=1/2/4/8/16、forced-major、反向 reaction 注册与三种 state-executing queue Test262 均通过；
  `%AsyncGeneratorPrototype%[Symbol.toStringTag]` 已安装，完整目录从 58/96 提升到 80/96。显式 `await`、yield
  thenable assimilation、`%AsyncIteratorPrototype%` 与完整 async function/module substrate 仍未实现，不能勾
  完整 async/await。
- [x] M10.2 `%AsyncIteratorPrototype%` intrinsic chain slice：realm 发布独立 async-iterator prototype，设置
  `Symbol.asyncIterator` identity 方法、`AsyncIterator` toString tag，并令 `%AsyncGeneratorPrototype%` 继承该
  prototype；well-known symbol 作为 realm root 保持 GC 安全，新增 intrinsic-chain regression。
- [ ] generator/async generator 的 next/return/throw request queue 和启动时机分离。
- [ ] await 穿越 try/finally、rejection、nested async 和 top-level await 专项测试。

### M10.3 Driver 与 actor

- [ ] `VmDriver: Future + Send` 使用标准 `Context/Poll/Waker`，不依赖 Tokio。
- [ ] runnable fiber/microtask 按非零 quantum 执行；只有本次 poll 有进展且仍 runnable 才 self-wake；
  等待 host future/command 时正确注册/更新 waker并立即 Pending，禁止空轮询和 wake storm。
- [ ] `IsolateRunner` 拥有 isolate 和有界 command mailbox；completion queue 独立并优先。
- [ ] mailbox 使用成熟 bounded MPSC/AtomicWaker 协议，只在 empty-to-nonempty/idle-to-scheduled转换
  时 wake；禁止 spin、空 `try_recv` 循环和阻塞 sender，每 poll drain 有界且不饿死 JS/command。
- [ ] microtask/reaction/runnable/completion queue 使用 `VecDeque` 或专用有界队列；初始容量来自
  corpus 分布，host command/completion 容量受配置硬限制且满时不隐式扩容。
- [ ] `IsolateHandle: Clone + Send + Sync` 支持 execute/call/with_scope/cancel/close。
- [ ] direct `&mut Isolate` path 不构造 mailbox/waker/shared interrupt；actor batch-latency interrupt 只允许
  单个 compact atomic bitset 每 batch 一次 load，producer 只 fetch-or+wake，禁止 CAS retry/spin。
- [ ] executor migration 测试在不同 OS thread 轮流 poll 同一 driver，不使用持久 TLS。
- [ ] close 停止新 command、abort host tasks、拒绝 responses、释放 persistent roots。
- [ ] loom 或等价模型测试 mailbox/waker/lost wakeup/close race。

### M10.4 `tachyon-async-runtime`

- [ ] 创建独立公开 adapter crate，features 为 additive `futures`、`smol`、`tokio`，默认只启用
  `futures`；三者可同时编译，不使用 mutually-exclusive `compile_error!`。
- [ ] 定义不依赖 `async_trait` 的 generic adapter contract，以及统一 `SpawnedIsolate`：包含
  `IsolateHandle`、可 await task completion、abort/close 和结构化 join error。
- [ ] futures adapter 同时支持 `LocalPool` deterministic path 与 thread-pool path；不把 local-only
  future 误标为 Send，也不要求 core isolate 实现 Sync。
- [ ] smol adapter 显式接收 executor/spawn context 或调用明确的 `smol::spawn` path，统一 detach/drop
  语义，不能在 task handle drop 时静默泄漏 isolate。
- [ ] Tokio adapter 支持 current handle 与显式 `tokio::runtime::Handle` constructor；current-handle
  缺失返回错误，不隐式创建隐藏 runtime/thread。
- [ ] 三个 adapter 共用同一 contract test macro/函数，不复制测试逻辑：execute、host future、wake、
  fairness、backpressure、cancel、abort、close、pending drop、multi-isolate 和 shutdown。
- [ ] Tokio multi-thread 与 futures thread-pool 验证 `IsolateRunner` 可跨 worker poll；smol 覆盖其
  executor migration 能力。JS job/microtask trace 在三套 runtime 上必须完全一致。
- [ ] 所有异步测试使用 deterministic barrier/channel 和有诊断的 timeout；禁止依赖 sleep 排序造成
  flaky result。lost-wakeup 和 close race 另由 loom/core model test 覆盖。
- [ ] runtime adapter benchmark 使用相同 host-future burst、quantum CPU task、mailbox saturation 和
  graceful shutdown workload，报告 throughput、wake count、allocation、P50/P99 与 runtime overhead。
- [ ] rustdoc/example 分别展示 futures、smol、Tokio；adapter 未启用对应 feature 时不泄漏依赖类型。

验收：Promise/async/generator test262 分类通过阶段目标；三个单 feature、all-features 和
core-no-adapter dependency gate 全部通过；同一 async semantic trace 在三个 adapter 上一致；
迁移/取消/关闭压力测试无泄漏和 lost wakeup，runtime adapter overhead 有独立 baseline。

## 18. M11: Host Async、Loader 与 FFI Readiness

### M11.1 Typed async function

- [ ] async registration 在 isolate 中完成 `FromJsOwned`，future 只持有 owned `Send + 'static`。
- [ ] future completion 发送 erased owned result，`IntoJs` 只在所属 isolate 执行。
- [ ] pending table 同时拥有 Promise root、abort handle、memory accounting 和 tracing span。
- [ ] cancellation policy 固化：内部终止、Promise rejection 或 per-call policy；决定写回 DESIGN。
- [ ] late completion、double completion、close-after-complete 和 dropped receiver 都有确定结果。
- [ ] backpressure 同时限制 command queue 与 max pending host operations。

### M11.2 Async module loader 与 synthetic modules

- [ ] resolve/load future 与 host function 共用 completion infrastructure，但使用独立 request ID。
- [ ] cyclic load、并发同 URL 去重、失败缓存和 retry policy 明确。
- [ ] synthetic module export 通过 typed Rust values 初始化，支持 async evaluate。
- [ ] loader cancellation 不留下半链接 module graph 或永久 root。

### M11.3 不发布的 FFI smoke adapter

- [ ] 定义 opaque engine/isolate/module/persistent/async-token handle，不暴露 Rust layout。
- [ ] 所有 entrypoint 返回 status/error handle；panic 直接 abort，禁止 `catch_unwind`。
- [ ] 外部语言异常必须由调用方 adapter 在进入 ABI 前转换为 status，禁止异常跨 ABI。
- [ ] native callback 使用 function pointer + userdata + drop callback。
- [ ] async token 可从任意线程 resolve/reject；double/late resolve 返回错误。
- [ ] manual poll 返回 ready/pending/deadline，并通过 wake callback 集成外部 event loop。
- [ ] string/buffer API 使用 pointer+length 只在调用期借用，owned input 有明确 transfer/drop。
- [ ] C smoke program 完成 compile/eval/native callback/async completion/module load/cleanup。
- [ ] adapter 不发布、不承诺符号稳定；其作用是验证 core 模型没有 Rust-only 隐藏假设。

### M11.4 Typed Debugger 与 CDP Inspector

- [ ] `tachyon` 暴露 `DebuggerHandle: Clone + Send + Sync`、typed command/event、session ID 和
  structured error；handle 只经 actor mailbox 操作 isolate，不暴露 VM `Value`/GC pointer。
- [ ] debug command 使用独立有界高优先级队列，保证普通 command 饱和时仍可 pause/terminate；
  event queue 定义 backpressure、重要事件不可丢、console/profile 可聚合或显式报 overflow。
- [ ] 脚本生命周期事件包含 stable script ID、URL、source/source-map、hash、line/column offset；
  URL/regex/location breakpoint 支持 pending resolution、condition、enable/disable 和 remove。
- [ ] 实现 pause/resume、step into/over/out、break-on-start、caught/uncaught/all exception 和
  blackbox；step plan 组合 source site、frame depth、tail call 与 async task identity。
- [ ] paused command pump 不运行普通 JS job/microtask；只允许 inspect/release/evaluate/snapshot/
  resume/terminate。race tests 固化 pause 与 close、cancel、host completion、GC 的优先级。
- [ ] call frame/scope API 映射 register、environment、module/global binding，保留 TDZ、with、eval、
  optimized-out/unavailable 和 getter/proxy 的 observable 边界。
- [ ] `evaluateOnCallFrame` 使用独立 debugger fiber 和 lexical environment，限制 fuel、wall timeout、
  allocation、host call 与 side effect；syntax/throw/timeout/termination 后恢复原 pause generation。
- [ ] object preview 默认不调用 getter/toString/proxy trap；显式 property/evaluate 请求才允许按 policy
  执行 observable code，并返回 thrown/aborted/side-effect-rejected outcome。
- [ ] `RemoteObjectId` 包含 session/pause generation 与 slot generation；每个 `ObjectGroup` 限制 root
  count、preview depth/property count、bytes 和 lifetime，支持单对象/group release。
- [ ] resume、session disconnect、isolate close、snapshot failure 和 quota failure 强制释放 group roots；
  forced-GC/root-table tests 验证 stale ID 无 UAF、重复 release 幂等、循环对象不会永久存活。
- [ ] async stack 记录 Promise/fiber scheduling parent 与 source site，按 typed resource limit 截断并
  标记 truncation；不能让长期 Promise chain 无界保留完整 heap graph。
- [ ] diagnostic heap snapshot 在 GC safepoint 枚举 roots/nodes/edges/external bytes/native types，使用
  有界 chunk 流式输出；取消、慢 consumer、memory pressure 和 isolate close 可中止且不泄漏 root。
- [ ] `tachyon-inspector` 逐项映射 CDP Debugger/Runtime/Console/Profiler/HeapProfiler 支持矩阵；未知或
  未实现 method 返回 protocol error，golden fixtures 验证 request ID、event order 和 JSON schema。
- [ ] CDP session 仅消费/产生 bytes/typed frames，不依赖 socket 或 executor；server tool 分别用
  futures/smol/Tokio integration test 验证 WebSocket/stdio transport、fragment、disconnect/backpressure。
- [ ] debugger differential scenarios 在 Tachyon/Escargot/Chrome 可比子集上验证 location、scope、step
  和 exception order；protocol parser、state sequence、source-map 和 remote ID 建立 property/state-model tests。

验收：Rust host async 在至少三种 executor 下工作；C smoke 在 ASan/UBSan 下无 leak/UAF；删除
FFI adapter 不影响 core crate。typed debugger 与 CDP golden/integration tests 全通过，paused/close/
GC stress 无 root leak，CDP/transport crate 不反向污染 VM executor 依赖。

## 19. M12: Test262 全面收敛

### M12.1 通过率口径

- [ ] `test262_config.toml` 固定 commit 和 release target feature manifest。
- [ ] 通过率定义为 `passed / applicable_total`。
- [ ] ignored、timeout、panic、crash、harness failure 均不算 passed，不能从分母删除。
- [ ] 固定 commit 中已标准化的 ECMA-262 与 ECMA-402 测试全部 applicable，包括 Intl 和 `$262`
  host harness 所需能力；不能通过 Cargo feature、平台或 host-specific 标签移出分母。
- [ ] 只有尚未进入 release target 的 proposal 可标为 non-applicable；每项记录 TC39 状态、理由
  和 owner。proposal 进入目标版本后立即纳入主通过率。
- [ ] proposal-signals 虽不进入已标准化 ECMA-262/402 的 98% 分母，但因默认启用而有独立 100%
  gate；不能以 Stage 1、Cargo feature 或 test262 尚未 upstream 为理由跳过 pinned suite。
- [ ] 同一 test 的 strict/non-strict variant 按 runner 的独立 execution 计数并保留路径。
- [ ] 目标：applicable total 通过率 >= 98%，panic/crash = 0，非 allowlist timeout = 0。

### M12.2 `$262` Host API

- [ ] `global`、`evalScript`、`createRealm`、`detachArrayBuffer`、`gc`。
- [ ] `agent.start/broadcast/getReport/sleep/monotonicNow` 和 SharedArrayBuffer coordination。
- [ ] realm 隔离、cross-realm prototype/error identity。
- [ ] async test 的 `$DONE`、promise rejection 和 timeout。
- [ ] module resolution fixture、import attributes 和 dynamic import。

### M12.3 收敛循环

- [ ] 每次全量运行生成按根因聚类：parser、lowering、runtime semantics、builtin、module、async、GC。
- [ ] 优先修复影响多个 feature 的抽象操作，不逐个测试打补丁。
- [ ] 每个修复先加入最小专项 regression，再运行 test262 原目录。
- [ ] 每提升 1 个百分点保存 baseline；本地 compare 不允许无解释 regression。
- [ ] 达到 90% 后每日全量；达到 95% 后每个语义工作包跑受影响目录并定期跑全量。
- [ ] 98% 时审计剩余失败，确认没有 crash、silent wrong result 或 harness classification bug。

阶段参考门槛不是 release 豁免：

| Gate | 预期能力 | 最低适用通过率 |
| --- | --- | --- |
| C1 | primitives、control、function、basic object | 35% |
| C2 | environment、class、descriptor、core builtins | 65% |
| C3 | module、Proxy、TypedArray、collections、RegExp | 85% |
| C4 | Promise、async、weak/agent、完整 host harness | 95% |
| Release | 剩余长尾语义收敛 | 98% |

百分比只用于观察整体进度；每个 gate 仍要求对应 feature suite 达标，不能用大量简单测试掩盖
某个核心 feature 完全缺失。

## 20. M13: Escargot-class 非 JIT 性能基础

所有项目必须有独立微基准和 suite 影响数据。优化顺序先消除算法级浪费，再调整 layout/dispatch。

### M13.1 Bytecode 与 dispatch

- [x] 完成 Rust/C interpreter cost audit，不把 RAII 或大 enum 当作未经测量的根因。Apple AArch64
  release type/ABI 证据：`Value=8B`、`DecodedInstruction=16B`、`FunctionExecutable=16B`、
  `FunctionObject=56B`、`CallSite=72B`、`ResolvedCallTarget=56B`、`Frame=104B`、
  `ExecutionError/Result<_, ExecutionError>=40B`；LLVM 已为 opcode match 生成 jump table，N=8 也没有
  复制八份 dispatcher。当前首要嫌疑按顺序是 callable logical-GC-ref 全验证、通用 call 参数与 40B
  Result ABI、完整 Frame push/pop、每 opcode frame/pc/register reload，dispatcher I-cache 属次级问题。
  dirty optimization baseline 已从原始 `8.049 ms` 降至稳定约 `5.691 ms`，rquickjs 约 `1.572 ms`；
  affinity/governor gate 不可用，因此这些数字只用于方向选择，不是 release parity 证据。
- [ ] 建立私有 `VerifiedExecutionCursor` kernel。进入时一次缓存 bytecode start/end、local `pc`、
  register-window base/end、active frame index 与 local budget；hot opcode 不调用通用 `&mut Isolate` 方法，
  不分配、不 GC、不扩容 frame/register storage。普通 load/move、primitive arithmetic/comparison、
  jump/branch 在 cursor 内完成，只有 slow/safepoint/exit 才一次 flush。
- [ ] verified fast decoder/register access 只在 cursor 小模块内使用 audited unsafe；每个 unsafe 原地列出
  immutable backing、verified operand/jump、window length 和 no-reallocation lifetime 不变量。debug/test
  checked mirror 对 compact/normal/wide、最小/最大 register、错误 jump 与全部 opcode 对拍；Miri/sanitizer
  专项不得把 fast decoder 暴露成可接收未验证 module 的公开 API。
- [ ] 把 kernel 内 control/fault ABI 缩为 pointer-sized closed sum 或 out-of-band cold fault；普通成功 opcode
  不传播 40-byte `ExecutionError` sret。只有离开 kernel 的 host/public boundary 才物化完整
  `ExecutionError`；ECMAScript throw 仍是显式 `Value` completion，不使用 Rust panic/unwind。
- [ ] 定义 trusted internal `Value` capability：FFI/host ingress、GC resume 和 debug verifier 做完整
  owner/span/liveness/alignment/layout 检查；active rooted register/constant/frame 在 no-GC kernel 中只做
  callable descriptor/class guard。任何 GC、host callback 或外部 value publication 都终止 capability。
  immediate、普通 object 和 stale/wrong-isolate handle 的边界测试必须证明 fast path 不接受伪造 callable。
- [ ] ordinary bytecode call 建立专用 activation transition：call feedback 命中后直接取得
  `code/function/environment/layout`，容量足够时无 fallible reserve；需要 growth、environment、
  constructor、bound/native continuation 时 flush 到 slow path。独立量化 direct-known-target、完整
  callable validation、frame push/pop 和 bytecode dispatch 各自成本。
- [ ] 将 104-byte `Frame` 拆成固定紧凑 hot activation 与按需 cold side state；普通空函数不得复制
  constructor/bound/handler/native continuation 冷字段，也不得为 cold state 每 call 做 Box allocation。
  layout test 固定 `size_of/align_of`，并覆盖 ordinary/closure/constructor/try-catch/debugger stack walk。
- [ ] 所有 interpreter educated guesses 集中在 `tachyon_vm::tuning`：batch candidates、初始 frame/register
  reserve、call feedback width、poll interval 和 hot/cold thresholds。默认值必须注明 workload/架构证据，
  benchmark 可以覆盖但 SDK/FFI 不公开不稳定的内部 knob。
- [ ] 为高频语义提供专用 opcode：local/env/global slot、GetById/SetById、GetByIndex/SetByIndex、
  call/construct/native call、iterator、typeof identifier、strict/non-strict variants。
- [ ] compact opcode 优先覆盖 profile 中 90% 动态指令；normal/wide 保留大程序正确性。
- [ ] exception handler 使用 side table，正常 opcode 不逐条检查 try state。
- [ ] 比较稳定 Rust match、函数表 dispatch 和可维护的 threaded-dispatch 实验；任何 unsafe
  dispatch 必须在 x86_64/aarch64/riscv64 对拍和 sanitizer 后才可进入。
- [ ] 将 `execute_batch<const N: usize>` 收敛为 kernel 内轮询组，基准 N=1/2/4/8/16。完成 N 条后
  local cursor 继续存活，不把 batch group 当成 frame/pc flush 边界；普通 jump/branch 修改 local `pc`；
  call/return 通过 activation transition 刷新 code/register cursor；throw、await/yield/suspend、GC、
  interrupt/cancel 按各自 slow/safepoint contract 退出。
- [ ] hard fuel 按实际执行指令数扣减，quantum 小于 N 时使用短尾路径；测试 batch 提前 return、
  throw、jump loop 和 cancellation 时不多执行、不多扣 fuel、不改变 microtask/job ordering。
- [ ] 小型 macro 只统一单步 control propagation，不复制整份 opcode match。记录各 N 的 text size、
  I-cache/branch 数据与吞吐，防止手工 unroll 的代码膨胀抵消 dispatch 收益。
- [ ] 基于 opcode pair profile 选择少量显式 superinstruction，不生成组合爆炸的大宏。
- [ ] branch hint/layout 只依据 profile；cold slow path 使用 `#[cold]`/`#[inline(never)]` 隔离。
- [ ] opcode counters 为 opt-in feature，release 热路径编译时完全移除。
- [ ] 固定四类归因 microbench：纯 loop 无 call、direct-known bytecode call、仅 frame/window push-pop、完整
  callable resolution；另把同一 QuickJS checkout 以 `DIRECT_DISPATCH=0/1` 对拍。每次 kernel 变更记录
  wall time、instructions retired、branch miss、spill/load-store、text size；`call-loop` 不能单独代表整体 VM。

当前 implementation checkpoint（2026-07-19）：`6f9a01e` 的 `VerifiedInstructionDecoder` 已由
`&VerifiedBytecode` 安全构造，并在 unsafe instruction-start contract 下跳过 opcode/format/reserved/bounds
重验；compact/normal/wide/escape 及首尾 instruction 与 checked decoder 对拍。decoder backing 由
cursor epoch 一次捕获，operand count 改为 repr(u8)-indexed 70-entry data table，不再为这一步生成第二个
opcode jump table。VM 第一层 kernel 在每个
batch 入口一次检查 register window，local 保存 `pc`，load/move、numeric unary/binary/relational、
primitive strict-equal 与普通 branch 使用 unchecked verified register access；slow opcode 先 flush `pc`，
结束 cursor epoch，再进入 non-inlined 原有语义层并 rebind。N=1/2/4/8/16 的
call/throw/cross-code/continuation/GC 专项测试已通过；test-only checked dispatcher differential 覆盖全部
hot opcode、destination alias、numeric edge、branch PC 与 heap truthiness slow exit。该 checkpoint 仍会在
每 N 条 flush，callable trusted guard、紧凑 control ABI、call IC 与 hot/cold frame 尚未完成，因此上面的
目标项保持未勾选。

`016369b` 的 rebind-time environment cursor 实验虽通过完整 workspace tests、clippy、fmt、architecture
gate 和 raw-pointer 边界测试，但 closure median 从 `732.741 ms` 回退到 `760.046 ms`，独立复测为
`787.617 ms`，已由 `f22f252` 完整 revert。根因是每次 call/return/batch rebind 重新做 environment chain
typed validation，成本高于省下的 slot loads。后续 direct environment slot 必须随首次 checked call target/
activation 缓存 trusted identity，并在 GC/host/topology change 统一失效；不得恢复该方案或保留无收益 unsafe。

clean HEAD `b7487ee` Apple M5 release 15-sample median：call-loop `4.094 ms`、nested-loop
`17.858 ms`、closure `732.741 ms`；同轮 rquickjs/QuickJS 分别为 `1.614 ms`、`6.631 ms`、
`189.924 ms`，Tachyon 慢 `2.54x/2.69x/3.86x`。相对本轮前 Tachyon call-loop `5.691 ms` 再降低
`28.1%`，相对旧 nested `30.506 ms` 降低 `41.5%`，证明 trusted decode/local cursor 是正确方向；closure
差距明确把下一优先级指向 environment direct-slot kernel path 与 ordinary call target/frame，而不是先做
computed goto。macOS affinity/governor probe 不可用且部分 background precheck invalid，数字只用于同机
方向选择，不是 release parity evidence。

### M13.2 Compiler 优化与 register pressure

- [ ] CFG 可达性、dead block elimination、规范安全 constant folding、copy propagation。
- [ ] peephole 合并 load/move/branch、compare+branch、property key materialization。
- [ ] linear-scan/free-list register reuse，减少 frame/register file；生成 safepoint liveness map。
- [ ] lexical/global binding 在无 eval/with 时 lowering 为直接 slot opcode。
- [ ] literal template/stencil 避免每次执行重复构建 property metadata。
- [ ] inner function eager/lazy bytecode 策略用 cold-start 与 steady-state 数据决定；若加入 lazy
  compilation，只保留 owned stencil，不保留 Oxc AST，并更新 DESIGN。
- [x] strict proper tail call substrate：编译器传播 direct/method/conditional/logical/coalesce/comma
  tail position，VM 在无 handler 保护的 bytecode target 上原位复用 frame，并保留 native/bound/Proxy、
  `try/catch`、`try/finally` 与 `arguments` source 的回退路径；N=1/2/4/8/16、100,000 层和 forced-major
  已覆盖。完整 debugger stack-frame elision、tagged-template/labeled/with 前端语法和 arguments snapshot
  仍是后续 ECMAScript/frontend 工作，不把本条扩展为已完成的全部 Test262 TCO 目录。

### M13.3 Shape 与 inline cache

- [ ] shape transition table 共享相同属性添加路径，shape/version ID 可快速比较。
- [ ] GetById 单态 cache：receiver shape + slot；命中只做 shape compare 和 indexed load。
- [ ] 小型多态 cache 保存有限 shape/slot pairs；超过阈值进入 megamorphic/global cache 或 slow path。
- [ ] prototype property cache 保存短 shape chain/watchpoint，prototype mutation 精确失效。
- [ ] SetById cache 同时支持 existing slot 和 shape-before -> shape-after transition。
- [ ] accessor、Proxy、dictionary object、indexed prototype 进入明确 slow path，不污染普通 cache。
- [ ] global lexical/property access 保存 cell/shape guard，避免每次 hash lookup。
- [ ] call cache 保存 callable kind、bytecode function/native descriptor 和 arity adaptation fast path。
- [ ] feedback vector isolate-local；共享 `CompiledModule` 不被运行时修改。
- [ ] 若 profile 证明收益，再添加 isolate-local COW quickening；未证明前不修改 bytecode。

### M13.4 Object/array layout

- [ ] benchmark 8/16-byte header、inline property slots 数量和 out-of-line storage break-even。
- [ ] object literal 使用预计算 shape/template，一次分配或最少 transition。
- [ ] packed/holey/dictionary 元素切换基于 density/length 阈值，阈值由 corpus profile 固定。
- [ ] 可选 SMI/double element kind 只有在 conversion/transition 成本净收益后加入。
- [ ] Array push/pop/shift/unshift、slice、concat、iterator 对 packed array 使用专用 fast path。
- [ ] prototype 出现 indexed property、non-writable length 或 exotic receiver 时可靠 bailout。
- [ ] enumeration cache 绑定 shape/prototype version，Object.keys/for-in 避免重复排序和去重。

### M13.5 String 与 atom

- [x] Latin-1 与 UTF-16 表示，ASCII/Latin-1 路径避免无条件扩宽。
- [ ] short inline string 的容量/layout 用 object-size 与 benchmark 决定。
- [ ] rope concat O(1)，记录 depth/length；按 depth、random access 或 external boundary flatten。
- [ ] substring/slice 对大 parent 设置保留阈值，避免小 slice pin 住巨大 backing。
- [ ] atom hash lazy cache，identifier/property lookup 以 atom ID/hash 快速比较。
- [ ] StringBuilder 为 join/replace/JSON 提供单次容量规划和 8/16-bit specialization。
- [ ] regexp input 提供连续 view/flatten 策略，避免每个 match 重复 materialize。

### M13.6 GC 与 allocation

- [ ] Eden span bump allocation 内联到 object/array/closure fast path，slow path 才调用 collector。
- [x] size class/span-local free list 优化 old allocation；批量 sweep，不逐对象系统 allocator。
- [ ] young storage cap/cohort age 依据 allocation rate、survival、span occupancy、fragmentation 与 pause
  自适应，不只依据 heap size。
- [x] card scanning 跳过无 young ref 的 clean card，记录 false-positive rate。
- [ ] whole-span promotion/old allocation failure/full GC 路径可预测，避免重复 collection loop。
- [ ] external string/buffer、feedback、host pending value 全部进入 memory limit。
- [ ] 老年代增量三色 marking 在单 mutator safepoint slice 执行；baseline insertion barrier 在 marking
  active 时 shade 每个未标记的新 target，不查询 source color、不使用 atomic/lock/marker thread。
- [ ] incremental budget 以 bytes/edges/spans/work units 与 quantum 为主；time cap 只稀疏采样宿主 clock，
  记录 mutator utilization 和 P99 pause。

### M13.7 Builtin 与数字 fast path

- [x] int32 arithmetic overflow 后退到 double，NaN canonicalization 只在产生 JS Number 时执行。
- [ ] equality/relational 对 number/string/same-object 提供短 fast path，conversion 进入 slow operation。
- [ ] Math 常用函数、Number parsing、String char access、Array push/pop、Map/Set lookup 专门化。
- [ ] JSON parse/stringify 使用 iterative stack、fast string builder 和 cycle detection。
- [ ] RegExp literal 缓存 compiled program，但 match state isolate-local。
- [ ] Promise then/await 内部 reaction 使用专用对象/layout，减少普通 property protocol 开销。
- [ ] typed array bounds/type check 合并，detached/shared 情况可靠 bailout。

### M13.8 Hash table 与缓存治理

- [ ] atom、shape transition、dictionary property、Map/Set 分别选择符合 key/lifetime 的 hash table。
- [ ] hash seed 可由 host entropy 提供，test/benchmark 可固定；生产默认防碰撞攻击。
- [ ] 所有 cache 有容量、失效和内存统计；不能以无限增长换 benchmark 成绩。
- [ ] code/atom cache 的 engine-global limit 与 isolate accounting 分离。
- [ ] benchmark 同时报告 warm cache 和 cold cache，防止只优化重复执行。

### M13.9 Collection capacity 与 realloc 治理

- [ ] 建立 collection inventory，至少覆盖 bytecode words/constants/source map、HIR/scope/binding、
  fiber frame/register/handler/completion、temporary roots、GC gray/card/sweep、object property/elements、
  shape transition、IC feedback、atom/module tables、Promise jobs、Signal graph/worklists、host task/mailbox
  和 clone worklist。
- [ ] 可精确推导的容量全部从 compiler/layout metadata 传递，使用 checked multiplication 和
  `try_reserve_exact`；不得在 VM 运行时重新扫描 bytecode 猜 register/handler 数量。
- [ ] bounded-small collection 比较 `Vec`、`SmallVec`/`ArrayVec`、fixed array 的 object size、clone
  cost、stack pressure 和 cache miss；inline capacity 取版本化 corpus 分布与 layout Pareto 点。
- [ ] reusable buffer 使用 capped high-water mark；记录上次峰值、衰减/idle 回收和 memory-pressure
  行为，禁止每轮 `shrink_to_fit`，也禁止历史尖峰永久锁住大容量。
- [ ] persistent handle、host task、module ID 等稳定索引表使用 generation slab/free list；FIFO
  使用 `VecDeque`，避免 `Vec::remove(0)`/中间搬移和 tombstone 无界增长。
- [ ] 不可信 source/JS/serialized/FFI length 先执行语义上限、`usize` overflow、element-size 和配额
  检查，再渐进 `try_reserve`；allocation failure 转为结构化 resource error，不 abort 进程。
- [ ] `capacity-stats` 在 test262、V8 scripts 和 host async 压测中输出每个 subsystem 的 growth
  event、peak slack bytes 与 retain ratio；steady-state interpreter/call/property/array fast path
  的 realloc event 必须为零。
- [ ] 每个默认 hint 的提交附带 histogram/before-after 数据，并同时报告吞吐、RSS、peak heap 和
  cold-start；删除只降低 realloc 但造成不可接受 capacity slack 的 guess。
- [ ] 所有默认 capacity/high-water/decay/inline threshold 只定义在所属 crate 的
  `tuning::capacity`；benchmark 显式实例化候选值，选定后同步更新 `TUNING.md` evidence metadata。

### M13.10 RegExp performance 与资源治理

- [ ] benchmark 分离 literal creation、dynamic compile、cache hit/miss、test-only、exec captures、named
  groups/indices、global/sticky iteration、replace/split 和 RegExp.escape，不能只测 `/hello/i.test()`。
- [ ] 输入矩阵覆盖 short/large ASCII、Latin-1、BMP UTF-16、surrogate/unpaired surrogate、rope/substring；
  记录 flatten/widen/copy bytes 与 scratch growth，steady-state borrowed contiguous input不得分配。
- [ ] cache benchmark 比较 entry/byte cap 与 eviction 策略，报告 compile latency、hit rate、retained
  source/program bytes 和 engine-global RSS；cache 不能无限增长。
- [ ] matcher benchmark 比较 regress classical backend、可用 Pike/linear path 与 prefix/first-set hint；
  任何 backend selection 必须保持 capture/backreference/lookaround 与 leftmost-first 语义。
- [ ] checkpoint interval 候选值同时测普通吞吐和 cancel latency；`max_regex_steps` 资源测试验证
  catastrophic backtracking 无法长期占用 executor，且不同 N 不改变正常 match 结果。
- [ ] match result/capture buffer 按 compiled count exact reserve；test-only fast path验证无 JS array、groups、
  indices allocation，并确保 backreference 内部 capture 状态仍正确。
- [ ] 使用 Boa regexp microbench、Boa V8 regexp suite、test262 regression corpus 和额外 ReDoS corpus；
  报告 Tachyon/Boa/QuickJS/Escargot 的 compile、execute、memory 和 interruption 数据。

### M13.11 Debugger 与诊断开销

- [ ] 分离测量 no-debugger build、detached、attached-no-breakpoint、dense breakpoint、single-step 和
  pause/resume；报告 JS throughput、branch/I-cache、text size、pause latency、allocations 和 RSS。
- [ ] detached 相对同构建 no-debugger 基准的 geomean 回退预算由固定机器证据决定；任何 per-opcode
  trait call、JSON work、socket poll 或 remote-object bookkeeping 出现在 detached profile 都阻止合并。
- [ ] 比较 breakpoint bitmap/sorted site table、source-site density 与 attach redispatch 成本；默认容量和
  bitmap/chunk threshold 只从 `tuning::debugger` 读取，并同步 `TUNING.md` evidence。
- [ ] scope/object preview benchmark 覆盖深 frame、large object、accessor/proxy 和 truncated output；
  配额命中必须保持有界 latency/memory，不得为了 benchmark 绕过 observable-semantics policy。
- [ ] heap snapshot 记录 nodes/edges per second、chunk backpressure、peak scratch、GC pause 和取消延迟；
  snapshot buffer 使用受限 high-water policy，慢 consumer 不允许无界积压。
- [ ] async stack retention benchmark 覆盖长 Promise chain，验证深度/字节 hard limit 后 heap plateau。

### M13.12 Signals

- [ ] benchmark matrix 覆盖 State create/get/same-set/changed-set、Computed cold/warm/throw、Watcher
  watch/unwatch/notify、chain/diamond/fanout、dynamic dependency churn 和 introspection。
- [ ] graph size 覆盖 1/10/1k/100k nodes，degree/depth 使用 proposal tests 与 framework fixtures
  的版本化 histogram；报告 throughput、P50/P99、alloc/realloc、node bytes、GC scan/pause 和 RSS。
- [ ] 同一语义 trace 对比 pinned reference polyfill；性能对比 polyfill 与可比 Floem native primitive，
  但不得通过删除 subclass/callback ordering/introspection/GC semantics 获得虚假优势。
- [ ] inline source/sink capacity 候选同时比较 node size、spill rate、cache miss 与 full-GC trace cost；
  传播 worklist/双 buffer/edge compaction 候选比较 steady-state realloc、churn 和 retained slack。
- [ ] steady-state same-set、warm clean get、fixed-dependency recompute 和 bounded fanout set 不分配；
  初次 graph growth/observable Array introspection 的必要分配必须 checked、accounted、可归因。
- [ ] lazy diamond 保证每个 changed Computed 最多重算一次，unchanged checked chain 不重算下游；
  instrumentation 记录 callback count/ordering，防止吞吐提升隐藏 glitch 或重复计算。
- [ ] GC benchmark 覆盖 unobserved computed、active watcher、unwatch、unreachable cycle、whole-span
  promotion 与 watched graph churn；堆在 root release/full GC 后回到统计容差内。
- [ ] debugger graph inspection detached 时零工作；paused inspection 有 depth/node/byte quota 并测量
  snapshot latency，不能触发 lazy Computed callback。

验收：pinned proposal suite 与 differential semantic trace 无 regression；所有 knob 有
`tuning::signals` evidence；默认开启 Signals 后整体 Boa parity gate 仍成立，Signal microbench 无
无界 graph/queue/capacity 增长。

验收：每一小节都有 before/after 报告；删除无收益实验。最终批准 suite 达到 M14 性能门槛，
同时 test262 无 regression、Miri/sanitizer 仍通过。

## 21. M14: Benchmark Parity 与 Release Hardening

### M14.1 公平构建矩阵

- [ ] 固定同一机器、CPU governor、OS、compiler versions 和 benchmark corpus commit。
- [ ] Rust 引擎使用同一 rustc、`--release`、LTO/codegen-units 和 target-cpu 规则。
- [ ] QuickJS/Escargot 使用官方 release flags；记录其 GC/JIT/feature 配置，Tachyon 不启用 JIT。
- [ ] Boa、QuickJS、Escargot 与 Tachyon 都使用相同脚本、iteration 和 process isolation。
- [ ] cold start 每 sample 新进程；steady state 在同 isolate 内预热后测量；两者不混合。
- [ ] 输出原始样本，summary 可从原始数据重建。

### M14.2 性能放行门槛

对批准的 non-Intl 核心 suite：

- [ ] Tachyon steady-state throughput 几何平均 `>= 1.00x Boa`。
- [ ] 任一核心 benchmark `< 0.80x Boa` 必须修复或由设计文档记录明确语义/安全原因。
- [ ] parse+compile+first-execute median 不高于 Boa；若某项较慢，整体 cold-start geomean 仍需
  `>= 1.00x Boa` 且单项不得灾难性回退。
- [ ] peak heap/RSS、binary size 和 GC P99 单独报告，不允许吞吐提升隐藏无界内存增长。
- [ ] host sync call、host async completion、module loader 和 actor mailbox 有独立 Rust SDK 基准。
- [ ] futures/smol/Tokio adapter 使用同一 workload 单独报告，任何一个 adapter 的正确性失败都阻止
  release；性能差异不得通过改变 VM quantum/microtask semantics 掩盖。
- [ ] debugger detached overhead、pause P99、remote preview 与 snapshot throughput 有固定 baseline；
  inspector transport 不参与核心 JS suite 进程，避免网络调度污染引擎对比。
- [ ] Signals 默认开启的 binary/RSS/realm-init 与普通 JS suite 开销单独报告；不得用关闭 Signals 的
  build 达成 Boa parity。
- [ ] 与 QuickJS/Escargot 的差距作为持续目标报告；首发硬门槛仍是不得弱于 Boa。

门槛只在固定性能机器上做 release gate。开发期用稳定微基准的宽阈值发现大回退，不根据共享
runner 的噪声阻塞提交；自动化矩阵与调度策略属于 `POSTPLAN.md`。

### M14.3 安全与可靠性放行

- [ ] test262 applicable pass rate >= 98%，panic/crash = 0。
- [ ] pinned proposal-signals suite pass rate = 100%，默认、no-default-features、all-features 三种构建
  行为一致；proposal revision/API hash 与结果一同归档。
- [ ] Miri 覆盖 Value、bytecode verified decoder、GC pointer、scope/persistent 和 FFI smoke。
- [ ] ASan/UBSan 全套专项测试；TSan/loom 覆盖 actor/completion/waker；无 suppressions 隐藏项目 bug。
- [ ] 随机 GC、低 heap limit、低 fuel、取消和 close stress 连续运行无 root leak。
- [ ] 不同 executor、不同 worker migration 和多 isolate 并发压力测试通过。
- [ ] debugger stale ID、disconnect/resume/close race、evaluation timeout、snapshot cancel 与 root quota
  stress 通过 forced GC、sanitizer 和 protocol state-model tests；暂停会话结束后 root count 回到 baseline。
- [ ] cargo-deny/audit、license、依赖 feature、MSRV 和 reproducible lockfile 检查通过。

### M14.4 Rust SDK 放行

- [ ] `EngineBuilder`、`Isolate`、`IsolateHandle`、module loader、extension、native function/class、
  typed conversion、persistent、buffer transfer 和 cancellation 有稳定 rustdoc。
- [ ] 示例覆盖同步 embed、Tokio/smol/manual executor、custom module loader、native class、
  deterministic sandbox、resource limits 和 structured clone。
- [ ] `tachyon-async-runtime` 的 futures/smol/Tokio features、join/abort/drop 语义和版本兼容策略
  属于首发文档与 semver contract。
- [ ] typed debugger、CDP method matrix、remote-object lifetime/quota、evaluate side-effect policy 和
  snapshot 非恢复格式属于首发文档；`tachyon-inspector` 不要求特定 executor/transport。
- [ ] `Signal` 默认全局、锁定 proposal revision、升级策略与“不内置 Effect scheduler”写入首发文档；
  SDK 不提供关闭标准 surface 的 convenience switch。
- [ ] 公开 API 不泄漏内部 crate layout、Oxc type、raw Value bits 或 GC pointer。
- [ ] semver policy、feature policy、panic policy、threading model 和 security assumptions 写入文档。
- [ ] `ffi-smoke` 继续通过，但 `tachyon-capi` 不作为首发 artifact。

## 22. Test262 与 Benchmark 持续节奏

| 阶段 | 每次提交 | 每日/夜间 | Milestone gate |
| --- | --- | --- | --- |
| M0-M3 | unit + fixtures | test262 smoke、microbench | 全量 metadata scan |
| M4-M7 | affected test262 dirs、forced GC | 更大 language 子集、sanitizer | 分类 baseline + host bench |
| M8-M11 | affected dirs + module/async/signals/debugger | 全量 test262、Signals、V8、CDP golden | C3/C4 + Signals/debugger gate |
| M12 | affected dirs | 每日全量、结果聚类 | >=98% |
| M13-M14 | correctness + touched bench | 全量 test262 + fixed-machine suite | Boa parity + release gate |

任何 performance PR 先跑正确性，任何 conformance PR 检查受影响微基准。不能建立“正确版本”
和“优化版本”两套长期分支。

## 23. Stage Gates

提交按每个工作包的可审查边界切分，不把本计划绑定到固定提交序列。以下 Stage 是执行和验收
边界；进入下一 Stage 前必须满足前一 Stage 的 gate。

| Stage | 范围 | 进入下一 Stage 的 gate |
| --- | --- | --- |
| S0 Foundation | M0-M1：workspace、质量门、harness、Value/bytecode 契约 | architecture gate、test262/benchmark skeleton、Value/decoder 边界测试 |
| S1 First Engine Slice | M2-M3：Oxc、owned HIR、verified bytecode、fiber interpreter | source 到 `1 + 2` 的完整内存内链路，未引入 host I/O |
| S2 Language/Heap Core | M4-M8：GC、object/function、host SDK、builtins/modules/Signals | forced minor/major、stable offsets、feature suite baseline、Signals 100% pinned suite |
| S3 Async/Integration | M9-M11：incremental major、Promise、actor、host async、inspector | executor contract matrix、FFI smoke、debugger/CDP gate |
| S4 Conformance | M12：test262 收敛 | applicable ECMA-262/402 >=98%，无 panic/crash/hidden ignore |
| S5 Performance/Release | M13-M14：非 JIT 优化和放行 | Boa parity、capacity audit、安全/SDK release evidence |

S1 完成时必须能运行：

```text
source "1 + 2"
  -> Oxc parse/semantic
  -> owned HIR
  -> verified CompiledModule
  -> fiber interpreter
  -> Value::int32(3)
```

这是第一次架构证伪点。如果此切片需要 GC 依赖 VM、bytecode 依赖 Oxc lifetime、Value 访问
active isolate 才能分类，或任何 engine crate 为加载 source 而访问 host filesystem，立即停止
扩功能并修正 crate boundary。

## 24. 明确不做

- 不为了看起来模块化而拆 `tachyon-object`、`tachyon-promise`、`tachyon-builtins`。
- 不创建 `tachyon-core`/`tachyon-common` 收容跨层类型。
- 不把 Oxc AST 当 bytecode compiler 的长期 IR。
- 不把 QuickJS 的手工 `dup/free` handle 模型翻译成 Rust API。
- 不允许 live extension unload 破坏 function/class/persistent 生命周期。
- 不在普通 property/opcode 热路径查询动态 plugin registry。
- 不修改共享 immutable bytecode 来启停断点，不在 VM poll 内运行 CDP JSON 或 socket transport。
- 不把 Signals 放入默认关闭 experimental feature，不把 framework Effect scheduler 塞进 proposal core。
- 不用 ignore list、unsupported 或 timeout 从 test262 分母中隐藏失败。
- 不只看单个 Richards/Splay 分数宣称性能达标。
- 不在 profile 证明前引入大面积 unsafe dispatch、宏生成 opcode 或 COW quickening。
- 不在达到 Boa parity 前投入长期 PGO、JIT 或并发 GC。

## 25. 计划完成判定

RAB vertical slice: `ArrayBuffer(length, {maxByteLength})`, branded `resize`, and
current-length validation for TypedArray snapshots are implemented; full OOB,
length-tracking views, DataView and transfer semantics remain follow-up work.

本计划完成需要同时提交以下证据：

- 固定 test262 commit、release target manifest、全量 JSON 和 >=98% summary。
- 固定性能机器的原始 benchmark samples、构建 metadata、Boa/QuickJS/Escargot comparison。
- Tachyon >=1.00x Boa 的核心 suite geomean，以及所有 <0.80x 单项的清零或设计说明。
- Miri/sanitizer/concurrency/resource stress 报告。
- Rust SDK examples 和 API docs 全部作为测试运行。
- FFI smoke adapter 的 C integration test，证明未来 ABI 是薄层。
- typed debugger/CDP contract、detached overhead、pause/root/snapshot stress 与 protocol state-model 报告。
- pinned proposal-signals 100% 结果、graph/GC stress 与默认开启性能报告。
- `DESIGN.md` 与最终 crate graph、ownership、GC、async、Signals、debugger 和 Host SDK 一致。

这些证据缺少任一项，都不能把“可执行 JavaScript”描述为项目完成。
