# Tachyon JS Design

## 1. 文档状态

本文档记录 Tachyon JS 的长期设计决策、当前设计方向和仍待验证的问题。
实现发生变化时，应同步更新本文档，避免代码与架构假设分离。

状态标记：

- **决定**：已经作为实现约束接受。
- **方向**：当前首选方案，允许在原型或基准结果出现后调整。
- **开放**：尚未决定，需要实验或进一步讨论。

## 2. 项目目标

Tachyon JS 是一个自主品牌、Rust-native、可嵌入的 ECMAScript engine。它实现语言语义与
engine execution，不在 core 内扮演应用 runtime。Escargot
是可合法参考的语言实现和性能基线，但 Tachyon JS 不追求 Escargot API、ABI、对象布局
或字节码兼容。

### 2.1 核心目标

- **决定**：使用 Rust 实现，遵循 Rust 的所有权、模块化和安全边界。
- **决定**：目标平台仅为 little-endian `amd64`、`aarch64` 和 `riscv64`。
- **决定**：使用 Oxc 作为 parser/lexer 前端，避免重复实现 ECMAScript 语法前端。
- **决定**：固定 release target 中已标准化的 ECMA-262 与 ECMA-402 test262 适用测试通过率
  最终不低于 98%；ignored、timeout、panic 和 crash 不计为通过。
- **决定**：性能不得弱于 Boa；Escargot、QuickJS 和 Boa 均作为持续基准对象。
- **决定**：生产构建统一 `panic=abort`，引擎、host adapter 与 FFI 不捕获或恢复 Rust panic。
  ECMAScript abrupt completion、host error 与 ABI status 都使用显式数据流，绝不借用 Rust unwind。
  性能基准中的引擎与 runner 可执行文件使用 release profile。Cargo 当前会忽略 test/bench
  harness profile 的 `panic` 配置，因此这两个 libtest 基础设施 profile 不伪装成 abort；其存在
  不授权引擎、adapter 或 FFI 使用 `catch_unwind`、`resume_unwind`、`C-unwind` 或任何恢复路径。
- **方向**：优先优化启动时间、解释器吞吐、内存占用和 P99 暂停时间，不引入JIT。
- **决定**：异步接口基于标准库 `Future`/`Waker`，不绑定特定 executor；Tokio、
  async-std、smol 等通过相同接口驱动。
- **决定**：日志、span 和运行时观测统一使用 Rust `tracing` 生态。
- **方向**：提供可安全跨线程调度的执行任务和针对主流 executor 的薄 adapter。
- **决定**：首版提供可嵌入的 typed debugger API 与 executor-neutral CDP adapter；debugger
  未连接时不能在每条 opcode 上产生动态分派或回调开销。
- **决定**：TC39 proposal-signals 的 `Signal` API 是默认安装的 VM 原生能力；不存在默认关闭的
  experimental Cargo feature 或 runtime switch，proposal revision 由 release manifest 固定。

### 2.2 非目标

- 不兼容 Escargot 的公开 API、C++ ABI 或内部字节码。
- 1.0 不实现 moving/compacting GC、并行或并发 marker；分代能力使用非移动 cohort spans。
- 第一阶段不以 wasm32 为编译目标。
- 第一阶段不提供可恢复 heap snapshot；debugger 的诊断型 heap snapshot 只用于分析，
  不构成序列化、恢复或跨版本兼容格式。
- 首个商业版本只提供 Rust crate：稳定 core SDK `tachyon` 与可选
  `tachyon-async-runtime`/`tachyon-inspector` adapter；不提供或承诺稳定 C ABI。
- 不允许多个线程同时修改同一个 JavaScript heap。
- 不把 Oxc AST 作为长期运行时表示或解释执行对象。

## 3. 总体架构

```text
Source Text
    |
    v
Oxc Parser + Semantic Analysis
    |
    v
Tachyon Lowering / Validation
    |
    v
Immutable Register Bytecode + Constant Pools
    |
    +--------------------------+
    |                          |
    v                          v
Isolate-local Feedback     Debug / Source Maps
    |
    v
Fiber Interpreter
    |
    +--> Object Model / Builtins / Precise GC
    |
    +--> Promise Jobs / Microtask Queue
    |
    +--> Rust Host Future Bridge
```

### 3.1 Oxc 与前端语义

前端行为以 Deno 为基线，支持 JavaScript、JSX、TypeScript、TSX、MJS/CJS 和 MTS/CTS
等 source/media type。实现直接依赖精确锁定版本的最小 Oxc crates，
`deno_ast-oxc-port` 仅作为 Deno 行为和 Oxc API 的参考，不作为 runtime 依赖。

编译管线为：

```text
Oxc parse JS/TS
    -> pre-transform semantic/scoping
    -> Oxc transform with Deno-compatible options
    -> post-transform JavaScript AST and scoping
    -> Tachyon validation/HIR/ScopePlan/BindingPlan
    -> register allocation and bytecode
    -> drop Oxc allocator
```

Oxc transformer 负责 TypeScript type erasure 和运行时 lowering、JSX/TSX、decorator 以及
必要的 TypeScript module syntax。transform target 固定为 ESNext，不降级 async/await、
class、optional chaining 等 Tachyon 应原生支持的 ECMAScript 功能。若 transformer 返回的
scoping 未保证反映转换后的 AST，则重新运行 `SemanticBuilder`。

JSX 的 classic/automatic/precompile、`@jsxImportSource`，TC39/legacy TypeScript decorator，
以及 TypeScript CommonJS syntax 均遵循 Deno 的 compile/transpile options 与 media-type
行为。core module runtime 仍维护自己的 ESM module record、live binding 和 host loader。

Tachyon 必须将 Oxc symbol/reference/scope ID 映射为自有稳定 ID。runtime storage plan
至少区分 register/frame slot、captured environment slot、module cell、global lexical、
global object property 和 dynamic lookup。`with`、direct eval、TDZ、Annex B 与 closure
capture 由 Tachyon lowering 实现，不能把 Oxc scoping 直接当作 runtime environment。

`CompiledModule` 保留完整原始源码 `Arc<str>`，用于诊断、stack trace 和 transform source
mapping。运行时不持有 Oxc AST、arena 引用或 Oxc ID。unresolved identifier 使用 module-owned
`scope_names: Arc<[Arc<str>]>` 的 verified index；共享 module 不保存 isolate-local `AtomId`。isolate
加载 code 时只解析/intern 一次 scope name，并把结果放入 isolate-local loaded-code entry。

ordinary function 的首个垂直切片把 declaration、simple identifier parameters、body statements 和
direct-call arguments 复制为 owned function stencils；entry function 固定为 `FunctionId(0)`，stencil
按稳定顺序映射到 `FunctionId(stencil + 1)`。顶层 function declaration 在 entry bytecode 开头统一
`CreateClosure`，使 source-order 之前的 direct call 命中 hoisted binding。每个 ordinary function 的
parameter registers 固定从 0 开始，module 内所有 functions 共享一个 immutable constant pool。
含 default initializer 的 parameter list 使用 activation-owned declarative slots 表达独立 parameter
environment：所有参数名在任何 initializer 前以 uninitialized state 建立，随后按源码顺序先求值当前
initializer、再初始化对应 slot。这样 self/later reference 产生 TDZ，prior reference 可见，并且具名
function expression 的 immutable self-binding 仍保持独立 slot；无 initializer 的 simple parameter list
继续保留 register/captured-slot 快路径，不为常见调用增加状态字节或动态查找。
function body 的 direct `FunctionDeclaration` 同样在 activation statement execution 前实例化并发布到其
BindingId-selected register/environment storage，因此声明前调用与多层 captured declaration 都成立。HIR
statement context 明确区分 `ScriptBody`/`ScriptNested`/`FunctionBody`/`FunctionNested`；两种 Body 允许 direct
declaration，两种 Function context 允许 return。nested block declaration/Annex B 继续结构化 unsupported，不能通过 source-order
创建 closure 假装 hoisting。
call lowering 必须先求值 callee，再预留连续 `(callee, args...)` window，随后按 source order 求值并
Move arguments，不能让临时 register 打断 VM verifier 的 call-window contract。function expression 与
captured direct declaration 已接入；named function expression self-binding、arrow、default/rest、
async 仍返回结构化 unsupported，不能静默降级为 ordinary non-capturing function。generator declaration
复制为独立 `FunctionKind::Generator`；普通 `yield` lowering 发出 verified `Yield(source, destination,
suspend_id)`，并为每个 function 从 owned HIR 一次精确估算 suspend-point capacity。immutable
`SuspendPoint` 记录真实 wide-aware instruction/resume offset、destination 和 finally completion depth。
delegated `yield*` 由 compiler 展开为显式 iterator protocol CFG，而不是把 delegated iteration 错当
普通 yield：verified `YieldDelegate(result, resume_base, suspend_id)` 将已验证的 delegate result object
原样交给 caller，并在相邻 `resume_base`/`resume_base + 1` 中接收 value 与 Normal/Return/Throw kind。

首个 `var` 垂直切片在 lowering 时递归收集当前 script/function 的 `VarDeclaredNames`，但不穿过独立
function stencil。ordinary function 在参数寄存器之后为去重后的非参数 `var` 预分配 frame register，
入口初始化为 `undefined`；声明位置只执行 initializer，因此 block/if/for/switch/try 内的 `var` 不会被
lexical checkpoint 截断。同名参数直接复用参数 register。script 使用 verified `DeclareScope` 在所有
statement 之前仅创建缺失的 global binding，随后 initializer 用 `StoreScope`，所以后续 source unit 的
无 initializer 重声明不会覆盖已有值。完整 GlobalDeclarationInstantiation 的 property descriptor、
global lexical collision、restricted property 与 Annex B 规则仍等待 global object/BindingPlan 闭合。

### 3.2 Workspace 与依赖边界

Tachyon 按可独立验证的不变量拆分 crate，而不是按 ECMAScript 功能数量拆分。首版核心
workspace 固定为：

| Crate | 职责 | 允许依赖 |
| --- | --- | --- |
| `tachyon-value` | `Value`、`RawHeapRef`、tag/immediate 编解码 | 标准库和位级测试依赖 |
| `tachyon-bytecode` | opcode、operand、常量池、`CompiledModule`、验证器和反汇编 | 不依赖 GC、VM 或 Oxc |
| `tachyon-gc` | logical span table、allocation、trace、roots 和 collection | `tachyon-value` |
| `tachyon-compiler` | Oxc parse/transform、HIR、binding plan、寄存器分配和字节码生成 | Oxc、`tachyon-bytecode` |
| `tachyon-vm` | isolate、fiber、解释器、对象、builtin、module 和 job queue | value、bytecode、GC |
| `tachyon` | `Engine`、配置、公开错误和稳定 Rust SDK facade | compiler、VM |
| `tachyon-async-runtime` | futures/smol/Tokio 的显式 spawn/join/drive adapter | `tachyon`、可选 runtime dependencies |
| `tachyon-inspector` | executor/transport-neutral CDP session adapter | `tachyon`、Serde/JSON |

工具放在 `tools/tachyon-cli`、`tools/test262-runner`，基准放在 `benches`。Serde 集成等可选
生态能力使用独立 adapter crate。首个商业版本发布 `tachyon`、`tachyon-async-runtime` 与
`tachyon-inspector`；其他内部 crate 设置 `publish = false`，其公开可见性只服务 workspace
依赖，不构成稳定 SDK 承诺。两个 adapter 都是 Rust-only convenience/portability layer，
不改变首版不发布 C ABI 的决定。WebSocket/HTTP server 只放在工具或宿主应用中，CDP crate
本身不绑定 Tokio、smol、futures executor 或具体网络栈。

`tachyon-value` 只负责表示和分类，不进行 heap allocation 或 dereference。它返回未经解引用的
`RawHeapRef`，由 `tachyon-gc` 在给定 isolate logical span table 中验证和解析。`tachyon-gc` 不依赖
`tachyon-vm`，也不认识 `Object`、`Promise` 等 JavaScript 类型；VM 通过 trace/type descriptor
向 GC 描述对象。字节码常量池保存编译期常量，不保存 isolate-local runtime `Value`。

对象、builtin、Promise、fiber、realm 和 module 首版保留为 `tachyon-vm` 内部模块，因为它们
共享 heap、completion 和 execution scope，不为追求 crate 数量制造循环依赖。不得创建无明确
不变量所有者的 `tachyon-core` 或 `tachyon-common` 杂物 crate。字符串和 derive macro 只有在
表示与安全契约经非移动分代 collector、forced-GC 和 barrier verifier 验证稳定后才允许独立拆分。

crate 内部同样按不变量所有权拆分，不允许 `lib.rs` 同时成为类型定义、Realm 安装、builtin
算法、对象 internal methods、解释器状态机和测试的共同写入热点。`lib.rs` 只负责模块声明、稳定
re-export 和少量 crate 根契约；Realm/builtin installer、property semantics、environment、completion、
module record 与各 builtin family 各有单一模块 owner，通过 `pub(super)` 窄接口接入 `Isolate`。
测试在模块超过约一千行前移入相邻 `tests.rs` 或 integration-test target，不和生产实现交错。
生产模块约一千行是强制重新审视职责的触发点，不是通过机械切片规避的硬指标；verified VM
dispatch kernel 可因代码布局与寄存器局部性保持集中，但初始化、slow path、数据结构和测试不能
借此例外继续堆入 dispatch 文件。结构性拆分必须保持公开 API 路径和运行语义，并在独立提交中
通过 workspace test、Clippy、architecture gate 与受影响的 forced-GC/dispatch-batch matrix。

### 3.3 Engine 与 Host Runtime 边界

engine core 明确定义为 `tachyon-value`、`tachyon-bytecode`、`tachyon-gc`、
`tachyon-compiler`、`tachyon-vm` 与 `tachyon`。这些 crate 的非测试代码只处理调用方传入的
内存数据、opaque identifiers、typed provider 与 `Future`/`Waker` contract，不主动发现或操作
宿主环境。它们不得使用 `std::fs`、`std::net`、`std::process`、ambient `std::env`、stdin/stdout、
线程创建/休眠、系统 DNS 或隐式当前目录；也不得通过第三方依赖间接加入同类 I/O 行为。

默认 GC storage 只使用 Rust allocator，不直接调用 mmap/VirtualAlloc、page protection 或其他
OS-specific virtual-memory API。instruction/fuel counter 在 engine 内维护，wall/monotonic
clock、deadline、timezone、entropy、locale/ICU data、source text、module bytes 和 tracing subscriber
均由宿主显式注入。默认构造器不能读取环境变量、用户目录、locale 文件或配置文件来改变语义。

compiler 入口接收 owned/borrowed source bytes、media type 与仅作诊断的 opaque source name；它不把
source name 当 path 打开。ESM core 只维护 module record/link/evaluate，`ModuleLoader` trait 接收
specifier/referrer 并返回 source/precompiled/synthetic module；文件、HTTP、package resolution 和
cache persistence 是宿主或 adapter 的实现。diagnostic snapshot/inspector 只产生 bounded chunks/
typed frames，不自行创建文件或 socket。持久 bytecode/cache、CLI REPL、test262 checkout、benchmark
corpus loading 与 WebSocket server 全部位于 tools/adapters，不得反向渗入 engine crate。

首个 ESM record/link 垂直切片只接收 host 已 canonicalize 的 owned specifier；core 对其执行精确字符串
identity 比较，不解释 path、URL、redirect 或 package rules。module record、local binding cell 和已解析
import alias 使用 append-only `NonZeroU32` ID，local export cell 在 graph traversal 前建立，因此 cyclic
import 能共享稳定 live cell 并保留 TDZ。link 使用显式 frame/SCC/rollback worklist 的 iterative Tarjan，
按 requested-module source order 确定性遍历；状态只沿 `Unlinked -> Linking -> Linked` 前进，失败时只将
本 transaction 中仍为 `Linking` 的 record 恢复为 `Unlinked`，已完成 dependency SCC 不回滚。
所有 graph/worklist growth 受 host limit 和 checked fallible reserve 约束。

该切片只解析 named local export、local-import re-export 和 named indirect export。star/namespace export、
ambiguous resolution、parser/compiler record 生成、declaration instantiation、dynamic import 与 async module
execution 保留给后续 M8.4/M10。同步 module lifecycle 已由 `Isolate` 唯一持有：host `ModuleLoader`
分别执行 canonical resolve/load，load 只返回 owned record 与 synthetic/precompiled body；core 使用显式
bounded worklist 装载完整依赖图，identity mismatch、missing dependency 或 link failure 都回滚本次新增
record/cell。同步 evaluate 按依赖 postorder 只执行一次并缓存 completion；标记 TLA 的 body 在 Promise/TLA
调度接入前返回 `AsyncEvaluationRequired`，不伪装成同步完成。

`ModuleGraph` 是 allocation-triggered GC 的精确 root，必须同时出现在完整 isolate trace 与每个 `VmRoots`
safepoint，live cell 和缓存的 evaluation completion 都参与 tracing。module/module-cell/edge hard limit 由
`IsolateConfig::with_module_limits` 独立配置；默认 edge-per-module educated guess 只定义在
`tuning::modules`，不得与 global binding 配额形成隐藏耦合。

这条边界用编译检查而非约定维持。每个 engine crate 开启 Clippy disallowed-types/methods，禁止
`std::fs::File/OpenOptions`、`std::fs::*` operations、`std::net::*`、`std::process::Command`、ambient
env/current-dir、thread spawn/sleep 与 stdio；测试 target 可局部 allow。`cargo xtask architecture check`
同时检查 crate layer、依赖 features、build scripts 与 source imports，并用 compile-fail fixture 证明
在 engine crate 加入文件/网络/进程调用会让 CI 失败。tools/adapters 的例外必须通过依赖方向隔离，
不能靠在 core 中散布 `cfg(feature = "runtime")`。

## 4. 线程与所有权模型

### 4.1 类型边界

这是 Tokio 集成和 GC 正确性的核心约束。

| 类型 | `Send` | `Sync` | 说明 |
| --- | --- | --- | --- |
| `Engine` | 是 | 是 | 编译配置、共享只读数据和代码缓存 |
| `CompiledModule` | 是 | 是 | 不可变字节码、常量和元数据 |
| `Isolate` | 是 | 否 | 独占 JavaScript heap，可在 safepoint 间迁移 |
| `SuspendedExecution` | 是 | 否 | 不含线程借用或 TLS 地址的暂停状态 |
| `IsolateHandle` | 是 | 是 | 仅通过 channel 向 isolate 发送命令 |
| `RunningScope<'vm>` | 否 | 否 | 活跃解释器和临时 GC roots 的借用作用域 |
| borrowed `Value`/对象引用 | 否 | 否 | 不得逃逸出 `RunningScope` |

**决定**：对外提供 `Send + Sync` 的 handle，而不是让可变 heap 本身实现 `Sync`。
同一时刻仅允许一个 `&mut Isolate` 执行字节码。这样 Tokio 可以调度和迁移整个执行任务，
但解释器热路径不需要锁或原子引用计数。

低层 `&mut Isolate` 驱动路径不创建 mailbox、`Waker` 或 shared interrupt state，必须保持零锁、
零 atomic。只有选择跨线程 actor/debugger/host-completion 能力时才允许在边界使用 atomic/channel；
这些同步原语不得进入 `Value`、GC metadata、Signal graph、register/frame 或普通 opcode/property path。

**决定**：运行时不得依赖持久 TLS。若某些平台优化需要线程局部缓存，它必须能在每次
进入 `poll` 或 safepoint 时重新绑定，且不能成为暂停状态的一部分。

### 4.2 异步驱动器

`VmDriver` 应基于 `std::future::Future` 和 `std::task::{Context, Poll, Waker}` 实现
`Future + Send`，core crate 不引用 Tokio 类型。多线程 executor 可以在不同 worker 上
轮流调用 `poll(&mut self, cx)`，但 Rust 的独占借用保证不会并发执行同一 isolate。

`poll` 的基本行为：

1. 若存在 runnable fiber 或 microtask，在配置的指令预算内运行。
2. 若预算耗尽，自唤醒并返回 `Poll::Pending`，避免长期占用 Tokio worker。
3. 若仅等待宿主 future，保存 waker 并返回 `Poll::Pending`。
4. 收到宿主完成事件后，在 isolate 内 resolve/reject 对应 Promise。
5. 顶层执行完成时返回 `Poll::Ready`。

`quantum` 必须非零。只有本次 poll 已执行至少一条指令或完成一个状态转换、且仍有 runnable work
时才允许 self-wake；等待 host completion、mailbox 或 debugger command 时注册 waker 后返回
`Pending`，不得周期轮询、空 `try_recv`、CAS/spin loop 或无进展 `wake -> poll`。

执行预算分为两个独立维度：

```rust,ignore
struct ExecutionBudget {
    hard_fuel: Option<u64>,
    quantum: u32,
}
```

`hard_fuel` 是不可自动补充的资源上限；`quantum` 是每次 poll 的公平调度时间片。
quantum 耗尽时自唤醒并返回 `Poll::Pending`，但不改变 hard fuel。长时间运行的 builtin
也必须提供 safepoint 或分段执行，不能只在 opcode 边界检查预算。

compile、execute、GC、host call 和 Promise job 使用 `tracing` span/event。逐 opcode
trace 只能作为默认关闭的诊断 feature，避免观测代码进入正常热路径。

### 4.3 Isolate 驱动模式

执行内核同时提供低层独占 API 和高层 actor API：

```rust,ignore
impl Isolate {
    fn execute<'a>(
        &'a mut self,
        module: Arc<CompiledModule>,
    ) -> ExecutionFuture<'a>;
}

struct IsolateRunner {
    isolate: Isolate,
    mailbox: Arc<Mailbox>,
}

struct IsolateHandle {
    mailbox: Arc<Mailbox>,
}
```

`IsolateRunner: Future + Send` 拥有 `Isolate`，但不绑定固定 OS thread，可在 executor
worker 间迁移。`IsolateHandle: Clone + Send + Sync` 是商业 Rust SDK 的默认入口；直接
`&mut Isolate` 是 benchmark、test262 和高级嵌入接口。不得建议用户以
`Arc<Mutex<Isolate>>` 替代 actor。

command mailbox 默认有界且容量可配置。host completion 使用独立队列，容量由
`max_pending_host_ops` 限制，并优先于新的外部 command。显式 `close()` 停止接收请求、
abort pending host futures、拒绝未完成 response，并在 safepoint 清理；最后一个外部
handle 被丢弃时触发相同的 graceful shutdown。

mailbox/waker 协议必须事件驱动：producer 只在 empty-to-nonempty 或 idle-to-scheduled 转换时 wake；
consumer 采用“注册 waker 后重新检查队列”的 lost-wakeup-safe 顺序，并按每次 poll 的有界 budget
drain。队列为空立即 `Pending`，completion 优先级不能导致 command 或 runnable JavaScript 饥饿。

Rust SDK 允许通过 actor 在 isolate scope 内执行可信同步 closure：

```rust,ignore
async fn with_scope<F, R>(&self, f: F) -> Result<R, HostError>
where
    F: for<'scope> FnOnce(&mut RunningScope<'scope>) -> Result<R, HostError>
        + Send
        + 'static,
    R: Send + 'static;
```

closure 本身不能是 async，也不能让 `Local<'scope, T>` 或 borrowed `Value` 逃逸；需要
跨 await 保留的对象必须转换成 opaque persistent handle。closure 属于可信宿主代码；panic 直接
abort 进程，不转换成 poisoned isolate 或可恢复 command error。
任意 closure 属于可信 host code，无法被 bytecode quantum 抢占；默认建立 `tracing` span
以暴露长时间阻塞。

### 4.4 Async Runtime Adapter

core 的 `tachyon-vm` 与 facade `tachyon` 只依赖 `std::future::Future`、`Waker` 和 executor-neutral
channel/wake contract。单独的公开 crate `tachyon-async-runtime` 提供三种 additive Cargo feature：

```toml
[features]
default = ["futures"]
futures = ["dep:futures"]
smol = ["dep:smol"]
tokio = ["dep:tokio"]
```

`futures` 使用 `futures::executor`（包括 deterministic `LocalPool` test path），`smol` 与 `tokio`
使用各自 spawn/join 能力。Cargo features 是可加的，因此不能把三者实现成互斥 backend；每个 adapter
位于独立 namespace，单 feature 和 `--all-features` 都必须编译。宿主显式构造/选择 adapter，库不得
通过 TLS、`Handle::try_current` 或运行时探测隐式改变执行语义；可提供明确命名的 convenience
constructor，但失败必须可见。

adapter 的共同能力是接收 `IsolateRunner: Future + Send`，返回 `IsolateHandle` 与可 await/abort 的
runtime task handle，并统一正常关闭、executor join error、abort 和 drop 语义。它可以在 adapter 边界
做 future type erasure，但不能让 runtime-specific type 进入 VM、mailbox、host future 或公开 core
handle。Tokio/Smol/Futures 的调度差异不得改变 JavaScript job/microtask ordering；公平性仍由 VM
quantum 保证。

同一组 runtime contract tests 必须分别实例化到 futures、smol、Tokio：wakeup/lost-wakeup、host
future completion、quantum fairness、thread migration、backpressure、cancel、abort、close、
pending drop 和多 isolate stress。CI 分别执行三个单 feature matrix 和 all-features；benchmark 使用
同一 workload 比较吞吐、wake count、allocation、completion latency 与 shutdown latency。core 的
dependency audit 必须证明禁用 adapter 时依赖图中不存在 Tokio、Smol 或 futures executor。

## 5. Value 与堆引用

- **决定**：`Value` 固定为 64 位、`Copy` 的 tagged representation。
- **决定**：只支持 64 位目标和 little-endian，构建时对不支持的平台直接报错。
- **决定**：使用 NaN boxing；进入 JS `Number` 的所有 NaN canonicalize 为一个正 quiet-NaN，
  ArrayBuffer/TypedArray backing store 中的原始位模式不受影响。
- **决定**：对象引用使用 32 位 logical heap byte offset，即 `GcRef<T>` 的存储大小为 4 字节。
  高 16 bits 是 `SpanId`，低 16 bits 是 64 KiB span 内 offset；offset 0 保留为空引用。
- **决定**：1.0 heap 永不移动对象；minor、major、晋升和增量 sweep 都保持 logical offset 稳定。
  `GcRef` visitor 仍接收可变引用以避免冻结内部 API，但 1.0 collector 不依赖对象重写。

初版位级契约使用负 quiet-NaN 空间作为 tagged domain：

```text
numeric double: 不匹配 0xfff8_0000_0000_0000 tagged prefix 的 IEEE-754 bits
tagged value:   0xfff8_0000_0000_0000 prefix + 3-bit primary tag + 48-bit payload

primary tag 000: heap reference，低 32 位为 logical heap byte offset，其余 payload 位必须为 0
primary tag 001: int32，低 32 位保存二进制表示
primary tag 010: immediate，payload 区分 undefined/null/false/true/hole/uninitialized
primary tag 011-111: 保留给后续表示，未定义值必须被拒绝
```

精确 mask、shift 和 immediate 编号应只定义在 Value 模块中，并通过 compile-time assertion
和 property tests 固化。不得利用 aarch64 TBI、amd64 canonical address 宽度或其他平台特定
指针位假设。

任意 raw bits 的分类测试必须使用不调用生产 `is_*`、`as_*`、tag/payload helper 的独立 oracle，
分别判断 numeric domain、合法 payload 和 reserved tag，避免测试与实现共享同一个错误。property
tests 负责广域分类与 roundtrip；Miri 使用 deterministic、isolation-on、strict-provenance 专项用例
验证 bit conversion 和 `RawHeapRef` 首尾边界，避免 proptest failure-persistence 的 host `getcwd`
污染隔离执行。property tests 与 Miri 是互补的 1.0 证据，均不单独宣称穷尽全部 `u64` 或证明无 UB。
coverage-guided fuzzing、corpus 维护和 differential fuzzing 属于 `POSTPLAN.md`，不得阻塞 1.0 Stage Gate；
未来 target 也只能 decode/classify raw bits，不能把未验证 heap payload 解析或解引用为对象。

普通 JS heap object 不得跨 isolate 直接共享。允许的跨 isolate 机制包括 immutable
`CompiledModule`/atom 元数据、structured clone、ArrayBuffer transfer、SharedArrayBuffer
的线程安全 external backing store，以及显式包装的 `Arc<T: Send + Sync>` host data。

普通 ArrayBuffer 使用两个 GC payload：`ArrayBufferObject` 保存 ordinary property 状态和可清除的 backing
edge，`ArrayBufferData` 保存 externally-accounted fixed `Box<[u8]>`、当前长度、最大长度和 resizable bit。
resizable bit 由构造参数是否提供非 `undefined` 的 `maxByteLength` 决定，不能由
`maxByteLength != byteLength` 推断：
`new ArrayBuffer(n, { maxByteLength: n })` 仍是可缩小并可恢复到 `n` 的 RAB。
detach 只清除 object edge，views 共享 backing identity；resize/transfer 必须采用 allocate-copy-swap，不能在
已发布 GC payload 内推动 Vec 扩容。该模型不使用 mmap、atomic 或 host allocator callback；SharedArrayBuffer
后续使用独立共享 backing 类型，不能污染普通 ArrayBuffer/TypedArray 热路径。

fixed non-shared `ArrayBuffer.prototype.slice` 复用五槽 `NativeCallState`，不为单一线性操作新增 heap type：
source、start、end、constructor、result 在 ToIntegerOrInfinity/species/Construct 阶段按明确生命周期原地复用，
所有仍 live 的 Value 始终处于 traced slot。初始 byteLength 在 start/end conversion 前快照；Construct 返回后
依次验证 ArrayBuffer brand、result detach、source identity、result byteLength，再重新检查 source detach，不能把
任一 callback 前的 backing witness 带过可观察边界。安全 GC borrow API 有意不提供两个 payload 的潜在 alias，
因此 copy 使用 `tuning::buffers` 的 bounded stack chunk：每块先在 source no-GC borrow 中读入 scratch，再在
destination no-GC borrow 中写出。该路径不建立未计费临时 Vec、不缓存裸指针、不使用 unsafe；chunk 大小只影响
borrow/copy 吞吐，不改变可观察语义，可由 M13 benchmark 单点调优。RAB/SAB/transfer 接入时仍需扩展 witness，
不能让本 fixed byteLength snapshot 冒充动态语义。

该纵切的回归矩阵显式覆盖 Proxy `@@species`、source own constructor getter、start/end conversion detach、
source/result 双向 mutation independence、N=1/2/4/8/16 与 forced-major；这些边界共同证明 property/Construct
continuation 没有把 callback 前的 backing witness 或未追踪 Value 带过可观察点。

当前 Realm substrate 为每个 Realm 保存 well-known-symbol slot；跨 Realm SpeciesConstructor 因此必须从
constructor 所属 Realm 取得其 `@@species` key，不能用调用方 Realm 的 slot把外来 intrinsic accessor误判为
missing并回退当前 `%ArrayBuffer%`。这只是现有表示下保持规范共享 well-known-symbol identity 的 lookup规则；
最终将 well-known symbols 提升为 engine共享 identity 后，该分支应自然收敛为普通单-key property Get。

fixed non-shared `transfer` 与 `transferToFixedLength` 共用同一 ArrayBufferCopyAndDetach 内核；两者保留独立
native identity，为后续 RAB 的preserve/fixed mode预留规范分叉。brand/shared校验先于 newLength观察，但
显式newLength的ToIndex先于detach复查；对象转换以`NativeCallState`保存source并走conversion continuation，
所以callback自行detach后不会读取旧backing。当前DESIGN已经要求transfer采用allocate-copy-swap：新fixed
backing及wrapper先按完整external charge发布，随后以bounded stack chunk复制，所有步骤成功后才清source edge。
OOM、conversion throw或length错误都不由引擎detach source；旧、新backing在transaction窗口同时精确计费，
因此低heap-limit可能拒绝最终大小本可容纳但峰值无法容纳的transfer，这是当前无unsafe/无可变external-charge
replacement API下有意的资源语义。未来若加入GC-aware backing ownership transfer API，必须同时转移header
charge且在失败时恢复edge，不能直接move已发布`Box<[u8]>`或绕过accounting。fixed模式下两个方法结果均为
`resizable=false,maxByteLength=byteLength`；RAB preserve、SAB、detach key与immutable buffer仍由后续substrate接入。

fixed `DataViewObject` 保存原始 ArrayBuffer `Value` edge、`u32` byte offset/length 和独立 ordinary
property state；它不复制 backing，也不缓存可失效的裸字节指针。每次 element access 在完成 `ToIndex`、
value conversion 和 endian conversion 后重新经 ArrayBuffer object edge 解析 backing，并在 no-GC scope 内
执行最多 8-byte checked read/write。这样 detach 会通过清除 ArrayBuffer object edge 对既有 view 立即可见，
同时普通 fixed view 热路径不需要 atomic、锁或 mmap。
固定 ArrayBuffer 的 detach 是幂等的单 edge clear；backing 在之后的 GC 中按普通不可达 external payload
回收，不维护 view observer list。`$262.detachArrayBuffer` 只安装在 embedding 显式启用的 Realm hooks 中，
直接调用同一原语，engine core 不接触 test262 文件或 host I/O。detach 后 ArrayBuffer 的 `byteLength`/
`maxByteLength` 为 0，fixed `resizable` 保持 false；TypedArray 的 length/byteLength/byteOffset 为 0、整数索引
读取为 undefined、写入失败；DataView 的 buffer 仍返回原对象，而 byteLength/byteOffset/element access 抛
TypeError。DataView 必须先完成规范规定的 ToIndex/element conversion，再检查 detach，且 detach 检查先于
bounds check；构造器在 `GetPrototypeFromConstructor` 后再次检查，不能发布已失效 view。

DataView 的 Float16 复用同一 2-byte checked access 与 endian assembly，不引入第二套 view/backing
模型。binary16 codec 使用整数 bit decomposition：decode 精确扩展 normal/subnormal/zero/infinity，NaN
进入 ECMAScript Number 时 canonicalize；encode 以显式 remainder/halfway/低位奇偶完成
round-to-nearest-ties-even，并处理 normal carry、subnormal 到最小 normal 的边界和 overflow-to-infinity。
因此结果不依赖宿主 endian、unaligned access 或动态浮点舍入模式，也无需为了两个字节引入外部 half-float
依赖。该 codec 后续由 Float16Array 与 `Math.f16round` 复用，不能复制不同的 NaN/rounding 规则。

fixed Number TypedArray 使用单一 `TypedArrayObject`，其 48-byte payload 保存原始 ArrayBuffer `Value` edge、
`u32` byte offset/element length、紧凑 `TypedArrayKind` 和 ordinary base。concrete element 不进入 shape，
integer-indexed exotic MOP 将 canonical numeric key 直接转换为 checked backing offset；非 canonical numeric
字符串仍走 ordinary property。内部 byte storage 固定 little-endian，不依赖宿主 endian 或 unaligned load，
也不缓存 backing 裸指针，因此 GC、未来 detach 和 transfer 始终通过 ArrayBuffer object edge 观察最新 backing。
九种 Number kind 共享这一 payload 和一套 MOP，BigInt/Float16 只扩展 element conversion，不复制对象类型。

BigInt primitive substrate 采用双表示但保持唯一数学语义：NaN-box tag 3 保存 signed 48-bit `SmallBigInt`，
超出范围的值使用 GC-managed `BigIntValue`，其 payload 为规范化 sign-magnitude little-endian `Box<[u64]>`；
zero 永不带负号，最高 limb 永不为 zero，能落入 signed 48-bit 的结果必须回到 immediate。OXC 的 BigInt
字面量在 HIR 中保存无分隔符的精确十进制文本，bytecode constant 在 code load 时解析，并通过
`CodeLoadRoots` 让已发布常量参加 forced-major tracing；任何解析、格式化、严格相等和 modulo 2^64 都不得经过
`f64`。十进制 parser 依据输入位数一次 `try_reserve_exact` 估计 limb 容量，formatter 使用 base-1e9 chunk；
limb `Box<[u64]>` 的字节数进入 GC external-memory accounting。该 substrate 先闭合 `typeof`、truthiness、
strict equality、primitive string conversion 和 unary negation；BigInt arithmetic 与 wrapper/constructor 属于
后续 slices。BigInt64Array/BigUint64Array 已复用同一 modulo-64 helper 和既有 TypedArray witness/storage
contract，未增加第三种 BigInt 表示或复制 TypedArray object type。

BigInt arithmetic 继续以该 canonical sign-magnitude payload 为唯一发布表示：SmallBigInt 的
Add/Subtract/Multiply/Divide/Remainder/Exponentiate/bitwise/shift 先走 checked allocation-free VM fast path，
不能留在 signed 48-bit 时退出 verified register epoch，再复制 GC payload 到 exact-capacity temporary limbs；结果
canonicalize 后只发布 immediate 或 `Box<[u64]>`。multi-limb 加减乘、truncating division/remainder、square-and-
multiply exponentiation、infinite two's-complement bitwise 与 arithmetic shift 均不经过 f64，也不把第三方 bigint
容器作为 published payload。对象 operand 复用 `ConversionContinuation::BinaryLeft/BinaryRight` 的 iterative
ToPrimitive，primitive finish 统一执行 ToNumeric type agreement，Number/BigInt 混用抛 TypeError。division/remainder
zero、negative exponent 和 BigInt unsigned shift 分别映射 RangeError/RangeError/TypeError。

为防止 hostile shift count 或 exponent 在 embedding isolate 中制造无界 allocation/CPU，`tuning::bigints` 集中定义
16 Mi-bit materialized-result 上限；shift 和 exponentiation 在分配及长循环前以 bit-length checked arithmetic 拒绝，
超限映射 RangeError。该策略不缩窄上限内的任意精度语义，后续若将 resource policy 提升为 host-provided typed
limit，只替换 admission boundary，不改变 arithmetic 或 payload contract。当前 multi-limb division 使用 bounded
binary long division；未来基于 benchmark 换成 Knuth/Burnikel-Ziegler 不得改变 truncation、remainder sign 或 rooting。

BigInt function conversion 延续同一 primitive substrate，并严格分离 `NumberToBigInt` 与 `ToBigInt`：
`BigInt(value)` 先以 number hint 执行一次 resumable `ToPrimitive`，只有所得 primitive 为 integral Number 时
直接解 binary64 significand/exponent 构造精确 limbs；NaN、Infinity 和 fraction 抛 RangeError。通用 `ToBigInt`
拒绝 Number/null/undefined/Symbol，Boolean 映射 0n/1n，String 按 `StringIntegerLiteral` 解析空串、Unicode trim、
十进制符号以及无符号 0b/0o/0x，不借 Number parser 或 f64。BigInt 具有规范要求的 `[[Construct]]` 识别能力，
但 construct dispatch 在 argument coercion 前因 NewTarget 抛 TypeError。对象 callback、getter 与异常值均
由现有 `ConversionContinuation` 保持 traced，返回的 String/BigInt 在 forced-major 后才进入 primitive finish。
BigInt TypedArray 的 element conversion 复用同一 `ToBigInt` primitive finish；该 constructor slice 当时尚未
包含 wrapper object、BigInt.prototype methods、asIntN/asUintN 和 arithmetic，后续模型如下。

BigInt wrapper/prototype slice 保持规范要求的两层对象身份：`%BigInt.prototype%` 是不含 `[[BigIntData]]` 的
ordinary object，因此直接调用其 `valueOf`/`toString` 必须抛 TypeError；只有 `Object(bigint)` 分配独立
GC-managed `BigIntObject { bigint_data, ordinary }`。primitive property read/write 显式路由 Realm-local
`%BigInt.prototype%`，wrapper 则继续复用统一 ordinary shape/storage/prototype MOP，不用可观察 property 模拟
private slot。`thisBigIntValue` 同时接受 primitive 和 genuine wrapper，并拒绝 Number、ordinary object 与
`%BigInt.prototype%`。`BigInt.asIntN/asUintN` 先以 typed continuation 完成 observable `ToIndex(bits)`，再把
bits 保存在 traced continuation receiver 中完成 observable `ToBigInt(value)`；截断直接在 canonical limbs 上
构造固定宽度 two's-complement residue，结果重新 canonicalize 为 SmallBigInt 或 boxed limbs。radix 2..=36
formatter 使用 exact-capacity magnitude scratch 和 small-radix division，不经 Number；`Number(bigint)` 作为
规范明确的 constructor exception 单独转换为 binary64，普通 ToNumber(BigInt) 仍抛 TypeError。

RAB 接入利用 `TypedArrayKind` 后现有 alignment padding 增加 `ViewLengthMode::{Fixed,Tracking}`，不扩大 48-byte
TypedArray payload；fixed view 保存 element length，tracking view 每次操作从 buffer 当前 byte length 计算有效
length。DataView 没有可复用 padding，因此同一显式 mode 将 payload 从 40 bytes 增至 48 bytes；这是为了避免
`u32::MAX` sentinel 与合法最大显式 byteLength 冲突，不能以缩小可表示范围换回 8 bytes。
所有 integer-indexed MOP 和 prototype method 在完成规范要求的 observable conversion 后重新解析 buffer edge，
生成只在当前 no-GC borrow 内有效的 `TypedArrayWitness`，统一处理 detached、out-of-bounds、resize generation 与
byte range，绝不把 backing pointer 或旧 length 缓存在 continuation。TypedArray/iterable/array-like source
construction 使用 traced `PendingTypedArrayConstruction` 跨用户 iterator/callback，最终大小确定后一次分配
ArrayBuffer。当前收集阶段复用共享 ArrayStatic IteratorToList，并以 realm intrinsic Array 作为 GC-managed
临时列表：这保证 iterator 完整收集先于任何 element conversion、复用 IteratorClose/abrupt 语义，并避免在
construction state 中引入不受 trace 的 Rust Vec。该 Array 是临时实现而非长期性能承诺；profile 证明它形成
瓶颈后，可替换为受配额、保留 high-water capacity 的 chunked isolate scratch，但不得改变完整收集、顺序
conversion、moving-GC rooting 与异常传播合同。

`TypedArrayWitness` 必须同时保留 detached 与 attached-but-OOB 两种状态；把两者都压成 `length=0` 会让
`slice` 的 ValidateTypedArray 错误接受 OOB receiver，同时破坏合法零长度 view。tracking RAB 在省略 length
时允许 `(byteLength - byteOffset)` 不能整除 element size，有效 element length 使用向下取整；只有 fixed
省略-length view 才要求整除。`subarray` 从 payload 的 `ViewLengthMode` 决定 species argument list：tracking
且 end 为 undefined 时只传 `(buffer, beginByteOffset)`，其余路径传第三个 fixed newLength。mode 可以跨 callback
重读，因为 payload mode 不变；当前 byte length、OOB 和 backing identity仍必须在 callback 后重建 witness。
共享收集器的 intrinsic Array iterator、原生 `Array.from` 结果，以及 TypedArray iterable-list/ordinary
array-like 的同步 data-property 路径必须使用显式 Rust loop；不能用 `resume -> advance` 的同步递归模拟
规范步骤。Accessor、Proxy、iterator callback 和对象 ToNumber 等可观察边界仍使用 typed continuation，恢复后
重新 snapshot traced state。该划分既保留逐项可观察顺序，也保证 10,000+ 元素输入不按元素增长 Rust stack；
任何新增 collection/bulk builtin 都必须有同规模回归覆盖这两条路径。

`%TypedArray%` 是不可构造 native function，但规范要求它独立拥有 `.prototype`；因此 function metadata 将
“拥有默认 prototype property”与 `IsConstructor` 分开判定。concrete constructor 的内部原型指向
`%TypedArray%`，concrete prototype 指向 `%TypedArray.prototype%`。这不是把 base function 伪装成 constructor，
也不允许 function prototype 虚拟槽继续假设所有 owner 都可构造。
fixed TypedArray 使用显式 `ContentType::{Number,BigInt}` 区分九种 Number kind 与 BigInt64/BigUint64 两种
BigInt kind。BigInt element read 在 no-GC borrow 内只复制八个 little-endian bytes，退出 borrow 后才分配
canonical BigInt；write 通过 primitive substrate 的 modulo 2^64 helper 生成 two's-complement bytes。constructor、
typed source copy 和 integer-indexed Set 在两个 ContentType 之间混用时抛 TypeError，同 ContentType 的
BigInt64/BigUint64 转换复用逐元素 modulo 路径。Float16、auto-length RAB 和 SAB 必须在各自 substrate 完成后
接入同一 view witness contract，不能用固定长度快照冒充动态语义；BigInt shared methods 和 DataView BigInt
accessors 也不能由本基础 slice 虚报完成。

integer-indexed `[[Set]]` 与 `Reflect.set` 共用一条 receiver-aware 路径。target 与 receiver 相同时，canonical
numeric index 直接执行 element conversion/write，并按 TypedArray `[[Set]]` 规则把 detached/OOB/invalid index
映射为成功调用而不创建 ordinary property；alternate ordinary receiver 走 OrdinarySetWithOwnDescriptor，alternate
TypedArray receiver 对其自身 index 执行 element write并返回实际 boolean。对象 value 在写入前发布到五槽
`NativeCallState`，以 number hint 完成 resumable ToPrimitive，再从 traced target/key/value 重建写入；因此
callback、forced-major 与 detach 都不依赖 Rust 局部或 backing pointer。Number/BigInt content mismatch 仍由统一
element conversion 抛 TypeError。Proxy prototype-chain、BigInt wrapper ToPrimitive 与 RAB witness 尚未闭合，
不能由 fixed direct/alternate receiver 路径推断完整 integer-indexed MOP。

fixed Number TypedArray 的 `indexOf`/`lastIndexOf` 共用方向参数化 native descriptor 和一个四 Value conversion
state：receiver、searchElement、原始 fromIndex 与初始 length 都跨对象 ToIntegerOrInfinity 精确 trace，方向编码在
state scalar mode，不建立逐元素状态或 Vec。初始 ValidateTypedArray 和 attached backing 验证先于 length-zero
短路；空 view 不观察 fromIndex。反向默认 cursor 使用参数 presence 而非 undefined 值区分，cursor 统一表示
forward next-index 或 reverse index-plus-one，避免 signed sentinel。primitive fromIndex 不分配 state；对象转换后
重新 snapshot view，并只扫描 initial/current length 交集。转换期间 detach 等价于后续 IntegerIndexedElementGet
全部得到 undefined，strict search 因而返回 -1，而初始 detached receiver 仍由 ValidateTypedArray 抛 TypeError。
扫描在一次 checked no-GC backing borrow 内直接按 little-endian kind decode Number；searchElement 不转换，NaN
永不匹配，正负零通过 IEEE equality 匹配，每元素不分配。BigInt/Float16 与 RAB witness 接入后扩展同一搜索边界，
不能复制第二套 cursor 或 detach 规则。该边界对照 QuickJS `js_typed_array_indexOf`、Escargot
`builtinTypedArrayIndexOf`/`builtinTypedArrayLastIndexOf` 与 Boa `BuiltinTypedArray::{index_of,last_index_of}`。

fixed Number TypedArray 的 `every`/`some`/`find`/`findIndex`/`findLast`/`findLastIndex` 共用一个五 Value
`NativeCallState`：receiver、callback、thisArg、initial length 与 direction-aware cursor。六种算法只由紧凑
mode 决定方向、短路条件和 value/index/miss 返回，不复制 callback trampoline。入口先完成
ValidateTypedArray 与 attached backing 检查，再验证 callback callable；初始 length 是规范要求的迭代上界快照，
不是 backing witness。每个索引调用前先提交 cursor，随后重新从 traced receiver 解析当前 buffer edge并读取
element；callback continuation 的第二 Value 槽保留该 element，因此 callback throw、bytecode suspension 与
forced major GC 都不依赖 Rust 栈局部。callback detach 后后续索引映射为 undefined 并继续到 initial length；
未来 RAB 接入时同一读取点重建 TypedArrayWitness 并把 OOB映射为 IntegerIndexedElementGet 的 undefined，不能
让 initial length 恢复旧 backing range。该边界对照 QuickJS `js_array_every`/`js_typed_array_find` 对 JS land
失效 length 的警告、Escargot TypedArray callback builtins 与 Boa `find_via_predicate`。

`forEach`、`reduce` 与 `reduceRight` 延伸同一个五 Value callback state，不增加 continuation 或 managed
payload：`forEach` 保留第三槽为 thisArg；两个 reducer 将该槽复用为 accumulator，并以独立 mode 区分方向及
“初值省略/显式提供”状态。显式 `undefined` 必须算已提供初值；省略初值时 driver 同步读取首个或末个元素并
原地切换为 initialized，空 view 才抛 TypeError。reducer callback 固定以 undefined this 和
`(accumulator, element, index, receiver)` 调用，返回值经 generational write barrier 写回 state；`forEach`
丢弃 callback completion value。三种模式仍在 callback 前提交 cursor、逐项重新读取 traced receiver，并在
同步 native callback 返回时继续显式 loop，因此 detach、动态 element mutation、20,000 次调用和 bytecode
suspension 都不缓存 backing witness、不递归增长 Rust stack。三/四参数 prefix 是规范固定大小的一次性
boxed slice，不使用可增长 Vec 作为 published payload。Number 与 BigInt TypedArray 不复制 callback driver：
每次 element read 在退出 backing 的 no-GC borrow 后按 `ContentType` 解码，BigInt64/BigUint64 可在此后分配
canonical BigInt 并由 continuation 的 element 槽追踪；callback 参数不做 ToNumber 或其他内容类型转换。

fixed Number `TypedArray.prototype.fill` 的普通 primitive 路径不分配状态：入口 ValidateTypedArray 并保存
initial length，随后严格按 value ToNumber、start ToIntegerOrInfinity、end ToIntegerOrInfinity 顺序转换，最后
重新解析 receiver/buffer edge。任一参数为对象时才分配五槽 `NativeCallState`，保存 receiver、value、start、
end 与 initial length，并以三个 typed conversion consumer 跨 valueOf/toString callback 精确恢复；converted
value 与相对 index 原地覆盖对应参数槽，不增长 payload 或引入 Vec。最终 revalidation 先于空区间短路，因此
任一 conversion detach 都抛 TypeError；start/end 仍按 initial length 归一化，再把 end 与当前 fixed view length
相交，为 RAB witness 接入保留规范边界。Number element 只编码一次，在一个 checked no-GC backing borrow 内以
显式 chunk loop 写完整范围；不缓存裸 backing pointer、不按元素进入 safepoint，也不让长 range 增长 Rust 栈。

BigInt TypedArray `fill` 不复制 Number fill driver：在 value observable conversion 完成后按 receiver
`ContentType` 复用 `primitive_to_bigint`，将 canonical BigInt 保存在同一五槽 state，再通过已有 modulo-2^64
helper 生成一次 little-endian element pattern；start/end 转换和最终 backing witness 仍保持规范顺序。`includes`,
`indexOf`, `lastIndexOf` 共享 `TypedArraySearchNeedle`：BigInt searchElement 必须先是 BigInt，并通过 signed /
unsigned kind 的 64-bit representability check，再把其 modulo bits 与 no-GC backing 中的 raw bytes 比较；这保留
跨 `BigInt64Array`/`BigUint64Array` 的负值和超范围不匹配，同时避免逐元素分配和 Rust recursion。RAB/OOB/ES2024
detach 仍由后续 witness slice 负责，fixed backing 的定向结果不能推断动态语义已完成。

fixed Number `TypedArray.prototype.copyWithin` 同样将 primitive target/start/end 保持为零状态分配路径；任一
object index 才建立五槽 `NativeCallState`，按 target、start、end 顺序通过三个 typed conversion consumer
恢复，并以 normalized index 原地覆盖参数槽。initial count 只由入口 length 计算；count 非零时重新建立当前
backing witness，并再与 current length 的 source/destination 可用范围相交，为 RAB shrink 保留边界。实际移动
将 element range 换算为 checked byte range，在单个 checked no-GC mutable borrow 内调用 safe
`slice::copy_within`；其重叠语义等价于 memmove，并允许 Rust 后端生成 bulk move，同时保留 NaN payload/完整
bit encoding，不需要 unsafe、裸指针或元素 decode/re-encode。initial count 为零时按规范不重新访问 backing，
即使 index coercion detach 也正常返回 receiver；整个过程没有跨 safepoint 裸 pointer、逐项分配或 Rust
recursion。

fixed Number `TypedArray.prototype.reverse` 不经过 integer-indexed Get/Set：入口先验证 TypedArray brand 与 attached
fixed backing，再在一个 checked no-GC mutable borrow 内限定到 view byte range。实现按 `TypedArrayKind` 的
1/2/4/8 byte width 分派到 const-generic safe block swap；每对 disjoint element slice 使用
`swap_with_slice`，因此不会 decode/re-encode float、canonicalize NaN payload，也不需要 unsafe、裸指针、Vec
或逐元素 safepoint。当前 fixed ArrayBuffer 不会在操作中 resize；未来 RAB 必须把入口验证替换为 write-mode
TypedArray witness并从该 witness取得动态长度/OOB结果，immutable backing也必须在 borrow前拒绝，二者都不能由
当前固定 snapshot 冒充完成。

fixed Number `TypedArray.prototype.set` 将 typed-source primitive-offset 作为零状态分配 bulk path；object offset
或 array-like source 才建立五 Value traced state，复用 length 槽依次保存 target length 与 source length，index
槽保存已提交 cursor。入口只先验证 receiver brand，offset ToInteger 完成后才验证 attached target backing；该
顺序保证 offset callback 可以 detach target，随后抛 TypeError，且不会读取 source.length。array-like 路径再按
ToObject、Get/ToLength(length)、逐项 Get/ToNumber/write 恢复；iteration 中 detach 让 integer-indexed write
成为 no-op，但不能停止之后的 observable source Get。typed source 同 kind/same backing 直接在 checked no-GC
borrow 中使用 `slice::copy_within` 保证 overlap 和 raw-bit preservation；其他 backing 先以 exact-capacity byte
snapshot 固定源值，再执行 raw same-kind copy 或 cross-kind Number conversion。临时 Vec 只存在于同步 bulk
边界，不进入 GC-managed state、不跨 safepoint、不缓存 backing pointer；allocation failure 显式返回 engine
error。BigInt、SAB、RAB/OOB 与 immutable backing 必须接入各自完整 witness/storage contract 后扩展此路径，
不能让 fixed snapshot 或非 atomic byte copy 冒充支持。

fixed Number `TypedArray.prototype.join` 在 separator conversion 前完成 ValidateTypedArray、attached backing
检查与 internal length snapshot；undefined separator 不做转换，对象 separator 通过二 Value traced state 和
string-hint continuation 恢复。separator callback detach backing 后，后续 integer-indexed Get 视为 undefined，
仍按 initial length 保留全部 separator；调用前已 detached 则必须在任何 separator callback 前抛 TypeError。
conversion 后不再有用户代码，因此 Number element assembly 使用同步两遍扫描：第一遍复用
`primitive_string_unit_length` 计算精确 UTF-16 unit 数和 checked separator product，随后只执行一次
`try_reserve_exact`，第二遍复用 `append_primitive_string_units` 填充最终 backing。该策略付出一次额外 fixed
decode scan，换取零增长、零逐元素 managed state、无 Rust recursion，且 NaN/Infinity/-0/指数格式与引擎唯一
ECMAScript Number formatter 一致，不经过 Rust Display。RAB witness 接入后必须保留 initial length 与动态
IntegerIndexedElementGet 结果的区别；BigInt、immutable backing 与 SAB 也必须扩展统一 storage contract，不能
由当前 Number/fixed path 推断支持。

fixed `TypedArray.prototype.slice` 使用五 Value `NativeCallState` 跨越所有 observable boundary：source、
归一化 start、count、constructor scratch 与 species result 均由 GC trace/write barrier 管理。入口先执行
ValidateTypedArray 并冻结 initial length，再依次完成 start/end ToIntegerOrInfinity、constructor Get、
`@@species` Get 与 Construct(count)；custom/cross-kind species 不走 Rust 特判，而是复用通用 Construct machinery。
species result 必须再次验证 TypedArray brand、attached backing 和最小 length，短结果使用独立
`TypedArraySpeciesResultTooShort` engine error 在统一边界映射为 JS TypeError，不能复用 RangeError 对应的
`InvalidArrayLength`。count 为零时不重新验证 source；非零时重建 source snapshot/backing witness，并按当前
length 截断实际 copy count，为后续 RAB shrink 语义保留边界。

same-kind copy 必须区分 backing identity。不同 backing 使用 `try_reserve_exact(count * elementWidth)` 的同步
raw-byte snapshot 后一次写入，既保留 Float NaN payload/signed zero，也不跨 safepoint持有 backing borrow；同一
backing 的 offset overlap 不能使用 snapshot 或 `memmove`，因为新版规范的逐 byte 前向 GetValueFromBuffer/
SetValueInBuffer 允许 earlier target write 改变 later source read，实现必须在一个 checked no-GC mutable borrow
中按 offset 正序读写。cross-kind species 必须先比较 source/target `ContentType`，即使 count 为零也不能绕过；
Number/BigInt 不匹配抛 TypeError，相同 ContentType 则逐元素通过统一 integer-indexed read/write 转换，因而
BigInt64/BigUint64 跨 kind 保持 modulo-2^64 语义而不经过 Number。整个算法不保留裸指针、不使用 unsafe、
不按元素建立 managed state 或增长 Rust stack；RAB/OOB、immutable backing 与 SAB 必须接入各自完整
witness/storage contract 后再扩展，不能从 fixed backing path 推断支持。

fixed Number `TypedArray.prototype.subarray` 不复用 slice 的 copy phase：它创建共享 view，且 TypedArrayCreate 的
三参数形式不施加单 length 参数的最小结果长度检查。五 Value state 保存 source、原始 ArrayBuffer edge、begin、
end/count 与 initial length/constructor scratch；buffer 必须在任何 argument callback 前作为 traced identity
捕获，不能在 detach 后通过 getter 重建。入口只 RequireInternalSlot 并建立 witness：detached fixed source 的
initial length 为 0，但 begin/end conversion、constructor与 species lookup 仍必须发生，最终 intrinsic constructor
才因 detached buffer 抛 TypeError；若 conversion detach 后 custom species 返回独立 attached TypedArray，则结果
合法。begin/end 都相对同一个 initial length 归一化，beginByteOffset 使用对象保留的 original byteOffset 与 source
element width，不使用 detached 后公开 getter 返回的零值。

fixed view 始终以 `(buffer, beginByteOffset, newLength)` 三参数调用 selected species constructor；result 只执行
TypedArray brand 和 attached backing 验证，不能要求同 kind、同 backing 或指定 length。constructor/`@@species`
Get 与 Construct 全部经 typed continuation，foreign constructor 因而在所属 Realm 创建 prototype。RAB 的
auto-length source 且 end 为 undefined 时规范要求 `(buffer, beginByteOffset)` 两参数以保留 length tracking；
该分支必须等 RAB witness/storage contract 后实现，不能由 fixed 三参数路径冒充。算法不复制 element、不缓存
backing pointer、不使用 unsafe，运行时间与 source length 无关。

SharedArrayBuffer backing store 是唯一允许多个 ECMAScript agent 并发访问的 JS memory。普通 heap、
ArrayBuffer、TypedArray 和 GC field 不因支持 Atomics 而变成 atomic。`Atomics.wait` 必须通过宿主注入的
waiter/parking 能力挂起 agent，并由 notify/timeout 事件唤醒；不得 busy-spin，core 不自行创建线程或
读取时钟。test262 `$262.agent` 的 thread/sleep 只存在于 runner/provider 边界。
首版公开 Rust API 只暴露 scoped 或 persistent opaque handle，不公开 `Value` 的位表示。

Number 的 `parseFloat`/`parseInt` 属性必须直接引用 global intrinsic function，避免为同一规范函数复制
native identity；`Number.prototype.toLocaleString` 在无 Intl host 的 engine core 中走 number-brand 的
ECMAScript decimal fallback，locale/options 参数仍保留在 API 位置但不触碰 host I/O。Math namespace 的
`@@toStringTag` 是 ordinary configurable data property，不能仅依赖 `Object.prototype.toString` 的硬编码。
`Object.prototype.toString` 必须在任何 observable operation 前计算规范 fallback brand，再用 typed native
continuation 保留 receiver 和紧凑 tag id，执行 Proxy/accessor-aware `Get(O, @@toStringTag)`。只有 primitive
String 可以覆盖 fallback；String object 与其他值必须被忽略。最终字符串直接拼接 UTF-16 code units，不经过
Rust UTF-8。`%IteratorPrototype%` 及具体 iterator、Generator/AsyncGenerator 和三类 specialized Function
prototype 的 `@@toStringTag` 均是真实 Realm-local descriptor；`%AsyncFunction%`、`%GeneratorFunction%` 与
`%AsyncGeneratorFunction%` constructor 也必须是真实可追踪 callable，并通过 prototype 的 `constructor` 属性形成
规范回边，不能由普通 `%Function%` 冒充。当前 specialized dynamic constructor 的 source compilation 尚未扩展
host callback 合约，因此调用明确返回 unsupported；这不改变已发布 intrinsic graph。
`Object.prototype.toString` 必须在 observable lookup 前冻结规范 builtin fallback，然后通过 typed native
continuation 执行 Proxy/accessor-aware `Get(O, @@toStringTag)`；continuation 精确 trace boxed receiver，并仅用
compact integer 保存 fallback kind。只有 primitive String tag 覆盖 fallback，结果按 UTF-16 code unit 精确预留
和拼接。Iterator、concrete iterator、Generator/Async function family 与 namespace tag 必须由 Realm 原型图上的
真实 configurable data property 提供，禁止把具体 collection/iterator 名称硬编码进 toString dispatcher。
普通 async closure 继承独立 `%AsyncFunction.prototype%`，async generator function prototype 再继承该对象，
使 Proxy 函数通过正常 prototype lookup 获得 `AsyncFunction`/`AsyncGeneratorFunction` tag。
未来若增加其他语言 ABI，也必须复用该不透明 handle 边界。

任何 `unsafe` 的 Value decode、对象 header 转换和 bytecode decode 都必须集中到小型模块，
写明安全不变量，并为边界值、错误 tag、越界 offset 和对齐增加测试。

## 6. 字节码解释器

### 6.1 字节码格式

- **决定**：采用寄存器式字节码。
- **决定**：使用 32-bit word-coded 混合格式。常见指令在一个对齐 `u32` 中编码
  `opcode + u8 operands`；normal 和 wide 形式分别使用 `u16` 和 `u32` operand word。
- **决定**：register、local、constant 和 jump 的逻辑索引类型为 `u32`。超过紧凑或
  normal 范围时编译器发出资源 warning，但仍生成 wide 指令并接受程序。
- **决定**：`pc` 是 `u32` word index，常量索引和跳转目标使用稳定整数，不在字节码中
  嵌入进程地址。
- **决定**：不实现持久 bytecode cache，也不访问文件系统；调用方通过持有
  `Arc<CompiledModule>` 复用内存中的编译结果。
- **决定**：首版不修改共享 bytecode。属性和调用 fast path 读取 isolate-local feedback；
  只有 profile 证明值得时，才为热点函数增加 isolate-local COW quickening。
- **方向**：先使用 `#[repr(u8)]` opcode 和 Rust `match` 分派，基准后再考虑
  superinstruction 或其他 dispatch 优化。
- **方向**：算术、比较、局部变量和常见属性访问保留短且可内联的 fast path，复杂语义
  进入模块化 slow path。

`CompiledModule` 必须保持不可变和 `Sync`。运行时反馈、shape guard、属性 slot 等
inline-cache 状态保存在 isolate-local `FeedbackVector`，通过 bytecode 中的 feedback slot
索引访问，不能直接修改共享字节码。

### 6.2 Verified execution kernel

稳定 Rust 缺少可移植的 computed-goto 时，解释器采用 const-generic dispatch batching，但 batching
只是 safepoint/fuel 的轮询粒度，不是每 N 条重新物化 VM 状态的边界。解释器的目标形态是一个
**verified interpreter kernel**：进入 kernel 时从 active activation 一次建立 execution cursor，随后把
`pc`、bytecode start/end、register-window base/end、frame index 和本地 budget 保存在 Rust local 中；
普通 opcode 不再经由通用 `&mut Isolate` API 重新寻找这些字段。

```rust,ignore
fn run_kernel<const N: usize, const BOUNDED: bool>(&mut self) -> KernelExit {
    let mut cursor = VerifiedExecutionCursor::enter(self)?;
    loop {
        for _ in 0..N {
            match cursor.execute_hot()? {
                HotControl::Continue => {}
                HotControl::Slow(operation) => return cursor.flush_slow(operation),
                HotControl::SwitchActivation(change) => cursor.switch(change)?,
                HotControl::Exit(outcome) => return cursor.flush_exit(outcome),
            }
        }
        if BOUNDED && cursor.reached_poll_boundary() {
            return cursor.flush_safepoint();
        }
    }
}
```

`execute_hot` 包含一份 opcode `match`。允许使用小型 macro 统一 operand extraction 和 control
propagation，但不能用宏复制 N 份完整 match；是否 unroll 由 LLVM 和基准决定，避免 text size 与
instruction-cache 随 N 线性恶化。大 enum 本身不是优化目标：编译器可以生成 jump table，必须依据
汇编、branch miss 和 I-cache 证据决定是否拆分 dispatcher。

kernel 按下面的权限边界拆分：

- **Hot opcode** 只能读取 immutable verified bytecode、当前 register window 和 cursor locals。它不能
  分配、GC、调用 host、扩容 frame/register storage，也不能调用任意可修改整个 `Isolate` 的方法。
  load/move、primitive arithmetic/comparison 和普通 jump/branch 在该层完成；jump 只修改 local `pc`。
- **Activation transition** 处理 ordinary bytecode call/return。容量足够且 callable/cache guard 命中时，
  它发布或弹出紧凑 activation 并在同一 kernel 中刷新 cursor；任何可能扩容 storage、创建 environment、
  进入 constructor/bound/native continuation 的情况先 flush，再进入 slow path。
- **Slow exit** 在离开 kernel 前恰好一次把 next `pc`、budget 和必要的 activation 状态写回 fiber，并终止
  所有 register/bytecode raw borrow。slow path 才能分配、GC、调用 host、构造规范异常或改变 backing；
  返回后必须从已发布的 active activation 重建 cursor，禁止沿用旧 pointer。
- **Safepoint/exit** 与 slow exit 使用同一 flush 规则。await/yield/host suspension 立即退出；bounded
  quantum、GC、interrupt/cancel 请求在 N 条轮询边界退出。effectively-unbounded 路径编译时删除 budget
  分支，但仍保留语义要求的 allocation/backedge safepoint。

bytecode verifier 已证明 opcode、operand count、register window、instruction end 和 jump target 后，kernel
不得每条指令重复同一动态验证。release fast decoder/register access 可以在私有
`VerifiedExecutionCursor` 模块内使用最小的 `unsafe get_unchecked`；安全性依赖必须原地写明：immutable
module backing 在 cursor lifetime 内保活，cursor 不跨任何 capacity mutation，active window 长度不小于
verified function layout，且所有写入 `Value` 都满足 VM internal-value invariant。debug/test 构建保留
checked mirror，并对每个 opcode、compact/normal/wide、最小/最大 register、错误 jump、module owner move、
loaded-code 扩容以及 N=1/2/4/8/16 做结果对拍。除此之外不得为了省一条 branch 扩散 raw pointer。

`Value` 的安全边界也分为两层。FFI/host ingress、GC 后恢复和 debug verifier 对 heap-tagged value 完整检查
owner、span liveness、alignment、descriptor 和 payload layout；已经位于 active rooted register/constant/
frame 中的 value 是 trusted internal value，在禁止 GC 的 kernel 中不得为每次 call 重复上述全部检查。
ordinary call 仍必须检查 callable descriptor/kind，但只做直接 header/class guard；命中 isolate-local call
feedback 后可直接读取 bytecode target。GC、host callback 或任何可能发布外部 value 的 slow exit 会终止该
trusted capability。错误 object kind 是正常 TypeError，不得通过 unchecked typed cast 假装 callable。

旧 batch-local `BytecodeCursor` 是迁移基线：它只缓存 immutable backing，却仍每条 opcode 回读 active
frame 的 `code/function/pc/base`、回写 `Frame::pc`，并通过 `Result<_, ExecutionError>` 调用通用
read/write/dispatch。AArch64 release 审计显示 `ExecutionError` 与这些 Result 都是 40 bytes；call-loop
基准则显示完整 callable heap 验证和 activation materialization 是另一个主要瓶颈。

2026-07-19 的第一层 kernel 已把 verified decoder、local `pc` 和 checked-on-entry register window 接入：
allocation-free opcode 使用 unchecked register access，slow exit 前 flush 并 rebind；operand count 使用
repr(u8)-indexed data table，完整 slow dispatcher 不再强制内联。test-only checked dispatcher differential
覆盖全部 hot opcode、destination alias、numeric edge、branch PC 与 heap truthiness slow exit。它仍在每 N
条 flush/re-enter，ordinary call 仍走通用 callable/frame/Result 路径，所以只是上述目标的 proof point，
不得靠继续叠加 `#[inline(always)]` 冒充完整 kernel。

一次把 shallow environment chain 在每次 cursor rebind 重新做 mutable typed validation、再缓存 raw payload
pointer 的实验已被 benchmark 否决并 revert：closure median 从 `732.741 ms` 回退到 `760.046/787.617 ms`。
这不否定 direct environment slot，而是否定“在错误边界重复建立 trusted state”。后续 environment fast path
必须让 call target/activation 在首次 checked resolution 时一并持有可失效的 trusted environment identity，
并在 GC/host/activation topology change 时统一失效；不能用更多 rebind-time heap validation替代原有
per-op validation。heap-reference store 仍必须经过 generational barrier。

hard fuel 必须按实际执行的每条指令精确扣减；batch 提前退出时不能多扣。bounded kernel 可把计数保存在
local register，在 flush 时统一发布，但不能越过 fuel/quantum 的精确终点。唯一例外是调用方同时传入
`fuel=u64::MAX, quantum=u32::MAX` 的 effectively-unbounded sentinel：production 选择独立 const-generic
monomorphization并消除逐 opcode budget check/decrement；任一字段不是 MAX 就走精确 bounded 路径。
quantum 不足 N 时走短尾路径，不能越过公平调度边界。N 只作为内部调优常量，至少基准比较
1、2、4、8、16，并在三个
目标架构检查吞吐、binary text size 和 instruction-cache；不得把 N 变成不稳定的公开 API。

### 6.3 显式 Fiber

解释器不得依赖 Rust 原生调用栈表达 JavaScript 调用栈。普通 JS 调用压入显式 `Frame`：

```rust,ignore
struct Fiber {
    frames: Vec<HotFrame>,
    cold_frames: Vec<ColdFrameState>,
    registers: Vec<Value>,
    handlers: Vec<ExceptionHandler>,
    state: FiberState,
}

struct HotFrame {
    code: CodeId,
    function: FunctionId,
    pc: u32,
    base: u32,
}

struct ColdFrameState {
    environment: Option<GcRef<Environment>>,
    return_register: Option<RegisterId>,
    this_value: Value,
    new_target: Value,
    argument_base: u32,
    argument_count: u32,
    handler_base: u32,
    completion_base: u32,
}
```

这里的 hot/cold 拆分是物理布局目标，不改变显式 fiber 语义。`HotFrame` 目标固定为 16 bytes，只有每次
fetch 必需的四个 `u32` identity/cursor 字段；return destination 也只在 activation transition 使用，不能
因为“经常 return”又塞回逐 opcode 访问的记录。当前 104-byte `Frame` 把 constructor、bound
arguments、native continuation、exception checkpoint、`this`/`new.target` 等冷字段复制给每个普通空函数
调用；Rust 的 `Option<Value>` 又因没有 niche 占 16 bytes。普通 activation 必须缩为固定紧凑记录，只有
函数 metadata 或 call kind 需要额外状态时才在 side storage 建立扩展项，不得为每个 ordinary call 做
Box/heap allocation。`frames` 与基础 `cold_frames` 是同 depth 的并行 Vec，push 前一起 reserve，发布、
rollback 和 pop 必须保持相同长度；GC/debugger 按 frame depth join，optional constructor/bound/native
continuation payload 再由 cold entry 的 compact index 引用。具体字段分界由 call-loop、closure、constructor、
try/catch 和 debugger 栈采样共同决定，layout size 必须有 compile-time/runtime test 固定并记录。

frame/register backing 允许继续使用 Rust allocator 和显式 `Vec` limits，但 execution cursor 不得跨越
任何可能 reallocate 的操作。entry/function metadata 应按集中在 `tachyon_vm::tuning` 的 educated-guess
常量预留常见深度和 register slots；容量命中走无分配 activation transition，容量不足 flush 后进入
fallible growth slow path。是否改成 fixed-size segment 只由深调用、Wasm、RSS 与 pointer-stability 基准
决定；禁止用 native Rust recursion 或一次性按 host hard limit 预分配来换取快路径。

`Isolate` 从构造开始拥有 `Heap` 与预注册 VM descriptor tokens；heap、frame count 和 aggregate register
slots 都是显式 host hard limits，descriptor 注册失败返回 structured creation error。`FunctionObject`
必须是 GC payload，保存 isolate-local `CodeId`、module-local immutable `FunctionId` 与 captured
environment；裸 `FunctionId` 不能跨 source unit 唯一标识代码。禁止为了缩短首版实现把 function ID
编进 reserved `Value` tag。`CreateClosure` 使用 managed allocation，并把 fiber/job/realm roots交给
collector 后再发布结果 register。

`CodeId(NonZeroU32)` 索引 isolate-local loaded-code table；每个 frame 也保存 `CodeId + FunctionId`，因此
从 body module 调用 harness closure 时，batch 下一次 fetch 必须切到 closure 所属 module。loaded-code
table 通过 immutable backing pointer identity 复用同一 `CompiledModule`，不同 module 数量由 host 显式
`RealmLimits::max_loaded_modules` 限制；load slow path 在发布前 reserve scope-atom storage，失败时回滚
本次新增 atom。当前不卸载 code，后续 generation slot/unload 必须同时证明没有 FunctionObject、frame、
debugger 或 module record 引用旧 CodeId，不能复用裸 index 产生 ABA。

embedding API 明确拆分 `load_module(&CompiledModule) -> CodeId` 与
`execute_loaded(CodeId, ExecutionBudget)`；`execute(&CompiledModule, ...)` 只是组合两步的 convenience。
benchmark 的 precompiled/steady-state 在计时前 load，parse-compile-execute 把 compile、load 和 execute
全部计入样本。不得在 steady-state 每次 iteration 重复 module identity/scope-name resolution，也不得为
隐藏 load 成本而从 parse-compile-execute 中删除它。

realm substrate 将 global object data-binding storage 与 declarative global lexical record 分开。前者用于让顶层 function declaration 在连续
script source units 间可见。`StoreScope` 发布/更新 binding，`LoadScope` 读取；binding value 是精确 GC root，
binding 数受 `RealmLimits::max_global_bindings` 限制。Realm 将可枚举 `GlobalBinding` storage 与
atom-indexed `GlobalSlotId` resolution table 分离：AtomId 是 isolate-local 稠密稳定 ID，首次发布按实际
atom upper bound 精确 reserve，之后 lookup/update 不做字符串比较、hash 或 binding-order scan。该 slot
identity 是 BindingPlan/loaded-code resolution cache 的基础：每个 loaded scope operand 保存 AtomId 和
optional stable slot，首次看到已发布 binding 后缓存 slot，之后直接访问 storage；尚未解析的项继续探测，
因此之后由 script/eval 发布的 binding 可以自愈。当前 global binding 不支持 delete，接入 configurable
delete/slot reuse 时必须同时加入 generation/version guard，不能复用裸 slot。该 cache 不是最终 global
object descriptor 本身。

declarative record 使用独立 `GlobalLexicalBinding`/stable `GlobalLexicalSlotId` 与 atom-indexed resolution table，
保存 value、mutability 和 initialized bit。script entry 在任何 statement 前发出 verified
`DeclareGlobalLexical(name, mutable)`，声明位置用 `InitializeGlobalLexical` 恰好初始化一次；普通
`LoadScope`/`StoreResolvedScope` lexical-first，因此 function 与后续 source unit 读取同一 binding。未初始化
读取/写入、const 写入和跨 source redeclaration 返回结构化 engine completion error；loaded code 在 lexical
稍后发布后可 self-heal stable slot。global lexical 与 object binding 共享 realm binding hard limit，但 storage
和 lookup table 不合并。N=1/2/4/8/16、声明前 TDZ、const、跨 source、预加载 code self-heal 与 benchmark
setup→main invocation 已覆盖。

完整 GlobalDeclarationInstantiation 的 property attributes/configurability、所有 var/function/lexical collision、
delete、block declarative environment 与 Annex B
尚未实现；这些缺口不能用当前两张 stable-slot table 假装完成。

`StoreResolvedScope` 是 BindingPlan 前的 identifier-reference 慢路径：simple/compound/update 在 RHS
求值顺序不变的前提下更新已存在 binding；缺失时读取或 strict 写入产生 native ReferenceError abrupt
completion，sloppy 写入发布 global binding。它不能成为 1.0 热路径。参考 Escargot 的
`ResolveNameAddress`/binding-slot 做法，最终 bytecode location 应区分 frame/environment/global declarative/
global object slot，并用 environment/version guard 失效；只有 direct eval/with 等动态情形保留按名查找。

`CompiledFunction` 的 `BindingPlanEntry` 是 verifier-owned immutable contract，不是可选 debug annotation。
每项保存独立于 runtime atom table 的 shared immutable name、mutability 和 `BindingLocation`；location 枚举
完整区分 frame register、environment(depth/slot)、module cell、global lexical、global property 与 dynamic
lookup。module freeze 拒绝空 name、越界 frame register 和 environment slot。局部/debug binding name 不得
塞入 runtime `scope_names`，否则 module load 会为纯编译元数据永久 atomize。当前 compiler 已为实际使用的
parameter/var/let/const/catch frame binding、captured environment、global lexical 和 global-property binding 生成 plan；尚未实现
的 module/dynamic location 不能伪装成 frame/global。plan 服务 debugger/scope materialization 和 freeze-time
一致性验证，不进入 resolved captured-binding 的逐次执行路径；后续 global operand 完成迁移后删除旧
`LoadScope/StoreScope` 按名入口。

Frontend scope identity 直接取自已经用于 syntax validation 的 Oxc semantic pass，但只在 arena 存活期读取。
HIR 将 Oxc ScopeId/SymbolId/ReferenceId 的稳定数值复制进 Tachyon-owned `ScopeId/BindingId/ReferenceId`，
同时复制 scope parent/strict/function/arrow/direct-eval flags、reference owner scope、resolved binding 和
read/write mode；返回值不包含任何 Oxc 类型、AST node 或 arena lifetime。bytecode local lookup 以 BindingId
匹配，不再按同名字符串反向扫描，因此 nested shadowing 不会错误命中。Oxc arena 存活期内还会比较 binding
owner function 与所有 resolved reference 的 owner function，只有真正跨 function boundary 的 binding 才标记
为 captured。compiler 为每个 owning activation 按稳定源码遍历顺序分配 exact contiguous environment slots；
未捕获 local 继续保留 register storage。

binding/assignment pattern 现在先复制成 owned recursive HIR（含 array/object/default/rest、computed key
和源码顺序），再递归收集 BoundNames、capture slots 与 bytecode capacity；这取代了旧的
“一个 declarator 等于一个 identifier”假设。同步 object/array pattern 已在编译期展开成普通 property、
call、branch 和 binding/store bytecode，不由 VM 递归解释 pattern；array 通过 `Symbol.iterator` 获取并
缓存 iterator receiver、`next` 与 `done` register，normal early completion 会调用可选 `return`。这复用
可恢复 property/call opcode，不在 VM 中做 pattern 专用递归。同步 `for...of` 使用同一 record，在每轮读取
`next`/`done`/`value`，continue 回到取值点，break/return/throw 都经 compiler-generated `IteratorClose` finalizer
调用可选 `return`；它不另建 VM iterator state。该 finalizer 使用独立 handler kind，若原 completion 是 throw
则抑制 close 中的新 throw，return/break 的 close throw 则照常覆盖旧 completion。rest、iterator-result object
校验、per-iteration lexical environment 与 async iteration
仍显式未完成，不能用 Array index shortcut 或省略 close 冒充完整实现。

`LoadEnvironment`/`StoreEnvironment` 直接编码 `(register, depth, slot)`；compiler 从 owned scope graph
解析一次 capture 后同时生成该 opcode 和对应 debug binding plan，不允许 VM 每次反查 plan。module freeze
验证 register 及 slot 落在模块声明的最大 environment slot 上界内；VM 仍对实际 environment chain 和 slot
做 checked access。opcode 保持 compact/normal/wide 三种整数宽度，不嵌入 native pointer。当前 metadata
尚未冻结完整 lexical parent graph，因此 verifier 还不能独立证明任意 depth 必然到达特定 ancestor layout，
这项证明必须随 lexical environment record metadata 补齐，不能用 unchecked traversal 代替。

`Environment` 是 traced GC payload，保存 optional parent 与 exact-size `Box<[Value]>`，backing bytes 进入 heap
external accounting，slot mutation 走统一 old-to-young barrier。只有拥有 captured slots 的 activation 才分配
environment；slot count 为零的函数直接继承 closure 捕获链，因此普通 non-capturing call 不增加 allocation。
`FunctionObject` 捕获创建时的最近 environment，callee frame 再按自己的 layout 选择继承或创建 child。
forced-major 与 dispatch N=1/2/4/8/16 已覆盖可变 closure state 和多层 environment chain。TDZ、parameter
environment、per-iteration cloning、named function self-binding、direct eval invalidation 和完整 environment
record kind 仍未实现，不能由本 substrate 推断为已支持。

compiler 另在 immutable `FunctionMetadata` 冻结 exact-size `EnvironmentSlotMetadata` slice；slice index
就是 dense runtime slot，不再把 owner declaration 塞进表示可执行引用位置的 `BindingLocation`。每项保存
name、mutable 与 activation-entry initialized state，record kind 独立保存；module verifier 要求 slice 长度
精确等于 layout slot count，并校验所有 depth-0 binding plan 与 owner name/mutability 一致。parameter、var、
hoisted function declaration 标为 activation entry 已初始化，let/const 与 catch-entry 才初始化的 binding
保持未初始化。当前 runtime 仍保留 captured fast storage，尚未消费该 metadata 自动选择 state-bearing
storage；block/catch 独立 record 与 lexical parent layout proof 到位前，不能声称 TDZ/environment 完整。

simple function 的 parameter initializer 由 owned stencil 与参数 binding 平行保存。callee frame 在参数复制后
仍以 `undefined` 填充缺失 formal；compiler 为每个 initializer 发出 `StrictEqual(parameter, undefined)`
和一个跳过 label，只有 true 分支才按源码顺序求值 initializer 并写回参数 register，因此显式 `null` 等值
不会触发默认值，后一个 initializer 可以读取前一个参数。该 prologue 的 instruction/label/constant/
scope-name capacity 在 lowering 前 checked 计数；它暂不实现 destructuring/default 的独立 parameter
environment、`arguments` aliasing、rest 收集或 initializer 中的复杂 early-error 规则。

Realm 用 `IntrinsicBinding`/stable `IntrinsicSlotId` 与 atom-indexed resolution table 预留 mandatory global
名称、初始化值和 writability；初始化先 intern 完整固定名称集，再按确切 binding count 与 atom upper bound
一次 reserve。`undefined`、`NaN`、`Infinity` 为 non-writable，Error constructors 为 writable；它们不消耗
host 的 user-global quota，loaded-code resolution cache 直接缓存 intrinsic slot，不再按字符串 fallback。
global object publication 不从 `writable` 猜 descriptor flags：`undefined`、`NaN`、`Infinity` 明确发布为
non-writable/non-enumerable/non-configurable，其他 mandatory globals 为 writable/non-enumerable/configurable。
intrinsic 标识符的可观察读取以当前 Realm global object 的同名 property storage 为 canonical source，不读取
`IntrinsicBinding.value`；因此 `global.Object = replacement`、删除和 descriptor mutation 不会与标识符
`Object` 形成双状态；direct intrinsic assignment 同样写入该 data property，并按当前 frame strictness映射
non-writable rejection。当前纵切只覆盖无 callback 的 data-property Get/Set；global accessor Get/Set、
Proxy-like host global、普通 var/function property publication 仍需 object-environment continuation，完成后
应进一步删除 `IntrinsicBinding.value` 的运行时存储职责。局部 lexical/register binding 继续正常遮蔽
global property。
`Object.getOwnPropertyDescriptor` 在 key conversion 前对 non-nullish target 执行真实 ToObject；String primitive
由 GC-managed `StringObject` 暴露 non-writable/non-configurable index 与 length exotic descriptors，不能把
primitive 统一映射成 absent。wrapper 在进入 resumable ToPropertyKey 前已进入 pending operand root contract。
static `Object.hasOwn` 同样先 ToObject target 再执行 resumable ToPropertyKey；prototype
`hasOwnProperty/propertyIsEnumerable` 则保留各自规范的 key/receiver ordering，在 key completion 后才 boxing
receiver。三个 consumer 最终共享 String exotic-aware own descriptor 查询，不复制 index/length 规则。

首版 `Call` operands 固定为 `(destination, callee, actual_argc)`；实际参数连续放在 callee register
之后，verifier 证明整个 argument window 位于 caller register file。callee frame 按 immutable
`FunctionLayout` 一次 reserve，复制 `min(actual_argc, formal_count)` 个参数，其余 formal 初始化为
undefined；frame 保存 caller-owned actual argument base/count，供后续 arguments/rest 读取全部多余参数。
`Return` 按 frame 保存的
register/handler/completion checkpoints 截断状态并回写 caller destination，不经过 Rust recursion。

`FunctionObject` executable 是 `Bytecode { code, function, environment } | Native(id) | Bound(data)` closed sum type，不用
optional code/native 字段组合表示 kind。Realm 在 isolate publication 前 managed-allocate 并精确 root
`%Function.prototype%`、`call` 与 `bind`；ordinary closure 的 internal `[[Prototype]]` 指向共享 Function
prototype，两个方法作为 own data property 参与普通 prototype traversal。CallSite 将 callee Value、absolute
argument base/count、可选 GC-traced bound prefix view 与 this 分开保存，因此 native `call` 可消费 thisArg，
bound call 可在 prefix 后拼接 caller register window，二者都不复制参数。prefix identity/offset/count 进入
callee Frame 并参与 root tracing，`arguments`/rest 后续必须读取同一逻辑参数序列。native/bound forwarding 在
`call` 内迭代解析，不用 Rust recursion；最终 bytecode target 仍只 push 一个显式 frame。callee tagged value
的 logical address 通过 heap-direct `NoGcScope::borrow_raw_reference` 一次完成
registered Rust type、header descriptor、slot liveness 与 owner address 验证并复制 closed callable
descriptor；不得在热路径重复 `checked_reference`、temporary-root publication 与 payload validation。
这个只读 fast path 不创建 `RunningScope` temporary-root checkpoint，因此没有空 rollback drop；generative
callback 不能接收或返回 `Local`，且仍不能分配或收集。
raw-to-typed retype 只存在于 GC crate 的 checked boundary，borrow 不逃出 no-GC lifetime；错误 type 和
immediate callee 仍由统一 completion boundary 转为 TypeError。函数 strictness 由 frontend scope flags 先复制进 owned HIR function，再在 compile 时冻结为
immutable bytecode metadata，
VM 不再从 function kind 重复猜测；directive strictness 会传给本函数，外层 strictness 也会按 Oxc semantic
scope 继承到 nested ordinary function。Realm 在 intrinsic initialization 时额外 old-space 分配并精确 root
一个 managed global object：script entry `this` 指向它，module entry `this` 为 undefined；ordinary strict
call 原样保留 thisArgument，sloppy ordinary call 只将 undefined/null 替换为该 global object。该绑定发生在
显式 frame publication 前，不递归、不分配，也不改变 native `call` 的零拷贝 argument forwarding。

当前 global object 仍是 identity/prototype substrate，尚未把 stable global binding slots 暴露为其 property；
sloppy primitive thisArgument 也尚未执行 ToObject boxing。因此当前切片不能声称完成
`OrdinaryCallBindThis`，后续必须在 descriptor/global-environment 与 primitive wrapper object 到位后闭合，
而不能保留第二套 global state。当前 native ID 覆盖 Function prototype/call、全局 Function constructor
identity、核心 Object/Array 方法与 closed native Error constructors，不是 host callback ABI；动态
Function call/construct 在 source compilation 接入前返回 host-visible unsupported，不伪装成规范异常。
`BoundFunctionData` 是独立 GC external-backed payload，同时保存规范 immediate `[[BoundTargetFunction]]` 与
ultimate call target、第一次 bind 的 bound-this、exact `Box<[Value]>` 参数前缀和创建时缓存的 name/length。
nested bind 仅把参数扁平化为单 prefix，顺序为旧 prefix 后接新参数；call 直接使用 ultimate target 与缓存
bound-this。construct 忽略 bound-this，但沿 immediate target chain 逐层执行 `SameValue(F, newTarget)` 替换，
因此未来 `Reflect.construct(C, args, B)` 不会因参数扁平化丢失可观察 newTarget 语义；该遍历不分配也不复制
参数。instanceof 可直接委托 ultimate target。bound function 不拥有 prototype；
native callable 只有 `NativeFunction::is_constructor()` 为真时才暴露 lazy prototype，避免把 call/bind 等普通
builtin 误建模为 constructor。`Function.prototype.apply`、`Reflect.apply` 与 `Reflect.construct` 共享
GC-managed、external-backed 的 `CreateListFromArrayLike` continuation：先验证 target，再完整执行
`Get(length)` 与逐项 `Get(index)`，每一个 getter 都经显式 frame/completion trampoline 恢复，随后只创建一次
immutable bound-prefix 并启动 call/construct；backing 在 length 已知后精确一次分配，写入已 promoted state 时
发布 write barrier。当前 `ToLength` 可处理 primitive number/string，但 object length 的 ToPrimitive continuation、
Proxy/exotic dispatch 与完整 accessor descriptor 仍未完成；host callback ABI 也仍未完成。

Realm 精确 root `Error`、`ReferenceError`、`SyntaxError`、`TypeError` 各自的 native constructor/prototype
pair；subclass prototype 指向 Error.prototype，constructor 的 inline prototype slot 与
`prototype.constructor` 保持 identity。Error instance 使用独立 GC-managed `ErrorObject` descriptor 保存
unforgeable `NativeErrorKind` brand 和共享 ordinary-property base，不在所有 ordinary object 中增加 brand
字段，也不允许通过修改 prototype chain 伪造 Error。call 与 construct 共用 `create_native_error`；实例
message 使用 writable、non-enumerable、configurable descriptor，prototype 发布规范 name/message 与
`Error.isError`。`Error.prototype.toString` 的 data-property/primitive conversion 路径按 UTF-16 精确组装，
constructor message、accessor/Proxy `Get`、string-hint object ToPrimitive 和 `InstallErrorCause` 复用 typed
native continuation，不从 Rust 递归进入 interpreter。constructor state 固定保存 error/options/message，
toString state 固定保存 receiver/name/message；callback suspension 时 completion stack 是精确根，callback
恢复或同步路径弹出 parent 后，任何仍可能分配的步骤都会临时重新发布同一个 typed continuation。这个协议
覆盖 atom/String 创建、message/cause descriptor storage 扩展和最终 UTF-16 result allocation，禁止依赖
Rust 局部 `Value` 跨 GC safepoint。`Error.prototype.stack` 先建立 proposal accessor 契约：每个 Realm 拥有
独立 `get stack`/`set stack` function identity，getter 直接检查独立 `ErrorObject` descriptor 作为 realm-agnostic
`[[ErrorData]]` brand，普通对象、继承对象和 Proxy wrapper 都不穿透 target；在 debugger frame capture 到位前
只返回稳定的 `NativeErrorKind` 名称 String，不读取 name/message 等可观察属性。setter 从 callee identity 找到
defining Realm 的 `%Error.prototype%` home，前置 TypeError 也在该 Realm 创建；随后按
`SetterThatIgnoresPrototypeProperties` 先执行 receiver `[[GetOwnProperty]]`，缺失时以 W/E/C 全 true 执行
CreateDataPropertyOrThrow，存在时执行 Set with Throw=true。Proxy 路径复用 get-own/define/set dispatcher，外层
`ErrorStackSetter` continuation 仅保存 receiver/value 两个 traced edge，保持 32-byte continuation 与 4-byte kind，
不增加 Frame 字段、不递归 Rust interpreter stack。完整 source location、同步/async frame formatting 和 debugger
策略仍由 M11 stack-capture substrate 接管；cross-realm accessor/error identity 已在本切片闭合，
AggregateError 与 SuppressedError 共用同一 branded `ErrorObject` 和 closed `NativeErrorKind`，不增加对象布局。
SuppressedError constructor 复用 Error message 的 string-hint typed continuation；fixed five-Value state 在
callback/GC 期间保存 result、options scratch、converted message、error 与 suppressed，最终依次创建 message、
error、suppressed own property。该类型不接受 Error `cause` options，也不经过 AggregateError iterator driver。
Error constructor 的 intrinsic fallback 同样遵守 `GetPrototypeFromConstructor`：显式 prototype 非对象时先取
newTarget 的 function Realm，再从该 Realm 选择对应 NativeErrorKind prototype；只有无法解析 callable Realm 时
才回退 active Realm。这个 lookup 只读取 Realm root table，不切换 active Realm，也不增加 callback 边界。
interpreter fetch loop 在 dispatch 返回后的
单一 Result 分支只将已分类的规范失败转为这些 managed error object，并复用 `throw_value` 的显式
frame/handler 传播；heap/resource/
decode/verifier/invariant failure 不伪装成 JS throw。该分类边界是后续 abstract operation 的单一入口，opcode
不得自行构造 ad-hoc error shape。public `Isolate::native_error_kind(Value)` 只接受独立 Error descriptor
并返回 stable `NativeErrorKind`，不暴露 `GcRef`、shape 或 constructor heap identity；test262 adapter 用它
保留 runtime negative 的准确 error type，非 native thrown value仍报告 generic Error。

首个结构化 lowering 切片把 `Block`、`If` 和 `Throw` 保留为 owned HIR。block lowering 使用 local-binding
checkpoint，在离开块时截断编译期可见绑定；`If` 使用 symbolic labels，不把源码 offset 直接编码成
跳转目标。script entry 在出现结构化控制流时分配一个 completion result register：expression statement
只在实际执行的分支更新它，empty declaration/branch 保留此前的非 empty completion。compiler 对嵌套
statement 的 instruction、literal、label 和 binding 递归做 checked upper-bound count，builder freeze 后
不保留预留 slack。

logical expression 使用独立 owned HIR 节点，不能降成 eager binary opcode。lowering 先把左操作数复制
到 result register，再以 `JumpIfFalse`、`JumpIfTrue` 或 `JumpIfNotNullish` 跳过右侧；需要右侧时才求值
并覆盖 result，因此 `&&`、`||`、`??` 返回原操作数且保持副作用顺序。三个 branch opcode 共享 verifier
target/register contract，并在 `execute_batch::<1/2/4/8/16>` 下逐条 fetch，jump 不强制提前退出 batch。
数值 `Negate` 保留 IEEE-754 `-0`，`-Infinity` 通过正常 scope load 加 unary opcode 实现，不在 compiler
按 identifier 名称做常量折叠。

zero-argument ordinary call 直接以 callee expression register 编码 `Call`，不为不存在的 argument window
复制 callee；function statement/for-update 明确丢弃 update result 时，register-local increment/decrement
允许以同一 register 作为 binary destination，避免 old-value snapshot 与写回 Move。对象 coercion 与
captured/global target 仍走通用 lowering，不能用该 peephole 改变 prefix/postfix observable result。

public `execute/execute_loaded` 使用 `tachyon-vm::tuning::dispatch::DEFAULT_DISPATCH_BATCH` 选择唯一
production monomorphization，初始值为 8；不能把只供测试的 `execute_with_batch` 对拍误当成生产已启用。
N=1/2/4/8/16 共享 bounded 逐 opcode fuel/quantum 检查、unbounded sentinel 与控制流语义测试。默认值是 formal 跨架构调优前的
educated guess，后续只修改 tuning owner，并同时比较吞吐、text size、I-cache 与 cold start。

switch 使用 owned discriminant 与 source-ordered case table，每个 case 保存 optional test 和 consequent。
lowering 先把 discriminant value 复制到独立临时 register，防止 case-test 副作用改写其 local binding，
再按源码顺序求值所有 non-default case tests，以 `StrictEqual + JumpIfTrue` 选择首个匹配；全部
未匹配才跳到 default 或 end。clause body 按源码连续布局，因此 default 位于中间、matched/default
fallthrough 都无需额外状态机。每个 switch 分配一个共享 end label；无标签 `break` 读取 Lowerer 的
active break-target stack 并跳到最近 switch end，嵌套 switch 不会误跳外层。stack capacity 按 owned HIR
中的 switch 总数做 checked 预估，case-label临时 Vec 使用 cases exact capacity。

当前 switch control semantics 不等于完整 switch lexical semantics：case block 的 `let/const/class` 应在
单一 switch lexical environment 中统一实例化并带 TDZ，而当前 linear local lowering 只能覆盖声明后的
引用。该缺口归 M2 BindingPlan/M5 environment record；在完成前不得把带跨 case lexical 引用的测试记为
已支持，也不得用逐 case scope 假装规范行为。

classic `for` 在 owned HIR 中分别保存 initializer/test/update/body；lowering 使用 condition、update、end
三个 symbolic label，并维护独立 break-target 与 continue-target stack，因此 nested switch 中 continue
仍跳到最近循环 update。identifier/static `++/--` 保留 prefix/postfix result 差异且 member receiver 只求值
一次；numeric `LessThan` 是共享 bytecode/VM opcode，在 N=1/2/4/8/16 下对拍。当前 `let` loop binding 只
提供单个 lowering local，不实现规范每迭代 environment cloning，因此 closure capture loop binding 尚不支持。

`for-in` 不借用 JS Array、host iterator 或 Rust 栈状态：owned HIR 区分 single declaration head 与
assignment target，lowering 将 RHS 求值一次并生成 `CreateForInIterator`、`ForInNext`、undefined sentinel
comparison 和显式 back-edge。VM payload `ForInIterator { keys: Box<[AtomId]>, index: u32 }` 是独立
GC type，register 是唯一执行期 root；payload 不含 GC edge，`Trace` 为空，但 backing bytes 通过
`GcExternalMemory` 精确计费。创建迭代器或 materialize 返回 key 时触发的 collection 都只依赖 fiber/
realm/loaded-code 的显式 roots，不建立 native shadow root 或 unwind cleanup。

ordinary-object key snapshot 先无分配遍历 prototype chain，计算 shape property 与 virtual function key 的
checked upper bound，再一次性 reserve output Vec 和 50% load 的 power-of-two seen table；容量、load factor
与 AtomId multiplicative hash 常量统一归 `tachyon-vm::tuning::objects`。第二遍按 prototype 层级和 shape
insertion order 扫描：Hole 不进入 seen set，因此不屏蔽 prototype；present non-enumerable key 进入 seen 但
不进入 output，因此正确屏蔽同名 enumerable prototype key。primitive string 单独按 code-unit length
exact reserve index AtomId。Object/Array/Function/Error prototype 上当前已实现的 builtin data properties
在 realm 初始化时使用 writable、non-enumerable descriptor（Array.prototype.length 仍 non-configurable），
不能让错误的默认 enumerable flags 污染所有 for-in 结果。bytecode delete 对 non-configurable property 在
sloppy code 返回 false，在 strict code 按 DeletePropertyOrThrow 抛 TypeError；两个 delete opcode 共用 active
frame strictness helper，不把 strict 位复制进每条指令。

该快照是首个 ordinary subset，不等于完整 `EnumerateObjectProperties`：integer-index keys 尚未先做 numeric
ascending 排序；创建快照后删除的 property 尚未在 yield 前重新检查，期间添加的 property 固定不加入；
let/const 复用单个 binding storage，尚无 RHS 求值期 head TDZ 与 per-iteration environment cloning；
destructuring、Proxy/exotic、symbol filtering 的完整路径也未闭合。这些缺口必须由 M5/M13 的 indexed property ordering、prototype/
shape version 与 iteration-environment 工作统一解决，不能在 test runner 中重写结果。

key-only enumeration 必须与 value observation 分离。for-in、Object.keys 与
Object.getOwnPropertyNames 使用 shape lookup 的 attributes 加当前 fixed slot 判定 presence；不得通过
data-property helper 过滤 accessor，也不得执行 getter。该规则使 own non-enumerable accessor 仍能遮蔽
prototype enumerable key，而 structural delete 后 prototype key 可重新出现。Object.values、
Object.entries 与 Object.assign 不属于该 fast path：它们要求 observable `[[Get]]`，accessor consumer 完成
resumable suspension 前继续明确限制为 data property，不能为了扩大表面通过率静默跳过 getter。

`while` 与 `do-while` 共用 owned `Loop { test, body, test_first }` 节点；`test_first` 只描述语法顺序，不
改变 completion register 的规则。while 先跳到 condition，do-while 先进入 body；两者都把 continue 绑定到
condition label，使 continue 在 do-while 中仍执行尾部 test。body 内的 expression、break 和显式 abrupt
completion复用现有 statement lowerer，condition 本身不覆盖脚本的最后非 empty completion。当前 loop
仍不提供 labelled control、per-iteration lexical environment cloning 或 completion replay through finally。

`JsString` 作为 GC external-backed primitive payload 注册进每个 isolate。module load 将 immutable UTF-16
string constants 一次 fallibly materialize 到 loaded-code constant cache；cache 与 load 期间 pending Vec
都参与精确 tracing，因此 `LoadConstant` 热路径只复制 Value。`typeof` 的规范固定词汇在 isolate 创建时
一次 old-space 分配并由 Realm root，执行不分配。string strict equality 按 code units 比较，empty string
参与 falsy；N=1/2/4/8/16 与 forced-major module-load fixture 共享这些语义。rope/concat、general string
StringToNumber 现使用显式 ECMAScript WhiteSpace/LineTerminator 集、decimal grammar 和精确 Infinity spelling，
任意长度 power-of-two radix integer 用 top-53/guard/sticky 一次完成 binary64 ties-to-even 舍入，且拒绝
unpaired UTF-16 surrogate；heap String
到 detached code-unit buffer 与 Rust String 的两次分配尚未优化，后续应由 borrowed Latin-1/UTF-16 scanner
消除，不能用 `unsafe` cast 绕过表示边界。opcode/general builtin ToPrimitive、well-formed Unicode APIs 和 string exotic properties仍未实现；
String call 的 ordinary object ToPrimitive(string hint) 已通过上述 native continuation 接入。

首个 ordinary data-property substrate 将 shape metadata 与 GC payload 分开：`ShapeTable` 由 isolate
单线程拥有且 append-only，`ShapeId(0)` 是共享 empty shape；add-transition 按 `(from, PropertyKey,
attributes)` 复用，同一属性添加序列因此共享 shape。`PropertyKey` 是 `Atom | Symbol` 的 closed sum；
shape/transition Vec 只在 transition miss 的 cold
path 按 `tachyon-vm::tuning::objects` 的有界 chunk 增长，shape 总数受 host `max_shapes` hard limit，
普通 property hit 不触发 collection growth。当前 shape entry 已保存 parent/key/slot/attributes/version，
prototype transition、attribute transition、watchpoint 与 dictionary conversion 尚未接入，不能据此把
完整 M5.2 记为完成。

`OrdinaryObject` 保存 `ShapeId`、optional `GcRef<PropertyStorage>` 与 traceable `prototype: Value`；
`PropertyStorage` 拥有 fixed
`Box<[Value]>` 与只为实际 Symbol slots 建立的 sparse fixed key-edge backing，两者都按准确 backing bytes
走 GC external accounting，不在热写入路径使用可扩容 `Vec`。
新增属性先创建/复用 target shape，再构造可 trace 的 exact-size pending storage、复制旧 slots、发布新
storage，最后切换 object edge 并执行 object-to-storage insertion barrier；已有 slot 原地更新，对真正
拥有 Value edge 的 storage 执行 value barrier。managed allocation 期间 receiver 作为显式 root 参与
rewrite-capable trace，返回后重新解析 typed reference，不依赖“当前 collector 不移动”逃避 rooting contract。
prototype 暂存 GC payload 而非 Rust-owned `ShapeTable` 是有意选择：当前 ShapeTable 不参与 tracing，且把
每个 constructor 的独立 prototype 编入 shape 会破坏 empty/property shape 共享。后续 IC guard 必须组合
shape identity 与 prototype identity/version；若未来把 prototype 移入 GC-managed shape，必须先给 shape
建立精确 tracing、barrier 与 mutation invalidation，不能把 heap Value 放进未追踪 side table。

Symbol shape identity 不能只保存 `RawHeapRef`：删除最后一个 Symbol property 后 collector 可以回收该
Symbol，而 logical slot 可被新对象复用；append-only shape 若只比较地址就会把新 Symbol 错认成旧 key。
因此每次 Symbol 分配取得 isolate-local、单调且永不复用的 `NonZeroU32` serial，`SymbolId` 将 serial 与
logical heap ref 组合为 64-bit identity。Shape 只保存不 tracing 的 `SymbolId`；每个 present Symbol own
property 由 `PropertyStorage` 额外保存原始 `Value` strong edge。普通 configurable delete 对照 Escargot
移除 structure entry，按剩余 chronology 从 empty shape 重放 descriptors，并精确压缩 slots 与 Symbol
edges；re-add 因此走普通 append transition，在 String/Symbol 分区中自然移动到末尾，不需要 per-object
ordinal sidecar。首次 storage 发布的 receiver、property value 与 Symbol key 都是
显式 temporary roots；forced-major tests 覆盖首次发布、只由 key edge 保活、删除回收与 re-add。

ArrayObject 是独立注册的 GC payload，内嵌 ordinary named-property base、初始 `length` descriptor 与 optional
`GcRef<ArrayElements>`。默认可写/可枚举/可配置整数索引进入 fixed `Box<[Value]>` backing，不再进入 shape
parent chain；Hole sentinel 与 `hole_count` 区分 packed/holey。已有 slot 原位写入并执行 backing-to-value barrier；
扩容按 `tuning::arrays` 的 4/3 educated guess allocate-copy-swap，pending payload 和 receiver/value 都参与
rewrite-capable trace，发布后执行 array-to-backing barrier。远端首次写入、超过最大 dense index 或容量 gap
阈值的写入走 ordinary dictionary-style indexed property，避免稀疏索引制造长度级 backing。

dense read 在 ordinary/prototype lookup 前命中 present own slot；hole 继续 shape 与 prototype 链，因此 inherited
indexed accessor/property 仍可观察。delete 写 Hole，length shrink 在不可配置 ordinary index 检查成功后截断
dense used range；非默认 data descriptor 或 accessor 先原子发布 ordinary slot，再移除 dense value。ownKeys
把 present dense indices 与 structural String/Symbol keys 合并排序。generic Array builtin 仍保留逐项 MOP 与
continuation 边界；本 substrate 消除默认 dense 构造的 O(n²) shape lookup/storage copy，但不以 bulk memcpy
绕过 Proxy/accessor/prototype 语义。原先超过三分钟的 TypedArray copyWithin detach fixture 现为 2/2，约 13 秒。

dense-present 属性转入 ordinary storage 时必须以现有完整 descriptor 为基准应用
ValidateAndApplyPropertyDescriptor：默认 dense element 的 value 与 writable/enumerable/configurable
均必须在新 descriptor 省略时保留。CompletePropertyDescriptor 中“缺省为
undefined/false”只适用新属性，不能用于这条迁移路径；否则 `Object.freeze`
会丢失 dense value，把现有 element 改为 accessor 也会错误丢失 configurable，进而
改变 ArraySetLength shrink 和 change-array-by-copy 可观察语义。

`Array.prototype.copyWithin` 不复用 change-array-by-copy 的 `PendingArrayCopy`：后者拥有新结果 Array 且只需
逐项 Get，前者修改原 receiver 并必须在每项之间保留 Has/Get/Set/Delete 的可观察边界。独立固定大小
`PendingArrayCopyWithin` 因此拥有 receiver、三个原始参数、一个 retained source Value 与纯标量 traversal；
没有 Vec，也没有与 source length 成比例的 backing。length、target、start、end 的对象转换分别用 closed
`ConversionConsumer` 恢复，转换完成前不执行 indexed operation。重叠时仅调整 from/to/direction，count
保持剩余工作量；每次 Set/Delete 成功后才提交 from/to/count，abrupt completion 不可越过 cursor commit。

普通同步 property operation 在显式 loop 中连续推进，避免像早期 insertion 草案那样以 resume recursion 让
Rust 栈随 array-like length 增长；真正执行 JavaScript 的 accessor/Proxy 路径才发布 typed continuation。
Get 得到的 source Value 同时进入 state traced edge 和 continuation retained slot，并在 state mutation 后执行
generational barrier。QuickJS `JS_CopySubArray` 只用于验证 overlap direction，Escargot/Boa 用于验证逐项
Has/Get/Set/Delete 顺序；QuickJS fast-array memcpy/element swap 不在 generic path 复刻。未来 packed/holey
fast path 必须以无 Proxy/accessor、无 indexed prototype、可写目标和稳定 elements kind 为 eligibility，且由
M13 profile 证明收益，不能恢复 direct own-data mutation 作为所有 receiver 的默认行为。

`Array.prototype.reverse` 使用独立固定大小 `PendingArrayReverse`，而不扩张 copyWithin 的单向搬运 owner。
reverse 每个 pair 必须同时保留 lower/upper 两个 Get 结果，并在任一 setter/Proxy trap 执行 JavaScript 时保持
二者可追踪；把这套双值、双 mutation 协议塞进 copyWithin 会让 cursor commit 和 stage 合法组合无法由类型
边界表达。state 因此保存 receiver、两个 retained Value、length/lower 和两个 presence bit，不含 Vec 或随
length 增长的 backing；两个 Value mutation 都执行 generational barrier。

pair 的 observable 顺序固定为 lower Has/Get、upper Has/Get、第一 mutation、第二 mutation。present/present
执行 Set lower 后 Set upper；hole/present 执行 Set lower 后 Delete upper；present/hole 执行 Delete lower 后
Set upper；双 hole 不 mutation。lower 只能在完整 pair 成功后提交，第一 mutation 成功而第二 mutation abrupt
时不得伪装成原子交换或推进 cursor。同步完成 pair 在显式 loop 中推进，避免 host-stack recursion；只有
accessor/Proxy 路径通过 `ArrayReverseStage` continuation 恢复。QuickJS 的 fast-array 原地 swap 只证明未来
packed path 的价值，Escargot/Boa 的 generic 分支用于校验 MOP 顺序；在 M13 eligibility 完成前不复制其
storage fast path。

本批实现新增 realm-local `Symbol.iterator`、`%IteratorPrototype%`、`%ArrayIteratorPrototype%` 和
GC-managed `ArrayIteratorObject`。迭代器的 `[[IteratedObject]]`、cursor 和 kind 都属于 traced payload，
不会放进 Rust 栈或未追踪 side table；Array `keys`/`values`/`entries`/`@@iterator` 共用 creator 与 next，
creator 统一执行 ToObject，next 每步重新观察 live length。Key 模式只返回 cursor、不读取 indexed element；
Value 与 KeyAndValue 模式的 accessor length/index 通过既有 property callback continuation 恢复，后者在
data、hole 和 accessor completion 三条路径都创建 `[index,value]` intrinsic Array。RAB grow/shrink 中途
语义仍属于 ArrayBuffer/TypedArray substrate，不以普通 array-like 行为冒充完成。

`Array.prototype.flat` 不使用 Rust recursion，也不保留旧同步 `Vec<FlatWork>`：独立
`PendingArrayFlat` 保存当前 source/length/index/depth、target cursor 和显式 parent frames，每个
HasProperty/Get、nested LengthOfArrayLike、species Get/Construct 与 target Define 都通过 typed continuation
恢复。`Infinity` 是独立 boolean tag，递归下降时保持无限；有限 depth 使用 u64 并逐层递减。真正 Array 才
展开，holes 跳过但 inherited indexed property 仍由 Has/Get 观察，target cursor 只在
CreateDataPropertyOrThrow 成功后提交。QuickJS、Escargot 与 Boa 的递归 FlattenIntoArray 只作为语义顺序
参考，不复制其 C/C++/Rust host-stack 控制流。

flat frame backing 遵守 `GcExternalMemory` 生命周期内 charge 不变契约：状态持有固定长度
`Box<[ArrayFlatFrame]> + frame_count`，初始容量集中在 `tuning::arrays`，满载时分配双倍且重新计费的
replacement payload并复制 active frames，再替换 destination root。禁止对已发布 payload 内 Vec 原地
reserve，也禁止让 no-GC borrow 跨 replacement allocation。frame source、current source、species/result 与
retained value 都参加 rewrite-capable trace；12层深链在初始8帧容量后强制 replacement，并以每次分配
forced-major 回归覆盖。

`Array.prototype.sort` 的首个路径只接受默认 comparator，先 materialize primitive ToString 的 UTF-16 units，
再将 defined values、undefined、holes 分层写回；提供 comparator 时仍需通过 host/native reentry contract，
不能在 Rust sort closure 中直接执行 JavaScript。

fixed `TypedArray.prototype.sort` 的 callable comparefn 复用同一个 GC-managed bottom-up stable merge owner，
但在任何 comparator call 前从 attached backing 收集 exact-length `Box<[Value]>` snapshot。Number/BigInt element
因此不依赖 callback 后的 backing 状态；每次 compare 都发布 typed continuation 并通过 iterative JS frame
trampoline 执行，返回 object 使用 resumable ToNumber，NaN 与零都映射为 stable equality。callback 中 detach
不会留下裸 pointer/no-GC borrow；排序完成后的每个 Set 都重新解析 receiver 的 integer-indexed backing，已
detached receiver 按当前 MOP contract no-op 并仍返回原 receiver。callback throw 与 ToNumber throw 不包装，
沿显式 completion stack 保持 identity；全流程不使用 Rust unwind 或 Rust sort closure JS reentry。

fixed `TypedArray.prototype.toSorted` 不建立第二套 merge continuation。入口先验证 comparefn 与 source witness，
按 source kind/internal length 创建 active-Realm intrinsic TypedArray，不观察普通 `length`、`constructor` 或
`@@species`。为覆盖 moving GC，source 在 buffer/view 分配前写入 caller destination，分配后从 register
重新读取并 same-kind raw-copy bytes；copy 无 engine safepoint，完成后 target 才替换该 root。随后用只替换
`this_value` 的 CallSite 进入共享 TypedArray sort：default path 保留 NaN payload/signed zero，callable path
继续使用 GC-managed stable merge 与 resumable ToNumber。callback 可 detach 或修改原 source，但 compare values
来自独立 target 的 pre-callback snapshot；返回值始终是 target，source identity/backing 不被写回。

fixed `TypedArray.prototype.with` 使用五槽 `NativeCallState` 保存 receiver、initial length、actual index、
replacement 与保留槽。状态机先完成 index 的 `ToIntegerOrInfinity`，再依据 receiver `ContentType` 完成 replacement
的 `ToNumber` 或 `ToBigInt`，此后才重新解析当前 TypedArray witness 并执行 IsValidIntegerIndex；因此越界 index
不能跳过 replacement conversion，conversion 中 detach 必须映射为 RangeError 而不能泄漏 backing TypeError。
initial length 只决定负 index 基准和结果长度，current length 只决定 revalidation；结果由 active Realm 的 same-kind
intrinsic 创建，不读取 `constructor`/`@@species`。fixed backing 在 conversion 后 byte-copy source，再用已经转换的
replacement 覆盖一个 target element；state 保证 heap BigInt 跨 ArrayBuffer/view allocation 精确存活，且任何
continuation 都不保存 backing pointer。未来 RAB 接入必须保留 initial/current length 分离，不能把 fixed snapshot
假设扩散到 resize 路径。

TypedArray 大输入构造回归必须保持 oracle 与源码 completion type 一致：同时覆盖 `Array.from`、关闭
`@@iterator` 后的 array-like constructor 与恢复 intrinsic iterator 后的 iterable constructor时，fixture
以三组 predicate 的 boolean conjunction 返回，Rust harness 只能断言 `Immediate::True`。整数 bitmask 只有在
fixture 显式构造相同 mask 时才可使用，禁止只修改 harness sentinel 造成长期稳定假失败。

Number constructor 的 `isNaN`/`isFinite`/`isInteger` 只接受已经是 numeric Value 的输入，避免全局
`isFinite` 那种 ToNumber coercion；BigInt 和 Symbol 仍由后续 conversion/BigInt slices 处理。
`isSafeInteger` 额外使用共享 `MAX_SAFE_INTEGER` spec constant，避免在 predicate implementation 中复制 magic number。
Number 的规范静态常量走独立 non-writable/non-enumerable/non-configurable helper，不能复用 builtin method
的 writable/configurable descriptor；值存入 Number function object 的普通 traced property storage。
`NumberObject` 与 `BooleanObject` 是独立注册的 GC payload，内嵌 ordinary property base 与 traced
`[[NumberData]]`/`[[BooleanData]]`；
`Number.prototype` 本身以 `+0` 初始化，`new Number` 根据 observable newTarget prototype 分配 wrapper。
数字 primitive 的 property get 从该 prototype 起步，但 method receiver 仍保留原 primitive，避免临时 wrapper
allocation；`thisNumberValue` 同时接受 numeric Value 与真实 NumberObject，并拒绝其他 ordinary/exotic payload。
`thisBooleanValue` 同样只接受 Boolean primitive 或真实 BooleanObject；`Object.prototype.valueOf` 的 primitive
分支通过 ToObject 使用 truthful wrapper allocator，Boolean constructor 的 call/construct 分流不把 Boolean
data 塞入 Number/String payload。Boolean primitive property lookup 从 Boolean prototype 起步，wrapper 的
shape/storage mutation 复用 `ObjectReceiver` 的 traced barrier 分支。
该新增 wrapper 只增加 cold/object classification 的一个 typed descriptor 分支；clean HEAD
`basic/call-loop` median 4.233 ms，与前一轮 4.240/4.298 ms 在采样噪声内一致。性能结论仍必须以
满足 affinity/governor probe 的 benchmark host 为准。
`toString` 的十进制路径使用共享 canonical formatter；radix 2..36 使用独立、无 heap allocation 的
IEEE-754 adjacent-boundary formatter，2200-byte scratch 上限集中在 `tuning::numbers`。算法按相邻 double
距离停止 fractional digit emission 并 ties-to-even，覆盖所有 radix 的最大 exponent 与最小 normal/subnormal；
所有 cursor/digit 访问返回结构化 invariant error 而不 panic。非法 radix 进入真正的 RangeError abrupt
completion，不能作为 host ExecutionError 泄漏。
`toFixed` 复用 workspace 已固定的 `ryu-js::Buffer::format_to_fixed`，精度先经 primitive
ToIntegerOrInfinity 并限制 0..100，范围错误进入 RangeError；该 backend 已覆盖 NaN/Infinity、负零、
`1e21` fallback、100 digits 与规范 exactness case。Number 的 `toFixed`/`toExponential`/`toPrecision`/
radix `toString` 通过显式 native continuation 执行 object ToPrimitive(number hint)：按顺序读取并调用
`valueOf`、`toString`，primitive 结果进入既有转换路径，两个方法都返回 object 时抛 TypeError，用户 callback
异常保持原值传播。`String(value)` 复用同一 consumer，但按 string hint 执行 `toString`、`valueOf`；默认
ordinary-object `toString` 可同步完成，自定义 bytecode callback 则走相同 trampoline。VM 不从 native Rust
frame 递归进入 interpreter；`Completion::Native` 保存原 caller base、
destination、call-site、receiver、object 和 conversion stage，全部由 fiber completion root trace。callback frame
只增加 `return_continuation` 标记，当前仍为 104 bytes；返回时 trampoline 恢复 native work，throw 则沿 callback
frame 的原 call-site 直接进入 caller handler。

native continuation 必须在调用 callback 前进入 traced completion 栈，并位于 callback 自有 completion 区间
之前：随后创建的 callback frame 自然把
`completion_base` 前移一项。这样 callback 内部 catch/finally 截断自己的 completion 时不会删除 native state；
callback 返回前自身 completion 已清空，return path 可精确 pop 一个 `Completion::Native`。该不变量已由
N=1/2/4/8/16、forced-major、callback 内部 catch、外部 caller catch 和源码端到端测试覆盖。typed native
continuation 使用 4-byte closed kind 加两个 traced Value payload 的 32-byte closed struct；conversion、
PropertyGet 与 PropertySet 通过 typed constructor/reconstruction 共享布局。callee 不进入 continuation：callback
frame 发布前由 receiver 的 accessor pair 或 descriptor pending state 到 source 的 graph 保活，frame 发布后由
call frame 持有执行所需 identity/environment。ordinary accessor read/write 保留 prototype traversal 的原始 receiver；
getter 返回值写 destination，setter 返回值被忽略并恢复 RHS，missing/non-writable 的 false 只在 strict
bytecode boundary 转 TypeError。conversion method lookup 也走同一 accessor resolver，并用 Getter/MethodCall
phase 串联 accessor-valued valueOf/toString，不递归进入 Rust interpreter。Proxy/exotic 和其他 builtin
consumer 仍须复用同一 trampoline 模型逐项接入。

`@@toPrimitive` 作为 ordinary object conversion 的 exotic 首阶段复用上述 trampoline，但 getter 返回的
callable 需要更强的保根协议：parent `Conversion` 保存 consumer、pending operand、input object 和 phase；
child `ConversionCallRoot` 保存 input/callee。只有两项都进入 traced completion stack 后，预分配 hint 才可
写入 caller destination 作为单参数窗口。callback frame 返回时先消费 child、保留返回 Value，再恢复 parent；
同步 call 失败、第二项 completion limit 失败和 caller abrupt unwind 都必须对称移除两项。这样既不把
NativeContinuation 扩大到 40 bytes，也不会在 destination 从 fresh callable 改写为 hint 时丢失唯一 callee
edge。N=1/2/4/8/16、getter/method 两处 forced-major、limit 1/2、getter/method this、exact hint 与异常后再次
转换共同锁定该协议；kind 仍为 4 bytes，entry 仍为 32 bytes。

当前 well-known identity 随 isolate 的单 Realm 初始化并由 Realm root；未来增加同一 Agent 的多 Realm 时，
well-known Symbol 必须提升到 Agent/Engine 共享 identity，不能为每个 Realm 创建不同 serial。当前接入范围是
Add、relational、numeric unary/binary、loose equality、ToPropertyKey 和 String/Number call consumer；String
construct 尚未完成 wrapper boxing，Date、Proxy 与 BigInt 也未闭合。`SymbolObject` 已作为独立 GC payload
接入：`%Symbol.prototype%` 保留 absent `[[SymbolData]]`，而 `Object(symbol)` 保留 traced primitive data，二者
均走 ordinary property receiver/storage/write-barrier 契约，且 `thisSymbolValue` 严格拒绝 absent slot。上述边界必须保持显式
unsupported/后续工作，不能把本切片描述成完整 ToPrimitive。

Abstract Equality 不需要左右两个 continuation consumer：两个不同 object 只比较 identity，不做转换；
object 与 null/undefined 直接 false；因此进入 ToPrimitive 时恰有一个 object，另一 primitive 可放在既有
receiver traced slot，`Equality(opcode)` 同时保存最终是否取反。callback 返回后在独立 equality 模块迭代
执行 primitive redo：same type strict equality、null/undefined pair、Boolean 单侧变 0/1、Number/String 只
转换 String，Symbol 与异型 primitive 直接 false。不能把“任一侧 number”实现成“两侧 ToNumber”，否则
`1 == Symbol()` 会错误抛 TypeError。Hole/Uninitialized 等内部 immediate 不属于 eligible primitive，禁止由
该边界触发用户 conversion。BigInt/String、BigInt/Number 与 Annex B IsHTMLDDA 是后续显式分支，不能借普通
object/nullish 路径猜测。

computed property reference 不允许把 ToPropertyKey 隐藏在最终 Get/Set/Delete/Has 中，因为 object key 的
getter/method 可以挂起并改变后续求值顺序。compiler 因此发出两个三寄存器 prepare opcode：
`ToPropertyKey [dst, raw_key, base]` 先对 base 做 RequireObjectCoercible，`ToPropertyKeyForIn [dst,
raw_key, rhs]` 先要求 rhs 是 Object；guard 失败时绝不调用用户 callback。object key 使用
`ConversionConsumer::ToPropertyKey` 和 string hint 进入既有 trampoline，guard 放在 receiver traced 槽，
不增加 continuation Value 或扩大 32-byte entry。conversion 产出的 Symbol/string/number/boolean/null/
undefined 保持 primitive normal form，最终 property opcode 才经 `property_key()` atomize；这避免 numeric
computed key 在 prepare 与 indexed/property dispatch 之间强制分配临时 String，也为未来 array-index fast
path 保留空间。

computed method call 不能先把 `object[key]` 降为普通 value 再发 `Call`，否则 receiver identity 会丢失。
lowering 在 prepare 后把 receiver/callee/arguments 发布为连续 call window，使用 `GetByValue` 加
`CallWithReceiver`；key conversion 完成后才执行 arguments，callback throw 时 arguments 不运行。

求值顺序必须按现代规范而不是 QuickJS 的旧 FIXME：read/delete 是 base、key expression、base guard、
ToPropertyKey、operation；simple assignment 是 base、key expression、RHS、base guard、ToPropertyKey、Set，
所以 `null[key] = rhs()` 会先执行 rhs、再抛 TypeError，且不调用 key conversion；compound/logical/update 在
Get 和 RHS 前 prepare 一次并让 Set 复用同一 primitive；object literal 在 value expression 前 prepare；`in`
先求值两侧并验证 RHS Object，再转换左 key。Proxy、super/private key 与 BigInt 也保持后续显式分支。

native Object key consumer 不能在 callback 后从 bytecode call-site猜测原参数位置：bound prefix 和
`Function.prototype.call` forwarding 都会改变 argument view。`BuiltinPropertyKeyConsumer` 因此编码五种
closed finish operation；primitive key直接携带栈上两个 Value同步完成，object key才分配 16-byte
`PendingNativePropertyKey { first, second }`。conversion continuation 的 receiver保存 pending ref、object
保存 key object，仍只有两个 traced Value且 entry保持32 bytes。callback返回 primitive后必须先写 caller
destination再调用 `property_key()`，否则 fresh Symbol只存在于 Rust局部变量，define storage allocation触发
GC时会失根；define进入 ToPropertyDescriptor getter后，新的 descriptor pending state接管 target/source/key。

五个 consumer的 guard顺序不是统一策略：`Object.defineProperty`要求 Object target，
`Object.getOwnPropertyDescriptor`/`Object.hasOwn`先 ToObject/RequireObjectCoercible再转 key；相反
`hasOwnProperty`和`propertyIsEnumerable`先 ToPropertyKey，之后 nullish receiver才抛 TypeError。该差异由
consumer-specific finish保留，不能抽成一个“先 guard”布尔开关。完整 primitive String indexed descriptor和
Proxy/exotic ToObject仍是后续边界。

ToPropertyDescriptor 使用独立 cold state machine 按 enumerable/configurable/value/writable/get/set 顺序执行
HasProperty-equivalent lookup 与 observable Get。纯 data/missing 路径只使用栈上 closed partial record；首次
getter suspension 才把 target、source、PropertyKey、presence mask 和六个 partial Value 发布为 GC-managed
pending state，每个返回 edge 在继续下一次 Get 前执行 barrier。pending state 作为 continuation payload 与
caller destination 临时 root，getter 始终以原 descriptor object 为 this；abrupt completion 复用原 call-site。
FromPropertyDescriptor、hasOwnProperty 和 propertyIsEnumerable 读取统一 complete own descriptor，不再把
accessor storage 误判成 unsupported。该协议不允许递归进入解释器，也不让 data-only defineProperty 支付
pending allocation；Proxy/exotic 接入后必须替换 lookup resolver，不能复制第二套 descriptor parser。

Number call 与 construct 也复用 number-hint consumer。call 在 conversion 后直接发布 primitive Number；construct
先完成全部 callback/abrupt 语义，再按 observable `newTarget.prototype` 分配 NumberObject，不能在 conversion 前
留下可观察 wrapper。continuation 的 `construct` 标记占用现有 padding，NativeContinuation/Completion 仍为
32 bytes，Frame 仍为 104 bytes。call/construct callback throw、object fallback 和 wrapper brand 均有源码回归；
通用算术与关系 opcode 尚未接入 object conversion，不能让这些 opcode 递归执行 JS callback。

`ConversionConsumer` 将 continuation 从 native-function identity 解耦为 closed operation identity，当前区分
native call/construct、numeric unary/binary、Add、relational、equality 与 ToPropertyKey；它与 stage 正好占满
NativeContinuation 的既有 padding，因此 continuation/Completion 仍为 32 bytes。这些 opcode 的 primitive
fast path 保留在 interpreter dispatch 内，ToPropertyKey 的 non-heap primitive 也在 verified hot kernel 原样
move；只有确认 object 后才调用共享 cold/noinline continuation builder，
避免常见 numeric unary traffic 支付额外函数调用。object 分支按 number hint 执行 callback、fallback、内部
catch 与原 call-site throw，consumer 在 callback 返回后只执行一次最终 ToNumber/operation；已有
N=1/2/4/8/16、forced-major 和源码端到端覆盖。binary/relational opcode 接入 operation consumer 时必须
逐个保留求值顺序与中间 primitive root，不能用同步 `convert_to_number` 假装完成。

URI globals use the same conversion boundary rather than a synchronous object shortcut. `GlobalUriFunction`
is a four-entry closed identity table (`decodeURI`, `decodeURIComponent`, `encodeURI`,
`encodeURIComponent`); the native enum and realm publication table carry only identity and function
metadata, while `builtins/uri.rs` owns the algorithm. The string-hint conversion consumer is extended for
these entries, so an object argument observes `toString`, `valueOf`, `@@toPrimitive`, accessor and callback
throws in the existing resumable order. URI malformed input is a structured `InvalidUriEncoding` execution
error mapped at the VM boundary to the managed `URIError`; no Rust unwind or new continuation field is used.

Encoding validates UTF-16 surrogate pairs and computes the exact encoded length before one allocation;
decoding reserves the input length because
decoded UTF-16 cannot exceed it. Percent bytes are parsed structurally, overlong encodings, surrogate code
points and values above `0x10FFFF` are rejected, and `decodeURI` preserves percent escapes for the ECMAScript
reserved set while the component variant decodes them. Batch regression runs cover dispatch `N=1/2/4/8/16`.

Generic String prototype algorithms must convert `thisValue`, not argument zero and not only branded
String/StringObject receivers. The native conversion entry therefore accepts an explicit operand and final
receiver: existing argument consumers and the new String-receiver consumer share the same continuation
publisher, while no Frame, Completion or GC payload field is added. Nullish receivers fail
RequireObjectCoercible before conversion; object receivers use the existing string-hint observable order and
primitive receivers finish synchronously. This boundary is reusable by subsequent generic String builtins.

Default case conversion does not carry ICU into the engine binary. Valid UTF-16 is converted through Rust's
Unicode default upper/lower algorithms, including multi-code-point mappings and contextual Final Sigma, then
materialized into one exact-capacity UTF-16 allocation. Unpaired surrogates split valid segments, are copied
verbatim and prevent case context from crossing the invalid code unit. Generic ToString rejects Symbol;
`String(symbol)` alone explicitly invokes the canonical `Symbol(description)` formatter, matching the
ECMAScript distinction instead of globally weakening ToString.

`%String.prototype%` itself is an old-generation `StringObject` whose traced `[[StringData]]` is the canonical
empty string, not an ordinary object with methods attached. Constructor publication and the existing
String-exotic property dispatcher retain it through the realm graph. This makes direct generic calls on the
prototype observe the required empty primitive while preserving the same shape/storage mutation contract as
constructed String wrappers. Both `string_prototype` and the staged `global_uri_functions` table are explicit
Realm trace roots because intrinsic construction can allocate before global publication makes those values
reachable through the final object graph.

`toLocaleUpperCase` and `toLocaleLowerCase` are separate native identities even while they share the current
locale-insensitive Unicode kernel. This preserves function names, descriptors and a future locale-aware
dispatch point; it deliberately does not claim Turkish/Azeri/Lithuanian locale semantics without a locale
provider and canonical locale-list implementation.

Oxc stores directive-prologue string literals in `Program.directives`, outside `Program.body`. Tachyon's owned
HIR now prepends those directives as `StatementCompletion::Value` String expressions; strictness remains a
scope capability, so this does not conflate directive execution with parser policy. This is required for
ECMAScript Script/Eval completion: a source consisting only of a string directive still completes with that
String value.

Owned HIR string expressions store `Arc<[u16]>`, not `Arc<str>`. Oxc 0.140 marks `StringLiteral` and cooked
template elements with `lone_surrogates`; in that cold case its arena string encodes every exact code unit as
`U+FFFD` followed by four hexadecimal digits, including a real replacement character as `U+FFFDfffd`.
Lowering decodes that documented representation while the arena is alive and publishes exact UTF-16 units to
`BytecodeConstant::String`; it does not reparse raw source spelling or attempt to recover surrogates from a
lossy Rust string. Malformed upstream sentinel data becomes a structured compile error, and all temporary unit
buffers use fallible exact reservation. Static identifier/numeric/well-formed string keys remain scope-name IDs;
a string-literal object/class/pattern key containing a lone surrogate is represented as a constant computed key,
so runtime `ToPropertyKey`/SetFunctionName-by-value semantics remain exact without admitting invalid Unicode into
the `Arc<str>` scope-name and atom-loading boundary.

Host `$262.evalScript` runs nested code on the explicit suspended-fiber path. A nested thrown value is not
returned as data: the callback reports `ExecutionError::HostThrown(Value)`, and the outer interpreter handles
that marker at the dispatch boundary by invoking the normal `throw_value`/handler machinery. The marker is
internal control flow for host callbacks, not a user-visible Rust unwind or a replacement for direct-eval
lexical environment binding; those dynamic scope semantics remain a later M5.3 slice.

Direct eval now has a distinct verified opcode selected only for an unresolved syntactic `eval(...)` call. At
runtime it first checks that the callee is the current Realm's eval intrinsic; an alias, comma call, overwritten
binding, or foreign-Realm eval falls back to the ordinary indirect call path. A direct-eval-capable activation
forces every observable parameter/var/lexical binding into its existing exact-size environment layout. The
environment stores only cold immutable-code owner identity `(CodeId, FunctionId)`; dynamic eval name access walks
the caller chain and maps names through `CompiledFunction::environment_slots`, so local names are neither copied
per activation nor permanently interned. A Fiber-level `dynamic_scope` gate is enabled only for direct-eval/debugger
execution, so ordinary unresolved global access never scans closure environments; the 104-byte hot Frame remains
unchanged. The caller fiber remains a
traced suspended root while the eval entry frame inherits that environment chain; direct slot bytecode outside
eval therefore keeps its precomputed depth.

This slice deliberately does not make exact-size environments grow for sloppy eval declarations. New eval
`var`/function bindings require a sparse activation-aligned variable-environment overlay (or an equivalent stable
indirection) so they survive eval completion without inserting a new node ahead of depth-encoded local slots.
Strict eval additionally needs its own declarative record and caller-strictness inheritance. Until those records,
`with`, and Annex B invalidation exist, direct eval completion remains partial even though caller binding read/write
and direct-versus-indirect identity are implemented.

Both direct and indirect eval perform the specification's Type check inside the VM before invoking the host compile
callback. A non-String argument is returned unchanged, including object and Symbol identity; eval never applies
ToString to it. This keeps the callback contract restricted to actual source text and prevents host adapters from
silently changing observable eval semantics.

`EvalKind::Direct` carries the active caller's strictness to the host compile boundary. Because Oxc strict early
errors must run during parsing/semantic analysis, the current compiler adapter prepends `"use strict"; void 0;` for
an inherited-strict eval: the directive selects strict grammar and the explicit undefined-valued statement prevents
the synthetic directive from becoming the completion of empty or declaration-only source. Compiler diagnostics map
to `InvalidEvalSource`, which the interpreter converts to the Realm's managed SyntaxError through normal abrupt
completion; compiler feature gaps remain unsupported rather than being mislabeled as syntax errors. This strictness
contract does not itself provide strict eval's isolated declaration environment.

Sloppy direct eval declarations use a sparse var-object analogue rather than resizing ordinary environments.
Before execution, the VM scans the verified entry bytecode twice for `DeclareScope`: the first pass computes the
exact slot count and the second resolves already-loaded scope atoms into one `EvalVar` record. Existing caller
bindings remain in their direct slots; only new var/function names occupy the overlay. A sparse Fiber vector roots
the overlay by owner frame depth, so it survives eval completion but is discarded on frame exit or tail replacement.
The sparse Fiber roots define owner boundaries: declaration checks walk the current-depth head only until the nearest
ancestor head, while ordinary dynamic reads continue through the complete parent chain. Keeping owner depth out of
`Environment` avoids inflating every ordinary environment allocation while preserving nested-eval shadowing. Nested
closures therefore see eval-created vars without changing depth-encoded environment operands or the 104-byte Frame.

Strict eval receives the same exact record only on the child eval Fiber, so var/function declarations disappear at
completion; sloppy global eval bypasses overlays and retains the Realm global var environment. `strictEval` is the OR
of inherited caller strictness and the compiled eval entry's own directive strictness. Function declarations now emit
an explicit `DeclareScope` before closure creation, keeping declaration discovery verifier-driven rather than inferred
from stores. For functions with parameter expressions, immutable environment-slot metadata carries a cold
`parameter` bit. Sloppy direct-eval declaration instantiation consults this bit before reusing a named caller slot:
crossing the parameter environment with a colliding `var`/function name produces managed SyntaxError before eval
execution, while simple parameter lists continue to reuse their ordinary binding and strict eval remains isolated.
This adds no field or branch to the hot Frame/environment access path and remains robust if lowering later
interleaves self, arguments, or parameter slots. Lexical eval declarations, deletability/configurability, the
remaining nested parameter-expression environment interactions, `with`, and Annex B still require their dedicated
environment semantics.

首组 pure-numeric binary opcode（Sub/Mul/Div、bitwise and/or/xor、三种 shift、Remainder、Exponentiate）
使用 `BinaryLeft(opcode)`/`BinaryRight(opcode)` 两阶段 consumer。左侧 callback 挂起时，continuation 既有
`receiver` 槽 trace 已求值但尚未转换的右 operand；左侧完成后若右侧也是 object，状态机原地把 `receiver`
改为已转换的左 Number、`object` 改为右 operand，并继续同一个 loop，不递归进入 Rust。由此 continuation
仍为 32 bytes，且左 callback 对右 object 的 `valueOf`/`toString` 修改会被随后 lookup 观察到；左侧 abrupt
不会开始右侧转换。Add 的 default-hint/string concatenation 与 relational 的 string comparison 不能复用
只执行 ToNumber 的 binary consumer，必须各自保留 primitive kind 后再决定最终操作。

Add 使用独立 `AddLeft`/`AddRight` consumer，以 ordinary default hint 的 valueOf/toString 顺序得到两侧
primitive；任一侧为 String 时才对两侧执行 ToString 并连接，否则执行 ToNumber 后走原 numeric Add。
continuation sentinel 在 method property lookup 前进入 traced completion stack，因而左 callback 返回的临时
GC String 在右 method lookup、native/bytecode callback 和 forced-major 中始终可达；左 abrupt 仍阻止右转换。
拼接先计算两侧精确 UTF-16 code-unit 长度并一次 `try_reserve_exact`，结果全部位于 Latin-1 时压缩为单字节
owned backing，含宽 code unit 时直接接管 owned UTF-16 Vec；number+number 在 dispatch 中保留原 i32/f64
fast path。N=1/2/4/8/16、forced-major、mutation/fallback/throw/Symbol TypeError 均覆盖，addition test262
从 25/95 提升为 59/95（34 fixed、0 broken）。relational string comparison 必须使用独立 consumer；
`@@toPrimitive`、accessor/exotic property lookup 与 BigInt 分别由后续对应 substrate 接入。

四个 relational opcode 使用 `RelationalLeft(opcode)`/`RelationalRight(opcode)`，与 Add 一样保留两侧
ToPrimitive(number hint) 的 primitive kind 和原始左到右 callback 顺序；`>`/`<=` 只在最终比较方向上反转，
不能按规范伪代码中的参数排列反转可观察转换顺序。两侧都是 String 时直接借用两个已 root `JsStringView`，
按 UTF-16 code unit 排序且不分配；否则两侧各执行一次 ToNumber，再走 `<`/`>`/`<=`/`>=` numeric fast
comparison。number+number 仍在 dispatch 内直达 numeric helper。双 String callback、fallback/mutation/abrupt、
N=1/2/4/8/16 和 forced-major 均覆盖；test262 `<` 53->71/89、`>` 47->79/97、`<=` 45->79/93、
`>=` 45->71/85，合计净增 110 pass。每组剩余唯一 semantic-failure 文件依赖尚缺 direct eval，unsupported
全部为 BigInt；不能用发布一个假的 global `eval` 绕过 M5 direct-eval scope/strictness 语义。

`toExponential` 不使用 Rust exponential formatting，因为其 decimal midpoint 使用 ties-to-even，与
ECMAScript 选择较大整数的规则在 `25 -> 3e+1` 等边界不一致。最短形式只规范化 pinned `ryu-js`
round-trip digits；显式精度把 binary64 解码为 `mantissa * 2^exponent`，用 32 个 `u32` limb 的栈上
numerator/denominator 精确除位并 ties-up。101 significant digits、112-byte output 与 limb 上限全部集中在
`tuning::numbers`，所有 cursor/limb overflow 都返回结构化 formatter error，不分配 heap bigint。
ryu shortest 的展示 exponent 不能直接作为显式精度算法的数学 exponent：例如 binary64 `1e-21`
实际略小于该十进制边界，但 shortest round-trip 仍打印 `1e-21`。显式精度路径用 exact ratio 将候选
校正为 `floor(log10(x))`，低精度舍入可能再 carry 回展示 exponent；undefined shortest 路径保留 ryu
选择。`toPrecision` 复用同一个 significant-digit generator，并在 carry 后的 exponent 上按
`e < -6 || e >= p` 选择 exponential，否则直接从 digits 渲染 fixed，不调用 `toFixed` 的 `1e21` fallback。

首个 `SymbolValue` 是非 ObjectReceiver 的 GC-managed primitive，每次 `Symbol()` 分配唯一 identity，
`typeof` 返回 `symbol`，构造调用被拒绝，ToNumber 明确进入 TypeError；description 只是 traced GC edge。
该 substrate 用来消除 Number brand/conversion tests 被缺失全局 Symbol 假通过的问题，并已作为 ordinary
`PropertyKey` 接入 dynamic get/set/delete/has 与 descriptor/hasOwn/assign 路径。随后接入 prototype methods、
description、global registry、well-known symbols 和 GC-managed `SymbolObject` boxing；Reflect own-key API、
完整 descriptor/cross-realm 与其余 Symbol consumers 仍由 M8 Symbol package 闭合。

`Object.getOwnPropertyNames` 在 ordinary shape keys 之外会发布 callable 的虚拟 `length`/`name`，以及可构造
函数的虚拟 `prototype` key；shape tombstone 优先，删除后的 metadata 不会被重新注入。该 API 明确过滤
Symbol key，Object.keys/values/entries 同样只遍历 string keys，Object.assign 则保留 enumerable Symbol。
这里仍未建模 `arguments`/`caller` 等 legacy poison-pill properties，也未实现 integer-index 排序或
Object.getOwnPropertySymbols/Reflect.ownKeys。

Oxc arrow expression 已复制为 owned function stencil：表达式 body 在 HIR 中显式转成 `Return`，block body
复用 ordinary function lowering。unresolved `arguments` reference 按 Oxc scope tree 绑定到最近 non-arrow
activation；仅该 owner 分配 synthetic environment slot，并在 parameter initialization 后物化一次 arguments
object，任意深度/逃逸 arrow 都走普通 environment capture。sloppy mapped arguments 在 owner frame 退出前将
仍有效的 parameter-map values 同步到 own properties 并清除 mapping metadata，不能悬挂 register window。
该设计不扩大 hot Frame 或 FunctionObject；当前调用 frame 仍按 ordinary `this` 规则执行，因此 lexical
`this`、`super`、`new.target` 和 arrow-specific constructability 不能据此视为完成。

member HIR 覆盖非 optional `object.name` 和 `object[key]` read/assignment/update。computed reference 在
最终 property opcode 前显式执行一次 prepare，compound/update 的 Get 与 Set 复用同一转换结果；
`GetById/SetById` 的
property operand 索引 immutable module scope-name table，运行时 load module 时转为 isolate `AtomId`；
missing property 沿 ordinary prototype chain 查找，null 终止后返回 undefined。`CallWithReceiver` 使用连续 `[receiver, callee, arguments...]`
window 并把 receiver 写入 callee frame 的 `this_value`，避免把 method call 错降为普通 `Call`。
`GetByValue/SetByValue` 通过统一 `PropertyKey` 接受 string、Symbol、number、boolean、null 与 undefined；
int32 key 用 11-byte stack buffer 产生十进制 code units，AtomTable 先以 borrowed Latin-1 view probe，
命中不构造 owned candidate，miss 才 fallibly 分配并受 atom quota。heap Symbol 先完成 descriptor/liveness
验证，再读取永不复用的 serial，不能把任意 heap object 当成 Symbol key。
object/key expression 在 RHS 前各求值一次，但 simple assignment 的 ToPropertyKey 在 RHS 后执行；
compound/update 不重复 reference side effect。ArrayObject 接入后合法 array index 必须在 atom 化前进入
indexed fast path。BigInt、Proxy/super/private key、String/Boolean object boxing 和完整 primitive receiver
property semantics 仍是明确缺口；在这些完成前，
该路径只代表 ordinary own data-property substrate。

plain object literal 目前使用 owned `HirObjectProperty { key, value }`，接受 `PropertyKind::Init` 的
identifier/string/numeric static key；numeric key 在 HIR copy 时用与 VM 相同的 ECMAScript number formatter
canonicalize，因此 `0x10`/`1e2` 分别成为 `"16"`/`"100"`。每个 literal evaluation 先 `CreateObject`，再按源码顺序对同一 receiver 发出
`CreateDataPropertyById`。computed key 使用 `HirObjectPropertyKey::Computed`，先执行 key expression、string-hint
ToPropertyKey，再执行 value，
sequence expression 则保留全部 owned operands，按源码顺序发射 expression lowering，并将最后一个 register
作为结果；前项不能被 constant-fold 或丢弃，因为其 assignment/call 可能 observable。
通过 `CreateDataPropertyByValue` 进入同一 ordinary data-property definition。两种 opcode 都创建
writable/enumerable/configurable 全 true 的 own property，不查询 inherited accessor/setter；重复 key 仍按
源码顺序重定义当前 own property。VM 的 dynamic PropertyKey 支持 rooted
string、Symbol 与 primitive number/boolean/null/undefined：string 在禁止 GC 的 typed borrow中复制 exact
code units 后才进入 atom table，避免缓存 payload pointer 或在 atom table 中持有 heap borrow。重复 key 的
后写覆盖、key/value 左到右副作用和普通 shape transition 复用已有 data-property contract。spread、
BigInt/Proxy/super/private ToPropertyKey、getter/setter 仍在 HIR boundary 或 VM slow path 明确拒绝；
后续 descriptor/accessor 工作必须引入
独立 property definition/completion 路径，并保持普通 object literal fast path 不携带 accessor 分支。

array literal data elements 不复用 assignment opcode。compiler 对真实元素发出
`CreateDataPropertyById/ByValue`，VM 以 writable/enumerable/configurable 全 true 的 own descriptor 定义，
因此不会查询或调用 `Array.prototype` 上同名 inherited setter/accessor；这与 QuickJS
`OP_define_array_el` 和 Escargot Array `defineOwnProperty(AllPresent)` 的语义边界一致。当前 HIR 为准确表示
尾部 elision 仍附加 synthetic `length` property；该 bookkeeping property 必须继续走 `SetById` 和 Array
`[[Set]]`，不能走 CreateDataProperty，因为 Array 自带的 length 是不可配置 exotic property。未来若 array
HIR 改为显式保存 literal length，应删除 synthetic property，但真实元素的专用 own-definition opcode 不变。

ordinary property read 先查当前 shape/storage，再沿 `OrdinaryObject.prototype` 迭代；null 终止链。
`OrdinaryObject` 在 ShapeId 后的 64-bit alignment padding 中保存 object-local `extensible` bool，
`repr(C)` 与 layout test 固定当前 payload 为 24 bytes；FunctionObject 继续复用内嵌 ordinary base。
`Object.preventExtensions` 只翻转该状态，不分配、不触发 barrier；新增 own property 在 non-extensible
receiver 上返回统一失败，bytecode assignment boundary 按 active function strictness 将其转换为 sloppy
静默失败或 managed TypeError，defineProperty/Object.assign 等 throwing caller 保留 TypeError。该状态不能
塞进共享 ShapeId，也不能用 logical-address side table，否则会污染 IC identity或给每次 property add 增加
额外查找。seal/freeze、Proxy/exotic internal method 与完整 descriptor compatibility 仍是后续边界。

ordinary data descriptor 使用 compact `PropertyAttributes` 的 writable/enumerable/configurable 三位；
Shape 分离 zero-based property slot 与 property_count。重配已有属性时追加 immutable overlay transition，
复用原 slot 与 fixed storage；普通 delete 是 cold structural mutation，重建只含 retained properties 的
共享 shape 并一次发布 exact compact backing，最后一个 property 删除后恢复 empty shape/无 storage。
lookup 取最近 overlay，own-key materialization 反向扫描一次 shape chain，
写入 exact-capacity `Box<[Option<PropertyKey>]>` 后按 slot 顺序零额外分配迭代；iterator 先输出 Atom、
再输出 Symbol，并在两组内保持插入顺序。该 materialization 是 ownKeys/enumeration cold path，普通 lookup
不支付 snapshot allocation；算法保持 O(shape depth + property count)，不能退化成逐 slot 回溯的 O(n^2)。
integer-index Atom 的 numeric ascending partition 尚未实现，因此这里不宣称完整 ECMAScript property order。

`DataPropertyDescriptor` 的每个字段用 Option 保留 absent 与 present-undefined 的区别；parser 从 descriptor
prototype chain 读取 value/writable/enumerable/configurable，data compatibility 独立于 shape/storage mutation。
新 descriptor property 的 absent flags 默认 false；non-configurable property 禁止 configurable/enumerable
冲突，non-writable property只允许 SameValue value 与保持 non-writable。Object.getOwnPropertyDescriptor
创建结果前先把可能新分配的 native name Value 写入 caller register 临时保根，结果对象随后占用同一 traced
destination，再立即发布 value slot，不能跨 GC allocation 持有未根 Value。Accessor descriptor 在 getter/
setter storage、调用与 conversion 完整接入前返回明确 unsupported。Function virtual name/length 以
non-writable、non-enumerable、configurable 默认值暴露；ordinary shape slot 优先于 virtual fallback，首次
defineProperty 将 override 物化到共享 property storage。函数 metadata 尚未像 Escargot 一样预置于共享
function shape，因此删除 shape 外 virtual key 暂时仍以 Hole marker 抑制 fallback；重新创建会先 structural
remove marker 再 append。该例外必须在 reserved function-key shape 接入时删除，不能重新扩散到普通属性。
prototype 仅属于 constructible callable，保持 writable、non-enumerable、
non-configurable 的独立 lazy slot。

普通 bytecode function 在 inline lazy slot 中保存可观察的 `prototype` property；`CreateClosure` 不创建
prototype object。第一次读取、construct 或 instanceof 才一次物化默认 prototype，并用预构造单槽
storage 建立 `prototype.constructor` 回指；直接赋值 prototype 只更新 inline slot，不先物化默认对象。
所有 prototype/storage/value edge 参与 pending-payload tracing 或 write barrier；forced-major 覆盖 lazy
prototype 创建与 receiver 链发布。该生命周期与 Escargot `ensureFunctionPrototype` 一致，避免每个普通
closure 额外产生 prototype object 和两次 property-storage allocation。
当前没有公开 setPrototypeOf，因此不会产生 prototype cycle；接入原型 mutation 时必须先拒绝 cycle。

`FunctionObject` 内嵌 `OrdinaryObject` base，而不是维护第二份函数属性 map；lazy prototype slot 是
constructible function 的规范专用字段，不是任意属性容器。descriptor/ownKeys 接入时必须把该 slot 暴露
为 writable、non-enumerable、non-configurable 的 own property并与 redefine/delete 规则统一。property resolution 先按
GC descriptor 恢复具体 `OrdinaryObject` 或 `FunctionObject` typed reference，再读取共享 shape/storage
snapshot；新增 backing 时分别通过具体 payload 的 typed borrow 更新 edge，并以实际 receiver 作为 barrier
source，不用 raw cast/unsafe 抹平类型。FunctionObject trace 同时访问 captured environment 与 ordinary
storage。scalar property 在 N=1/2/4/8/16 下与普通对象对拍，callable→storage→heap value 的 forced-major
fixture 验证 pending allocation roots、trace 和 barrier。

anonymous ordinary function expression 复用 owned `HirFunction` stencil 与 `CreateClosure`，其 name 明确为
`None`；declaration name 为 `Some`，不再用空字符串冒充匿名。function body lower 完成后才按当前 vector
length 分配 stencil ID，因此先发现的 nested function 排在 parent 前，二者仍满足 module
`FunctionId(stencil + 1)` 稳定映射。当前 named function expression 明确 unsupported，因为其 immutable
self-binding 需要 function environment；arrow/generator/async、lexical capture 和 constructor/new.target
同样不能从“可以创建匿名 closure”推断为已支持。

ordinary `NewExpression` 采用 `Construct(destination, callee, argc)`，callee 后是 verifier 证明连续的参数
window；callee 与全部 argument 在 receiver allocation 前按源码顺序各求值一次。VM 先验证 executable 是
当前可构造的 bytecode FunctionObject、`NativeFunction::is_constructor()` 或向其 target 委托的 bound exotic
（native prototype methods 明确拒绝 construct）。bytecode constructor 读取 effective newTarget 当前的
`prototype` data property，并以 object 值作为新 receiver 的 prototype，再 managed-allocate ordinary receiver，
并把 receiver、constructor
new.target 与 optional construct receiver 一起写入显式 frame。`LoadThis/LoadNewTarget` 只读取 active frame；
普通 Call 明确写 undefined new.target 且无 construct receiver。constructor Return 若值是当前已注册的
object/function payload则替换 receiver，primitive/undefined 则回退 receiver；throw 沿相同显式 handler/
frame 路径传播。frame trace 覆盖 this/new.target/construct receiver，forced-major fixture 验证构造期间
property backing allocation不会回收 receiver。

`InstanceOf` 对 RHS 做 callable 检查，bound RHS 先迭代解包到 ultimate target，再读取当前 constructor
prototype，并从 LHS 的直接 prototype 开始迭代 identity 比较；primitive LHS 返回 false。当前尚未实现
`@@hasInstance`、Proxy trap 与跨 realm GetFunctionRealm。

derived class constructor 使用 immutable bytecode `FunctionKind::DerivedClassConstructor`，不在每个
`FunctionObject` 增加 runtime flag。`CreateClass` 一次求值 heritage，建立 `C.[[Prototype]] = superclass`、
`C.prototype.[[Prototype]] = superclass.prototype` 与 `C.prototype.constructor = C`；公开 `prototype`
保持 non-writable/non-enumerable/non-configurable。class 普通 call 直接 TypeError。derived frame 的 `this`
以内部 `Uninitialized` immediate 开始，`LoadThis` 在 `super()` 前抛 ReferenceError，`InitializeThis` 只允许
一次；return object 直接替换，undefined 回退已初始化 this，其他 primitive 抛 TypeError。`super()` 每次从
active class function 当前 `[[Prototype]]` 动态读取 superclass，并转发 active `new.target`，因此
`Object.setPrototypeOf(C, Other)` 后不会使用 class evaluation 时缓存的旧 constructor。只有 derived/base
class constructor call 才分别进入 sparse `derived_activations`/`base_class_activations` side vector，普通
call/frame 不增加字段或 push/pop。

当前 class frontend 接受 base/derived class 的唯一显式 instance constructor 或 synthetic default constructor，
以及静态名称的普通 instance/static methods 与 getter/setter。base constructor 使用独立 immutable
`FunctionKind::BaseClassConstructor` 与 `CreateBaseClass`，普通 call 同样 TypeError；construct 复用普通
receiver-first 状态机，所以 object return 替换 receiver，primitive/undefined 回退 receiver。base class 直接发布
`C.[[Prototype]] = %Function.prototype%`、`C.prototype.[[Prototype]] = %Object.prototype%`，公开 `prototype`
与 derived class 一样 non-writable/non-enumerable/non-configurable。method
function 使用独立 `FunctionKind::ClassMethod`，严格模式、无默认 `prototype`、不可 `new`；
`DefineClassMethodById` 只负责 closure 已发布后的非枚举 data descriptor，普通 `SetById` 热路径不增加分支。
`DefineClassGetterById`/`DefineClassSetterById` 对 class accessor 使用相同 closure/root 协议，但固定
`enumerable: false, configurable: true`；class method/constructor 在发布时保存 `[[HomeObject]]`，并由
`LoadSuperBase`、`GetSuperById`、`GetSuperByValue` 在执行时从 active function 解析动态 superclass，同时
保留当前 frame 的 `this` 作为 receiver。构造器 activation 与 method HomeObject 复用既有 frame slots/稀疏
side vectors，不扩大 `Frame` 或普通调用热路径。
computed public key 已升级为 owned `HirObjectPropertyKey::Computed`，按源顺序执行 `ToPropertyKey` 后使用
`SetFunctionNameByValue` 与对应 `*ByValue` descriptor opcode；Symbol name 采用规范 `[description]` 拼接，
name publication 使用 fresh-property path 以保证 forced-major 时新字符串被 `PropertyMutationRoots` 保持。
public static fields 使用 ordered `HirClassElement`，不能拆成 methods/fields 两张丢失 source order 的表。
每个 initializer 是独立 `FunctionKind::ClassFieldInitializer` hidden stencil：strict、non-constructible、无公开
prototype，以 class constructor 作为动态 `this` 与 `[[HomeObject]]`，因此 static `super` 继续通过 home object
当前 prototype 解析。class evaluation 先为全部 elements 求 computed key 并创建 closure record，之后才执行
static initializers；`DefineFieldById/Value` 创建 writable/enumerable/configurable own data property，不复用
assignment。Oxc 不把 PropertyDefinition value 建成 function scope，因此 compiler 单独发现 initializer direct
references，把 outer parameter/let/var 提升到 environment；class-name binding 仍归 class environment，不因共享
BindingId 被误提升。该 static-field 纵切的 N=1/2/4/8/16、class ordinary
call/return/TDZ/dynamic-super、method descriptor、super property 与
forced-major 已覆盖，不能据此把完整 class 或 M5.3 construct 总项标为完成。

public instance fields 复用 ordered `HirClassElement` 的 key 求值阶段，但不在 class evaluation 时执行
initializer。每个 constructor stencil 冻结 `initialize_instance_elements`，base constructor 在
parameter/default/body 前执行 `InitializeInstanceElements`；derived constructor 仅在首次成功
`InitializeThis` 后执行，因此重复 `super()` 在字段重复初始化前抛 ReferenceError，而 `super()` 前返回 object
不会运行字段。instance initializer 的 `this` 是新实例，`[[HomeObject]]` 是动态 `C.prototype`；字段使用
CreateDataProperty 等价的 writable/enumerable/configurable own descriptor，不经过 inherited setter。

class evaluation 把每个已规范化 key、payload closure、infer-name bit 与 closed element kind 写入连续四寄存器
record window，verified `AttachInstanceFields` 检查 `base + count * 4` 不溢出且窗口末端在 register file 内。
VM 随后一次性复制为
exact-capacity `Box<[ClassInstanceElementRecord]>`；`ClassInstanceElementPlan` 精确报告 external memory 并 trace
key/payload。
只有带 instance fields 的 constructor 才把普通 bytecode executable 升级为 16-byte rare
`ClassBytecode(GcRef<ClassConstructorData>)` payload，payload 保存 code/function/environment/plan；这保持
`FunctionExecutable` 16 bytes、`FunctionObject` 56 bytes 与 `Frame` 104 bytes，普通 function/call 热路径不承担
字段 vector 或 cursor。

`InitializeInstanceElements` 是迭代、可恢复的 VM 状态机，不通过 Rust 调用栈递归推进。每次运行 initializer 前
push `InstanceElements(Initializer)` continuation；返回值先写入 caller scratch register，再 push traced
`InstanceElements(Define)` continuation，之后才执行可能分配的 anonymous `SetFunctionName` 和 field define。
这条 root 规则保证 hidden frame 已退出后，forced-major 仍不能回收 initializer 返回的 function。普通 receiver
同步 define 后递增 cursor；Proxy receiver 把既有 resumable `[[DefineOwnProperty]]` continuation 嵌套在 parent
Define stage 下，trap 返回后只推进一次。initializer/define abrupt completion 会丢弃 continuation、保留已经定义的
字段和 derived 已绑定 `this`，constructor 内 catch 可继续观察该部分状态。

instance private data field 使用 module-local `HirPrivateNameId { class, element }` 表达词法 identity；每次
class evaluation 为每个 private declaration 分配 fresh Symbol payload，并把它写入 class lexical environment
的精确 slot。运行时 key 进入独立 `PropertyKey::Private(SymbolId)` 域，不可与公开 Symbol/string 伪造或碰撞；
private get/set/define 只查询 receiver 自身隐藏 shape slot，不走 prototype、ordinary descriptor API 或 Proxy
trap。`OwnPropertyKeys` 在 public key publication 前过滤 private key，`PropertyStorage` 同时 trace private
Symbol identity 与 value。普通对象沿用现有 shape/storage；没有 ordinary base 的 Proxy 仅增加 optional
private sidecar edge，第一次 private define 才分配 null-prototype ordinary backing，并以显式 allocation roots
和 write barrier 发布，revocation 同时清除该 edge。ordinary receiver 的 private define 遵守
`[[Extensible]]`，而 Proxy identity 的独立 sidecar 保持可扩展且不查询 target；重复 define、
错误 receiver brand 和缺失 slot 均转为 TypeError。当前已覆盖 instance data declaration/default/initializer、
read、simple/compound assignment、prefix/postfix update、nested closure、nested class shadowing、outer private
capture、non-extensible rejection、Proxy bypass、N=1/2/4/8/16 与 forced-major；static private fields、private
methods/accessors 和 `#x in object` 留给后续纵切，因此完整 class semantics 与 M5.3 construct 总项继续不标记完成。

synchronous instance private methods 与 fields 共享 `ClassInstanceElementPlan`，但 record 通过闭合的
`ClassInstanceElementKind` 保留语义类别，不能退化成多个 boolean。class evaluation 创建 private method closure
一次，设置 `#name` 和 prototype `[[HomeObject]]`，再把 exact-capacity private-method 与 field 两段队列冻结成
“methods first, fields in source order”的四槽 record window；bytecode builder、verifier 与 VM 均以 `count * 4`
验证边界。初始化把同一 closure identity 作为 non-writable hidden private slot 写入每个 instance，因此 private
assignment/update 抛 TypeError，而 private data fields 仍可写。private-member call 必须直接形成
`receiver/callee/arguments` contiguous window 并发出 `CallWithReceiver`；先通过普通 `GetPrivate` 再发 `Call` 会
丢失 reference base，使方法内 `this` 错绑。该路径不新增 opcode、Frame 字段或热对象布局，并覆盖 Proxy
sidecar、ordinary non-extensible rejection、dynamic `super`、initializer-before-textual-method、共享 identity、
N=1/2/4/8/16 与 forced-major。private accessors、static private elements 与 `#x in object` 保留后续独立 kind。

synchronous instance private accessors 在 HIR 中把同一 lexical private name 的 getter/setter 合并为一个
`HirPrivateAccessor`，避免运行时以两个 slot 表示一个规范 private element。class evaluation 分别创建 strict
`ClassMethod` closure、设置 prototype `[[HomeObject]]` 和 `get #name`/`set #name`，再通过 cold
`CreateAccessorPair` opcode 分配一个 shared `AccessorPair`；缺失一侧规范化为 `undefined`。四槽
`ClassInstanceElementPlan` 新增闭合的 `PrivateAccessor` kind 并保存 shared pair，实例初始化只把 pair 作为
`PropertyKind::Accessor`、non-writable hidden private slot stamping 到 receiver，因此每个实例不重复分配 pair，
也不会把 private accessor 暴露为 ordinary descriptor。ordinary non-extensible receiver 拒绝新增 brand slot，
Proxy 继续使用 private-only sidecar 并绕过 target/traps。

`GetPrivate` 与 `SetPrivate` 不新增专用 call frame 或递归进入 Rust，而是把 accessor lookup 的结果接入已有
`PropertyGet`/`PropertySet` native continuation：getter/setter 都以原实例为 `this`，setter 参数是右值，setter
返回值被丢弃而 assignment expression 保留原右值；缺失 getter/setter、错误 receiver brand 与 accessor
abrupt completion 走统一 TypeError/completion 路径。该设计不扩大 104-byte `Frame` 或 continuation enum，且
compound assignment/update 保持 getter -> RHS -> setter 的一次性求值顺序。N=1/2/4/8/16、forced-major、
dynamic `super`、Proxy trap bypass、ordinary non-extensible rejection 与多 accessor exact-capacity 已覆盖；static
private elements 已由下述 constructor-only 模型闭合；`#x in object`、lexical arrow-`this` 和 async/generator
accessor 仍是后续纵切。

synchronous static private fields/methods/accessors 与 instance private elements 共享 private-name lexical identity
和 hidden `PropertyKey::Private` storage，但不进入 `ClassInstanceElementPlan`：static brand 的唯一 receiver 是每次
class evaluation 新建的 defining constructor，继承 constructor、instance 和包裹 constructor 的 Proxy 都不能
通过 own private lookup。HIR 在既有 `HirPrivateField/Method/Accessor` 上显式保存 `is_static`，而不是复制三套
static variant；getter/setter 只在 private identity 与 staticness 同时一致时合并。

class lowering 先按 class element evaluation 创建全部 private method/accessor closure，以 constructor 作为
`[[HomeObject]]`，并通过 `DefinePrivateMethod`/`DefinePrivateAccessor` cold opcode 把 shared payload 装入 constructor；
因此即使 static field 文本出现在 method 前面，initializer 执行时 brand 与 method 已存在。随后初始化 inner
class-name binding，再让 private static field 和 public static field/static block 进入同一 exact-capacity ordered
queue：initializer 都以 constructor 为 `this`/`[[HomeObject]]`，完成后 private field 用 `DefinePrivateField` 建立
writable hidden data slot，public field 走普通 DefineField，block 走现有可恢复 call。三条 private define opcode
显式编码 data/method/accessor kind，不能靠 runtime value type 猜测，因为 private data field 本身可以保存 function
或 `AccessorPair` 值。

该路径复用现有 shape transition、write barrier、accessor continuation 与 class lexical environment；没有新增
class object、104-byte `Frame`、native continuation、Rust 递归或普通 dispatch 热路径状态。纯 static private class
也不会加载 prototype register 或创建 instance plan。N=1/2/4/8/16、forced-major、method-before-field、
field/block source order、dynamic `super`、nested identity、shared closure、non-writable method 和 subclass/instance/
Proxy wrong receiver 已覆盖；`#x in object`、direct eval、lexical arrow-`this` 与 async/generator 仍由后续纵切闭合。

private brand check `#x in object` 使用独立 `HirExpressionKind::PrivateIn` 与 cold `HasPrivate` opcode，不把 lexical
private name伪装成普通 binary `in` 左值。lowering先从class lexical environment加载engine-private Symbol identity，
再按源码求值 RHS；opcode 对 primitive receiver 抛 TypeError，对 ordinary object只查询 own hidden shape，对
Proxy只查询其 private sidecar。缺失 sidecar/slot 返回 false，存在任意 data/method/accessor private slot 返回 true；
getter/method不执行，prototype、ToPropertyKey、Proxy `has` trap和 resumable public-property continuation均不参与。

Tachyon 把 field/method/accessor identity 都落为同一 private shape key，因此 brand check 不需要像将 method brand
单独存储的实现那样按 private kind 分支；一次 shape lookup 同时保持 nested class同名隔离和每次 class evaluation
fresh identity。stamped Proxy sidecar返回 true，包裹 constructor/instance 的普通 Proxy没有自动品牌。该路径不分配、
不新增 Frame/native continuation，也不污染普通 `in` 热路径；N=1/2/4/8/16、forced-major、RHS abrupt/单次求值和
trap bypass 已覆盖。剩余 private-in Test262 仅依赖 await/yield 的 RHS suspension。

class static blocks 在 HIR 中保留为独立 ordered element，并拥有自己的 strict `ClassStaticBlock` synthetic
stencil；bytecode/runtime 则有意复用 non-constructible `ClassFieldInitializer` function kind，因为两者具有相同
的 call contract：无公开 prototype，以 class constructor 为 `this` 和 `[[HomeObject]]`，允许动态
`super.property` 与 `new.target`（调用时为 undefined），不允许 `super()`/arguments/await/yield。早期错误由 Oxc
semantic checker 在 HIR 前拒绝，不能靠运行时伪造；nested ordinary/generator/async function 内部的 arguments
属于其自身边界，不应被 static-block ContainsArguments 规则误拒绝。

class lowering 不为 static fields 与 blocks 建立两张队列，而是在全部 computed keys 求值和 initializer closure
创建期间构造 exact-capacity ordered static-element queue；inner class-name binding 初始化后，field define 与 block
call 按源码顺序交错执行。block completion value 被丢弃，throw 通过既有 VM frame/completion 路径中止后续元素，
不增加 opcode、native continuation 或 Rust 栈递归。Oxc 的 `ClassStaticBlock` scope 必须在 HIR capture 判定中视为
function-like owner：它让 block-local binding 被 nested closure 捕获；synthetic initializer capture discovery 则
补足 outer binding 穿过 Oxc 非 Function 边界进入 block 的反方向。两者不可合并，否则会分别丢失 local slot 或
outer environment edge。private data names、methods、accessors 与 static private elements 已使用下述多槽 class
environment；`#x in object` 仍未实现，因此完整 class semantics 与 M5.3 construct 总项继续不标记完成。

named class expression、class declaration 与含 private names 的 anonymous class 都使用动态 exact-size
declarative inner Environment，不把 inner 名称写入
global、外层 register 或 `FunctionObject`。HIR 用 BindingIdentifier 的 symbol 保存 identity，但以
`class.scope_id` 作为 environment owner；这是 Oxc 中两个不同概念，不能用 binding scope 替代 class evaluation
scope。slot 0 在存在 inner class name 时归该 TDZ binding，后续 slots 保存 private-name Symbol identity；匿名
class 的 private slots 从 0 开始。compiler 发出带 slot count 的 `EnterClassEnvironment`，先创建并初始化所有
private-name slots，再求值 heritage、创建 constructor，并在 class-name binding 仍处 TDZ 时按
源码顺序求全部 computed element keys与创建 method/initializer closures；全部 element records 建立后才执行
`InitializeClassEnvironment`，随后运行 static fields，最后 `LeaveClassEnvironment` 恢复 parent。因此 self
heritage和computed key读取未初始化slot抛ReferenceError，而static initializer可读取已经初始化的class name。
function-owned binding 与
class-owned binding 在 metadata 中分别使用 `Environment`/`ClassEnvironment`，即使同一 function 的不同 PC
上 depth=0 指向不同 owner，verifier 也不会错误合并；相同BindingId同时拥有 outer declaration slot与inner
class slot时，resolution沿当前scope ancestors选择最近environment owner。Fiber只保存active class environment的frame-depth
栈，不扩大 104-byte Frame；current environment edge、closure capture 与 environment parent 链承担精确 GC
root。handler metadata 冻结进入 handler 时的 class environment depth，same-frame abrupt completion 先恢复
该深度；frame return/unwind 清除 stale depth。bytecode verifier 同时检查 enter/initialize/leave 平衡、handler
depth 和 jump 不跨 environment depth，同时按当前 active environment 的精确 slot count 验证 initialize slot。

无显式 constructor 的 derived class 不在 runtime 猜参数，也不物化 rest Array。frontend 生成 synthetic
`DefaultDerivedConstructor` stencil，compiler 冻结成普通 `DerivedClassConstructor` metadata 与三条 bytecode：
`SuperConstructForwardAll(result)`、`InitializeThis(result)`、`ReturnUndefined`。forward opcode读取 active Frame
已有的 absolute argument base、native argument source、bound prefix offset/count、total count 与 new.target，
因此普通调用、bound forwarding 和 generic Promise capability 共用同一参数视图。opcode verifier 限制它只能
出现在 derived constructor。无显式 base constructor 则生成只含 `ReturnUndefined` 的
`DefaultBaseConstructor` stencil，由预先分配的 construct receiver 完成规范返回。

`Throw` 在同一 opcode slow path 查询当前 function 的 immutable handler side table。handler range 是
verified half-open word-offset range，compiler 在进入 try body 前先 reserve outer handler slot，因此 nested
table 固定为 outer-first，VM 反向扫描得到 innermost match。compiler 从 owned HIR 精确计算 handler record
count 与最大同时嵌套深度；每个 catch target 以 `LoadException` 开始，从 fiber 的 traced pending-exception
slot 取走 thrown value并写入 catch-local register，不把异常藏进 global、TLS 或 reserved Value tag。

frame 保存 caller 的 call-site PC，因为 caller PC 在 `Call` dispatch 前已经前移，不能通过减常量猜 compact/
normal/wide instruction 长度。callee 无 handler 时 VM 弹出显式 frame、按 frame checkpoints 截断 register/
handler/completion storage，再用保存的 call-site PC 在 caller 继续查找。命中 catch 后设置 handler PC；
无匹配 handler 才返回 `RunOutcome::Thrown(Value)`。normal path、same-frame catch、nested rethrow 和跨函数
catch 在 `execute_batch::<1/2/4/8/16>` 下使用同一数据流，整个路径不使用 Rust panic/unwind。

当前只执行 simple identifier 或省略 binding 的 try/catch；catch destructuring 仍显式 unsupported。
finally 的 bytecode/compiler 半边已经固定为显式 completion replay：`EnterFinally` 保存 Normal 并进入
innermost covering finalizer，`ResumeCompletion` 恢复，Break/ContinueThroughFinally 保存 verified target；
Return/Throw 复用 runtime abrupt dispatcher。`HandlerEntry.handler_end` 是 finalizer exclusive execution end，
catch 使用 `handler_end == handler` sentinel；verifier 要求 finalizer 以 ResumeCompletion fallthrough 结束，
拒绝 crossing execution range、无覆盖的 Enter/abrupt opcode、未退出 finalizer 的 target 与 understated active
depth。compiler 只生成一份 finalizer body，catch 保持 finally 内层，并用独立 result register避免 normal
finalizer expression 覆盖 try/catch completion。

VM runtime 以 `ActiveHandler { handler_index, frame_depth, environment_depth }` 与紧邻的 traced language
completion 表示正在执行的 finalizer；进入顺序先 publish completion 再 publish active handler，Resume 按
verified handler execution range 精确 pop。统一 abrupt dispatcher 只处理具备 finally 的 Return、所有 Throw
及 through-finally break/continue：它按 immutable handler table 选择 innermost eligible handler，在
finalizer 自身 Return/Throw/escaping control 时丢弃被覆盖的旧 completion，而 finalizer 内 nested try/catch
仍保留外层 record。callback frame 的 `completion_base` 位于 native continuation 后；throw 先删除本 frame
abandoned native suffix，跨回 caller 后再删除 parent continuation，不会留下 stale callback state。

每个 Frame 缓存 verified `max_completion_depth != 0` 的 `has_finally` bit，放入既有 bool/strictness padding，
Frame 仍为 104 bytes。普通无-finally Return/ReturnUndefined 保持原 `finish_return` 直接热路径，callee 的
handler/completion reserve 也只在对应 verified depth 非零时执行；M6 不能让 call-loop 为未使用的语言
特性支付 metadata scan 或 reserve 调用。entry/callee 在执行前按 exact verified depth reserve，steady-state
EnterFinally 不触发 Vec growth。N=1/2/4/8/16 覆盖 normal/throw/return override、单层/多层 break/continue、
nested order、stale-record、accessor getter throw；saved heap payload 在 forced-major 下存活，completion hard
limit 返回结构化 host error。catch destructuring、labelled control、IteratorClose 与 await/yield replay 仍待后续 M6/M10。

`try`、`catch`、`finally` 和 abrupt completion 使用显式 handler/completion stack。
这使任意合法 `await` 或 `yield` 点都能暂停，不需要保留 Rust 栈，也不需要像 Escargot
那样动态复制和拼接恢复字节码。

解释器主循环保持同步：

```rust,ignore
enum RunOutcome {
    Completed(Value),
    Thrown(Value),
    Suspended(Suspension),
    Yielded(Value),
    BudgetExhausted,
}
```

不能把 opcode handler 实现成 Rust `async fn`。Rust Future 只包在 VM driver 外层，
普通字节码不承担 Future 状态机开销。

### 6.3 Debugger 与 Inspector

debugger 是 VM 执行状态的一部分，不是普通 JS plugin。compiler 为每个函数生成不可变的
`DebugSite`、source span、scope/binding map、call/return/throw site 与 async-parent metadata；
这些数据属于 `CompiledModule`，不得保留 Oxc AST。breakpoint、blackbox、step plan 和 pause
状态全部 isolate-local，通过 `CodeId + DebugSiteId` 的 bitmap/side table 引用，不能像 Escargot
那样把 breakpoint opcode 写回共享 bytecode。

解释器使用两个单态化路径：detached 时运行 `execute_batch::<N, false>`，只在 batch/safepoint
边界检查一次 debugger attach/pause generation；attached 时切换到
`execute_batch::<N, true>`，在每条指令前查询紧凑 debug-site bitmap。attach、detach、设置断点或
异步 pause 请求通过 high-priority actor command 触发 `Redispatch`。这避免 detached 热路径的
per-opcode trait call，同时把 pause latency 限制在一个 dispatch batch 或已有 safepoint 内。

跨线程 actor runner 若承诺 batch 级 pause/cancel latency，可使用单独单态化的 interruptible batch
路径：外部 producer 只对一个 compact `AtomicU32 interrupt_bits` 做 `fetch_or`并 wake，解释器每 batch
最多一次 acquire load，在 safepoint 由 isolate 自己 drain command并修改普通 generation/state。
direct `&mut Isolate` 路径编译掉该 load。禁止 CAS retry/spin，breakpoint bitmap、pause generation、
heap 和 fiber 始终非原子。若某 adapter 不启用 interruptible path，其 latency 明确退化为一个 quantum。

typed `DebuggerHandle` 与 actor handle 一样为 `Send + Sync`，只投递命令，不直接借用 heap。
暂停后 isolate 进入独立的 debug command pump：普通外部 job、timer 和 microtask 不得运行，只有
inspect、release、evaluate、step/resume/terminate 与 snapshot 命令可推进。call-frame evaluate
使用受限 debugger fiber，在所选 lexical environment 中编译和执行，具有独立 fuel、timeout、
allocation 配额和明确的 side-effect policy；完成后恢复原 pause state，不能递归启动普通 scheduler。

首版 typed API 覆盖 script/source 通知、URL/位置断点、conditional breakpoint、pause、resume、
step into/over/out、caught/uncaught exception policy、call frames、scope/variable inspection、
object preview/properties、call-frame evaluate、console events、bounded async stack 与诊断型 heap
snapshot。step 以 source site、frame depth 与 async task identity 为准，不能简单按 opcode 数量。
optimized/lowered binding 必须仍能通过 debug map 找到 register、environment slot 或不可用原因。

远程对象不是裸 `Value` 或永久 root。每次 pause/session 建立带 generation 的 opaque
`RemoteObjectId`，归属显式 `ObjectGroup`；group 有 root 数、预览深度、属性数、总字节和存活时间
配额，并支持 `releaseObject`/`releaseObjectGroup`。resume、session close、isolate close 和配额错误
必须释放对应 persistent roots；stale generation 返回结构化错误。heap snapshot 在 GC safepoint
以流式 chunk 输出 Chrome-compatible diagnostic graph，包含 object、edge、root、external bytes 与
Signal/GUI native node 类型，但不承诺可恢复或稳定磁盘格式。

`tachyon-inspector` 将 typed event/command 映射到已实现的 CDP Debugger、Runtime、Console、Profiler
和 HeapProfiler 子集。协议 session 接收/产出 byte buffer，由宿主选择 WebSocket、stdio、IPC 和
executor；未知 method 返回标准 CDP error，不能假装成功。CDP adapter 不拥有 isolate，不把
JSON parser、socket poll 或 transport backpressure 放进 VM poll。

## 7. Async、Promise 与 Rust Future

Promise 第一阶段将对象状态、reaction 和 job roots 都放在精确 GC 边界内。Promise payload 复用
`OrdinaryObject` 作为属性面；reaction 使用固定大小 managed node，避免在 Promise 内部 `Vec::push`
产生不可计量扩容。isolate job queue 使用 tuning 中统一的预分配容量，并分为 queued 与 traced
`active_job`：执行 job 前只能把队首移动到 active slot，不能先 pop 到未被 root set 覆盖的 Rust
局部变量。为此 `VmRoots` 显式包含 Promise job queue，所有可能触发 GC 的 VM allocation 都必须同时
trace fiber、finalization jobs、Promise jobs、realm 与 loaded code。

每个 Promise capability 的 resolve/reject 共享一个 GC-managed `PromiseResolutionCell`，其中只保存
Promise edge 与 `already_resolved`；不复制 QuickJS 的 malloc/refcount pair，也不以两个独立 bool 模拟。
Promise executor 仍走现有 native-continuation trampoline。正常 return 忽略 executor 结果并恢复原 Promise；
throw 在显式 frame unwind 即将跨过 Promise executor continuation 时被捕获为 rejection。其他 native
continuation 的 throw 传播保持不变，因此该机制不引入 Rust panic/unwind，也不把 Promise 特判放进
普通 opcode 热路径。

上述 `PromiseResolutionCell` 只用于 intrinsic `%Promise%` fast path。`SpeciesConstructor` 选择 custom
constructor 时，`NewPromiseCapability` 分配独立的固定 24-byte managed `PromiseCapability { promise,
resolve, reject }`，并创建一个 length 2、空 name、不可 construct 的 native executor 捕获该 record。
executor 按规范在任一字段已非 undefined 时拒绝重复初始化；custom constructor 正常返回后依次验证
result 是 object 且 resolve/reject 均 callable，再发布 promise edge。reaction 的 capability slot 仍保持
单个 64-bit `Value`：intrinsic 路径直接保存 result Promise，generic 路径保存 capability record；settlement
按 managed type 分派，前者直接进入内部 Promise Resolution Procedure，后者必须以 undefined 为 this
调用捕获的 resolve/reject。不得为了统一表示而让常见 intrinsic `.then` 多分配 capability record。

`Promise.prototype.then` 的 constructor 与 `@@species` 都是 observable `Get`，使用固定 5-slot managed
state 穿越 accessor/Proxy/bytecode callback；`Array` 与 `Promise` constructor 安装同一个语义为 `return this`
的标准 species getter。custom constructor throw 原样传播，不能转成 result Promise rejection。

generic `Promise.resolve`/`Promise.reject` 在 `this` 不是 intrinsic `%Promise%` 时复用同一 managed capability contract：
先分配 capability 与 capture executor，再把 capability、resolution、executor 放入固定
`NativeCallState`，并在任何 bound-prefix allocation 前把 state 发布到 native completion roots。custom
constructor 正常返回 object 且完成 resolve/reject capture 后，先同时验证两者 callable，再以 undefined 为
this 调用所选 resolve(resolution) 或 reject(reason)，最后返回 capability.promise；整个过程允许 bytecode
constructor/callback suspend，不递归进入 Rust 调用栈。当前仍缺 Promise input 的 observable `constructor`
identity fast path 的 inherited/accessor/Proxy observable Get 与部分静态方法规范顺序，因此不能把这一纵切
描述为完整 `PromiseResolve(C, x)`。

Promise resolve 在任何 observable property access 前设置 shared already-resolved guard。primitive 与
self-resolution 直接 settle；object resolution 使用 typed `PromiseResolution` continuation 执行
`Get(resolution, "then")`，因此 accessor getter 和 Proxy trap 可以进入 bytecode frame，而 promise、
resolution 与 caller mode 始终留在 completion root。callable `then` 只 enqueue
`PromiseResolveThenableJob`，job 执行时创建 fresh resolving pair，并以 thenable 为 `this` 调用捕获的
`then`；正常 return 忽略结果，throw 调 reject，resolve 后再 throw 由 shared guard 抑制。reaction handler
的正常返回必须重新进入同一 resolution procedure，不能把 Promise/thenable object 直接 fulfill 到结果
capability。job 从 queued 移入 active 后，直到 callback return/throw 完整处理前不得清除 active root。

`Promise.all` 使用独立 typed combinator payload 保存 aggregate Promise/result Array、generic capability、cached
resolve/iterator/next、当前 iterator result、remaining/index 和 abrupt-close 状态；每个 indexed reaction 的两个
function 共享独立 managed once-cell，aggregate settled bit 不能替代元素级 first-call-wins。状态机严格按
`NewPromiseCapability(C) -> GetPromiseResolve(C) -> GetIterator -> IteratorStepValue -> Call(resolve) ->
Invoke(then)` 可恢复执行；同步 `ExecutionError` 在 capability 创建后转 reject，iterator abrupt completion 按规范
区分是否 `IteratorClose`，close 自身 throw 不覆盖原始 throw。custom constructor/capability、observable
resolve/then 与最终 values Array 的 Promise Resolution Procedure 都走同一流程。

组合器实现的代码所有权按算法阶段拆分：`entry` 只拥有 constructor/capability 建立与 guarded intrinsic
入口，`driver` 只拥有可恢复 iterator/Get/Call/IteratorClose 状态机，`settlement` 只拥有 all/race/
allSettled/any 的元素 policy 和最终 resolve/reject，`storage` 只拥有 GC allocation、root refresh、barrier 与
remaining/once-cell mutation。子模块之间只通过 `pub(super)` helper 协作，不新增共享 continuation variant，
也不把 Promise combinator 映射为 Rust future 并发。`then` getter abrupt 属于 iterator 已取得后的 close
路径；close getter/call 再 abrupt 时必须保留最初 completion，不能以 close error 覆盖 rejection reason。

非空 Array 即使 brand、constructor 和 iterator 看似 intrinsic，也不能绕过每个输入的 observable `then`；在没有
shape/watchpoint 同时证明 iterator、`Promise.resolve` 和相关 `then` identity 前，非空 fast path 必须不存在。
当前仅 guarded empty intrinsic Array 直接 resolve 空 result。combinator handler/attachment 和 packed argument prefix
的分配 helper 都显式返回 GC 搬移后的 aggregate reference。普通 construct slow path还把完整 `CallSite` 放入
Fiber 的稀疏 root stack，prototype lazy materialization 与 receiver allocation 后重新取得 callee、newTarget、
argument source/prefix 和 receiver；不能只把 bound prefix 留在 Rust 局部变量跨 safepoint。

组合器的协议 driver 与结果策略分离：`PromiseCombinatorKind` 只决定 done/element settlement，不复制 capability、
iterator 或 abrupt-close stage。`race` 在每个 resolved input 的 `then` 上直接安装同一 capability resolve/reject；
因此第一个调用由 capability 自身 first-call-wins，空 iterator 结束时不递减 synthetic remaining、也不 settle。
`allSettled` 的两个 handler 继续共享 element once-cell，并把 settled record 先发布进 result Array 的预留槽；
record 后续属性分配从 native destination 保存的 aggregate root 重新取得 object，参数则先进入 aggregate 的 traced
temporary，不能跨 moving safepoint 再读 Rust 栈上的 `CallSite` argument source。IteratorRecord 创建前的
`NextGet` abrupt，以及 IteratorStep/IteratorValue 的 `NextCall`、`DoneGet`、`ValueGet` abrupt 都直接 reject；
只有 record 已建立且 done=false 后的 resolve/then 协议错误执行 IteratorClose。`any` 继续扩展同一个 kind
policy，不新建平行 iterator state machine。

`Promise.any` 的 fulfilled 与 rejected callback 不共享 allSettled 的双向 once-cell：fulfilled 参数必须直接使用
capability resolve，因为规范允许同一 thenable 多次调用它；只有 reject-element 拥有 once-cell、index、errors
和 remaining 语义。全 reject或空输入创建 branded AggregateError，`errors` 是保持输入顺序的 Array own data
property，属性为 writable/configurable 且 non-enumerable。public `AggregateError(errors, message, options)` 先完成
message ToString 与 InstallErrorCause，再执行 required IterableToList。IterableToList 复用 Array.from 的 resumable
iterator core，但以显式 `require_iterable` mode 禁止 undefined/null iterator method 降级到 array-like；这样共享
GetIterator/next/done/value/IteratorClose 实现而不把 Array.from 的不同 fallback 语义带进 Error constructor。

`Promise.try` 在 intrinsic `%Promise%` 上直接分配 pending Promise，再让 callback normal completion 进入统一
Promise Resolution Procedure、abrupt completion 进入 reject；custom constructor 必须先完成
`NewPromiseCapability(C)`，随后以 captured resolve/reject 执行同样的 normal/throw 映射，并返回 capability.promise。
callback 的 variadic suffix 只复制一次到精确容量的 GC-managed bound prefix；typed continuation state 只保存
capability/promise 与该 prefix，不能让参数、constructor result 或 thrown value依赖可暂停调用之外的 Rust 栈。
constructor throw 仍同步传播，callback throw 转 rejection，而 custom resolve/reject 自身 throw 按规范同步传播。
四个 continuation stage 独立放在 `promise_try.rs`，通用 capability 模块只负责分配、验证和 shared dispatch。

native continuation 的迭代 parent drain 必须受当前 JavaScript frame 的 `completion_base` 限制：只有 stack
depth 严格大于 frame base 时，下一个 native entry 才属于同一 resumable operation。位于 frame base 以下的
Promise executor/reaction 或其他 caller continuation 属于外层 JavaScript frame，内嵌 accessor、Proxy、
Array callback 完成时不得提前 pop。`Array.prototype.forEach` 使用固定 managed state，并让 length Get、
HasProperty、element Get 与 callback 复用该边界；不在 Rust 栈递归执行 callback。
`Array.prototype.filter` 与 `map` 复用同一 iteration state，但在 length conversion 与 callback validation 后增加
ArraySpeciesCreate 冷阶段：对 Array receiver 依次执行 observable `Get(constructor)`、`Get(@@species)` 和
`Construct(C, [length])`，其中 filter 的 construction length 为 0，map 为捕获的 source length；null/undefined
回退当前 Realm 的 Array exotic。constructor、custom result 与 thisArg
保存在 traced side-state；bytecode constructor、accessor 和 Proxy 路径通过 `ArrayForEach` typed continuation
暂停，不能递归进入 Rust interpreter。同步 HasProperty/Get/native callback 则由一个显式 Rust loop 连续推进，
任何一步发布 frame/continuation 都立即退出 loop，恢复后从固定 index state 继续，因此稀疏长数组不增长
Rust stack。初始化顺序固定为 Get length、ToLength、IsCallable(callback)、ArraySpeciesCreate、逐项遍历。
ArraySpeciesCreate 在 constructor 是 constructible 时先通过 Proxy/Bound-aware `realm_for_callable` 取得定义
Realm；若它是异 Realm 自身的原生 `%Array%`，必须在任何 `@@species` Get 之前将 constructor 视为 undefined，
并创建当前 Realm Array。realm intrinsic identity 查询只读 active/inactive Realm root，不切换 execution
context、不执行 JavaScript，也不把 RealmId 或额外字段塞入 output continuation。species length argument 的
bound-prefix 本身是 GC object，Construct 前必须从临时 constructor root 切换为 `OutputConstruct` continuation
retained root；只把 prefix 留在 Rust `CallSite` 局部变量会在 receiver allocation 的 forced-major 中被回收。
非 nullish primitive receiver 统一经过 `coerce_to_object` 分配现有 String/Number/Boolean/Symbol wrapper，
因此 string indexed properties、wrapper prototype 与 GC tracing 不在 Array 模块重复实现；null/undefined 仍在
ToObject 边界抛 TypeError。
`IsArray` 的普通 Array payload 仍走无分配 fast path；遇到 Proxy 才沿 target 迭代，nested Proxy 继续判断最终
target，revoked handler 由 Proxy snapshot 直接抛出规范异常。该判断被 ArraySpeciesCreate 等消费者共享，不能
在 Array filter 内另建“Proxy 视为普通对象”的近似路径。
filter 选中元素与 map callback result 必须以 writable/enumerable/configurable 全 true 的
CreateDataPropertyOrThrow 写入 species
result，不能使用 Set 或普通赋值语义；ordinary result 复用 descriptor validator，Proxy result 复用
`ProxyDefineMode::Object` 的可恢复 `[[DefineOwnProperty]]` trap continuation。`OutputDefine` parent
stage 在 trap 发布前保存 iteration state，filter 只有 define 成功后才递增 dense output cursor，map 始终使用
原 source index；false trap 直接映射为
TypeError，abrupt completion 原样传播。由于索引 atom、shape 和 trap 调用都可能触发移动 GC，选中元素写入
traced output side-state，SpeciesCreate 和写入前先将 iteration state 发布到 caller destination，并在分配后
重新取得 relocated `GcRef`，禁止继续使用 Rust 局部旧引用。
`Array.prototype.every` 与 `some` 也复用该 iteration state 和 typed continuation，不能另建只支持 dense
Array 的同步循环。两者只在 traced side-state 中保存 thisArg 与一个“callback 返回何种 truthiness 时继续”
位：`every` 以 true 继续、穷尽后返回 true，`some` 以 false 继续、穷尽后返回 false；首次不匹配时直接把
相反布尔值写入 caller destination 并终止，不再进入 advance loop。这个表示让短路策略不进入每元素的
NativeFunction match，也不扩张 32-byte continuation 或 104-byte Frame。length 仍只读取一次，callback
callability 仍在 Get length/ToLength 之后验证，holes 必须先执行 HasProperty，继承索引、Proxy trap、accessor
和 callback abrupt completion 都沿与 forEach/filter 相同的恢复边界。
`reduce`/`reduceRight` 位于 `array_for_each/reduce.rs` 子模块，并继续使用 Array typed continuation；主模块不因
新增两个方法突破 1000 行。reducer state 固定为 receiver、callback、accumulator、length、logical cursor 五个
Value，direction/initialized 是 `NativeCallState::count` 中只供内部识别的 scalar mode。initialValue 是否存在
必须由调用参数数量冻结，不能观察值是否为 undefined。logical cursor 始终从零递增，反向实际索引映射为
`length - cursor - 1`，因此无需 signed sentinel，也能覆盖 MAX_SAFE_INTEGER-1。

逐项循环仍以 HasProperty -> Get -> Call 的规范边界推进；首个存在值在无 initialValue 时只初始化 accumulator，
不调用 callback。同步 native callback 返回“继续 loop”而不递归调用 advance，长 receiver 不增长 Rust stack。
length Get 返回 object 时切换到通用 `ConversionConsumer::ArrayLength`，conversion continuation 的 receiver Value
保存 reducer/forEach/filter state，ToPrimitive/ToNumber 完成后再进入统一 ToLength 与 callback validation；这条
路径也服务其他共享 Array iteration，不能在 reducer 内复制 valueOf/toString 调用。

safe-integer 稀疏 receiver 不能逐 hole 扫描。仅当剩余距离超过
`tuning::arrays::ARRAY_ITERATION_SPARSE_SKIP_THRESHOLD`，且 receiver 到 prototype chain 全部非 Proxy 时，
reducer 与共享 forward Array iteration 枚举当前 ordinary numeric own-key并跳到方向上的最近候选；候选
处仍正式执行 HasProperty/Get，getter/callback 后重新枚举以观察增删。任一 Proxy 出现在链上就禁用快进并逐项
触发 has trap。该契约参考 Escargot nextIndexForward/Backward，但 property-name parser覆盖完整 safe-integer
范围而非只覆盖 ArrayIndex；QuickJS 的朴素循环不能满足 near-integer-limit Test262 的可完成性要求。

`Array.prototype.indexOf` 与 `lastIndexOf` 复用同一个 typed `ArrayForEach` continuation，但不进入 callback
validation：固定 state 保存 receiver、searchElement、fromIndex、length、cursor，方向和“反向省略 fromIndex”
编码在 scalar mode。Get length/ToLength 必须先完成；length 为零时不转换 fromIndex。对象 fromIndex 通过
`ConversionConsumer::ArraySearchIndex` 可恢复执行 ToPrimitive/ToNumber，之后按 ToIntegerOrInfinity 归一化。
正向 cursor 保存下一索引，反向 cursor 保存 `index + 1`，因此终止状态为零且无需 signed sentinel；省略
`lastIndexOf` 的 fromIndex 与显式 undefined 必须由 argument count 区分。每项严格执行 HasProperty 后才 Get，
比较使用 Strict Equality，不把 holes 当 undefined。ordinary prototype chain 复用双向 numeric candidate scan，
任一 Proxy 出现在链上则禁用跳跃并观察每次 has trap。该边界对照 QuickJS `js_array_indexOf`/
`js_array_lastIndexOf` 与 Escargot `builtinArrayIndexOf`/`builtinArrayLastIndexOf`、
`nextIndexForward`/`nextIndexBackward`。

`Array.prototype.includes` 复用上述固定五槽 state、length conversion 与 forward cursor，但使用独立
`SEARCH_INCLUDES` mode，不能进入 indexOf 的 HasProperty/hole-skip 分支。每个索引必须直接执行
Proxy/accessor-aware Get，因此 holes 产生 undefined，Proxy 只能观察 get trap；比较使用 SameValueZero，
命中与结束分别发布 boolean true/false。length 为零仍跳过 fromIndex 转换，对象 fromIndex 继续由
`ArraySearchIndex` consumer 恢复。同步 Get 在显式 loop 中推进，长区间不增长 Rust 栈；packed-array 或
ordinary proven-hole 快进只能在保持“searchElement 为 undefined 时首个 hole 命中”和 getter/Proxy 可观察
顺序后于 M13 加入。该边界对照 QuickJS `js_array_includes`、Escargot `builtinArrayIncludes` 与 Boa
`includes_value`。

`Array.prototype.join` 使用独立 `PendingArrayJoin`，不把 UTF-16 output 塞进通用 `NativeCallState`，也不
复用 includes/search 的 hole 规则。state 保存 receiver、原始 separator、retained conversion value、
safe-integer length/cursor/output_len，以及 externally-accounted `Box<[u16]>` separator/output backing；初始
容量由 `JOIN_INITIAL_UNITS_PER_ELEMENT` 与 `JOIN_MAX_INITIAL_UNITS` 规划，追加超出容量时 allocate-copy-swap
双倍 replacement，已发布 payload 内绝不扩 Vec。算法严格按 ToObject、Get/ToLength(length)、separator
ToString、逐索引直接 Get、nullish/self 空串、元素 ToString 和最终 UTF-16 String allocation 顺序推进。

length、separator 与元素的 object ToPrimitive 分别由 `ArrayJoinLength`、`ArrayJoinSeparator`、
`ArrayJoinElement` conversion consumer 恢复；每个 indexed Get 在 ToString 前提交 cursor，避免 nested
getter/Proxy 后重复观察。普通 Get/ToString 在显式 loop 中推进，只有 JavaScript frame 才暂停到
`ArrayJoinStage::ElementGet`。直接 self element 保持空串递归保护；packed fast path、prototype numeric
candidate skip 和更专门的 zero-copy StringBuilder 延后到 M13 profile，不能改变 generic observable order。
该边界对照 QuickJS `js_array_join`、Escargot `builtinArrayJoin` 与 Boa `join`。

`Array.prototype.toLocaleString` 复用 `PendingArrayJoin` 的 backing/cursor，但以 `locale` mode 改变元素
处理：索引仍是 direct Get；null、undefined 和直接 self 跳过；其它元素再 Get `"toLocaleString"`，以零参数
调用，并将返回值送入既有 `ArrayJoinElement` ToString consumer。相应 continuation 不覆盖 retained element，
因此 accessor、Proxy、用户函数挂起后仍能恢复原始 receiver。该设计参照 Escargot `builtinArrayToLocaleString`
与 QuickJS array locale path 的 observable order；当前已知缺口是元素继承 `%Object.prototype.toLocaleString%` 时
其内部二次 native `toString` continuation 尚未闭合，不能用直接同步调用或把 primitive 永久装箱规避语义。
此组合的定向 Test262 结果为 12/22（2 semantic、8 前置 unsupported），待后续 native continuation slice
补齐后再提升，不得误报为完整 locale semantics。

`find`/`findIndex`/`findLast`/`findLastIndex` 使用独立 `array_for_each/find.rs`，只复用 shared length conversion、
Proxy-aware Get 与 callback dispatcher，绝不复用 forEach 的 HasProperty/hole skip。固定 state 保存 boxed
receiver、predicate、thisArg、length、cursor；四种 mode 只编码 direction 与返回 value/index。forward cursor
保存 next index，backward cursor 保存 remaining，Get 前推进后仍可在 callback 恢复时重建当前 index；该表示
覆盖 MAX_SAFE_INTEGER-1 且不依赖 usize 或负哨兵。每个快照索引直接 Get，因此 holes 以 undefined 调用
predicate，Proxy 观察 get trap 而不能观察 has trap。callback 参数第三项必须是 ToObject 后的 receiver `O`；
QuickJS `js_array_find` 当前传原始 `this_val` 的行为不作为参考，采用规范与 Escargot 的 `O`。

`Array.prototype.splice` 使用独立 `PendingArraySplice`，不把可变参数列表塞入固定五槽
`NativeCallState`。插入参数按实际 argument count 做一次 `try_reserve_exact` 后转为 `Box<[Value]>`，由
`GcExternalMemory` 精确计费；receiver、species result、当前 moved value 与 constructor 都是 traced edge。
状态机严格执行 Get length/ToLength、ToIntegerOrInfinity(start)、argc 0/1/>=2 deleteCount 分支、
ArraySpeciesCreate、deleted-result Has/Get/CreateDataPropertyOrThrow、result length Set、方向敏感 move、tail
delete、items Set 和 final length Set。shrink 从低到高移动，grow 从高到低移动；source hole 必须对
destination 执行 DeletePropertyOrThrow，不能写 undefined。所有 Set 使用 throw-on-false 语义，species
result 写入使用全 true data descriptor；Proxy/accessor/custom constructor 可以进入 bytecode frame而不递归
Rust interpreter。

splice 的 managed state 在 caller destination 和 `ArraySplice` continuation 中交替保活。任何可能触发移动
GC 的 species prefix、ordinary define 或 nested property operation 完成后，都必须从 destination/刚弹出的
continuation 重新取得 `GcRef`；禁止继续使用分配前的 Rust 局部句柄。Proxy `defineProperty` 与 `set` trap
lookup 在 handler 自身为 Proxy 时复用 Proxy-aware `[[Get]]`，其 parent continuation 保存原 internal-method
state；这保证 species result 的 nested handler 精确观察 `defineProperty`, `set`,
`getOwnPropertyDescriptor`, `defineProperty` 顺序。

`Array.prototype.pop` 与 `shift` 使用独立固定大小 `PendingArrayRemove`，不伪装成 splice 的特殊参数组合。
它们可以复用相同的 Proxy/accessor-aware Get/Has/Set/Delete 分派形状，但算法生命周期不同：长度为零时仍
必须执行 `Set(O, "length", 0, true)`，返回的首/尾元素必须跨后续全部 mutation 保活，而且 shift 的每个
source index 必须先观察 HasProperty，再选择 Get+Set 或 Delete(target)。把这些阶段塞进 splice 会携带无关的
species/result/item backing，并使 zero-length 与 return-value ownership 隐藏在不匹配的状态中。

状态只有 traced receiver/retained、safe-integer length/cursor 和 shift mode，不包含 Vec、递归帧或容量猜测；
因此固定 GC payload 比通用五槽 native state或可变 backing 更准确。length 对象 conversion 使用专属
`ArrayRemoveLength` consumer，所有 property boundary 使用 `ArrayRemoveStage` typed continuation。retained 写入
执行 generational barrier，cursor 只在 Set/Delete 成功后递增，最终 length Set 成功后才把 retained 写回调用
destination。Delete 使用 strict Proxy mode，Set 使用 throw-on-false mode；这与 QuickJS `js_array_pop` 的
generic fallback、Escargot `builtinArrayPop`/`builtinArrayShift` 和规范顺序一致。当前不复制 QuickJS/Boa 的
dense-array memmove/remove fast path：packed-array eligibility、prototype indexed-property bailout 和
non-writable length guard 属 M13 profile 驱动 fast path，不能改变这个统一 slow-path contract。

`Array.prototype.push` 与 `unshift` 共用独立 `PendingArrayInsert`，而不复用 splice 的 species/deleted-result
生命周期，也不保留旧 direct-own-data 同步入口。receiver 与全部实参在 length Get 前进入 managed state；
实参数量在 call site 已知，因此一次 `try_reserve_exact` 后冻结为 `Box<[Value]>` 并按 Value 数量计入 external
memory，没有运行时容量猜测或 payload 内 Vec growth。push 直接从 captured length 左到右 Set items；unshift
在 argCount 非零时从 length 向零反向推进，每个 source 必须先 HasProperty，再选择 Get+Set(destination) 或
DeletePropertyOrThrow(destination)，之后才从零开始 Set items。两者无论 argc 是否为零都执行最终
`Set(O, "length", len + argCount, true)` 并返回该 safe integer。

length 对象使用 `ArrayInsertLength` conversion consumer，property boundary 使用 `ArrayInsertStage` typed
continuation；current moved value 是单独 traced/barriered edge，items backing 本身也参与 Trace。overflow check
只在 argCount 非零时执行，并发生于任何 indexed mutation 之前；这保留 unshift argc=0 对最大 safe length 的
合法 final Set。cursor 只在 Set/Delete 成功后提交。该顺序对照 QuickJS `js_array_push` generic fallback、
Escargot `builtinArrayPush`/`builtinArrayUnshift` 与 Boa 对应实现。QuickJS packed push 特判不进入当前 slice：
未来 M13 fast path 必须同时证明 genuine packed Array、exact dense length、writable length、extensible receiver、
无 indexed prototype interference，并在任一条件失败时回到本统一状态机；不能用旧 direct property helper
覆盖 generic object、String exotic、accessor 或 Proxy。

普通 receiver 的同步 property operation 不能通过每项递归调用 `resume_array_insert` 推进，否则 unshift 的
Rust 栈深度会随 length 线性增长。insert dispatcher 因此在 immediate completion 时返回重新校验后的最新
state reference；move/item owner 在显式 loop 中提交 cursor 并继续。只有 accessor/Proxy trap 真正发布
JavaScript frame 时 dispatcher 返回 suspended，之后由 typed continuation 进入单次 resume，再回到同一 loop。
每个可能分配的 Get/Has/Set/Delete 后都采用 dispatcher 返回的 `GcRef`，不使用操作前局部句柄。20,000
长度稀疏 unshift 在扩大 atom quota 的测试 isolate 中验证常量 Rust 栈；该测试不依赖 sparse skip，因而覆盖
完整逐索引 generic contract。

`Array.prototype.fill` 使用独立固定大小 `PendingArrayFill`，不复用 copyWithin 的方向、source lookup 与
hole-delete 状态。state 保存 traced receiver、填充值、原始 start/end 参数以及 length/cursor/end 三个
safe-integer scalar，不含 Vec 或动态 backing。算法必须先完整执行 ToObject、Get/ToLength(length)、
ToIntegerOrInfinity(start) 与 ToIntegerOrInfinity(end)，之后才允许第一次 indexed Set；因此 conversion
consumer 分为 `ArrayFillLength`、`ArrayFillStart`、`ArrayFillEnd`，不能把边界换算折叠进 mutation loop。

每个索引使用 Proxy/accessor-aware `Set(O, key, value, true)`，填充值由 managed state 和 continuation retained
edge 跨 nested JavaScript 保活，cursor 仅在 Set 成功后提交。普通对象的同步 Set 在显式 loop 中推进，避免
按元素递归 resume 导致 Rust 栈随区间长度增长；只有真正发布 JavaScript frame 时才暂停并通过
`ArrayFillStage::Set` 恢复。该顺序对照 QuickJS `js_array_fill`、Escargot `builtinArrayFill` 与 Boa `fill`。
当前不增加 packed-array bulk fill：该 M13 fast path 必须证明 genuine packed Array、精确 dense length、
writable length、extensible receiver 且无 indexed prototype interference，失败时回到统一 generic contract。

`Array.prototype.concat` 使用独立 `PendingArrayConcat`，不复用 splice 的 mutation state。它冻结 boxed
receiver 与调用参数，状态只保存当前 source、source/element cursor、目标 safe-integer cursor、species result、
constructor 和一个 retained Value；参数 backing 使用一次 `try_reserve_exact` 后转 `Box<[Value]>` 并由
`GcExternalMemory` 计费。算法严格按 ArraySpeciesCreate(O, 0)、IsConcatSpreadable、LengthOfArrayLike、
HasProperty/Get/CreateDataPropertyOrThrow 和 final Set(length) 推进，每个 cursor 只在对应 Define 成功后提交。
同步 Has/Get/ordinary Define 必须回到同一个显式 driver loop，不能从 Define completion 递归调用下一元素；
只有 Proxy/accessor/bytecode callback 真正挂起时才保留 typed continuation 并退出当前 Rust frame。该不变量
保证 native stack 使用量与 spread source 长度无关，也保证 Test262 Rayon worker 的较小线程栈不改变语义。
普通对象的长 hole run可枚举 prototype chain 的 numeric candidate，任一 Proxy 出现在链上则禁用跳跃并逐项
触发 has trap。IsConcatSpreadable 在 symbol Get 返回 undefined 后调用通用 IsArray；通用 IsArray 遍历 Proxy
前必须检查 handler 是否为 null，避免已撤销 Proxy 被错误当作普通 non-array。

Array constructor 的单 Number 参数是 length 而不是元素：必须是 0..2^32-1 内的有限整数，合法上界
2^32-1 不能因 `u32::MAX - 1` 误拒绝。非法 Array length 与通用 array-like 增长溢出是不同规范错误：前者使用
`InvalidArrayLength -> RangeError`，后者保留 `ArrayLengthOverflow -> TypeError`，避免 map 的 ArraySpeciesCreate
修复改变 push/unshift 超过 MAX_SAFE_INTEGER 的既有行为。

数组字面量 spread 不能 lowering 为 `Array.prototype.concat`：concat 是可替换的可观察 builtin，而且 primitive
String 按 concat 语义并不 spread。compiler 因此使用 owned `ArrayAccumulation` HIR 保留 element、elision 与
spread 的精确源码顺序，再展开为现有可恢复 iterator bytecode。Element/Spread 以
CreateDataPropertyByValue 写 own data property并由 Array exotic 更新 length；Elision 只推进 cursor，并显式
Set 下一 length。结尾不能无条件 Set length，否则会制造本不存在的 length setter观察点。

同步 iterator record 必须从执行 Realm 的 rooted well-known `Symbol.iterator` 取 key，不能经可替换的全局
`Symbol.iterator`。`LoadIteratorSymbol` 因而只加载 realm identity；`CheckObject` 在 iterator method 返回后和
每次 next 调用后执行，失败映射 language TypeError。读取 done/value 的 abrupt completion不执行
IteratorClose，这里采用当前 ECMA-262/Test262 顺序；QuickJS 在部分异常路径主动 close 的行为不作为兼容目标。
这组共享 lowering 同时服务 array spread、for-of 与 destructuring，避免三个 consumer 漂移协议检查。

调用参数 spread 使用同一 `ArrayAccumulation` iterator CFG，但结果 Array 只作为不可观测的编译器临时值；
`CallSpread`/`CallSpreadWithReceiver` 先直接抽取 packed dense backing 并冻结为 exact-size immutable
argument prefix；若内部 Array 因容量边界落入 ordinary storage，则回退到既有 GC-owned
`CreateListFromArrayLike` continuation 后再启动 call。callee（以及 method receiver/property lookup）必须先于
任何 argument 求值并保存在寄存器/managed state 中；spread 后的普通参数与后续 spread 继续严格按源码顺序
累积。direct eval 使用独立 `DirectEvalSpread` mode，在全部参数完成求值后仍保留 caller lexical environment；
tail variants 最终进入既有 frame-reuse contract。普通固定参数 call 不经过该路径，因而不为动态 spread 支付
分配或分派成本。扩展 opcode 超过首个 128 项时沿用 escape header，并用 header 高字节保存 64-opcode page；
page 0 的既有编码逐字不变，verified decoder仍只对 verifier 已确认的 dense opcode 做 unchecked decode。

静态 Array 构造算法使用 `PendingArrayStatic` 保存 constructor、result、retained value、boxed 参数和提交后
cursor。`Array.of` 在任何用户 constructor 运行前一次精确冻结调用参数；IsConstructor 为真时经通用
Construct(C, [len])，否则创建当前 Realm intrinsic Array。索引必须逐项 CreateDataPropertyOrThrow，不能用
Set 触发继承 setter；cursor 只在 Define 成功后推进。最终 length 独立执行 throw-on-false Set，从而保留
custom setter 与 Proxy trap。state 在 caller destination/typed continuation 间交替 rooting，后续
`Array.from` 的 iterable/array-like 分支复用该 managed owner，但增加 iterator record、mapper 和
IteratorClose stage，不另建同步旁路。

`Array.from` 先验证 mapper callable，再以 rooted well-known Symbol 执行 Proxy/accessor-aware GetMethod。
iterable 分支在调用 iterator method 前完成 `Construct(C)`/intrinsic ArrayCreate(0)，缓存 iterator 与 next，
每轮按 next Call、object validation、done Get、value Get、可选 mapper Call、CreateDataPropertyOrThrow 推进；
array-like 分支则先 ToObject/LengthOfArrayLike，再执行 `Construct(C, [len])`/ArrayCreate(len)，每轮重新 Get
source index，因此能观察 mapper 导致的 mutation。两条分支都只在 Define 成功后提交 cursor，最终 length
使用 throw-on-false Set。

IteratorClose 执行器不再把 owner 写死为 collection initializer，而是接收已经由 caller state root 的 iterator
identity；continuation 只保存 iterator 与 original throw。Array.from 的 managed state 另存
`close_on_abrupt`，在进入 CreateDataPropertyOrThrow 前置位、成功后清除，从而区分同一个 ResultValue resume
内“value getter 抛错不 close”和“随后 ordinary define 抛错必须 close”。mapper callback continuation可直接
按 stage 判定 close；next Call、result object validation、done/value Get 和正常 final length Set均不 close。

Change-array-by-copy 不复用 species-aware `slice`/`splice` 状态机。`PendingArrayCopy` 精确保存 source、当前 Realm
intrinsic result、retained value、方法参数、external-accounted exact item backing 和 safe-integer cursor；LengthOfArrayLike 与 relative-index
ToIntegerOrInfinity 通过 Conversion continuation 恢复，source Get 通过 ArrayCopy continuation 恢复。结果在任何
element Get 前按 ArrayCreate(length) 语义创建，超过 `2^32-1` 抛 RangeError；每个 hole 仍执行 Get 并定义
undefined own property。`toReversed` 的 source cursor 为 `len-k-1`，保证 Proxy/getter 观察到降序；`with` 在
replacement index 直接定义 captured replacement，不触发对应 source Get。`toSpliced` 在 length Get 前冻结 items，
按 argument count 区分 omitted/explicit undefined deleteCount，并将 output cursor 映射到 prefix、items、suffix；
deleted source range 因此没有 Has/Get。

`toSorted` 与 `sort` 共用独立 `PendingArrayToSorted` stable-merge owner，不把可变排序缓冲区塞进 copy payload，
也不让 mutating sort 直接借用 receiver elements。copy 模式在 ArrayCreate 后分配两个 exact-size、
external-accounted、traced Value buffer并 read-through-holes；in-place 模式严格以 HasProperty/Get 收集
present items，按 `tuning/arrays.rs` 的小数组猜测起步，满载时分配带几何扩容 backing 的 replacement GC
state，因此接近 `2^53-1` 的 sparse array-like 不会在首个 Has 前按 length 预留。replacement 发布前旧 state
仍由 destination root，返回值先写入专用 `retained` edge，所有新旧 buffer/temporary edge 都精确 trace/barrier。

两种模式随后执行同一个 bottom-up stable merge。每轮归并仅在两侧都非 undefined 时进入比较：undefined
固定后置且不调用 comparator；user comparator 经 typed continuation Call，返回值再以 numeric conversion
continuation 执行 ToNumber；默认比较每次分别以 string hint 转换左右 operand，再按 UTF-16 code units
比较，不缓存 object conversion。相等、+0、-0 和 NaN 均选择左 run。每个 pass 只交换同一 owner 内两个
backing；同步 Has/Get、primitive compare 与 ordinary Set/Delete 在显式 loop 推进，不递归 Rust stack。
比较全部结束后，copy 模式定义 dense result；in-place 模式按序执行 Set(O,j,value,true)，再从 itemCount 到
原始 len 执行 DeletePropertyOrThrow，comparator/getter/setter 对 receiver 的 mutation不会改写已收集 snapshot。

`Array.prototype.slice` 使用独立固定大小 `PendingArraySlice`，不把它塞进 `splice` 或 change-array-by-copy
owner。三者虽然共享 relative-index 数值规则和 typed property continuation，但生命周期不同：slice 只有
source/result/constructor/retained 与两个单调 cursor；splice 还拥有 exact item backing、deleted-result copy
和 receiver mutation；copy-by-change 则明确忽略 species 并 read-through-holes。强行共用会让每个热 resume
携带 mode branch 与无效字段，因此这里只复用统一的 Get/Has/Define/Set/Construct 分派契约，不复用状态对象。

slice 在任何 source observation 前完成 ArraySpeciesCreate：Array receiver 可观察 constructor 与
`@@species`，异 Realm原生 `%Array%` 在 species Get 前回退当前 Realm intrinsic，custom species 只接收一个
exact-reserved count 参数。复制严格按升序执行 HasProperty，再对 present property Get 和
CreateDataPropertyOrThrow；洞位只推进 source/target cursor，不创建 undefined。最终始终执行
Set(A,"length",n,true)，所以 custom object/Proxy species 能观察 setter/trap。每次同步 Get 后先把 state
重新 root 到 destination，并将返回值写入 traced `retained` edge，再生成可能分配的目标 atom；因此 ordinary
fast completion 和 forced-major callback completion 使用同一 GC 安全边界，长同步洞位扫描由显式 loop
推进而不增长 Rust stack。

`Array.prototype.flatMap` 不扩张 map/filter 共用的五槽 `NativeCallState`。flatMap 在 mapper 返回 Array 后
同时拥有 outer source/cursor、inner source/length/cursor、target cursor、callback thisArg 与 pending value；
把这些字段复用进 map side-state 会产生阶段相关 slot alias，把 state 扩成全局更大数组又会让每个 forEach、
map、filter、every、some 常驻付费。因此它使用独立固定大小 `PendingArrayFlatMap`，但继续走同一
Get/Has/Call/Construct/Define continuation 分派。depth 固定为 1，inner Array 扫描完成后直接回到 outer loop，
无需递归或 work Vec；普通长洞位使用受调优阈值约束的 numeric candidate scan，遇到任一 Proxy 就逐项观察。
custom species 只收到 0，custom object 结果不执行额外 length Set，所有输出只由
CreateDataPropertyOrThrow 驱动，这与 QuickJS `JS_FlattenIntoArray`、Escargot `flattenIntoArray` 的可观察
边界一致。

native builtin identity 已超过 256，继续维持 `NativeFunction repr(u8)` 会迫使无关方法共享 identity 或按
实现批次删除标准 surface。该上限不是可保留契约：NativeFunction 不进入 bytecode/FFI ABI，且
`FunctionExecutable` 的最大 payload 已固定为 16 bytes。identity 改为 `repr(u16)` 后 compile-time layout
gate 仍锁定 `FunctionExecutable=16 bytes`、`FunctionObject=56 bytes`，所以没有增加函数对象常驻尺寸；
换来的空间足以继续完成 Intl 之前的标准 builtins，并保持 exhaustive match 与调试名称一一对应。

String iterator 与 Array iterator 复用同一 GC-managed indexed cursor payload，但以不可伪造的
`ArrayIterationKind::StringValue` brand 区分，并使用独立 `%StringIteratorPrototype%`/next surface；这对应
QuickJS 共享 payload但区分类标识的布局，同时保留 Escargot 独立 prototype/brand 的可观察语义。每次 next 在
no-GC immutable String borrow 内读取固定 `[u16; 2]`：lead+trail surrogate pair 一次推进两 units，其余 BMP或
孤立 surrogate 一次推进一 unit；更新 cursor 后才分配结果 String与 iterator result，避免跨 moving-GC 保留
未 rooted Rust handle。String `@@iterator` 的 RequireObjectCoercible/ToString 复用 typed conversion
continuation，因此对象 receiver 的 valueOf/toString callback 仍可暂停，不递归解释器 Rust stack。

同步 child native operation 可以合法消费自身 continuation 并继续 drain 一个或多个 parent；因此调用者
记录入栈前 depth 后，`current_depth <= baseline` 都表示本 continuation 已完成，不能只检查等于 baseline
后再次 pop。只有 `current_depth > baseline` 时调用者才拥有尚未消费的顶部 continuation。

### 7.1 JavaScript `await`

JavaScript `await` 必须遵循 ECMAScript Promise 和 microtask 语义，不能简单翻译成一次
Rust `.await`。

字节码可以表示为 `Await { src, dst }`。执行时：

1. 先将 frame 的 `pc` 更新到 `Await` 后一条指令。
2. 对 `src` 执行规范要求的 `PromiseResolve`。
3. 给 Promise 添加内部 reaction：`ResumeFiber(FiberId)`。
4. 记录恢复目标寄存器和必要的 completion 状态。
5. 将 fiber 标记为 `Awaiting` 并退出当前解释循环。

即使 Promise 已经 settled，也必须将恢复动作放入 microtask queue，不能同步重入 fiber。
fulfilled 使用 `Completion::Normal(value)` 恢复；rejected 使用
`Completion::Throw(reason)` 恢复，由显式异常栈继续处理。

每次 async 函数调用拥有独立 fiber 和结果 Promise。调用 async 函数时，应按规范同步推进
该 fiber，直到首次暂停或完成，然后立即向调用方返回结果 Promise。

async function 的首次运行通过 active-fiber trampoline 实现，不递归进入 Rust 解释器。
父 fiber 在调用点创建结果 Promise 和子 fiber，并暂停自身的继续分派；子 fiber 同步运行
到首次 `await`、throw、return 或执行 quantum 耗尽。子 fiber 暂停/完成后，父 fiber 才能
执行调用后的下一条字节码。

quantum 耗尽只让出 Rust executor，不构成 JavaScript job 边界，也不允许同 isolate 的
另一个 job 插入。当前顶层 job 完成或按规范暂停后执行 microtask checkpoint；Promise
microtask 优先于下一个外部 job。多个外部执行请求在同一 isolate 中串行排队。

async generator 遵循不同的启动规则：调用函数只创建 generator，第一次 `.next()` 才启动
对应 fiber，不能复用 async function 的立即启动行为。

普通 async function 使用独立 GC-managed `AsyncFunctionState`，不伪装成 GeneratorObject。state 保存结果
Promise、creation activation、caller/paused Fiber 以及 verified await destination/instruction；frame 内 typed
`AsyncFunction` continuation 在执行期间固定 root state。调用时 argument-prefix 先暂存在 caller destination，
再分配 Promise/state，避免 forced-major 经过 Rust 局部变量形成未 traced 间隙；随后 caller Fiber 转入 state，
child Fiber 在同一个 iterative interpreter trampoline 内同步推进。

`Await` 先验证 suspend point 的 instruction/destination/resume pc。native Promise 直接挂内部 reaction；primitive
创建 fulfilled intrinsic Promise；object thenable 复用现有可暂停 Promise Resolution Procedure，并以
`PromiseResolutionMode::AsyncAwait` 在 observable `then` getter 返回后才发布 paused Fiber。所有路径最终都把
`AsyncFunctionState` 编码在 internal reaction capability 中，Promise checkpoint 识别该 brand 后交换 caller/
paused Fiber；fulfilled 写 destination，rejected 从 Await instruction origin 进入统一 abrupt dispatcher。checkpoint
在交换前把 caller pc 固定到 verified return site，async body return/uncaught throw 分别 settle 结果 Promise 后
恢复 caller，绝不把 body completion 直接写入 caller destination。当前普通 ES8 await Test262 为 22/22；
async-generator 内显式 Await、`for-await-of` 与 async module 仍属于后续切片。

同步 generator 采用同一启动规则。GC-managed `GeneratorObject` 保存
`code/function` 和仅用于首次恢复的 fixed-size activation；参数在 generator 创建时一次性冻结进已有的
immutable argument-prefix GC 对象，避免首次 `.next()` 再复制或分配参数 backing。对象显式维护
`SuspendedStart`、`SuspendedYield`、`Executing`、`Completed`；`Completed`/`Executing` 快路径只读取紧凑 state
header，不扫描 activation。函数调用只创建 `SuspendedStart` 对象；第一次 `.next()` 创建独立 generator
Fiber 并以 typed `GeneratorResume` continuation 接住 return/throw，不递归进入 Rust interpreter loop。
`Yield` 把完整 generator Fiber（frame/register/environment/arguments/handler/completion/pending state）移入
GeneratorObject 的 traced paused slot，恢复对象中保存的 caller Fiber，并返回 `{ value, false }`；后续
`.next(value)` 以单次 object borrow 校验并原子交换两条 Fiber，把 value 写入 immutable suspend metadata
指定的 destination 后从 resume offset 继续，不 replay body。首次 `.next(value)` 按规范忽略 value。
caller publication 的失败路径显式返还 Fiber ownership，不依赖 Rust unwind；frame 成功发布后清除 generator
的 creation-time roots。`Generator.prototype.return/throw` 在 SuspendedStart 不执行 body/finally，直接完成并
释放 activation roots；Completed 分别返回 `{ value, true }` 或抛出输入值，Executing 统一 TypeError。
SuspendedYield 不在 builtin 内提前完成：resume swap 保存 Yield instruction origin，`return(value)`/
`throw(value)` 分别从该 origin 注入现有 `CompletionRecord::Return/Throw`，复用统一 abrupt dispatcher 的
catch/finally 和 completion override。finally 再次 yield 时，原 completion 随完整 paused Fiber 保存，后续
`ResumeCompletion` 继续；未捕获 throw 穿过 typed GeneratorResume boundary 恢复 caller 后继续传播。
实例原型为每个 generator
函数自己的 `.prototype`，其上依次为 `%GeneratorPrototype%` 和 `%IteratorPrototype%`；generator 函数自身
继承 `%GeneratorFunction.prototype%`。普通 yield/next 已覆盖 try/finally、forced-major GC、N=1/2/4/8/16
和 512 次恒定 native stack 恢复；return/throw injection 另覆盖内部 catch、finally yield、override、
forced-major 和 512 轮 abrupt stress。delegated `yield*` 采用 compiler-expanded protocol loop：缓存
iterator/next，按 resume kind 调用 `next`/`return`/`throw`，对 iterator result 执行 Object 校验，
done=false 时通过 `YieldDelegate` 转发同一 result object identity。缺失 `throw` 时先执行 delegate
`return()` close，getter/call/非对象错误覆盖最终 TypeError；已有 throw 的 getter/call/result 失败不 close。
GeneratorObject 仅增加 optional resume-kind destination，不增加 delegate heap object；iterator/method/result
roots 均位于 paused Fiber registers，resume 直接写入 `(value, 0/1/2)` 后继续字节码 CFG，保持
allocation-free Fiber ownership swap、无 Rust recursion，并让 catch/finally 复用现有 abrupt dispatcher。
N=1/2/4/8/16、forced-major、nested delegation、result identity、error precedence 与 512 轮恒定 native
stack 均覆盖；Test262 `language/expressions/yield` 为 122/123，唯一 unsupported 是独立的 `with` statement
缺口。

async generator 第一纵切复用同一 Fiber ownership swap，但 function kind、prototype 与请求生命周期独立。
Realm 同时发布独立 `%AsyncIteratorPrototype%`：它继承 `%IteratorPrototype%`，以
`Symbol.asyncIterator` identity 方法返回 receiver，并由 `%AsyncGeneratorPrototype%` 继承；该层不把 async
iterator 方法混入普通 IteratorPrototype，保留规范可观察的 prototype/brand 边界。
`GeneratorObject` 自持 checked-capacity `VecDeque<AsyncGeneratorRequest>` 和单独 traced active request；请求保存
capability Promise、completion value/kind 与诊断 origin，Executing 状态下 `.next/.return/.throw` 只追加 FIFO，
不能同步重入 body。primitive `yield` 和 body `return` 先发布 iterator result/Promise settlement job；job settle
active capability、清除 active slot后立即 ResumeNext，因此 reaction 注册顺序不允许反转同一 generator 的请求。
completed `next/throw` 与 suspended-start `throw` 不额外制造 settlement job，而是按规范同步 resolve/reject
capability；completed/suspended-start `return` 暂经 primitive return job，后续 PromiseResolve/thenable assimilation
接入时替换为完整 AwaitingReturn 状态。

Promise checkpoint 内启动 generator 时，caller 的 fallthrough PC 可能已是 bytecode-end。ResumeNext 在交换 Fiber
前把 caller PC 明确设为当前 verified Return instruction；generator 再次暂停后恢复 caller并重入幂等 checkpoint，
不能把 request origin（可能属于 generator body 的另一 CodeId）当作 caller PC。invalid receiver 的 async method
调用先创建 capability，再返回 rejected Promise，不同步抛错。当前实现支持无显式 `await` 的
`async function*`、默认安装 `%AsyncGeneratorPrototype%[Symbol.toStringTag]`，Test262 目录为 80/96；尚未实现
`%AsyncIteratorPrototype%`，暂时继承 `%IteratorPrototype%`，也尚未实现 yield thenable assimilation、显式 await、
async function 与 async module execution。该偏差只属于 M10 第一纵切，不能冻结为公开原型链契约。

### 7.2 宿主 Rust async

宿主异步函数同步创建一个 JavaScript Promise，并返回一个可由任意 Rust executor 驱动的
future：

```rust,ignore
type HostFuture = Pin<
    Box<dyn Future<Output = Result<Box<dyn ErasedIntoJs>, HostError>> + Send + 'static>,
>;

enum HostCall {
    Ready(Result<Value, HostError>),
    Async(HostFuture),
}
```

Host future 不能直接访问 JavaScript heap，也不能持有 borrowed `Value`。完成后只能通过
`Send` channel 发送自有数据：

```rust,ignore
struct HostCompletion {
    isolate_id: IsolateId,
    promise_id: PersistentId,
    result: Result<Box<dyn ErasedIntoJs>, HostError>,
}
```

`ErasedIntoJs: Send + 'static` 是 generic `IntoJs` 结果在内部的类型擦除形式，实际转换只在
isolate 内发生。如需保留 JS 对象，只能使用不可在宿主线程解引用的 opaque persistent
ID；它对应的 root table 仍由 isolate 独占维护。

宿主完成事件回到 isolate 后，依次执行：调用 erased `IntoJs`、resolve/reject Promise、
加入 reaction jobs、在 microtask checkpoint 恢复 fiber。

### 7.3 跨线程值与 Host SDK

Rust SDK 的主要数据接口是 owned typed conversion，而不是要求用户直接构造固定的
`HostValue` enum：

```rust,ignore
trait FromJsOwned: Sized {
    fn from_js(
        scope: &mut RunningScope<'_>,
        value: Value,
    ) -> Result<Self, ConversionError>;
}

trait IntoJs {
    fn into_js(
        self,
        scope: &mut RunningScope<'_>,
    ) -> Result<Value, ConversionError>;
}
```

async host function 在 isolate 内先将参数转换成 `FromJsOwned + Send + 'static`，future
完成后把 `IntoJs + Send + 'static` 的 Rust 结果送回 isolate，再创建 JS value。Serde
集成放在独立 `tachyon-serde` crate 或 feature 中，core 不强依赖 Serde。

完整 JS graph 的跨 isolate 传输使用与 Web/Deno 语义一致的 structured clone。最终支持
cycle/shared identity、Map/Set、Date、RegExp、Error、BigInt、ArrayBuffer、TypedArray 和
DataView；Function、Promise、WeakMap/WeakSet 等不可 clone 的值返回 `DataCloneError`。
ArrayBuffer API 明确区分复制和 transfer，transfer 会 detach 源对象，不能隐式发生。

同 isolate 的长期对象引用使用 opaque `Persistent<T>: Send + Sync`。它仅保存 isolate
handle 和 root ID，不能 `Deref`，所有操作必须回到所属 isolate。用户自定义 host object
可以共享 `Arc<T: Send + Sync>`，每个 isolate 创建自己的 JS wrapper；共享 Rust 对象不代表
共享 JS heap object。

分层上，`tachyon-gc` 只实现 isolate-relative 的 8-byte `PersistentRootId<T>` 与 generation
slab/free-list；该 ID 只是可进入 actor command 的 opaque data，不携带 owner capability、不能自行
选择目标 isolate或解引用。公开 facade
的 `Persistent<T>: Send + Sync` 组合 `IsolateHandle + PersistentRootId`，Clone/Drop 转换成所属 isolate
的 actor command。direct `&mut Isolate`/`RunningScope` 路径执行同一 clone/release command 语义，
不得让 handle 的 Drop 直接解引用或修改 GC root table。

### 7.4 Rust-native 扩展与未来 FFI

Rust Host SDK 是 Tachyon 相对 C 引擎 wrapper 的核心产品能力，不得实现为 QuickJS 风格
C API 的一层语法包装。扩展边界按能力拆分，普通 JavaScript opcode、属性访问和函数调用
热路径不得经过通用 plugin trait object。

扩展在 `EngineBuilder` 或 isolate template 创建阶段组合：

```rust,ignore
trait Extension: Send + Sync + 'static {
    fn install(&self, builder: &mut ExtensionBuilder<'_>) -> Result<(), ExtensionError>;
}

trait ModuleLoader: Send + Sync + 'static {
    fn resolve(&self, request: ModuleRequest) -> ResolveFuture;
    fn load(&self, module: ResolvedModule) -> LoadFuture;
}
```

`ExtensionBuilder` 可注册同步/异步 native function、native class、accessor、synthetic module、
module loader、isolate/realm 初始化器、promise rejection hook、host state、clock、entropy source
和 external buffer factory。注册完成后生成不可变 descriptor/table；调用时只做专用表项的
一次间接调用，不在每个 opcode 上查询扩展 registry。

同步 native callback 接收 `CallScope<'scope>`，可以通过 scoped `Local<'scope, T>` 零拷贝读取
参数、分配 JS value，并在明确的 recursion limit 下同步调用 JS。borrowed handle 不得逃逸。
异步 native callback 在返回 future 前必须把参数转换为 `FromJsOwned + Send + 'static`；future
不能访问 heap，结果回到 isolate 后才执行 `IntoJs`。宿主 callback/command 属于可信 Rust 代码；
panic 直接 abort，不捕获、不恢复，也不转换为 JavaScript throw 或 poisoned isolate。

host state 使用 type-safe engine/isolate/realm resource table，不允许扩展依赖进程全局变量或
持久 TLS。native class 的 Rust payload 使用 `Arc<T: Send + Sync>` 或 isolate-local resource
ID；普通 JS wrapper 的属性仍由 VM shape/inline cache 处理，只有 native accessor/method 被调用
时才进入 host boundary。

extension 表示构建期组合能力，不支持从活跃 isolate 热卸载。已创建的 function、class、module
和 persistent handle 可能长期引用扩展状态，热卸载会破坏生命周期和 GC 不变量。需要动态能力
开关时，通过可更新的 `Arc<T>` host state 实现，而不是卸载代码描述符。

未来 C ABI 必须是同一执行内核的薄 adapter，不另建一套对象或 async 模型。ABI 只暴露 opaque
engine/isolate/module/scope/persistent/async-token handle、status/error、函数指针加 userdata/drop
callback，以及 manual poll/wake。不得暴露 Rust layout、`Value(u64)`、GC pointer、`Vec`、trait
object 或 Rust panic/unwind。外部语言异常必须由调用方 adapter 在进入 Tachyon ABI 前转换为显式
status；异常跨 ABI 属于调用方契约违例。异步 token 可从任意线程 resolve/reject，并复用 actor completion queue。
首个商业版本仍不发布或承诺稳定 C ABI，但实现期间必须维护一个不发布的 FFI smoke adapter，
证明公开 Rust SDK 没有依赖无法跨 ABI 表达的隐藏生命周期。

相对 QuickJS，Tachyon Rust SDK 至少必须提供：编译期约束的 scoped/owned conversion、无需手工
`dup/free` 的 handle 生命周期、executor-neutral host future、类型安全 host state、可组合扩展、
异步 module loader、零拷贝/transfer external buffer、显式配额与取消，以及跨线程 actor handle。

### 7.5 取消与关闭

- **方向**：pending host future 由 isolate 的任务表持有，并支持 abort/cancellation token。
- **方向**：丢弃 Rust 顶层 execution future 时，不得留下永久 GC root。
- **开放**：取消映射为内部终止、Promise rejection，还是宿主可配置策略。
- **决定**：取消和 isolate shutdown 只能在 safepoint 修改 VM 状态。

## 8. GC 设计

### 8.1 目标模型

BDWGC 适合作为 Escargot 的成熟嵌入式方案，但 Tachyon JS 应采用精确 tracing GC，避免
保守扫描造成的误保活和不可控根集合。Dumpster 的引用计数加循环检测不适合作为 JS
主堆模型，因为每次属性和寄存器写入都会引入引用计数成本。

计划演进：

1. **Phase 1A**：精确、非移动、stop-the-world epoch mark-sweep，先证明 trace/root/handle。
2. **Phase 1B**：非移动 young cohort spans、bump allocation、remembered cards 和 minor GC。
3. **Phase 2**：老年代三色增量 marking、incremental/lazy sweep 和 allocation-debt pacing。
4. **1.0 之后研究**：并发 marking 或选择性压缩；不是当前对象表示和性能门槛的前提。

1.0 的所有 collector phase 都由 isolate 的单 mutator 在 safepoint 同线程推进。“增量”只表示
一次 major collection 拆成多个有界 work slice，中间允许 mutator 继续执行，不表示后台或并发
marker。collector phase、bitmap、card table、bump cursor、gray queue 和 root table 都使用普通
非原子内存；禁止为它们引入锁、channel、atomic color、spin 或 GC thread。

Phase 1A 就按最终三色状态组织 mark，并从第一次 heap field/root store 开始经过统一 barrier API。
Phase 1A barrier 是可内联空操作；Phase 1B 增加 card marking；Phase 2 增加 incremental shading，
不重写对象系统或改变 `GcRef` 表示。

### 8.2 堆、对象头与 Side Metadata

heap 使用 32-bit logical byte offset，不要求连续 native address reservation。offset 的高 16 bits 是
`SpanId`，低 16 bits 是 64 KiB span 内 byte offset；offset 0 保留为空引用。span table 按历史峰值
增长并保留稳定 index，entry 的 storage 由 Rust allocator 按需分配；table 自身移动不影响 `GcRef`，
因为每次借用对象都重新通过 `SpanId` resolve。Tachyon 不直接调用 mmap/VirtualAlloc，也不依赖
page protection；底层 allocator 如何取得 memory 不是 engine contract，未来 Wasm backend 可复用
同一 logical addressing model，但 wasm32 仍不属于 1.0 target。

小对象 span 使用单一 size class，最小 slot 为 16 字节；大对象占用连续 logical SpanId range，
storage 可由独立 large allocation 持有并由 continuation entries 定位。每个 GC 对象包含 8-byte header：

```rust,ignore
#[repr(C)]
struct GcHeader {
    type_id: u16,
    flags: u16,
    aux: u32,
}
```

`type_id` 索引静态 trace/drop vtable。小对象大小来自 span size class；普通对象的 `aux` 保存长度、
对象变体或大对象信息。`flags` 的最高位由 GC 保留为 `EXTERNAL_BYTES`：置位时 `aux` 是该对象拥有的
精确 out-of-line backing charge，普通 allocation 不得伪造该 flag，也不能同时把 `aux` 用作类型字段。
charge 最大为 `u32::MAX`；更大的单 backing 在发布前返回 typed resource error。free slot 可复用对象
空间保存 free-list next。

实现 `GcExternalMemory` 的 immutable payload 只能通过 `try_allocate_external[_with_gc]` 发布。allocator
把 span growth 与 external bytes 合并做 hard-limit/pressure decision，但 young storage cap 只计算实际
Eden/Survivor span backing。minor/major sweep 在 unpublish 成功后、调用 destructor 前扣除 header charge，
并在调用 destructor 前完成 header charge 扣除；small/large owner 共用此契约。Drop panic 直接 abort，
collector 不提供恢复或重试语义。payload backing 若需
改变容量，必须等待带 heap accounting 的 replacement API，不能通过 `NoGcScope` 原地扩缩后让 header
charge 失真。宿主手工 charge 与 GC-object charge 使用两个独立的普通 `usize` 账本；公开统计与 hard
limit 合并两者，但 `release_external` 只能减少宿主账本，sweep 只能减少对象账本，避免宿主释放误扣
仍存活对象的 backing。

Map/Set 的 insertion-order backing 是该规则的首个 VM consumer：`OrderedCollection` 是 fixed-size
boxed entry slice，发布后不改变 external byte charge。满容量时 VM 在 GC-aware allocation point 分配并复制
replacement backing，再通过 barrier 更新 Map/Set exotic 的 private storage edge；iterator 保存 exotic
identity 与 physical cursor，而不保存 backing identity，因此 replacement 后仍正确观察后续插入，delete/clear
保留 tombstone 也不破坏 cursor。初始容量和增长倍率统一定义在
`tachyon-vm::tuning::collections`，之后的 hash-table specialization 必须保持这个 resource boundary。

`new Map(iterable)` 与 `new Set(iterable)` 不能把 Rust loop 或 native stack 作为 iterator protocol 的
owner。构造开始时分配 traced `PendingCollectionInitializer`，它精确保存 target、iterable、iterator、cached
`next`、当前 result、Map key 与一次性读取的 `set`/`add`；每个 observable `Get` 或 `Call` 都以 typed native
continuation 恢复。这样 accessor、用户 iterator、被替换的 adder 与 forced GC 均经过同一显式 fiber，且不会
重新读取 cached method。异常完成的 `IteratorClose` 仍由通用 iterator-close continuation 收敛，不得由此状态
机在 Rust 栈上补偿调用。

Map/Set `forEach` 使用独立的 traced callback state 保存 branded collection、callback、thisArg 与 physical
cursor；每次 callback 前重新读取当前 replacement backing 的 `used` 和 entry，跳过 tombstone。因此回调中的
delete、clear、re-add 和 append 按规范影响后续访问；state 可能在 callback 中晋升到 Old，写入新 entry
value/key 时必须执行 normal value barrier。callback return value 被丢弃，throw 仍经 fiber abrupt path 传播。

每个 live small-object span 的 side metadata 至少保存：size class、space kind、cohort age、allocation
bitmap、mark bitmap、`CollectionEpoch`、bump/free-list state、512-byte-granularity card bitmap、
sweep state、span reuse generation 和 accounting。span table 使用受配额的渐进 `try_reserve` 与
free-index ranges，不预先
分配 65,536 entries；初始 hint、增长和 retained peak 进入 capacity instrumentation。

`CollectionEpoch` 使用 `NonZeroU32`。每次 minor/major collection 取得新 epoch；span 的 bitmap epoch
不等于 current epoch 时，所有 allocated slots 逻辑上都是 unmarked。第一次在当前 epoch 标记该 span
时才清 bitmap 并更新 epoch，避免 collection 开始时扫描全 heap。epoch 溢出走全 span bitmap reset 后
从 1 重新开始，必须有 forced-wrap 测试，不能依赖“实际上不会溢出”。

三色不保存独立 `Color` 或 black bitmap：current epoch 未 mark 是 white；首次 set mark bit 并进入
gray queue 是 gray；descriptor trace 完成且不再位于 queue 是 black。gray queue 只保存 32-bit
logical offsets，mark bit 保证每对象每轮最多入队一次。对象出队到 trace 返回之间的瞬时 gray 状态
只有单线程 collector 可见，无需额外编码。sweep 根据 allocation bitmap 与 current-epoch mark 判断
dead slot 并批量 drop/rebuild free list；不得维护对象内 allgc/gclist/root-count 或 atomic color。

GC vtable 的 `drop` 不得重新进入 VM、分配 GC 对象或执行 JS。复杂 host resource 清理应
enqueue cleanup job 并在安全点执行。debug build 使用 allocation bitmap 验证 `GcRef`
的 SpanId/table entry、logical offset、slot boundary、alignment、allocation bit、type ID 和存活状态。

isolate memory limit 统计 span storage、span table/side metadata、feedback、external string/
ArrayBuffer backing store 和 pending host value。共享 backing store 另外受 engine-global
limit 约束，不能通过共享资源绕过配额。

collection trigger 以 allocated-byte debt、young storage cap、old growth、heap hard limit、显式 force
和 safepoint memory-pressure command 为输入。不得由后台线程、周期时钟或每-opcode atomic polling
触发。allocation slow path 偿还有界 GC work；Phase 2 的可选时间上限使用宿主注入 clock 并稀疏采样，
主预算仍是 bytes/edges/spans/work units。

`Heap::try_allocate` 是不隐式 collection 的底层 publication primitive：它没有 VM/realm/host subsystem
的完整 roots authority，但每次成功 publication 仍累计 effective Young/Old allocation debt。正常引擎
分配使用 `try_allocate_with_gc`，显式传入 `&mut dyn Trace` 完整 subsystem roots；preflight 先按 immutable
descriptor 归一化 `OldOnly` policy，再计算 object bytes、是否需要新 span storage 与 collection action。
若 collection 被触发，尚未 publication 的 Rust payload 自身也临时加入 strong roots，避免其 heap edge
在 forced collection 中成为唯一遗漏的引用。每个 allocation point 最多执行一次 minor 或 major，成功
后仅 publication 一次；collection 后仍无法满足 hard limit 时直接返回 typed error，不形成无进展 retry
loop。minor 偿还 young debt，major 同时偿还 young/old debt；manual collection 也使用同一偿还规则。

trigger config 是 typed per-heap policy，默认 debt/pressure knobs 只定义在 `tachyon-gc::tuning`。forced-minor
只作用于 effective Young allocation，forced-major 作用于每个 managed allocation；memory-pressure command
在 isolate 普通内存中 coalesce，并在下一 managed allocation 消费一次。统计分别记录 allocated/debt bytes、
minor/major attempts/successes 以及 forced、debt、heap-limit、heap-pressure 和 pressure-command 来源；整个
路径不含 atomic、lock、channel、thread、spin、clock polling 或 mmap。

### 8.3 精确根集合

GC roots 至少包括：

- 所有 runnable、awaiting 和 yielded fiber 的寄存器与 frame environment。
- microtask 和 Promise reaction queue。
- pending host Promise 表。
- persistent handle/root table。
- realm/global object、模块环境和宿主注册对象。
- 正在运行的 `RunningScope` 临时 root stack。
- debugger 暂停帧与尚未 release 的、受配额限制的 remote object groups。

首版可扫描 fiber 的全部有效寄存器；之后可为 safepoint 生成 register liveness map。
不得依赖原生栈保守扫描。

GC 设计必须显式支持 weak reference、ephemeron/WeakMap、FinalizationRegistry 和终结顺序，
这些能力不能在对象模型完成后再作为普通引用特例补入。

collection phase 固定为：strong roots/edges fixed point、ephemeron fixed point、kept-object 与 weak
clearing、enqueue finalization cleanup、sweep、safepoint 后运行 cleanup jobs。trace/drop/sweep 不得
执行 JS、poll host future、重新进入 allocator 或直接运行用户 finalizer。finalizer/pinned payload
直接进入 old space，避免 minor phase承担外部资源终结顺序。

`Tracer` contract 分离 strong edge、nullable weak edge 与 ephemeron key/value pair。strong trace 遇到
weak/ephemeron 只把当前 owner 的 logical reference 和 edge-kind flags 放入受 heap object quota 限制的
high-water worklist，不缓存 payload pointer，也不把 weak target 标记。strong fixed point 后只重扫这些
owners：live ephemeron key 使 value 进入同一 gray queue，反复 drain 到 fixed point；随后 re-resolve owner
并原地 clear dead weak slot 与 dead-key ephemeron。minor liveness 把 Old target 视为 live，只查询 young
mark bit；major 查询全部 target mark bit。invalid weak reference 中止 collection 且不清字段。kept-object
job lifetime 与 finalization enqueue 已接入该 phase boundary；JS cleanup callback 由 VM safepoint scheduler
执行，collector 当前不得执行 callback 或假装拥有 realm/callable 语义。

`WeakMap` 与 `WeakSet` 的首个 ECMAScript binding 使用独立 `WeakCollection` payload：其 fixed-size
`Box<[Option<Ephemeron<()>>]>` 是唯一 private backing，ordinary object base 与 backing 都由 exotic
payload 强持有，但 key/value 对只经 ephemeron phase 条件标记。ephemeron entries 保持 insertion order，
使常见链式 WeakMap 的 fixed point 可沿正向单 pass 传播；独立 bucket index 以稳定 `RawHeapRef` logical
address 做 power-of-two open addressing，查询只持有一次 no-GC borrow。删除用 tombstone 维持 probe chain，
collector weak phase 后从 entry key 原地重建 index/free-list；扩容时只 rehash live ephemeron。哈希乘数与容量策略
集中在 tuning；扩容必须先复制到新、准确计费的 payload，再以 old-to-young barrier 发布。构造器沿用 Map/Set 的 typed native
continuation，所以 iterable/adder/getter 的 JS 调用绝不落在 Rust 栈上。普通 Symbol 与 object key 都先按
CanBeHeldWeakly route 校验后生成 erased `GcRef`；该 erased reference 仅来自已通过 isolate type validation
的 `Value`，不引入 `unsafe` retype。Weak collection 没有枚举、size 或 iterator，因此 collector clear 后的
逻辑 live count 不可作为可观察结果；probe 最多检查 capacity 个槽，满表才进入受 heap quota 的扩容。

AddToKeptObjects 使用 isolate-owned、去重且受 object quota 限制的 job-scoped root buffer；collection
不会自动清除，只有显式 ECMAScript job boundary 才 clear。finalization registration 的 target 是 weak，
held value 在 registry live 时是 strong；dead target 在 weak clearing 后、sweep 前先为 FIFO pending queue
reserve，再把 `(owner cell, held value)` 发布为 cleanup record 并清 registration。pending records 在 scheduler
消费前作为精确 roots；queue 只保存命令，不执行 JS。VM scheduler 取出 record 后必须先建立自己的 scope
roots，再允许 allocation/callback。finalizer/pinned Rust payload 的 immutable descriptor policy 为
`OldOnly`，调用方传 Young 也不能绕过；重复 registration policy 冲突是 typed error。

VM cleanup scheduler 使用 isolate-owned `VecDeque`。只有 VM queue 为空时，才读取 GC pending queue 的
入场长度、为完整快照 `try_reserve_exact`，然后 FIFO 转移；reserve 失败不弹出任何 GC record。转移后的
job 在 callback 返回前一直留在队首，并由 `Isolate::trace_roots` 追踪 registry 与 held value，因此
callback 内 allocation/full GC 不产生失根窗口。callback 内新产生的 finalization record 留在 GC queue，
下一 safepoint 才转移，避免 cleanup 不断产生 cleanup 时单次 checkpoint 无界运行。

cleanup callback 返回 JS throw 时，当前 job 已完成且出队，错误连同 completed/remaining/deferred 统计
返回 VM job runner；尚未运行的旧 job 保持 FIFO，下一次调用先清旧队列，不与新 GC records 交错。
递归进入同一 cleanup scheduler 返回 typed `Reentrant`。callback 的显式错误会消费已经开始的 job 并
恢复 running state；Rust panic 直接 abort，scheduler 不提供 panic 后恢复语义。M5/M8 binding 从 owner
cell 解析 registry callback，复用 Promise checkpoint 和普通 non-recursive call trampoline 调用实际
ECMAScript cleanup callback；GC crate 与 generic host callback 均不代替该调用。

### 8.4 Non-moving Generations

small-object span 属于 `Eden`、`Survivor { age }` 或 `Old` cohort。Eden span 按 size class 使用 bump
cursor 分配；large、pinned、finalizer 和配置阈值以上的对象直接进入 Old。minor GC 永不复制对象：

1. 从全部 precise roots 与 dirty old cards 找到 young edge。
2. gray queue 只 enqueue Eden/Survivor target；young trace 遇到 old edge 不递归扫描 old graph。
3. 对 young spans 执行 ephemeron/weak phase 并 sweep dead slot；完全空 span 立即回到 eden pool。
4. 有存活对象的 Eden 变 Survivor；Survivor 达到 cohort age 后整 span 变 Old，不改变任一 `GcRef`。
5. promotion 时扫描该 span 并为仍指向 young 的 card 建立 remembered state；dead slot 变成 old free list。

Survivor span 在 promotion 前不接纳新对象，确保 span age 同质。低存活率造成的暂时空洞最多保留有限
minor cycles，promotion 后由 old size-class free list 复用；promotion age、young storage cap 和是否
提前晋升高 occupancy span 由 corpus/benchmark 决定。该策略用短期 span slack 换取零 forwarding、
零对象复制、稳定 FFI/debugger address 和简单 side metadata。

从 Phase 1A 开始，所有 trace visitor 接收 `&mut GcRef`/`&mut Value`，但 1.0 collector 不重写它们。
所有 heap/root pointer store 经过统一 barrier：Phase 1B 对 old-to-young store 标记 512-byte card；
Phase 2 major marking active 时，不查询 source 是否 black，而是把每个尚未标记的新 target 置为 current
epoch mark 并入 gray queue，新分配对象直接设置 current mark、语义上 born-black。这个保守 insertion
barrier 只需 isolate-local
普通 bool、mark bitmap 和 queue；不扫描 queue，也不增加 color/atomic bitmap。更精细 SATB/Dijkstra
方案只有 benchmark 证明 store/mark work 明显下降后才替换。

remembered owner discovery 使用稳定 `SpanEntry` 中的 intrusive single-linked index chain。clean-to-dirty
transition 只修改普通 side metadata 并 O(1) 链入，不分配 `Vec`、不扩容，也不扫描全部 Old spans；成功
minor mark 根据实际 direct young edge 重建 card/large-owner 状态并原地压缩链，失败则保留原 remembered
状态。链节点是 logical `SpanId`，span table storage 移动不影响它，也不引入 atomic/lock。

`Heap::verify_generational_barriers` 是独立的 full-Old-heap diagnostic traversal，不复用会修改 mark bits、
weak slots 或 remembered metadata 的 young marker。它从 allocation bitmap 枚举全部 Old small objects，
从 owner entry 枚举 large object，并通过 immutable descriptor 访问 strong、weak、ephemeron 与 finalization
的每条 direct edge。每条 Old→Young edge 必须同时满足：target 是有效 live allocation；small source 所在
512-byte card dirty 或 large owner remembered bit set；source owner 已进入 intrusive remembered chain。
错误精确报告 source/target 和缺失层级，统计 scanned owners/edges 与 small/large edge 数。

optimized dev/test profile 关闭 `debug_assertions`，因此自动验证不能依赖该 cfg。GC unit tests始终在每次
minor entry 运行 verifier；跨 crate diagnostic/stress 通过无默认开启的 `barrier-verifier` feature 接入，
普通 release 不启用该 feature 时没有 full-heap scan。固定 seed randomized stress 交错 forced-minor/major，
覆盖 rooted cycle、weak clearing、ephemeron closure、finalization enqueue、promotion、logical span reuse 与
独立 low-limit failure；seed 和步数属于可复现 test fixture，不是运行时 tuning knob。

young sweep 同样使用 `SpanEntry` intrusive chain，只遍历 Eden/Survivor，不为每次 minor 构造 worklist
或扫描历史 Old entries。完全空 span 优先进入 per-size-class bounded Eden pool；pool 使用由
`EDEN_POOL_SPANS_PER_SIZE_CLASS` 控制的二维 fixed array，默认每 class 1 个，不含 `Vec`、reserve 或 push。
pooled span 保留 allocator backing 但从 young intrusive chain 脱离，激活时 O(1) 重新链入并重置为干净
Eden metadata；pool 满时额外 empty span 仍立即 release。pool reuse 保持 logical/native backing，不保留
任何 live object identity，并递增诊断 reuse generation。

pooled backing 始终计入 committed heap bytes 和 hard limit。full major、heap-limit recovery、显式 memory
pressure 与 host `trim_eden_pool_storage` 会释放 pool；major stats 包含 trim spans/bytes，但 heap accounting
只扣一次。Apple M5/aarch64、rustc 1.99.0-nightly、bench profile thin-LTO/codegen-units=1/debug=2 上，
`cargo bench -p tachyon-gc --bench eden_pool` 的 9-sample median（每 sample 4,096 allocation+minor cycles）
为 pooled 185.1 ns/op、每轮 trim/reallocate 734.7 ns/op，immediate-release 慢 3.970x，因此默认 retention
取 1；该值仍是 tuning knob，不是 API contract。初始 whole-span promotion age 为 2，同样只定义在 GC
tuning 模块。

young growth 在需要新增 backing 且 active Eden/Survivor storage 将超过 cap 时触发 minor；pooled empty
backing 不算 active young storage，但仍计入 heap hard limit。默认 cap 为 8 MiB，`GcTriggerConfig` 提供
validated per-heap override，小于一个 64 KiB span 的不可满足 cap 被拒绝。high-survival span 可在正常 age
前 whole-span promotion：
current-epoch live marks 达到 size-class slots 的 80% 即为 early candidate。marker 先用 epoch-qualified
mark bitmap count 决定候选并准备 cards，sweep 使用同一 centralized integer predicate 晋升；禁止用本轮
allocation count 把 dead slots 算成 occupancy，也不能先晋升后补 barrier metadata。age=2、occupancy=80%
与 cap=8 MiB 都是初始 tuning defaults；完整 JS allocation/survival corpus 建立前不视为冻结结论。

minor 与 incremental major phase 不交错，共用一张 epoch mark bitmap。minor 是 bounded STW phase；
major active 时继续使用 Eden spans 分配但不启动 minor，新对象 born-black，所有初始化 edge 仍经过
insertion barrier。allocation slow path 按新增 bytes 偿还足够 major work，确保 marker 正常情况下在
major-allocation reserve 耗尽前完成；reserve exhaustion 强制完成 mark/weak closure 后才能 minor，
并单独记录该 fallback pause。不得通过第二套并发 marker 或 atomic bitmap 解决 phase overlap。

运行时区分 `RunningScope` 与 `NoGcScope`：前者可以分配但只能持有 rooted handle；后者可以取得
内部 `&T`/`&mut T`，但禁止任何可能分配或触发 GC 的调用。raw heap reference、slice 和内部指针
不得跨 allocation/safepoint。即使对象永久非移动，allocation/collection 仍可能回收未 root 对象，
且该限制保证 Rust alias、host callback 与内部表示不泄漏到 SDK。`NoGcScope::borrow_reference{_mut}`
只用于引用已经存在于 traced owner（例如 active frame environment）且无需跨未来 collection 保活的热路径；
它跳过重复 temporary-root publication，但仍执行 heap/type/liveness/owner validation，返回 borrow 不能逃出
no-GC lifetime。`borrow_raw_reference` 额外接受 tagged value 的 untyped logical address，但只在内部立即
retype 并复用同一 checked payload boundary，不返回 `GcRef<T>`。未由 traced owner 保活的引用不得借此跨
safepoint，安全 API 也不提供这种生命周期。

准备写入 heap payload 的 Rust 局部 `Value`、`GcRef`、descriptor 或参数 `Vec` 不属于 roots。只要进入的
helper 可能在真正 payload allocation 前执行另一次 allocation（例如读取虚拟 function metadata），调用者
就必须先把这些边发布到 fiber register/native continuation/managed pending state，或让该 allocation 的
`Trace` roots 直接拥有完整 pending payload。仅在最终 `try_allocate_with_gc` 调用处 root value 太晚；
forced-major 的 class inferred-name fixture 专门覆盖这个窗口。首次物化 function `name` 因此走 fresh
metadata slot 路径，不先分配一个仅用于比较的虚拟 name 值。

minor GC 只在 safepoint 同步执行，不跨 Rust poll 中断，通过 young storage cap 约束暂停。记录
allocated/marked/scanned/reclaimed bytes、eden/survivor/old span count、whole-span promotion、card
scan/false-positive、fragmentation/slack 和 pause distribution；不存在 copied/forwarded bytes 指标。
collection result 记录累计 Young/Old allocated bytes、非空 Young/Old backing span、pool retained bytes 与
dirty-card false-positive。pause aggregator 为 minor/major 各保留 66 个 fixed log2 nanosecond buckets，
输出 sample/total/max 与 P50/P95/P99 upper bounds；`Heap::record_collection_pause` 只接受宿主注入单调
clock 已测得的 `Duration`，collector 不读取系统时钟、不保存逐次 sample、不扩容。Eden benchmark 已接入
该 report：4,096 个 pooled minor samples 的 P50 upper bound 为 128 ns、P95/P99 为 256 ns，max 250 ns
（同上硬件/构建配置）。

## 9. 对象模型与性能基础

- `Object` 使用 shape/hidden-class 与连续 property storage。
- 常见属性读取和写入使用单态/多态 inline cache。
- 数组区分 packed、holey 和 dictionary elements。
- 字符串至少区分 interned/atom、普通 owned string 和延迟拼接表示。
- 内建函数和抽象操作需要清晰分层，避免解释器 opcode 中堆积规范实现。
- 分配器按 size class/page 管理小对象，避免每个 JS 对象单独调用系统 allocator。

对象语义使用闭合 internal-method 分派，而不是让 builtin、conversion 和 opcode 直接识别每种
GC payload。`ObjectReceiver`/后续 `ObjectKind` 只包含引擎已实现的对象种类；ordinary 路径经静态
`match` 内联，Array/String/TypedArray/Proxy/Module Namespace 等 exotic 进入 cold slow path，不使用
`dyn Trait` 或每次属性访问的 vtable。统一边界至少拥有 `[[GetOwnProperty]]`、
`[[DefineOwnProperty]]`、`[[Delete]]`、`[[OwnPropertyKeys]]`、`[[Get]]`、`[[Set]]`、
`[[HasProperty]]` 与 prototype/extensibility 操作。新增对象种类必须先接入该边界，builtin 不得新增
`checked_reference` 类型链或 shape/function metadata 特判。

`Object.prototype.toLocaleString` 是 observable composition，而不是 `ObjectToString` 的别名。实现固定按
`Get(receiver, "toString")`、callable validation、`Call(method, receiver, [])` 的顺序执行；null/undefined
在首次 Get 前抛 TypeError，primitive receiver 由统一 property lookup 处理。两阶段 typed continuation 的
`Get` stage 保存原始 receiver并允许 accessor/Proxy `[[Get]]` 挂起，`Call` stage仍以该receiver作为this。
continuation复用既有completion stack与callback trampoline，不给104-byte `Frame`增加字段，也不让ordinary
property read承担额外分支。任何同步dispatch失败必须弹出尚未消费的parent continuation；bytecode getter、
Proxy trap或toString method挂起时则由callee frame return消费。forced-major与N=1/2/4/8/16是该组合契约的
固定回归矩阵。

`Object.prototype.isPrototypeOf`必须先判断参数V是否为Object；只有V为Object时才对this执行ToObject，
因此null/undefined receiver加primitive V返回false，而相同receiver加object V抛TypeError。ordinary chain
在native builtin内迭代；遇到Proxy时发布只保存目标prototype identity与原始call site的typed parent
continuation，再调用统一`[[GetPrototypeOf]]` dispatcher。trap getter/call返回后继续同一循环，不递归运行
interpreter，也不为ordinary walk分配state。惰性ordinary function `prototype`对象的`[[Prototype]]`固定为
当前realm的`%Object.prototype%`，不是null；该realm edge与constructor identity在prototype/storage分配期间
必须同时进入`PrototypeInitializationRoots`。

Legacy `__defineGetter__`/`__defineSetter__` 保留 ES5 的 observable 顺序：先对 receiver 做
RequireObjectCoercible/ToObject，再验证 callback callable，之后才执行 ToPropertyKey。key conversion
continuation 的 pending state 同时 trace receiver/callback；完成后构造只有一个 getter 或 setter、且
`enumerable/configurable=true` 的 accessor descriptor。ordinary receiver直接进入已有 property mutation core；
Proxy receiver使用同一 `[[DefineOwnProperty]]` dispatcher，但增加 `LegacyAccessor` result mode 将成功映射为
undefined并将trap false转为 abrupt completion。该 mode 不能复制一套 Proxy invariant算法，也不能把
descriptor临时物化为用户可观察的普通对象。

Legacy `__lookupGetter__`/`__lookupSetter__` 共享同一个 prototype-chain consumer。ordinary object的
own descriptor与prototype读取保持同步无分配；遇到Proxy时，parent continuation保存canonical key、当前
Proxy identity、getter/setter mode和原始call site。Proxy `[[GetOwnProperty]]` lookup mode可在destination
写内部Hole表示descriptor absent：data descriptor、generic descriptor、accessor缺少目标slot都必须写
undefined而不是Hole，因为它们按规范终止搜索。只有parent消费Hole后才调用同一Proxy的
`[[GetPrototypeOf]]`，随后继续循环；Hole不得返回JS、进入property storage或跨越未受typed parent保护的
safepoint。该组合复用现有Proxy invariant实现，不物化descriptor object，不扩大Frame。

`%Object.prototype%.__proto__` 是真正的intrinsic accessor pair，不是属性读写opcode里的名称特判。
getter执行ToObject后调用统一`[[GetPrototypeOf]]`；setter先RequireObjectCoercible，再对invalid prototype或
primitive receiver返回undefined，最后调用统一`[[SetPrototypeOf]]`并把false转TypeError。Proxy
setPrototype dispatcher的LegacyAccessor mode只改变公开result mapping，不复制trap/invariant算法。
OrdinarySetPrototypeOf的cycle walk遇到Proxy必须停止，因为后续GetPrototypeOf不是ordinary internal method；
realm `%Object.prototype%`在same-prototype fast path之后拒绝任何不同prototype，表达immutable-prototype
 exotic约束。Proxy `[[Set]]` assignment使用独立 `ProxySet` typed continuation/state，保存target、canonical
key、value、receiver与active Proxy；trap调用严格传递 `(target, key, value, receiver)`，trap getter/call均可
跨GC挂起。缺省trap转入既有 ordinary `[[Set]]`/Reflect receiver路径，assignment false在strict边界抛错，
Reflect.set保留boolean结果。该dispatcher由所有属性赋值统一入口调用，`__proto__` 不再按名称特判。当前
truthy trap随后复用target own descriptor和SameValue，拒绝冻结data的不同值及non-configurable、
setter-less accessor。ordinary write resolver返回`Write | Proxy`，因此prototype链遇到Proxy时保留原receiver
进入同一dispatcher。缺省trap且receiver为Proxy时，只有确认handler的getOwnPropertyDescriptor与
receiver Proxy 的 descriptor composition 已实现为 `ReceiverGetOwn -> ReceiverDefine` typed parent stages，
复用既有 Proxy get-own/define invariant，不在 ProxySet 中复制 trap 规则。internal `SetReceiver` mode只发布
absent/writable/blocking三态，不为OrdinarySet物化携带无关heap value的公开descriptor object。ProxySet key/state、
get-own trap state及公开descriptor的value/get/set edges都必须在下一次intern/materialize allocation前进入
destination register、managed state或temporary typed continuation。Reflect.set ordinary prototype
boundary 同样返回 `Write | Proxy`。该路径同时修正 ArraySetLength shrink 对旧 indexed slots 的删除、
RegExp virtual flag 的只读写入、String wrapper own-index descriptor/read，以及 Function virtual prototype
的 value-only DefineOwnProperty。当前剩余仅 indexed accessor descriptor 前置能力和 cross-realm 测试边界。

Proxy 从 `ProxyCreate` 起使用独立、无 ordinary-property base 的 GC payload，只 trace `[[ProxyTarget]]`
与 `[[ProxyHandler]]`；这使尚未接入的 trap 无法通过普通 snapshot 路径被静默忽略。`%Proxy%` 本身是
constructible native function，但与 ordinary/class constructors 不同，不拥有默认 `prototype` property，
因此 callable metadata 分离 `is_constructor` 与 `has_default_prototype`。当前构造参数验证、realm root、
精确 GC edges 和全局发布已接入；call/construct capability 继承、revocation 与剩余 essential internal
methods 必须经 typed exotic dispatch/continuation 继续完成，不能用空 handler 特判绕过统一边界。

首批 typed exotic dispatch 覆盖 `[[GetPrototypeOf]]`、`[[IsExtensible]]` 与 `[[PreventExtensions]]`。
入口先读取并验证 handler，再以 ordinary observable `GetMethod` 查 trap；data/missing fast path同步完成，
accessor trap getter和其返回的 bytecode trap分别占用 `TrapGetter`/`TrapCall` continuation stage，JS callback
始终在显式 frame 中执行。continuation 的 first slot保留 Proxy identity，second slot在 getter stage保留
handler、在 call stage保留动态 trap callable，因此两次 callback与 forced-major之间不存在仅驻留Rust栈的
GC edge，entry仍为32 bytes。trap完成后重新读取 target状态并执行规范 invariant：getPrototypeOf限制返回
Object/null且不可扩展target必须identity一致，isExtensible要求布尔结果与target一致，preventExtensions在
trap报告true时要求target已经不可扩展。absent/nullish trap的nested Proxy target继续进入同一 typed
dispatch；纯同步 absent链在单个Rust frame内迭代，accessor返回nullish则从continuation恢复点重新进入，
不以Proxy嵌套深度递归增长Rust栈。`Object.preventExtensions`不能直接复用内层Proxy作为结果：外层先发布
`ForwardResult` parent continuation保存原始object，内层只执行布尔`[[PreventExtensions]]`，normal completion
再映射成throw-on-false/return-outer-object。同步native trap可能在内层dispatch中顺带消费parent，因此调用者
以completion/frame depth判断ownership，避免二次pop；bytecode trap挂起时parent留在inner continuation下方。

Proxy revocation function不能降为无状态`NativeFunction`加公开property，也不能用isolate side table按函数
identity查找。它使用`FunctionExecutable::ProxyRevoker(Value)`内联保存唯一私有`[[RevocableProxy]]` edge；
该variant仍落在既有16-byte executable布局内，所有普通FunctionObject不增大。首次调用在NoGcScope内先将
revoker slot替换为null，再把Proxy target/handler同时置null；过程不分配、只删除edge，因此不需要write
barrier，重复调用观察null后直接返回undefined。`Proxy.revocable`的结果shape固定按`proxy,revoke`顺序建立，
PropertyStorage一次exact分配两个slot；专用allocation roots在revoker、storage和result发布前保持proxy、
revoker、两个prototype与临时storage全部可追踪。revoked Proxy仍保有object identity，只有所有essential
internal method在读取null handler后抛TypeError；它仍可作为新Proxy的target/handler被ProxyCreate接受。

Realm 的 AtomId→global slot索引是单调增长的稀疏表。发布一个较低AtomId只能填已有空槽，绝不能通过
`Vec::resize`缩短表并删除较高AtomId映射；global lexical与object-environment binding表共同遵守该不变量。
这不是测试夹具细节：动态 trap getter、callback和跨source `LoadScope`都依赖已发布slot在后续低AtomId
binding插入后保持稳定。

`Object.setPrototypeOf`与`Reflect.setPrototypeOf`共享唯一`ordinary_set_prototype_of` mutation core：builtin
边界分别负责Object的RequireObjectCoercible/primitive-return/throw-on-false和Reflect的Object-only/boolean
result。core先验证prototype为Object/null，再做same identity、extensibility与cycle walk，最后对ordinary
payload或明确复用ordinary internal methods的Array ordinary base发布prototype edge和write barrier。其他
ordinary-backed exotic必须逐类接入并覆盖其internal-method差异，不能因结构内嵌`OrdinaryObject`就自动放行。

`Object.isSealed`/`Object.isFrozen` 使用独立 `TestIntegrityLevel` native entry，共享 `ordinary_own_property_keys`
与完整 own descriptor materialization；primitive 直接返回 true，extensible object 直接返回 false，sealed 只拒绝
configurable own property，frozen 额外拒绝 writable data property。Proxy 的 `[[IsExtensible]]`/`[[OwnPropertyKeys]]`
observable continuation 不在该同步 ordinary fast path 中伪装完成。
Proxy TestIntegrityLevel 先以 typed parent continuation 执行 `[[IsExtensible]]`，false 后把 existing
`PendingProxyOwnKeys` key list 的 cursor 归零，并按规范顺序对 active Proxy逐项调用统一
`dispatch_proxy_get_own`。integrity mode复用 ownKeys payload 的 mode/index/keys，不新增 GC type，也不扩大
Frame/Continuation；descriptor callback 可跨任意 bytecode frame/forced GC，early false 不继续观察后续 key。
Proxy不得调用该core读取其不存在的ordinary base；后续
`[[SetPrototypeOf]]`以typed exotic continuation保留prototype参数，并在trap后经target internal methods
验证extensibility/prototype invariant。需要向任意bytecode trap传递多个native-owned参数时，不借用相邻
register，也不复用只表达bound-call prefix的`BoundFunctionData`。固定五槽、带count的GC-managed
`NativeCallState`保存参数与状态机identity；`Fiber`用与activation严格等长的`argument_sources` side table
保存可选state reference，使`Frame`继续保持104 bytes。entry、ordinary return、frame publication失败与
abrupt unwind必须同步维护该表，trace时断言其与frame数量一致。`call_argument`只在当前activation存在native
source时从中读取suffix；普通call的热Frame与寄存器参数路径不增加字段。Proxy setPrototypeOf只在trap data
property或accessor getter形成observable callback时分配state；missing/nullish trap链保持无分配迭代。

`[[HasProperty]]`不能复用`[[Get]]`判断存在性，因为prototype walk不得执行getter。Opcode `in`与
`Reflect.has`在ToPropertyKey只执行一次后进入同一dispatcher：ordinary对象只读取完整own descriptor并沿
prototype迭代，首次遇到Proxy才进入cold state machine。Proxy trap以handler为receiver并通过
`NativeCallState`取得canonical `(target,key)`；false结果只检查target own descriptor，不把继承属性当作
invariant，并拒绝隐藏non-configurable own property或non-extensible target上的own property。missing/nullish
trap对nested Proxy迭代转发。若outer trap返回false而target本身是Proxy，检查通过下述可挂起的Proxy
`[[GetOwnProperty]]`和`[[IsExtensible]]`组合完成，禁止读取不存在的ordinary payload或跳过嵌套invariant。

Proxy `[[GetOwnProperty]]`是descriptor consumers的唯一exotic boundary；Object/Reflect descriptor、
hasOwn/hasOwnProperty/propertyIsEnumerable只选择结果映射，不复制trap或invariant算法。trap结果通过类型检查
后必须先观察target `[[GetOwnProperty]]`，按分支有条件地观察`[[IsExtensible]]`，最后才读取trap descriptor
对象的enumerable/configurable/value/writable/get/set；调换顺序会让descriptor getter看到错误的target状态。
既有`PendingPropertyDescriptor`因此使用typed consumer，在六字段getter挂起后回到Proxy completion，而
DefineProperty consumers保持原路径。Proxy consumer在解析后执行CompletePropertyDescriptor，generic完成为
默认data descriptor，所有缺省字段在compatibility与公开物化前已显式补齐。

nested Proxy target使用`TargetGetOwn`/`TargetIsExtensible` parent continuation，不递归运行解释器。五槽
`NativeCallState`在trap call后复用active-Proxy槽保存extensibility，并以两个槽保存trap result和内部物化的
target descriptor；这些临时descriptor对象只承载已经完成的own data字段，不重新触发用户getter。child同步
失败必须弹出尚未消费的parent continuation，bytecode suspension则由frame return消费。compatibility除普通
non-configurable kind/enumerable/value/get/set约束外，还拒绝把non-configurable writable target报告为
non-configurable non-writable；这是QuickJS对应实现明确标注未完整覆盖、但规范和Escargot要求的强化检查。

Proxy `[[Get]]`由bytecode属性读取与`Reflect.get`共享单一dispatcher。普通对象、String与RegExp现有读取
继续走`resolve_property_read_until_proxy`静态循环：它返回data/accessor/missing或首次遇到的Proxy，不为
ordinary hot path物化descriptor，也不增加Frame字段。slow path固定保存target、canonical String/Symbol key、
原始Receiver、active Proxy与一个复用槽；trap调用参数严格为`(target,key,Receiver)`且handler作为`this`，
不采用Escargot当前源码把active Proxy作为第三参数的偏差。第五槽在state分配safepoint前保存data trap或
accessor getter，使forced GC能够重写callee引用；trap完成后同一槽改存trap result，再查询target own
descriptor，因此nested descriptor callback覆盖destination register时不会丢失result。

missing与同步nullish data trap对nested Proxy target迭代前进，不随嵌套深度增长Rust栈；accessor trap getter
返回nullish后从typed continuation重新进入相同边界，并始终保留最初Receiver。trap结果只受两条规范
invariant约束：non-configurable且non-writable data property要求SameValue，non-configurable且getter为
undefined的accessor要求undefined；有getter的non-configurable accessor不额外限制结果。target本身为Proxy时
使用`TargetGetOwn` parent continuation取得其完整own descriptor，child同步错误必须清理未消费的parent。
当前opcode与Reflect consumer已接入；ToPrimitive、ToPropertyDescriptor、CopyDataProperties、iterator、
collection constructor、argument-list及其他builtin中的observable Get必须继续迁移，不能让各consumer复制
Proxy算法或把`get_data_property`旁路扩散为第二套internal-method语义。

Proxy `[[Delete]]`由`DeleteById/DeleteByValue`与`Reflect.deleteProperty`共享mode-aware dispatcher；mode仅决定
false completion是返回false还是按strict DeletePropertyOrThrow抛TypeError，不复制trap/invariant算法。
ordinary target直接调用现有shape/storage delete core；Proxy slow path的五槽state保存target、canonical key、
active Proxy、待调用callee和一个保留槽，argument count固定为2。data trap或accessor getter跨state分配
safepoint时必须先写入第四槽，分配后重新读取callee；getter continuation中的handler也从已迁移active Proxy
重新取得，禁止把分配前Rust局部中的旧heap reference发布到completion stack。

missing与同步nullish trap对nested Proxy target迭代转发；getter返回nullish时从typed continuation重新进入同一
边界。trap false不查询target descriptor。trap true先执行target `[[GetOwnProperty]]`：missing立即成功，
non-configurable descriptor抛TypeError，configurable descriptor还必须执行target `[[IsExtensible]]`并在false时
抛TypeError。最后一条是当前ECMA-262与Test262的`proxy-missing-checks`要求，QuickJS对应源码仍留空标记，
不能为对齐QuickJS而省略。nested target的descriptor/extensibility通过`TargetGetOwn/TargetIsExtensible` parent
continuation挂起；同步child失败必须移除未消费parent，普通路径不增长Rust栈或Frame布局。

Proxy `[[DefineOwnProperty]]`必须消费ToPropertyDescriptor已经生成的presence-aware record，禁止重新读取调用者的
descriptor object。Object/Reflect builtin因此在既有descriptor parser completion处选择Proxy dispatcher；
ordinary target继续直达ValidateAndApplyPropertyDescriptor。missing/nullish trap在FromPropertyDescriptor之前转发，
不创建descObj或pending state；nested Proxy链迭代前进并保留最外层Object.defineProperty返回identity。

`Object.create(proto, properties)` 在验证 prototype 并发布新对象到 destination root 后，必须把 properties
交给同一个 `PendingDefineProperties` 状态机；不得保留只读取 ordinary data property 的同步旁路。这样
descriptor-map ownKeys/enumerability/Get、六字段 ToPropertyDescriptor、Proxy trap、getter suspension 和“先完整
收集再 mutation”的原子边界与 `Object.defineProperties` 完全一致，最终 completion 仍返回新对象 identity。
这与 Escargot 的 `objectDefineProperties(state, obj, properties)` 和 QuickJS 的
`JS_ObjectDefineProperties(ctx, obj, props)` 复用边界一致，不增加新的 continuation kind 或 payload。
Properties 的 primitive ToObject 对当前 non-BigInt surface 使用无 allocation 等价路径：Boolean/Number/Symbol
和空 String wrapper 没有 own keys，直接返回 target；非空 String 的首个 enumerable index 值必为 primitive
String，ToPropertyDescriptor 确定抛 TypeError。该 fast path 不读取 prototype、不会跳过 callback，也避免为
零 key descriptor map 分配短命 wrapper；BigInt 接入时同样归入 zero-own-key 分支。

真实trap路径使用独立GC-managed `PendingProxyDefine`暂存target、PropertyKey、proposed Desc、captured target Desc、
outer result identity、active Proxy、callee及新建descObj。该payload只属于cold define slow path，不扩大Frame、
NativeContinuation或普通五槽argument source。FromPropertyDescriptor每个字段先intern名称，再从destination所root的
managed state重新取得object与值，然后调用现有property mutation core；这样任一字段分配触发moving GC后都不会
继续使用旧Rust局部引用，也不会二次执行descriptor getter。trap调用前压缩为五槽NativeCallState：前三槽是
`(target,key,descObj)`，第四槽root PendingProxyDefine，第五槽保存GetMethod已经captured的handler。

trap true后依规范顺序捕获target `[[GetOwnProperty]]`，再执行`[[IsExtensible]]`；captured target descriptor写回
PendingProxyDefine跨后一个callback保存。invariant使用与Proxy GetOwn共用的descriptor compatibility core，另拒绝
把non-configurable writable target通过present writable=false强化为non-writable；这是QuickJS源码仍标注
`missing-proxy-check`但当前规范/Test262要求的分支。nested target的两项internal method继续使用typed parent
continuation；GetOwn返回descriptor时先复用已空闲handler槽root结果，再把NativeCallState恢复到destination，避免
parent pop后descriptor与state争用唯一register root。

Shape 同时记录 property chronology、descriptor 和 `PropertyLocation`，不能再假定 chronology ordinal
等于 ordinary storage slot。普通新增属性使用 `Storage(slot)`；函数创建时直接采用共享的初始 function
shape，其中 `length`、`name` 和适用时的 `prototype` 分别使用 derived/lazy location。这样这些 key 从
函数创建时已经存在并拥有正确顺序，但普通 closure 不为 metadata 额外分配 `PropertyStorage`，首次读取
`prototype` 才创建对象。对 configurable `length`/`name` 执行 observable descriptor override 时，shape
保留原 ordinal 并把 location 迁移到新 ordinary slot；删除后重新添加则获得新的尾部 ordinal。

Property attributes 只保存 ECMAScript writable/enumerable/configurable flags，不保存创建来源、虚拟状态
或 consumer hint。禁止用 `VIRTUAL_ORIGIN` 一类隐藏 attribute bit 修补枚举顺序。结构删除重放 retained
shape entry 时必须保留 intrinsic location，并只重排 ordinary storage slots；Symbol storage edge 同步按
新 slot 精确重建。

`[[OwnPropertyKeys]]` 返回的快照在 observable callback 期间仍是规范上的活 List。只包含 String 且不会
调用 JS 的 key-only consumer 可以使用临时 exact-capacity Rust buffer；values/entries/assign/
defineProperties、Proxy trap 等可能挂起的 consumer 必须把 receiver、完整 String/Symbol key list、cursor
和部分结果放入 traced pending state。每次 callback 返回后重新执行 live `[[GetOwnProperty]]`，再按规范
执行 `[[Get]]`/`[[Set]]`，不能继续消费 callback 前的 descriptor 或 storage snapshot。

### 9.0 String 与 atom foundation

`JsString` 是 immutable GC payload，owned backing 只允许 `Box<[u8]>` Latin-1 或 `Box<[u16]>` UTF-16。
长度、索引、比较、排序与 hash 都按 ECMAScript UTF-16 code unit；UTF-16 backing 保留 unpaired
surrogate，Rust `str` 仅是一个保证 well-formed Unicode 的输入适配器。`JsStringView` 暴露 borrowed
Latin-1/UTF-16 view，RegExp/FFI 不需要先无条件扩宽。owned Latin-1/UTF-16、inline Latin-1/UTF-16、
rope、slice 与 atom 的 representation tag 已固定命名；inline capacity、rope/slice threshold 等默认值
只有 M13 layout/corpus benchmark 后才能选择。

`AtomTable` 是 `Isolate` 的普通单线程字段，`AtomId(NonZeroU32)` 只在所属 isolate 内稳定。创建 isolate
必须显式提供 entry quota、retained string-byte quota 和两把 hash keys；不存在生产 fixed seed 或
engine 自行读取 entropy 的 fallback。table 使用 keyed SipHash code-unit hash、power-of-two linear-probe
buckets 和 `tachyon-vm::tuning::strings` 的 load/growth constants；所有 entry/bucket reserve 与 byte quota
在发布前完成。相同 code-unit sequence 即使一个是 Latin-1、一个是 UTF-16，也返回同一 AtomId。
首版 atom 随 isolate 一起释放、不做单项 eviction；有界 immortal lifetime 避免 property key/shape 的
stale ID，后续 engine-global immutable atoms 必须使用独立 owner/limit，不能复用 isolate ID。

### 9.1 Collection Capacity 与扩容策略

解释器、compiler、GC 和 Host SDK 中的 `Vec`、`VecDeque`、hash table、small-vector 与 worklist
必须在创建时声明 capacity policy。不能在热路径中默认 `Vec::new()` 后依赖多次 `push` 的
通用扩容，也不能为了消除 realloc 无条件预留大块内存。capacity hint 按以下优先级取得：

1. **精确计数**：compiler 已知 register、constant、binding、handler、feedback slot、literal
   property/element 数时使用 checked `with_capacity`/`try_reserve_exact`，并把计数写入 function
   metadata 供 VM 使用。
2. **有界估算**：语义上只有少量元素的集合可使用 fixed array 或 inline storage。inline capacity
   必须来自 test262/benchmark corpus 的版本化分布和对象/cache-line layout 基准，不能使用无来源
   magic number。
3. **自适应 high-water mark**：可复用 fiber、GC gray queue、temporary root/work buffer 可依据
   上一次峰值保留容量，但必须受 isolate memory limit、元素大小和全局上限约束。
4. **不可信或无界输入**：先验证规范/实现资源上限，再使用 `try_reserve` 渐进增长。不得直接把
   JavaScript `length`、source count、serialized count 或 FFI length 当作可信 capacity。

function entry 在执行任何 opcode 前，按 `CompiledFunction` metadata 为 register window、handler、
completion 和必要临时区完成 checked reserve。正常 opcode dispatch、inline-cache hit、普通 call
fast path 和 property/array fast path 不得因内部 collection `push` 触发 realloc；无法提前知道的
slow path 必须显式进入带 memory accounting 的 growth path。

bytecode/HIR/source map、object/array literal、shape transition、module graph 等构建型数据结构应在
计数已知后一次预留并在冻结时转成 boxed slice/`Arc<[T]>`。FIFO job/completion/mailbox 使用有界
queue 或 `VecDeque`；会留下稳定 ID 空洞的 persistent/host task table 使用 generation slab/free
list，而不是通过 `Vec::remove` 搬移。GC gray queue 和 structured-clone worklist 使用 iterative
buffer，并把增长计入配额。

热 buffer 默认保留容量以供复用，不在每次 job/GC 后 `shrink_to_fit`；当 high-water mark 明显超过
长期使用量、isolate idle 或 memory pressure 到达阈值时才按域策略回收。debug/benchmark 的
`capacity-stats` instrumentation 记录每个 subsystem 的 initial hint、growth count、old/new capacity、
peak length、unused bytes 和 allocation failure。发布基准必须同时检查 steady-state growth 为零的
热路径与 capacity slack/RSS，防止用过度预分配换吞吐。

所有 performance tuning constant 必须放在所属 crate 的指定 `tuning` 模块，调用点只使用具名
常量，不允许散落 numeric threshold：

```text
tachyon-compiler/src/tuning.rs
tachyon-gc/src/tuning.rs
tachyon-vm/src/tuning/dispatch.rs
tachyon-vm/src/tuning/capacity.rs
tachyon-vm/src/tuning/inline_cache.rs
tachyon-vm/src/tuning/objects.rs
tachyon-vm/src/tuning/regexp.rs
tachyon-vm/src/tuning/strings.rs
```

每个 knob 必须有单位、合法范围、影响的子系统、正确性约束和 benchmark evidence doc comment。
根 `TUNING.md` 只做 registry，记录 owner/path、测量 corpus、最近调优 commit/hardware 和架构特例；
实际默认值只存在于 Rust `tuning` 模块，避免双重 source of truth。`cargo xtask tuning list/check`
负责列出 registry 并检查无主/无证据 knob。

Value tag、bytecode field width、object header、logical offset/span alignment 等 representation/layout constant
属于正确性契约，放在对应 `encoding.rs`/`layout.rs`，不能混进 tuning。heap/fuel/recursion/mailbox/
pending-host-op limit 属于宿主资源策略，进入 typed public config 并有安全默认值。const-generic
dispatch 等需要编译期常量的实验显式实例化候选值，最终默认由 tuning module 选择；不能为了
运行时可调牺牲 hot-path constant folding。

### 9.2 RegExp 子系统

首版使用精确锁定版本、启用 UTF-16 能力的 `regress` 作为 ECMAScript pattern backend，参考
Boa 的集成经验，但不得直接把 `regress::Regex` API 暴露到 builtin、SDK 或 bytecode。后端封装在
`tachyon-vm::regexp::backend`，只负责 compile pattern 和在指定 code-unit index 执行 matcher；
RegExp object、`lastIndex`、species/custom exec、String symbol methods、match result 和异常顺序由
VM 按 ECMA-262 实现。backend 是内部静态边界，不是可由 host plugin 替换的动态 trait object。

```rust,ignore
enum RegExpInput<'a> {
    Latin1(&'a [u8]),
    Utf16(&'a [u16]),
}

enum RegExpExecOutcome {
    Match(RegExpMatch),
    NoMatch,
    Interrupted,
    ResourceLimit,
}
```

flags parser 支持 `d/g/i/m/s/u/v/y`、重复 flag 错误和 `u`/`v` 互斥。cache key 只包含 source 与
影响 compiled program 的 `i/m/s/u/v`；`d/g/y` 属于 result/iteration state，不导致重复编译。
RegExp object 保存 source、全部 flags、`Arc<CompiledRegExp>` 和普通 observable `lastIndex` property。
literal 每次求值必须创建新对象，但相同 source/compile flags 可共享 immutable program。dynamic
constructor 与 literal 共用 engine-level bounded cache，缓存成功 program 和可复现 compile error；
cache 按 entry/compiled-byte 上限淘汰并计入 engine-global memory limit。硬上限属于 typed
`EngineConfig` 资源策略；eviction target、初始 hash capacity 等性能选择属于 tuning。

匹配索引一律使用 ECMAScript UTF-16 code-unit offset。pattern compile 通过 cloneable code-unit iterator
在非 Unicode 模式逐个提交 UCS-2 unit，在 `u/v` 模式只合并合法 surrogate pair 并保留 lone surrogate；
matcher 对非 Unicode input 选择 UCS-2 traversal，对 `u/v` input 选择 UTF-16 traversal，因此 astral match、
capture 和 `lastIndex` 仍直接返回原 String 的 code-unit offset。该边界同时承载 Unicode property escape
与 `v` Unicode sets，不允许由 builtin 做 pattern 特判。Latin-1 与 UTF-16 字符串应通过
borrowed input view 直接匹配。Boa 当前 Latin-1 fallback 会临时扩宽成 `Vec<u16>`；Tachyon 首选给
`regress` 增加/upstream Latin-1 input indexer。若阶段性使用 scratch widening buffer，必须复用受限
high-water buffer、计入配额并有 benchmark，不能每次 match 分配。

`regress` UTF-16/UCS-2 路径使用 classical backtracking，当前公开 API 没有 VM interrupt/fuel hook。
Tachyon 在采用前必须 upstream 或维护最小 patch：matcher 按可调 checkpoint interval 递减独立
regex step budget，并查询 hard fuel/cancel/interrupt；返回结构化 `Interrupted/ResourceLimit`，不能
让复杂 pattern 长期占住 executor worker。`max_regex_steps` 是 typed host resource config，不是
tuning constant；checkpoint interval 才属于 `tuning::regexp`。pattern/capture/quantifier/depth 与
backend work-stack allocation同样有 checked limit 和 `try_reserve`，错误不得 panic 或 abort。

`RegExpBuiltinExec` 必须覆盖 global/sticky `lastIndex` 更新与失败归零、custom `exec`、named capture、
duplicate named capture、`d` indices、groups、empty match、lookaround/backreference 和 Unicode sets。
`@@match`、`@@matchAll`、`@@replace`、`@@search`、`@@split` 以及 String 对应方法通过规范抽象操作实现，
保留 getter/setter/species/Proxy 的 observable 顺序。只在 receiver shape、builtin exec 和相关原型
watchpoint 均命中时使用专用 fast path。Annex B `compile` 与 legacy RegExp feature 按 release target
单独实现和统计，不能由 backend 私自改变语义。

首个完整 RegExpExec consumer 由独立 `regexp_exec` continuation 模块承载。`RegExp.prototype.test` 在
receiver object check 后先完成 resumable ToString，再做 Proxy/accessor-aware `exec` lookup；custom callable
的 receiver、已转换 String 与 callback state 必须全部位于 traced continuation/state，不允许靠 Rust stack
跨帧保活。non-callable `exec` 按 RegExpExec 的 internal-slot fallback 处理：genuine RegExp 进入 builtin，ordinary
object/Proxy 不会借 target 的槽。builtin test 使用 test-only matcher，不构造结果 Array 或 captures。

`RegExp.prototype.exec` 的 object ToString 与 test 共用 conversion substrate，但必须保留完整 result materialization。
固定五槽 `NativeCallState` 在继续分配前依次发布 input、receiver、result Array、active container 和当前
temporary；数组索引与命名 atom 在 capture String allocation 前完成，groups 在填充前先挂入 rooted state/result。
`d` indices 不扩大全局 state：slot 3 依次发布 groups、indices、indices.groups，每个 container 挂入已 root 的
result graph 后才复用；slot 4 发布 capture String 或 `[start, end]` pair，pair 挂入 indices 后才复用。
`groups`/`indices.groups` 无命名捕获时为 `undefined`，否则为 null-prototype ordinary object；命名 indices 指向
对应 numeric capture 已发布的同一 pair。`CompiledRegExp` 在冷编译路径保存 decoded name 与精确 numeric
capture index，match result 携带该 index；禁止按 range 相等反推 capture，因为 nested captures 可以拥有同一
span。该布局是 forced-major 正确性契约，不是容量 tuning，也不允许用 Rust
local managed edge 跨 allocation。

`RegExpBuiltinExec` 的 `lastIndex` 协议由同一固定 state 的 `LastIndexGet`、number-hint ToPrimitive/ToLength 与
`LastIndexSet` continuation 承载。Get 在读取 internal flags 产生的分支效果之前始终发生；非 global/sticky 只把
有效匹配起点归零而不写回，global/sticky 则在成功写 end、失败或越界写 `+0`，全部使用 `Set(..., true)` 的
Proxy/accessor-aware strict boundary。test-only state 只保存最终 boolean，不构造 match Array/capture String；exec
结果及写回值在任何可观察 property operation 前进入 traced state，回调恢复不依赖 Rust 栈或 unwind。

`RegExp.prototype[@@search]` 与 `String.prototype.search` 共用独立五槽 search state。String 入口只对 Object
searchValue 执行 GetMethod；Number/String/Boolean/BigInt/Symbol primitive 按现行规范直接进入 RegExpCreate，
不能观察其 prototype 上的 `Symbol.search`。RegExp 入口先完成 input ToString，再按 SameValue 保存、归零和恢复
lastIndex；custom exec/result.index、所有 Get/Set/Call 与 Proxy/accessor 都通过 typed continuation 暂停。
`String.search -> @@search -> exec` 会形成三层 native call：内层 ExecCall 完成后必须在 enclosing search frame
边界停止同步 parent drain，让正常 frame unwind 消费 StringMethodCall continuation；否则会提前跨 frame pop，
最终表现为 MissingNativeContinuation。该边界是通用 iterative trampoline ownership contract，不允许以 Rust
递归或 unwind 绕过。

`RegExp.prototype.flags` 不读取 private flag slot 拼接结果；它必须按 `d/g/i/m/s/u/v/y` 对 receiver 执行八次
可观察 `Get`。实现使用独立 `regexp_flags` continuation，固定 `NativeCallState` 保存 receiver 与 8-bit result
mask，每次 getter/Proxy 返回后执行 ToBoolean 并推进 index；最终结果由固定 8-byte stack buffer构造，不分配
增长 scratch。native accessor 可能同步消费 property callback continuation，因此 callback dispatcher 以入栈前
completion depth 判断 ownership，已被消费时不得再次 pop；该规则属于通用 iterative trampoline contract。

`String.prototype.split` 使用 fast/generic 双层边界。primitive String separator 直接执行 UTF-16 code-unit
scan；object separator 先通过 Realm-local `Symbol.split` 做 Proxy/accessor-aware GetMethod，只有 nullish method
才进入 receiver/limit/separator conversion。generic 路径以固定五槽 `NativeCallState` 保存原 receiver、原
limit、separator 与两个已转换 String，不把 callback 嵌入 Rust 栈。genuine intrinsic RegExp method 可直接
调用 backend，按 sticky-at-q 匹配并写 captures；arbitrary receiver、custom species constructor 或 custom
`exec` 必须进入后续完整 `RegExpExec` continuation，不能误用 genuine fast path。所有 substring/capture 发布
遵守 key-before-value allocation；若分配可触发 moving GC，结果 Array 必须从 VM root 重新读取，不能继续使用
分配前的 Rust local Value。RegExp literal 的 source/flags 两次独立分配同样使用显式 retained root。

branded `@@matchAll` 返回专用、非 ArrayIterator 复用的 `RegExpStringIteratorObject`。payload 只保存 ordinary
header、cloned matcher、input、global/unicode/done；每次 `next` 才执行一次 match，不预先物化结果列表。
clone matcher 与临时 exec state 必须先写入当前 native destination register再发生 shape/storage/result
allocation，最终 iterator/result 覆盖该 register；forced-major 以此验证 Rust local 不承担 managed edge。
global empty match 从 cloned matcher 的 observable cursor 执行 `AdvanceStringIndex`，Unicode 模式按 surrogate
pair 前进；non-global 成功后立即置 done。iterator `next()` 的 generic custom `exec` 已使用独立 typed
continuation：固定五槽 state 保存 input、matcher、iterator 与 result，按 exec Get/Call、result `"0"` Get/
ToString、empty-match lastIndex Get/ToLength/Set 分段恢复。builtin fallback 需要同时保活 outer iterator state
与临时 materialization state：前者放 completion stack，后者放 native destination register，backend 返回后先从
register 重新加载 movable exec state，再恢复 outer state。species constructor、`@@matchAll` 创建阶段 flags/
receiver conversion 与 Proxy ordering 仍不得塞入同步 branded kernel，后续继续使用 typed continuation 闭合。

compiled metadata 提供 capture/name count、是否需要 captures/backtracking、literal prefix/first-set
等信息。test-only path 在语义允许时避免构造 JS result 和 indices；需要 capture 才按精确计数预留
match buffer。RegExp literal、dynamic compile、cache hit/miss、ASCII/Latin-1/UTF-16、global/sticky、
captures/indices、replace/split 和 adversarial backtracking 均进入专项 benchmark 与 capacity stats。

branded `@@replace` 的 string replacement 直接消费 backend UTF-16 capture range，实现 GetSubstitution 的
最长有效 `$1..$99` 与 `$<name>`，不为捕获组分配 JS String/Array。functional replacement 不能复用这个
同步 kernel，也不能把参数硬塞进五槽 `NativeCallState`：callback 参数为 match、任意数量 captures、index、
input 及可选 groups。完整路径使用专用 traced RegExpReplace state 保存 receiver/input/replacer、global results
或 matcher cursor、nextSourcePosition 与 output backing；每次调用前按 compiled capture count 精确定容参数
backing，通过 iterative call trampoline 暂停，callback result 再经 resumable ToString 后继续下一 match。
callback、ToString、custom exec/result property 任何一步均不得由 Rust 栈跨越，也不得让未 rooted capture String
或 output backing 跨 safepoint。

当前 branded functional replacement 采用上述允许的 global-results 分支：backend 只编译/扫描一次，将全部
UTF-16 match/capture/name ranges 存入 external-memory-accounted `PendingRegExpReplace`，随后每次只物化当前
callback 的 match/captures/groups。参数 backing 按 compiled capture count 精确定容，并复用 bound-prefix
argument source 进入 iterative call trampoline；callback 返回对象通过 `RegExpReplaceResult` conversion consumer
执行可暂停 string-hint ToPrimitive，再把 primitive ToString 结果追加到 state-owned output。continuation kind
只有无 payload discriminant，故 `NativeContinuationKind` 仍为 4 bytes、`NativeContinuation` 仍为 32 bytes。
这一选择消除了 callback 次数乘以 pattern compile/完整输入复制的成本；generic custom `exec`/result property
仍必须走后续 RegExpExec continuation，不得伪装成 branded backend result。

`String.prototype.replaceAll` 的 generic protocol 使用固定五槽 `NativeCallState`，依次保存 receiver、
replacement、searchValue、converted input 与 converted search String。入口严格按 IsRegExp 的 observable
`Symbol.match`、flags Get/ToString/global 校验、`Symbol.replace` GetMethod/Call 推进；只有 genuine RegExp 没有 own
flags、prototype 是当前 Realm `%RegExp.prototype%` 且 flags getter 仍为 intrinsic 时才读取 private flags slot。
所有 receiver/search/replacement ToString、Proxy/accessor Get 和 functional callback 都通过 typed continuation
暂停，Rust local `Value` 不跨 allocation safepoint承担 managed root。

ordinary String search 使用 UTF-16 code-unit 非重叠位置迭代器，empty search 产生每个边界。static replacement
复用同一 GetSubstitution parser，但 output capacity 是 fallible contract：初始 educated guess 使用 checked add，
每个 unmatched slice、literal/token、matched prefix/suffix/capture 在写入前都检查目标长度并 `try_reserve`；共享
RegExp replacement kernel 返回 `Result`，不得由 `push`/`extend_from_slice` 触发隐式增长。因此重复 `$`` 或
`$'` 导致的二次/更高阶输出扩张只会成功，或结构化返回 `InvalidStringLength`/
`StringBufferAllocationFailed`，不会进入 allocator abort。functional replacer 则复用已计 external memory 的
`PendingRegExpReplace` output policy，不建立第二套未计费 backing。

### 9.3 默认原生 TC39 Signals

Tachyon 默认在每个 realm 安装 `Signal.State`、`Signal.Computed`、`Signal.subtle.Watcher`、
`Signal.isState/isComputed/isWatcher`、`untrack`、`currentComputed`、`introspectSources/Sinks`、`hasSources/Sinks` 与
`watched`/`unwatched` symbols。它们不是 GUI-only extension，也不需要 Cargo feature、extension
registration 或 `EngineBuilder` opt-in。proposal 当前仍为 Stage 1，因此 release manifest 必须固定
所实现的 proposal commit/API hash；升级 revision 需要单独的 compatibility/test/benchmark 提交，
但“默认存在”本身是 Tachyon 的产品契约。

三个 public guard 是 realm-local native function identity，但检查的是同 isolate 的 GC native type brand，
因此可识别 foreign Realm 实例和 subclass，不以 prototype chain 或 constructor identity 代替 internal brand。
它们对缺省参数、primitive、ordinary object、其他 Signal brand 与 Proxy wrapper 直接返回 false；不得读取
属性、展开 Proxy target 或触发用户代码。该 surface 必须进入 pinned API hash 和 guards fixture，不能只在
TypeScript reference wrapper 中存在而从 native manifest 漏掉。

构造器、prototype 和 symbols 是 realm-local ordinary builtins，State/Computed 可 subclass；实际
graph node、callback、cached completion 和 adjacency storage 是 GC-managed native internal slots。
隐藏的 `computing`、`frozen` 与 monotonic `generation` 属于 isolate 的 ECMAScript agent state，随
`Isolate` 在 worker 间迁移，不能放在 Rust TLS 或 Floem 风格的 thread-local runtime。跨 realm
brand/prototype/error identity 按普通 builtin 规则处理，同一 isolate 内仍共享一个 agent graph state。

跨 Realm 构造必须分开两个 Realm 决策：实例 prototype 按 `newTarget` 的 Realm/derived prototype 选择，
而 State/Computed options 的 `watched`/`unwatched` property keys 属于被调用 constructor 的 defining Realm。
因此 foreign constructor 被 local subclass 继承时，不能读取 active Realm 的同名 proposal symbols。两个
constructor-Realm symbol 在 observable options Get 前写入现有 GC-traced operation record，并随着 watched、
unwatched 结果到达原位覆盖；不为这条冷路径增加节点字段或新的 continuation kind。

```rust,ignore
enum ComputedState {
    Clean,
    Checked,
    Computing,
    Dirty,
}

enum WatcherState {
    Waiting,
    Watching,
    Pending,
}

struct SignalRuntime {
    computing: Option<GcRef<ComputedSignal>>,
    frozen: bool,
    generation: u64,
    propagation: Vec<SignalWorkItem>,
}
```

`Signal.State::set` 先运行 `equals`（默认 `Object.is`），只有值确实变化才同步传播。直接 sink
变为 dirty，传递 sink 变为 checked，Watcher 从 watching 经 pending 进入 waiting；notify 在 graph
着色完成后、`set` 返回前按规范 depth-first 顺序同步执行。notify、watched 与 unwatched callback
期间设置 `frozen`，任何 Signal read/write 即使包在 `untrack` 中也抛错。所有 notify 都必须运行完；
单个异常原样抛出，多个异常以 `AggregateError` 抛出。RAII-style VM guard 必须在 callback throw、
termination 或资源错误时恢复 `computing`/`frozen` 并把 graph 留在合法状态。

所有 proposal callback 位置使用 ECMAScript IsCallable 判定：State custom equals、Computed computation/
custom equals 和 Watcher notify 必须接受 callable Proxy，并由普通 call dispatch 保留 receiver 与 Proxy apply
语义；不能用“可借用直接 FunctionObject”作为 callable 的替代定义。Computed callback 期间允许 State
读写；当前 Computing 节点可能出现在旧 reverse edge 中，但本轮完成时由 pull/reconcile 状态机统一落回
Clean，callback 内的自写不得导致永久 Dirty 或绕过后续外部写入的 invalidation。

这里的 guard 是显式 VM continuation contract，不依赖 Rust `Drop` 或 unwind。Watcher notify/lifecycle hook
必须在设置 `frozen` 后对 continuation publication 与同步 call failure 做对称解冻；Computed callback、custom
equals 和 `untrack` 同理在进入 callback 前保存 previous owner，并在 completion quota/分配失败时恢复旧
sources 与 `Dirty` 状态。若 host 在 JS frame 已暂停后终止整个 execution，则由 execution-level cancellation
在返回 terminal error 时、以及 fresh execution 丢弃 suspended Fiber 前，按 native continuation 栈逆序调用
相同 restoration primitive；直接清空 Fiber 或只把 `computing = None` 会遗失 operation record 中的 old
sources，破坏 reverse-edge、liveness 与异常 identity，不是允许的实现。cancellation 本身不制造替代 JS
异常，host 继续观察触发终止的原始 typed resource error。

Computed 是 lazy pull，不在 State.set 时重算。`get` 对 dirty/checked graph 使用 iterative、ordered
DFS 找到 deepest-left-most dirty source，再自底向上计算；不得用 Rust recursion 处理不可信 graph
深度。Computed 缓存正常值或 thrown completion，依赖未变化时重复 get 重放同一 completion；递归
读取 `Computing` 节点报告 cycle error。每次计算动态重建 ordered sources，read order 必须保留，
因为 equals/computed/watched/unwatched/Watcher callback 的执行顺序可观察。无变化的 checked chain
清回 clean，变化则将 checked sink 升为 dirty，不能用“所有依赖变化都重算”的简化算法。

checked pull 的 DFS frame 存在单次调用的 GC-traced operation record，不进入常驻 `ComputedSignal`；frame
只保存 rooted Computed identity 与下一 source index，callback 前的 old sources 也发布到同一 record。
`ComputedSignal::generation` 的高位复用为 cached thrown-completion tag，低 63 位保留 agent generation，
因此正常值与异常 identity 都能在 clean/checked cleanup 后重放，同时不增加 Computed payload 或跨越 GC
size class。native continuation 弹出后，operation 先发布到 caller register，callback value/error 再写入
Computed 并执行 barrier，随后才做 old/new diff；任何 Rust 局部 Value snapshot 都不能成为跨 GC 的唯一 owner。

Computed 的默认实例继续让 `callback` field 直接保存 computation function；只有显式 custom `equals`、
`watched` 或 `unwatched` 的实例才让该 field 指向 GC-managed `NativeCallState` cold sidecar，sidecar 保存
computation/equals/watched/unwatched 四个强边。这样 options 的 observable
`Get("equals") -> Get(watched) -> Get(unwatched)` 与 equals IsCallable 校验不会给所有 Computed 增加常驻
field 或改变 size class。重算成功后，custom equals 在 `computing` 仍指向该 Computed 时以
`(oldValue, newValue)` 和 signal receiver 调用，因此 comparator 内的 Signal read 属于 inner Computed，
而不会泄漏给 outer consumer。old/new、old sources 和 comparator argument prefix 在任意 callback/forced
GC 前发布到 traced state；equals throw 作为 cached abrupt completion 原 identity 重放，任一 dependency
change 会使该 error cache 失效。Computed 的 first/last live transition 先同步运行自身 watched/unwatched，
再按 ordered sources 深度优先传播；hook receiver 是该 Computed，异常不得回滚已经完成的 graph coloring。

当前 notify/dynamic-dependency 纵切不在每个 `ComputedSignal` 常驻第二个 `Vec`：该字段会让节点跨越
GC size class，并为稳定节点永久增加 full-GC scan 成本。重算前把 old ordered sources 发布到本次调用的
GC-traced operation record，节点自身的 sources 原地收集 new ordered sources；callback 成功后做 old/new
diff，再执行 reverse-edge 与 live-hook transition。operation record 同时承载 notify queue 和异常；构造
AggregateError 时临时 Array/Error 必须先写回该 traced record，并在每个潜在 GC 后按索引重读异常，不能
依赖 Rust 局部 `Vec<Value>` 跨 forced GC。后续 reusable high-water 双 buffer 只有在提供同等 precise
rooting 且不扩大常驻 node size 时才能替换。

Notify fanout 的异常收集必须把每个 callback 的 abrupt completion 作为 GC-traced pending operation 的
尾段保存，不能依赖 Rust 临时 Vec 或在首个异常后提前返回。所有原 watching Watcher 先按图着色后的
depth-first 顺序执行并转为 Waiting；单错误原样重抛，多错误在构造 `AggregateError.errors` 时保留顺序和
identity。AggregateError 及其 errors Array 的分配发生在原 set continuation 仍 rooted 时，因此 forced-major
不得改变错误对象、Watcher state 或后续显式 `watch()` re-arm 行为。

graph liveness 按 proposal 的 watched/unwatched 模型实现，而不是给每条边创建 host persistent root。
Computed 强引用其 ordered sources；reverse dependency index 的每个 sink edge 保存 collector-cleared
`WeakGcRef` identity，只有递归连接 active Watcher 的 edge 同时保存强 `Value`。first/last live transition
沿 ordered sources 逐边 promote/demote，promotion 执行 generational write barrier；major weak closure 清除
cold target 后，snapshot 跳过 tombstone，下一次 insert 在扩容前清理并复用该槽。因此 rooted source 可以
保活 active Watcher，同时不保活同一 source 上已求值但 cold 的 dependent Computed。`introspectSinks` 必须
按 insertion order只投影 live edge，`hasSinks` 必须读取 live count，不能把内部 cold invalidation index
是否为空当作 proposal liveness。watch/unwatch 按参数顺序递归 attach/detach live edges，正确触发
watched/unwatched；minor/major/forced GC 精确 trace strong/weak 两类 edge，collector 不改变仍存活 identity。

Watcher 的 watched signals 是 ordered set；`watch()` 可无参数将 waiting 重置为 watching，重复
`watch(existing)` 同样显式 re-arm，但 `unwatch()` 只改变 membership、不得把 Waiting 隐式改回 Watching。
`getPending()` 返回 watched 原序中仍为 Dirty/Checked 的 Computed 子集：初始 Dirty 在 watch 时进入，
Waiting 期间后续 invalidation 仍更新该子集，zero-argument re-arm 不清除尚未求值的项，Computed 变回
Clean（包括 unchanged Checked cleanup 和 cached throw completion）时从所有直接 Watcher pending 中移除；
State 本身永不出现在 pending。`unwatch()` 校验全部参数后再修改，`getPending()` 每次返回 fresh Array
snapshot。pending 继续复用 `WatcherSignal` 既有 `OrderedSignals`，传播时的 watched/pending snapshot 使用
checked exact reserve，边插入执行 generational write barrier；不增加常驻 Vec/field 或改变 GC size class。
`untrack` 只暂时清除 `computing`，不能解除 frozen；入口必须先完成 frozen 与 IsCallable 校验，再把
previous owner 发布到现有 32-byte GC-traced native continuation，最后才令 agent-wide owner 为 None。
normal return、bytecode throw unwind 与同步 call failure 都从该 continuation 对称恢复；nested untrack
按 continuation 栈恢复，因此回调内启动的 Computed 仍可追踪自身 sources，但其 identity/read 不泄漏给
outer Computed。不得把 previous owner 仅存在 Rust 局部、TLS 或 unwind guard 中，也不得为 untrack 增加
常驻 Signal payload。`currentComputed` 是 realm-local native function，但返回 agent-wide `computing` owner；
因此 Computed callback 与 custom equals 返回当前 Computed，nested Computed 按栈切换，top-level、Watcher
notify、live hook 与 `untrack` 返回 undefined。它遵循 pinned polyfill 的只读语义：frozen notify 内仍可调用，
不分配、不建立依赖，也不暴露 sources/sinks 或其他 graph payload。introspection API 返回在调用点 rooted
的 JS Array snapshot，不能暴露内部 slice、tombstone 或 mutable iterator；debugger 可在 pause
safepoint 复用同一 graph visitor 展示 node state、ordered sources/sinks、generation 和 owner metadata。

sources/sinks 使用小型 inline ordered storage 加 generation-aware membership/index，recompute 复用
双 buffer；propagation/cleanup 使用 isolate-local capped high-water scratch worklist。inline capacity、
worklist initial capacity、tombstone/edge compaction 与 retained high-water decay 只存在于
`tuning::signals`。JS 参数个数、graph fanout 和 depth 先检查资源配额并使用 `try_reserve`；稳态
State.set、unchanged Computed.get 和不改变依赖集合的 recompute 不得分配。任何优化都必须保持
callback 顺序、AggregateError、cycle、GC liveness 和 introspection 结果。

Signals GC liveness oracle 使用 ECMAScript `WeakRef`，并把最后强引用清理、major collection 与 deref
断言放在不同 job。`WeakRef.deref()` 会按规范把 target 加入 current-job kept set，因此 unwatch 后必须再
跨一个 job boundary 才能证明 Watcher 已可回收。rooted source 的 live reverse edge 必须保活 active
Watcher；不可达 State/Computed/Watcher 强环由 tracing collector 整体回收，collector 不合成
watched/unwatched 生命周期事件。

Signals 的 forced-minor contract 采用仅测试用的高容量 isolate，并将 `ForcedCollectionMode::Minor`
设置在执行前，使 constructor、lazy get、set propagation、recompute、notify、watch/unwatch 和
introspection 的每个 young allocation 都经过 minor collector。该测试 isolate 的较大 heap limit
只避免验证过程中 quota 耗尽，不改变产品默认限制或 collector policy。

Signals 实现按所有权边界拆为五个真实子模块：`runtime` 负责 agent state、continuation cancellation
与 introspection dispatch，`state` 负责构造/set/notify，`computed` 负责 iterative pull 与 equality，
`watcher` 负责 operation record、hook 和 watch state machine，`graph` 负责 typed references、edge、
liveness 与 checked storage。顶层模块仅保存共享 GC layouts、trace 实现和 tuning constants；兄弟模块
调用的内部 helper 使用 `pub(super)`，不扩大 crate API。测试源同样将 JS fixture 数据、resource cases、
行为用例与执行 helper 分开，避免单个多千行文件成为 graph/GC 修改的隐式耦合点。

TC39 core 不定义 Effect、OwnerScope、batching、microtask 或 frame scheduling。Tachyon GUI 使用
Watcher.notify 只把 `EffectId`/binding 标为 pending，不在 notify 中读取或写入 Signal；后续由 GUI
scheduler 在 microtask/frame boundary 拉取 Computed 并合并 transaction。Effect/OwnerScope 是默认
Signal 之上的 Tachyon GUI native layer，不能改变同步 set、lazy Computed 或 Watcher frozen 语义。

性能优化必须有基准依据。不能为了理论优化扩大 `unsafe` 范围或破坏可维护性。

## 10. 测试与基准

### 10.1 正确性

- 复用 `./boa` 中的 test262 运行设施和适用测试数据。
- 最终 test262 通过率不低于 98%，并保存按 proposal/feature 分类的结果。
- 为 bytecode lowering、异常恢复、Promise ordering、await/finally、async generator、GC roots、
  weak references、跨线程完成事件、Signals graph/ordering/GC 和 debugger pause/evaluate/root release
  建立专项测试。
- 使用 Miri、sanitizer 和并发模型测试覆盖 GC 与异步边界中的 `unsafe`。

test262 runner 的 host-I/O 只存在于 `tools/test262-runner`/`xtask`。engine adapter 接收按顺序组成的
内存 harness/source unit、strict/module/async/can-block policy，并返回 phase-aware outcome 与捕获的
stdout/stderr/backtrace；engine crate 不解析 YAML、不遍历 checkout、不读取当前目录。runner 在执行前
验证配置中的完整 commit、checkout HEAD 和 tracked cleanliness，最终 source 以长度分隔的 SHA-256
记录，unsupported/timeout/panic/crash/harness failure 始终保留在结果与总数中。

Signals proposal conformance 同样只由 host-side `xtask` 驱动：`signals_suite.toml` schema v2 固定 proto-spec
commit、reference polyfill commit、API surface hash 和每个 checked-in JS fixture 的 SHA-256；
`cargo xtask test signals` 读取并校验这些 bytes 后，经 `TachyonAdapter` 以 owned `SourceUnit` 送入
VM。引擎不读取 proposal checkout，也不依赖 Node/Vitest。固定 reference revision 当前包含 19 个
非 benchmark 测试文件与 70 个 `it/test` 定义；manifest 逐定义保存 upstream path/line/name，runner
验证 definition 唯一性、case ownership 和精确 19/70 总数。11 组 fixture 各自声明动态 assertion
预算，host runner 在 fixture 后追加计数 oracle，当前 114 次预期 assertion 必须全部实际执行，不能以
死分支或提前完成伪造 pass；该 oracle 已暴露并移除 live-pruning 的 `if (false)`。VM 单测进一步对每组
运行 N=1/2/4/8/16、forced-minor 及 forced-major。definition 映射用于形成可审计 coverage ledger，
不代表 70 个定义已经逐句完整移植；完整 upstream/differential/GC-liveness 门禁仍需继续实现。

Proposal cycle tests intentionally assert thrown completion and identity where the upstream contract does
not mandate a concrete error constructor. Requiring `TypeError` for every multi-node cycle would overfit one
polyfill diagnostic rather than observable proposal semantics; the native cycle error remains cached and
replayed by identity.

Tachyon in-process adapter 是无状态 `Sync` 边界，每个 variant 建立独立 isolate。它先解析 body，确保
parse-negative 不会被尚未支持的 harness lowering 遮蔽；body parse 成功后再解析并按顺序执行每个
harness source unit，最后执行 body。source units 不允许为方便而拼成单一 script，因为这会改变 body
directive prologue、script/module boundary 和 diagnostic source identity。所有 units 共享同一 isolate，
当前 CodeId/global-object substrate 已让前一 script 的顶层 function declaration 对 body identifier/call
可见；后续完整 global realm/environment 仍须补齐 lexical/var/object record 分离。async completion、未实现的
lowering/VM surface 返回明确 unsupported；Rust panic 直接 abort runner，fuel exhaustion 是 timeout，
runtime throw 在拥有规范 exception object type 前不得猜测成期望的 TypeError/RangeError。

适用性由两份固定事实联合决定：已验证 checkout 自带的 `features.txt` 决定 proposal/standardized
状态，checked-in feature-edition 表决定首次规范 edition。unknown feature、standardized feature 缺失
edition 或 policy 要求猜测时整次扫描失败；不得默认归入 ESNext 后静默移出分母。报告保存 edition、
applicability、明确 reason、按 feature/path/phase/edition 分类统计，以及只覆盖 release target/feature
policy（不覆盖 suite commit）的 fingerprint。baseline compare 只有 fingerprint 相同才生成
fixed/broken/changed/reclassified/added/removed；test262 commit 升级仍可比较 source 集变化。

### 10.2 性能

持续记录：

- js-engine-zoo 可比总分。
- 与 Boa、QuickJS、Escargot 相同构建条件下的吞吐。
- 冷启动、首次执行和重复执行时间。
- 单 isolate 与多 isolate RSS、峰值 heap 和每对象开销。

benchmark corpus 必须 checked in 且 content-addressed；每个脚本记录 upstream repository/commit/path、
SHA-256、license、category、suite 与 entry contract，采样前先验证字节，不能直接运行浮动的 `./boa` 内容。首批 Boa
脚本按其 `MIT OR Unlicense` 条款复制并保留 license 文本。runner 的 serial adapter contract 明确区分
cold start、parse+compile+execute、precompiled execute 与 steady state；adapter 无法诚实提供某种
timing boundary 时必须返回 unsupported mode，不能用近似数据冒充。

script entry 明确区分 `script` 与 `main-function`。`main-function` 对齐 Boa Criterion harness：source 作为
setup 只 evaluate 一次，随后 timed iteration 直接调用其 global `main`。Tachyon precompiled/steady 在 prepare
阶段 compile/load/execute setup，再 compile/load 独立 `main();` invocation module；timer 内只执行 invocation。
parse+compile+execute 与 external cold-start 才使用带换行/statement boundary 的 setup+一次 main 合成 source。
原始 source hash 不因 harness composition 改写，entry 独立进入 report schema v3，comparison 对 entry drift
返回 classification mismatch。禁止把“每次重跑完整 setup”或“只执行顶层定义”报告为 Boa execution case。

external file adapter 只服务无法通过批准 Rust crate 直接链接的 Escargot：prepare 在计时外把已验证
source 写入复用临时 `.js` 文件，sample 用参数数组启动 release executable 并把 script path 作为最后一个 argv；
禁止 shell command string。外部 adapter 只实现 cold start。stdout/stderr 使用独立临时文件，避免 pipe
回压死锁，并按配置上限读取；deadline 后必须 kill + wait，正常非零 exit、signal/crash 和 timeout 分开
报告。version/commit/features/build flags 由调用方显式提供，binary size 从实际 executable metadata 获取。
Boa CLI 与 QuickJS CLI kind/profile/命令已经删除，不保留兼容 alias；进程启动数据是独立 cold-start
维度，不能与 linked adapter 的 steady-state 吞吐 ratio 或 geomean 混算。

真实外部引擎由 `benchmark_config.toml` 的 platform-specific profile 固定 repository、40 位 commit、
checkout/executable 相对路径、version/features/build flags、fixed argv 与按顺序的 build program/argv/env；
build step 不经过 shell。`build-profile` 与 `run-profile` 在信任产物前验证当前 OS/architecture、checkout
HEAD 和 tracked cleanliness。profile 不自动把无 affinity/governor 的 macOS smoke 提升为有效 parity
数据；这种运行仍保留 raw samples 和 comparison 的 invalid case，固定性能 Linux 才执行 release gate。

Tachyon、Boa 与 rquickjs in-process adapter 都直接位于 benchmark tool 层；`boa_engine = 0.21.0` 和
`rquickjs = 0.12.1` 只属于 benchmark runner，不能进入 facade 或任何 Tachyon engine crate 依赖图。
每个 prepared case 只创建一个 engine runtime/context/isolate，并在 timer 外 evaluate setup、解析 global
`main`。Boa sample 直接调用 `JsObject::call`；rquickjs sample 每个 sample 只获取一次 runtime/context lock，
不能每次 JS `main` 调用重复加锁。两者仅支持 `main-function + steady-state`，严格执行 request 声明的
iterations 并返回同一 work count；不支持的 entry/mode 必须返回 `UnsupportedMode`，不能退回 CLI 或重跑 setup。

Tachyon adapter 依赖 compiler/bytecode/VM，不进入 facade 或 engine 依赖图。它不实现 process cold start；
parse+compile+execute 在 timer 内构造 source、compile 并执行一次，
precompiled execute 在 prepare compile 后只计时一次 VM execute，steady-state 在同一 isolate/module 上执行
由每个 corpus script 独立声明的固定 N 次，不存在 adapter-wide repetition fallback；内含大循环的 workload
可以显式选择 N=1，短小 foundation workload 可以选择更高 N。request 冻结 N，warmup 与 retained sample
都必须返回相同 work count。VM entry 会 clear fiber logical state 并保留高水位 capacity，因此重复执行不泄漏
前次 register/frame 内容，也不反复扩容。report schema v4 为每个 case 保存 entry contract 与
iterations/sample，comparison 同时报告 ns/iteration 并拒绝 iteration-count drift；禁止把 N 次总耗时当成
一次执行或与不同 N 的报告比较。
public `cargo xtask bench run-in-process <tachyon|boa|rquickjs>` 只负责 re-exec Cargo release child；编译时间
不进入 sample，release child 再验证 root tracked-clean HEAD 并生成报告，不能让 optimized dev binary
冒充 release/LTO build。

每个 case 先执行配置化 warmup，再收集固定原始样本；默认收集 15 个且 outlier 后至少保留 10 个。
统计保存全部 raw nanoseconds、median、MAD、relative MAD、固定 MAD outlier 数，以及明确标注方法的
95% robust-standard-error interval。噪声或环境条件不满足时保留结果但标为 invalid，不得进入 Boa parity
gate。engine identity 必须保存 integration kind、version/commit、features、build flags 与 binary size；
host report 保存 OS、architecture、CPU 与完整 rustc identity。

baseline/candidate 只按相同 script/mode/source hash 配对，ratio 固定为 baseline median / candidate median，
并分别报告全局、category 和 suite 几何平均。缺失、invalid 或 classification drift 使 comparison 无效；
invalid case 不进入几何平均。host 公平性只用 OS、architecture、CPU、rustc 作为静态 identity，affinity、
governor 和 background-noise 属于每次运行的动态 validity evidence，不得因 probe 样本不同误报为另一台机器。
- GC 总时间、minor/full collection 次数和 P50/P95/P99 暂停。
- Tokio 下大量 pending host future 的吞吐、唤醒次数和调度延迟。
- debugger detached/attached-no-breakpoint/breakpoint/step、scope preview 与 heap snapshot 吞吐和内存。
- Signal State/Computed/Watcher 的 chain/diamond/fanout/dynamic-dependency、GC 与 GUI binding 开销。

基准必须固定平台、编译器、编译参数和测试版本。外部总分是目标之一，但不能替代
针对解释器、对象访问、调用、Promise 和 GC 的微基准。

## 11. 分阶段实现路线

### Phase 0: 基础设施

- 接入 Oxc，完成 source -> AST -> 自有最小 IR 的链路。
- 建立 test262 runner、Boa/QuickJS/Escargot 对比基准和 CI 检查。
- 确定 `Value` 原型与 bytecode 编码。

### Phase 1: 同步解释器

- 实现显式 frame/register stack 和基础控制流。
- 支持基础值、函数、闭包、对象、数组、异常和核心内建。
- Phase 1A 实现精确非移动 mark-sweep 和 persistent handle。
- Phase 1B 实现非移动 Eden/Survivor/Old cohort spans、write barrier、remembered cards 和
  `NoGcScope` 约束。
- 建立 immutable debug metadata、isolate-local breakpoint table 和 pause/resume/step 基础路径。
- 达到可测量的 test262 子集，并建立性能回归门槛。

### Phase 2: Promise 与 async

- 实现 Promise、job queue、microtask checkpoint 和 async fiber。
- 实现 `await`、async function、generator，再扩展到 async generator。
- 实现 `VmDriver: Future + Send`、`IsolateHandle: Send + Sync` 和 Rust host future bridge。
- 默认安装完整 proposal-signals core。

### Phase 3: 性能与兼容性

- shape、inline cache、fast array、字符串优化和 compact bytecode。
- 实现老年代三色增量 GC，并根据基准调整 young span cap、cohort age 和 whole-span promotion 策略。
- 扩大 test262 覆盖并持续与 Boa、QuickJS、Escargot 对比。
- 完成 typed debugger、CDP adapter、bounded remote objects 与诊断型 heap snapshot。

## 12. 从参考实现得到的具体结论

Escargot 的寄存器解释器、fast/slow path 和 async 状态堆化值得参考，但其
`ExecutionPauser` 保存 `ExecutionState*`、寄存器指针和动态恢复字节码的方式不适合
executor worker 间的任务迁移。Tachyon JS 使用显式 fiber 取代该机制。

Escargot 的 `Object` virtual internal methods、default function structures 与 accessor 后 live descriptor
recheck 证明对象语义必须有一个统一分派面。Tachyon 不复制 C++ virtual dispatch、`GCDisabler` 或
`multiset` own-key 实现；Rust 侧使用闭合 kind dispatch、shape-owned chronology、显式 property location
和 traced continuation。该差异保留 ordinary fast path，同时避免 Proxy/Array/String exotic 进入 builtin
特判。

Escargot/BDWGC 与 Lua 5.4 证明嵌入式引擎不依赖 moving collector。Tachyon 不采用保守 stack scan、
对象内 allgc/gclist 或 root counter，但采用 Lua 的非移动 generation、allocation debt、显式 weak phase
和 safepoint incremental work 思路，并把 age/color/list metadata 移到 span side metadata。

Boa 当前 collector 是非移动的逐对象 allocation/full mark-sweep，说明 Boa parity 不以 copying nursery
为前提。Tachyon 的差异化优化是 size-class spans、Eden bump、young-only minor、whole-span promotion、
epoch bitmap 与 lazy old sweep。abfall 只用于验证 iterative gray traversal；其 atomic color、shared queue、
background marker 和 intrusive allocation list 不进入 Tachyon。

参考位置：

- Escargot computed-goto 解释循环：
  `escargot/src/interpreter/ByteCodeInterpreter.cpp`
- Escargot lazy function prototype：`escargot/src/runtime/FunctionObject.h` 的
  `ensureFunctionPrototype` 与 `Object::createFunctionPrototypeObject`。
- Escargot `instanceof`/global binding slow path：`escargot/src/runtime/{Value,Object,EnvironmentRecord}.cpp`
  与 `InterpreterSlowPath::{storeByName,resolveNameAddress}`。
- `await` 到 `ExecutionPause` 的 lowering：
  `escargot/src/parser/ast/AwaitExpressionNode.h`
- async/generator 寄存器文件与 `ExecutionPauser`：
  `escargot/src/runtime/FunctionObjectInlines.h`
- 暂停和恢复状态重建：
  `escargot/src/runtime/ExecutionPauser.{h,cpp}`
- Promise reaction job：
  `escargot/src/runtime/PromiseObject.cpp`、`escargot/src/runtime/Job.cpp`
- Escargot breakpoint/pause/scope/CDP：
  `escargot/src/debugger/Debugger.{h,cpp}`、`DebuggerDevtools.{h,cpp}`
- Escargot 诊断型 snapshot：`escargot/src/debugger/HeapSnapshot.{h,cpp}`
- TC39 Signals proto-spec：`tc39/proposal-signals` 固定 release revision。
- Signals reference tests/benchmarks：`proposal-signals/signal-polyfill` 固定 release revision。
- Floem reactive architecture：`floem/reactive/src/{runtime,signal,memo,effect,scope}.rs`；只参考
  dependency tracking、memo/effect 与 scope disposal，不移植 TLS/`Rc<RefCell<_>>` runtime。
- Oxc 使用参考：`deno_ast-oxc-port/`
- test262 与 benchmark 参考：`boa/`

## 13. Strict proper tail calls

Tail position is a compiler semantic, not a runtime peephole. The lowerer propagates it through
sequence, conditional, logical/coalesce, direct call, and receiver call forms, then emits a dedicated
`TailCall` opcode followed by the ordinary `Return` fallback. Ordinary `Call` therefore gains no hot
branch, while native functions, Proxy/native continuation paths, protected handlers, and future
non-bytecode callables retain the already-tested call/return ownership path.

The VM replaces a frame only for strict bytecode calls whose instruction offset is outside every
immutable handler protected range. Replacement preserves the caller return register, native
continuation ownership, original call site, handler/completion bases, and activation-aligned fiber
vectors; it resets the target register window and allocates the target lexical environment without
growing the 104-byte `Frame`. A call inside a protected `try` falls back so a throw still reaches its
catch/finally, while a return from a catch or finalizer body may replace the frame after discarding the
overridden completion.

Functions using `arguments`, `arguments.length`, or rest parameters currently publish
`FunctionLayout::needs_argument_source` and deliberately fall back. Reusing their caller window would
make later parameter writes mutate the observable argument sequence. A future exact traced argument
snapshot may remove this restriction without adding a pointer to every frame. Tagged-template and
labeled/with syntax remain frontend gaps, and debugger stack presentation still needs an explicit
tail-frame elision contract before the broader debugger milestone can claim PTC integration.

Clean HEAD `e54319a` 的普通 call-loop 两次 release median 为 `4.240 ms` 与 `4.298 ms`；同轮 Boa
0.21.0 为 `8.993 ms`，Tachyon 约快 `2.09x`。相对旧 `4.094 ms` checkpoint 有约 3.6%-5.0% 的表面
回退，但 macOS affinity/governor 均不可用，首轮 background MAD 12.4%，报告按规则为 invalid；因此
当前证据足以排除 Boa parity 破坏，不足以宣称 ordinary-call regression。后续固定性能 Linux profile
应判断 extended tail opcode 加入 dispatch table 后是否存在真实 instruction-layout 回退。

## 14. Date branded object 与宿主时间边界

Date 不能以 ordinary object 上可见的伪属性模拟。Tachyon 使用独立 GC payload
`DateObject { date_value: f64, ordinary: OrdinaryObject }` 表示规范 `[[DateValue]]`；property shape/storage、
prototype、extensibility 和 write barrier 仍通过统一 `ObjectReceiver` 分派。`%Date.prototype%` 本身也是
`[[DateValue]] = NaN` 的 genuine Date object，因此 `getTime`/`valueOf` 只读取 payload 并对普通对象、Proxy
或伪造 property 执行 brand rejection。该布局当前为 32 bytes，不为尚未证明有收益的 local-time cache
增加每对象常驻字段。

每个 Realm 持有 `%Date%`、`%Date.prototype%`、`getTime` 和 `valueOf` roots。构造 derived Date 时先读取
普通 data-property `newTarget.prototype`，非对象 fallback 使用 `GetFunctionRealm(newTarget)` 所属 Realm 的
`%Date.prototype%`；这与 QuickJS `js_create_from_ctor(..., JS_CLASS_DATE)` 及 Escargot
`Object::getPrototypeFromConstructor` 的边界一致。单参数 genuine Date 直接复制 `[[DateValue]]`，其他当前
已支持的 primitive 数值输入经 ToNumber 和 TimeClip；TimeClip 拒绝非有限值及绝对值大于 `8.64e15`，
向零截断，并把 `-0` 或截断后为零的负小数规范化成 `+0`。

Proxy/accessor `newTarget.prototype` 的完整 observable Get 仍依赖通用 construct continuation，不能由 Date
单独建立同步近似路径；在该 continuation 闭合前，不把这一变体计入 Date foundation 的完成范围。

UTC getter 共用 `DateUtcField` native descriptor，避免为八个同构方法复制 enum dispatch 和 VM 函数。
已 TimeClip 的 millisecond 值必为 `[-8.64e15, 8.64e15]` 内整数，可无损转为 `i64`；实现用
`div_euclid/rem_euclid` 拆 day/time，再用常数时间 proleptic Gregorian civil conversion 得到 year/month/date，
因此负 epoch 不依赖 C/Rust 截断除法特例，也不需要每 Date 对象常驻 calendar cache。UTC getter invalid Date
统一返回 NaN，brand check 始终先于字段读取。

`Date.UTC` 保留规范浮点求值顺序：参数先按从左至右 ToNumber 后 trunc，MakeDay 规范化 year/month，MakeTime
依次累加 hour、minute、second、millisecond，MakeDate 最后乘 `msPerDay` 并 TimeClip。不能把各字段先转换成
整数毫秒再相加，因为 Test262 的大数 precision case 会观察不同舍入；也不能用 `mul_add` 或代数重排。
object 参数通过共享 conversion continuation 的 `DateNumericArgument` number-hint consumer 暂停；通用
32-byte continuation 仍只保留 state Value 和当前 object，不因 Date 的七参数上限扩张。Date 自己只拥有
128-byte cold payload `PendingDateNumericArguments`，其中 tracing 覆盖 receiver 与固定七槽原始参数，浮点字段、
operation、argument cursor 和 invalid snapshot 都是无 GC edge 的 scalar。全 primitive 调用在栈上直接推进，
不分配 payload；只有首次遇到 object 才复制完整 bounded 参数窗并分配，后续 object conversion 复用同一状态。

`%Date.prototype%[Symbol.toPrimitive]` 是 generic ordinary-conversion 入口，不执行 Date brand check。hint 必须
与 Realm-rooted `string`、`default` 或 `number` 字符串按 UTF-16 内容相等；前两者从现有 conversion trampoline
的 `ToString` stage 起步，后者从 `ValueOf` stage 起步，刻意跳过 `Exotic` stage。这等价于 QuickJS
`HINT_FORCE_ORDINARY`/Escargot `ordinaryToPrimitive`，避免方法再次读取 receiver 的 `@@toPrimitive` 而递归。
两个 closed consumer 只编码 first-method ordering，继续使用 32-byte continuation 的 receiver/object Value，
不新增 GC payload。symbol-key descriptor 为 writable=false、enumerable=false、configurable=true。

`Date.prototype.toJSON` 是 generic receiver operation，不执行 Date brand check。入口先用共享 `ToObject`
装箱 non-nullish primitive，再以 number hint 进入同一 resumable ToPrimitive trampoline；只有转换结果本身是
非有限 Number 时返回 null，String、Symbol 和其他 primitive 结果都必须继续观察原 boxed receiver 的
`toISOString`。后半段使用 `DateToJson(Get/Call)` 两阶段 32-byte typed continuation：Get 走统一
Proxy/accessor-aware property dispatcher，Call 以保存的原 receiver 为 this 且参数数为零。conversion continuation
已经 trace boxed receiver，切换后 typed continuation 的 first Value 接管该 root，因此不需要专用 heap state、
Frame 字段或 Rust 递归。同步 dispatch 失败必须弹出尚未消费的 parent continuation，bytecode callback 则由
既有 completion trampoline 恢复。

`Symbol` 不可构造，所以其 `prototype` 不能放入只由 `has_default_prototype()` 暴露的 constructor 虚拟槽。
Realm 初始化将 `%Symbol.prototype%` 发布为 `%Symbol%` 上 writable=false、enumerable=false、configurable=false
的真实 own data property；这与 QuickJS/Escargot 的可观察 surface 一致，并保证 Symbol primitive 经 ToObject
装箱后能继承用户添加的方法。

UTC setter 同样由 `DateUtcSetter` descriptor 表驱动，descriptor 只携带 method identity、name 和标准
length；执行器先读取 branded `[[DateValue]]`，再把 supplied arguments 从左至右转换到上述 traced state
的连续 UTC field 区间并复用 MakeDate。optional argument 缺失必须与显式 undefined
区分：缺失保留当前 field，undefined 转为 NaN。invalid Date 时仍先执行所有 required/supplied conversion；
只有 `setUTCFullYear` 以 +0 作为字段默认基准。其他 setter 根据转换前 snapshot 返回 NaN，且不能覆盖
ToNumber callback 对 receiver 的修改；这对应 Escargot `originalDateValue`/`isOriginalDateValid` 的分离，
避免错误地在 callback 执行后重新读取 Date。共享路径防止七个方法各自漂移 brand/conversion/default ordering。

UTC formatting 复用同一 `UtcDateParts`，使用容量 40 bytes 的栈上 `DateFormatBuffer` 和手写 zero-padded
unsigned decimal emission；`toISOString` 最大 27 bytes，`toUTCString` 在六位 signed year 下最大 32 bytes，
容量集中在 Date tuning constants 并有 extended-year 边界测试。该路径不调用 `format!`/`snprintf`、不分配
中间 Vec，也不引入 locale。invalid `toISOString` 产生无 GC payload 的 RangeError marker，invalid
`toUTCString` 返回固定 ASCII sentinel。Annex alias `toGMTString` 直接发布相同 native function Value，不能
重新分配一个同名 wrapper 破坏 identity。

`Date.parse` 的输入先通过现有 `ConversionNativeFunction` 以 string hint 完成可恢复 ToPrimitive/ToString；
对象 getter/method callback 不在 Rust 栈上递归执行。ISO parser 独立位于 `builtins/date/parse.rs`，直接消费
UTF-16 code units，并返回 `ParsedDateTime::Utc(value)` 或 `Local(fields)`。该枚举是 timezone capability 的
稳定交界：date-only 形式按 UTC，显式 `Z`/offset 先用未 clipped 的 MakeDate 计算，再减 offset 并执行最终
TimeClip；offsetless date-time 保留原始七字段，等待 M7 provider 应用 local offset。provider 未接入时 Local
结果返回 NaN，不能默认 offset=0。解析器验证 expanded negative-zero year、实际 month day 上限、24:00 特例、
minute/second/offset range 和尾部完整消费；不复制 QuickJS 的平台 timezone 调用或 Escargot 的 ICU fallback。

engine core 不拥有 wall clock、timezone、locale 或系统文件能力。`WallClockProvider` 只返回 Unix epoch
milliseconds；`TimeZoneProvider` 分别提供 UTC instant 的 local offset 与 local wall-time 到 UTC 的
ECMAScript-compatible gap/overlap resolution，两者不能与 monotonic deadline clock 合并。provider 由 isolate
独占 `Box<dyn ... + Send>`，避免单线程 Date 路径引入 `Arc`/atomic；`HostProviderError` 是无分配固定 code，
方便 Rust/FFI adapter 映射。`Isolate::new_with_host_providers` 显式注入，缺失 provider 返回结构化 host error。
`Date.now` 和零参数 `new Date()` 接 wall clock；函数形式 `Date()`、offsetless parse、多参数构造及 local
getter/setter/formatting 接 timezone provider。UTC→local 只把 provider offset 加到 clipped instant 后复用
`UtcDateParts`；local→UTC 先以 MakeDay/MakeTime 生成无时区 wall-time coordinate，再由 provider 完成 DST
gap/overlap disambiguation，最后 TimeClip。provider offset 在 VM boundary 验证不超过一个 civil day，普通
零 offset 显式规范化为 +0，避免 `getTimezoneOffset` 泄漏 -0。

单参数 Date 构造使用 default-hint resumable ToPrimitive：真实 Date 先复制 `[[DateValue]]`，其他 object 的
newTarget 与原 object 由 conversion continuation 精确 trace，primitive String 进入共享 Date parser，其余
primitive 才 ToNumber/TimeClip。多参数构造与 local setter 复用 `PendingDateNumericArguments`，operation tag
决定最终执行 UTC MakeDate、provider local→UTC 或 Date allocation；不建立第二个 argument state，也不把
callback 嵌套进 Rust 栈。`%Date.prototype%` 是无 `[[DateValue]]` 的 ordinary object，所有 branded method 对其
抛 TypeError；旧的 NaN Date payload prototype 模型已删除。

local `toString`/`toDateString`/`toTimeString` 使用同一 40-byte stack buffer，输出英文规范基础字段与 numeric
`GMT±HHMM`，不查询 locale 数据或 timezone display name。ECMA-262 `toLocale*` surface 在无 Intl provider 时
复用对应 local formatter 作为 implementation-defined fallback；未来 ECMA-402 层可在 core 外注入 locale
能力，但不能让 Date core 读取 ICU、系统环境或文件。parser 除 ISO grammar 外只接受引擎自身输出的 UTC/local
格式，以满足规范要求的 zero-millisecond round-trip，不扩张为宿主依赖的自由格式 parser。任何路径都不能
调用 `SystemTime`、libc clock、环境变量或时区文件；实现参考 QuickJS/Escargot 的字段归一化与解析边界，
但不搬入其平台分支。

## 15. WeakRef JS binding 与 job-scoped kept roots

WeakRef 使用独立的 32-byte `WeakRefObject { ordinary, target: WeakGcRef<()> }`，不把 target 放进 ordinary
property storage，也不建立 side table。weak edge 在 strong tracing 时不 mark，major weak phase负责改写或
清空；wrapper 的生存期因此不会延长 target 的生存期。object 与 non-registered Symbol 统一以 raw heap
identity 表示，`CanBeHeldWeakly` 复用 WeakMap/WeakSet 的判断，避免三套规则漂移。

Heap 对 VM 暴露 `add_to_kept_objects(RawHeapRef)`：入口验证 reference，只修改有显式容量上限的 job-scoped
kept-root set，不暴露 payload borrow。constructor 与成功 deref 都先加入 kept roots；宿主只能在明确的
ECMAScript job boundary 调用 `clear_kept_objects_at_job_boundary`。容量或分配失败以结构化
`ExecutionError::KeptObject` 传播，不能伪装成用户 TypeError，也不能 panic/unwind。

每个 Realm 精确 root `%WeakRef%` 与 `%WeakRef.prototype%`。prototype fallback 当前复用同步 data-property
`GetPrototypeFromConstructor` 基础；Proxy/accessor abrupt completion 与 constructor Realm fallback 仍依赖共享
construct continuation，不能在 WeakRef 内建立第二套递归 Get。FinalizationRegistry 发布后 Test262 当前为
52/58：4 个 accessor variants 与 2 个 cross-realm variants 保留为共享缺口。

## 16. FinalizationRegistry JS binding 与 cleanup jobs

FinalizationRegistry 使用 40-byte `FinalizationRegistryObject { ordinary, cleanup_callback, head }` 与
32-byte `FinalizationCell { registry, registration, unregister_token, next }`。registry 不拥有 Rust `Vec`：
每次 register 分配一个 GC-managed linked cell 并发布为 head，避免 registry 热路径扩容和 external-memory
计费分叉。cell 对 registry、held value、next 是强边，对 target 与 unregister token 是弱边；同 token
unregister 扫描链并停用全部仍 active 的 registration。collector enqueue 的 owner 是 cell 而不是 registry，
VM 因此能在 callback 前验证 payload type并解析所属 registry。

cleanup job 复用 isolate 的 Promise checkpoint，不建立第二套 interpreter loop。VM 先完整 reserve 并转移
collector snapshot，再把队首 owner/held value 作为精确 roots，通过 32-byte typed native continuation 调用
cleanup callback。bytecode callback normal return 恢复 entry frame 到原 return site；throw 路径消费当前 job、
复位 scheduler state 并保留原 JS thrown value继续显式 abrupt dispatch。callback 内新产生的 records 留在
collector queue 供下一 safepoint，避免当前 checkpoint 无界自增长。当前 Test262 FinalizationRegistry 为
88/94；剩余 2 cross-realm 与 4 observable accessor variants 统一依赖完整
`GetPrototypeFromConstructor` continuation，不在该 builtin 内复制同步旁路。

## 17. JSON serialization state

`JSON.stringify` 的 primitive `space` 使用固定 10 个 UTF-16 code unit 的 `JsonIndentation`，与规范允许的
最大 gap 完全一致。String primitive 只复制前 10 个 code unit；Number primitive 先执行无回调的
ToIntegerOrInfinity 语义，再 clamp 到 0..10 并直接填充固定 ASCII-space gap，NaN、负数和负无穷得到 compact
mode，正无穷得到 10 spaces。boxed Number/String 不直接读取 internal slot，而是分别通过 number-hint/string-hint
conversion consumer 观察 `@@toPrimitive`、`valueOf`、`toString` 与 abrupt completion；continuation 的 pending
receiver 精确 trace 原始 stringify value，转换完成后才进入同步 serializer。serializer 传递 scalar depth，需要换行时
直接追加 `gap * depth`，不为每层建立新的 indent String 或
长期增长 Vec。compact mode 保持原输出路径，pretty mode 仅在非空 Array/Object 的元素、成员和 closing token
前追加规范换行，并在成员冒号后追加一个 ASCII space。

stringify 的递归 Rust serializer 已删除。`PendingJsonStringify` 是 callback 间唯一 owner，精确 trace replacer、
property list/source、holder/key/value/temporary/space，并保存 iterative Array/Object frame 与已提交 UTF-16
输出。32-byte `NativeContinuation` 只携带 state Value 和 `JsonStringifyStage`；每次 `Get(holder,key)`、`toJSON`
lookup/call、replacer call、Proxy ownKeys/descriptor、Array length 与 wrapper conversion 都先把 state 发布到
destination register，恢复后重新读取 movable `GcRef`，任何 Rust local `Value` 都不得跨 allocation safepoint。

属性流水线固定为 `Get -> GetV(toJSON) -> Call(toJSON) -> Call(replacer) -> wrapper unbox -> serialize`。
Array length 和元素 Get、Object Proxy 的 ownKeys/getOwnPropertyDescriptor/Get 都保持规范可观察顺序；ordinary
Object 在进入 frame 时快照 enumerable own string keys。replacer Array/Proxy 先 observable Get length，再逐项 Get；
primitive/boxed String 和 Number 按 ToString string hint 转为 Atom，去重后存入内部 GC-managed Array，Object frame
共享该 property list，Array frame忽略它。

property read dispatcher 必须保留同步完成与挂起的区别：`Returned(Value)` 表示 continuation 尚在 completion
stack、调用者可在当前 Rust loop 消费结果；`Suspended` 表示 accessor/Proxy/user code 已发布 JS frame 或下层
continuation，当前驱动器必须立即返回。replacer property list 的 primitive entries 用这一协议同步 drain；Object
使用 property list 且无 replacer function 时，同步 Get 得到 `undefined` 的 missing member 同样在 container loop
内推进 cursor。无 replacer function 的 Object/Array 若同步 Get 得到无需 `toJSON` lookup 的 primitive，则在
同一 loop 内直接生成规范 UTF-16 units、应用 Object omission/Array null substitution、提交 prefix/value 并推进
cursor；不得通过 `finish -> complete -> advance` 为每个实际成员增长 Rust native stack。Object、BigInt、boxed
conversion 与任何可观察挂起继续回到既有 typed continuation pipeline，保证 getter/Proxy/toJSON/replacer 的
顺序和 abrupt identity 不被 fast drain 绕过。这个边界与 QuickJS/Escargot 的显式 member loop 等价，但保持
Tachyon 的 movable-GC root publication 规则。

external accounting 不允许发布后的 `Vec` 自行扩容。output/frame 达到容量前分配精确计费的 replacement state，
复制 committed output/frame 后切换 destination root；增长策略集中在 `tuning::json`。frame key list 使用普通
GC Array edge，由 property storage 独立计费和回收，不进入 JSON payload external charge。最终字符串复制 committed
units 后分配，旧 state 的 charge 由 sweep 正常释放。reviver 仍属于 JSON.parse 后续 slice。

## 18. Proxy ownKeys 的迭代推进与成员索引

`PendingProxyOwnKeys` 是一次 `[[OwnPropertyKeys]]` 操作的唯一 owner：它同时持有 trap result、已转换的
`Box<[PropertyKey]>`、目标 own keys 与推进 cursor。普通 data property 和 missing element 在同一个 Rust loop
内同步 drain；只有 accessor 或 nested Proxy Get 才发布 typed continuation 并退出解释器。这样大型同步
array-like trap result 的栈深度保持常数，同时保留 accessor/Proxy 的可观察顺序与 abrupt completion identity。

重复 key 检测和 target inclusion 使用 exact-capacity、external-accounted 的 open-addressed membership table。
容量取 trap-result length 的两倍并向上取二次幂，负载不超过 0.5；表中只保存稳定的 Atom index 或 Symbol
serial identity，不保存可能因 GC 移动而失效的 heap reference。traced `Box<[PropertyKey]>` 仍是 key 的唯一
存活 owner，membership table 只是 expected O(1) 的派生索引，不能参与 tracing 或替代 liveness ownership。
RAB view tracking uses the explicit `ViewLengthMode` stored in each TypedArray/DataView payload.
Omitted-length views derive their current range from the backing on every snapshot; shrinking before
the view offset produces OOB zero metadata and growing restores the view. Fixed views retain their
declared range and become OOB while the current backing is too short. No raw backing pointer or stale
derived length crosses a conversion/callback boundary; method-specific resize ordering still requires
the complete TypedArray/DataView witness matrix.
