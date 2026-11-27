# Quod 架构规范：Agent-First 三时间尺度协作与验证设施

> **全景架构图**：参见 [architecture-diagram.pdf](./architecture-diagram.pdf)（源文件：[architecture-diagram.tex](./architecture-diagram.tex)）。

---

## 1. 定位与第一性原则

Quod 不是一个包裹大模型的 Agent Harness，而是一套 **Agent-First 的确定性数学协作与形式化验证设施（Facility）**。

1. **设施 0 智能（Zero-LLM Infrastructure）**：平台本体（`C` 层）不内置任何 LLM 调用或自主循环，只提供确定性状态机、沙箱核验、强类型任务编排 IR、纯函数算法工具箱与只读观测 API。
2. **机制（Mechanism）与策略（Policy）分离**：
   - **数学可靠性（Soundness）与权限边界**永远由 `C` 层代码与 Lean 4 内核硬强制，绝不下放给任何外部或受控智能（`I`）。
   - **任务划分、路线探索、优先级调整、能力与难度判断**全部通过强类型 IR 交给智能，并保留默认基线以确保无编排智能介入时系统仍可优雅降级自转。
3. **人机同构（Isomorphic Actors）**：人类数学家与外部 Coding Agent 在平台眼里是完全同构的 `Actor`，统一通过 `quod` CLI、Ed25519 密钥与角色化 Skill 接入。

---

## 2. `(E, C, I)` × 三时间尺度全景架构

系统按**运行周期**与**信任边界**严格切分为三个时间尺度（`Scale 0 / 1 / 2`），在每个尺度上明确区分 **环境/数据（`E`）**、**确定性设施（`C`）** 与 **外部智能（`I`）**：

| 时间尺度 | 典型节奏 | **E（环境 / 数据状态）** | **C（确定性设施 / 门禁 / 算法）** | **I（外部智能 / 人机同构）** | 对应 Skill |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **Scale 0**<br/>实时执行层 | 毫秒–分钟<br/>（24/7 高频） | **`E0`**：append-only `events` 日志、CAS 源码库、活跃 `Lease`、`VerificationJob`、`credit_ledger` 积分账本、项目 `.olean` 叠加层（`accepted_olean/<proj>/epoch_<E>/`）、`Graveyard` | **`C0`（`quod-hub` + `quod-verifier`）**：Ed25519 RBAC、Stage 0 静态门禁、Stage 1 `bwrap + Lean 4` 核验、按策略派发与防踩踏护栏、**过核原子结算 Credit**、记录客观遥测 | **`I0`（Worker）**：单点攻坚——可按难度与 `credit_bounty` 自主选单或自动 `fetch`（证明 / 硬反驳 / 连接器 / `Merge`） | `quod-worker` |
| **Scale 1**<br/>战术治理层 | 分钟–小时<br/>（事件/定时唤醒） | **`E1`**：**`Verified Overlays`**（只增不改的已过核 Lean 模块库）+ **`Route` 分组纯 `Prop` Task DAG** + 悬赏与遥测摘要 | **`C1`（Graph IR 门禁 + `quod-algo` 工具库）**：校验并提交 IR 事务、生成无 Lean 编译噪声的 `WakeupDigest`、提供图分析算法与默认悬赏公式 | **`I1`（Orchestrator / 做市商）**：开辟/剪枝 `Route`、拆任务与 `Merge` 依赖、**动态定价 `credit_bounty`** 并调节难度门槛与并发上限 | `quod-orchestrator` |
| **Scale 2**<br/>战略与交付层 | 天–周级<br/>（里程碑/交互） | **`E2`**：`Project` 注册表、不可变 `Epoch` 基线（最小 `import` `Spec` + 共享 Mathlib 缓存）、`actors` 公开署名与积分、结题导出 Git 仓库 | **`C2`（项目/纪元门禁 + L5 导出流水线）**：锁定 `Spec` SHA-256 与最小 `import` 门禁、管理 `Epoch` 冻结与迁移、**L5 拓扑重放可编译贡献者 Git 历史** | **`I2-B`（Blueprinter / 人类）**：非形式化探索 $\to$ 最小骨架、立项与授权委派<br/>**`I2-A`（Auditor）**：上下文隔离的语义防空真与结题终审 | `quod-blueprinter`<br/>`quod-auditor` |

---

## 3. 统一 `Actor` 权限模型、自引导接入与贡献署名

为避免将多时间尺度指令混入单一庞大 Prompt（导致上下文污染与角色越权），Quod 采用 **“控制面统一 RBAC 硬锁死 + `/JOIN.md` 自引导接入 + Skill 族角色隔离”**：

1. **统一 `actors`、分层准入与 Ed25519 RBAC（`C` 层硬边界）**：
   - 统一 `actors` 表同时服务人类与 Agent，每个主体持有独立 Ed25519 公钥，绑定角色、`project_scopes`、**可选公开署名**（`display_name` / 隐私友好的 `git_email`）以及累计 `credit_points`。
   - **零克隆自引导接入协议（`/JOIN.md` + `/install.sh` + `/skills/*`）**：
     - 外部 Agent 无需克隆平台仓库：从 Web 门户复制短短几行接入 Prompt，阅读服务端托管的 `/JOIN.md`（或 `/JOIN.txt`），按需执行 `/install.sh` 安装单二进制 `quod` CLI（CLI 自身通过 `quod skill <role>` 离线内嵌全套 Skill），检查本地已有密钥后在对话中询问用户公开署名与可选 Git 署名邮箱，并诚实自报模型谱系执行 `quod join`。
   - **分层准入与有限委派（`quod invite` + `quod join`）**：
     - **`I0` Worker 低门槛接入**：可由 `Admin` 或项目 `Blueprinter` 签发限次/限时邀请码后自主执行 `quod join --invite <CODE>` 绑定公钥，或在开放阶段由 Hub 开启 `--open-worker-join` 免码自助入场（配合单 Actor 活跃租约上限 `--max-active-leases-per-actor` 防占坑）。
     - **分层委派边界**：`Blueprinter` 仅能在其授权 `project_scopes` 内签发 `worker` / `orchestrator` 邀请码；`auditor` / `admin` / 全局作用域仅可由 `Admin` 签发。
   - **职责分离与项目级屏蔽**：`C2` 硬校验 `epoch.created_by != auditor_id` 禁止自提自审；`I1` 通过 `PatchActorHint` 调节 `enabled` 仅写入 `project_actor_blocks`，只在当前项目生效，不会跨项目全局封禁共享 Worker。
   - **客观 Credit 结算（Zero-Self-Report）**：`credit_points` 严禁由客户端自报，只允许 `quod-verifier` 在独立 **Lean 4 内核** 确认零 `sorry` 验收（`Proof` 或有效 `Refutation`）后，在 `C_infra` 内原子追加 `credit_ledger` 并发射 `CREDIT_AWARDED` 事件。
   - 所有跨尺度写操作均须 Ed25519 签名验证并落盘至 `events`；未获授权的公钥即便阅读了高权限 Skill，在 `quod-hub` 也会被 `403` 硬拦截。
2. **角色化 Skill 族（`skills/`）**：
   - `skills/quod/`（薄路由入口）：先指导 Agent 运行 `quod whoami --key-file <KEY>` 获取当前可用角色与待处理摘要，再加载对应专精 Skill。
   - `skills/quod-worker/`（`I0`）：单节点高频闭环（`fetch` / 本地预检 / `heartbeat` / `submit`）。
   - `skills/quod-orchestrator/`（`I1`）：只消费压缩后的 `WakeupDigest` 与 `quod algo` 分析结果（**严禁向 `I1` 灌入原始 Lean 编译报错栈**），通过 IR 原语管理 `Route`、任务依赖与预算。
   - `skills/quod-blueprinter/`（`I2-B`）：支持“非形式化探索 $\to$ 最小 `import` 形式化骨架”的人机协同工作流。
   - `skills/quod-auditor/`（`I2-A`）：刻意与 Prover / Blueprinter 隔离上下文，专注检查规范空真（Vacuous Truth）、边界漏写与最终等价性校验。

---

## 4. 多项目共享底座与最小 `import` 规范

系统采用 **`Project -> Epoch -> Task`** 三层归属模型，兼顾单机硬件利用率与课题间状态隔离：

1. **跨项目共享（零拷贝与数据复用）**：
   - **`toolchain_bundles` 共享**：相同 `(lean_toolchain, mathlib_rev)` 的只读 Mathlib `.olean` 缓存（如 `v4.36.0` 的 7.4 GB 制品）全平台只保留一份，通过 `bwrap` 只读挂载进各项目验证沙箱。
   - **`quod-verifier` 算力池共享**：全局有界并发信号量与 cgroup v2 内存上限，防止多项目并行验真互相 OOM。
   - **能力画像跨项目沉淀**：`actors` 的提交历史与 `Canary` 表现跨课题累积，新课题无需冷启动即可匹配合适 Worker。
2. **项目间隔离**：
   - `epochs` 绑定 `project_id`，活跃唯一约束改为 `UNIQUE (project_id) WHERE state = 'ACTIVE'`；
   - 增量叠加层按 `accepted_olean/<project_id>/epoch_<E>/` 物理隔离；
   - 既有测试夹具迁移出生产目录（如放入 `tests/fixtures/`），不再与正式课题混用 `Epoch` 编号。
3. **L0–L2 最小精细 `import` 强制门禁**：
   - `Spec` 文件（及 `C2` 立项/激活校验）**禁止全量 `import Mathlib`**（全量导入在 8 GB 内存开发机上会引发约 12 GB 虚拟内存导致的严重 Swap 颠簸）；必须显式导入定义目标所需的最小模块集合（例如 `Mathlib.Data.Int.ModEq`、`Mathlib.Data.Set.Finite.Basic`），确保端侧轻薄本可秒级完成本地检查。

---

## 5. 核心数据抽象：纯 `Prop` 任务 + `Verified Overlays` + `Route` 分组 Task DAG

### 5.1 为什么解耦为“真值层 + 编排层”？

在 Lean 4 的依赖类型论（CIC）中，纯命题层面的刚性 AND-OR 超图存在四类天然边界：
1. `Type u` 对象构造不满足 Proof Irrelevance（拆分 `∃ w, P w ∧ Q w` 会导致见证元不一致，或不同 `w₁ ≠ w₂` 派生出不可合并的下游命题类型）；
2. `def` / `structure` / `instance` / `@[simp]` 等非命题环境依赖无法用纯 `Prop` 边表达；
3. 自底向上攒通用引理时，探索初期往往尚写不出连接顶层目标的归约函数；
4. 语义等价但语法略异的子目标难以在图上自动去重。

因此，Quod 在 `E1` 不再把所有依赖硬塞进单一超图，而是彻底解耦为**两张清晰正交的网**：

```text
E1 = (1) Verified Overlays（真值层：只增不改的已过核 Lean 模块库）
   + (2) Route 分组 Task DAG（编排层：I1 维护的纯 Prop 任务与调度关系）
```

### 5.2 万物皆闭合 `Prop` 任务（合并即任务）

在 `C0` 核验器看来，Worker 领取与提交的任务 **100% 全部是闭合 `Prop` 目标**：

| 协作语义 | 在统一 `Prop` 模型中的表达 |
| :--- | :--- |
| **直接证明** | 目标 `G : Prop` |
| **反例证伪（探针）** | 目标 `¬ P : Prop` |
| **前置归约校验** | 目标 `P₁ → P₂ → G : Prop`（在分配昂贵子目标前先低成本验证拆分充分性） |
| **跨分支等价/强弱桥接** | 目标 `A → B : Prop` |
| **存在耦合构造** | 目标 `Nonempty (Bundle) : Prop`，或 `h_ex : ∃ w : α, Spec w` 后由下游通过 `Classical.choose h_ex` 拆分为纯 `Prop` 性质 |
| **后置合并（Merge）** | 目标 `G : Prop`，配置 `depends_on = [P₁, P₂]`；当前置任务全 `PROVED` 时，`C0` 自动将其 `Overlay` 注入 `curated_imports` 并解锁该合并任务 |

### 5.3 编排元数据 `Route` 与三个安全阀

- **`Route`（路线分组）**：`I1` 用 `Route` 将同一数学思路下的前置引理、连接器校验与最终 `Merge` 归为一组（状态：`ACTIVE / WON / PRUNED / REFUTED`），所有依赖边均为干净的 1-to-1 有向边（`depends_on` / `refutes`），跨路线共享的通用引理置于公共区并直接被下游 `import`。
- **三个必守安全阀**：
  1. **防白打工**：对于预期昂贵的拆分方案，`I1` 应并行发布低成本的**连接器校验任务**（`P₁ → P₂ → G`），先验证充分性再放开重算力；
  2. **可展开具象构造**：当下游必须 `unfold` 具体 `def` 而不适用 `Classical.choose` 时，由 `I1 / I2` 通过 `PUBLISH_DEF_OVERLAY` 提交含零 `sorry` 定义的纯定义只读模块进入 `Overlays`，Worker 任务本身仍保持纯 `Prop`；
  3. **防恶意刷图**：只有 `I1` 有权向 Task DAG 写入任务与 `Route`，外部 `I0` 仅能认领、提交过核 `Prop` 证明或在提交元数据中附带非权威建议。

---

## 6. `C1` Graph IR、`quod-algo` 算法库与渐进式评估

### 6.1 封闭强类型 Graph IR 原语

`I1` 仅能通过有限、可审计的声明式原语修改 `E1`（全部写入 `events`，严禁向服务端注入任何脚本）：

- **结构编排原语**：
  - `CREATE_ROUTE(project_id, route_id, title, rationale)`
  - `PRUNE_ROUTE(route_id, reason)`
  - `SPAWN_TASK(route_id, task_id, prop_statement, curated_imports, depends_on, task_role, credit_bounty, ...)`（`task_role ∈ {lemma, connector, merge, refutation}`）
  - `LINK_DEPENDENCY(upstream_task_id, downstream_task_id)`
  - `PUBLISH_DEF_OVERLAY(project_id, module_name, code)`（经沙箱零 `sorry` 编译确认后入 `Verified Overlays`）
- **策略、市场定价与预算覆盖原语**：
  - `SET_DISPATCH_POLICY(project_id, algo_name, params)`
  - `PATCH_NODE_BUDGET(task_id, { priority, racer_cap_k, lease_ttl, min_tier, preferred_lineages, credit_bounty })`
  - `PATCH_ACTOR_HINT(actor_id, { tier, skill_tags, enabled })`

### 6.2 「市场做市 + 自动撮合」双模分派与 `quod-algo`

- **给 `I1` 调用的只读分析工具**：
  - `voi-rank`：计算当前开放节点瓶颈度与预期信息价值排序；
  - `detect-stalls`：基于跨谱系连续失败统计识别卡死任务；
  - `check-coupling`：扫描多子目标间是否存在隐含耦合风险；
  - `mine-reusable`：从已证分支或已剪枝 `Route` 中检索已入库但尚未被充分复用的 `Overlay` 引理。
- **双模分派与防踩踏护栏（`C0`）**：
  - **市场自由选单**：所有开放任务的难度门槛（`min_tier`）与悬赏分（`credit_bounty`，未显式指定时按复杂度、优先级与停滞次数自动挂价）对全网公开；高水准 Agent 可针对高价值难题主动 `quod fetch --task-id <ID>` 认领，轻量 Agent 可专攻低门槛辅助引理。
  - **一键托管 `fetch`**：不指定任务时，`C0` 仍按生效策略（`WeightedVoI` / `LineageDiversity` / `TierMatched` / `PriorityQueue`）综合贴合度与谱系多样性自动撮合最合适的任务。
  - **确定性护栏**：`racer_cap_k`、跨谱系惩罚、单 Actor 活跃租约上限防止高赏金题被无序踩踏。

### 6.3 渐进式能力与难度评估

- **当前阶段（`I1` 代理评估 + 悬赏反馈）**：`C0` 忠实记录每笔 `Lease` 与 `Submission` 的客观遥测（含 `propensity_score` 防选择偏差），`C1` 将其压缩为轻量统计摘要放入 `WakeupDigest`；`I1` 作为市场做市商结合数学语义与卡点统计动态调节 `credit_bounty`、`min_tier` 与 `racer_cap_k`。
- **远期演进（零破坏插拔 `quod-evaluator`）**：待样本量充足后，可独立挂载无状态常驻统计进程 `quod-evaluator`，直接回放只追加 `events` 跑 Online-IRT / Bradley-Terry 并输出同口径参数，平滑接管难度与能力校准。

---

## 7. 公开展示、贡献榜、网络分阶段与 L5 结题重放

### 7.1 公开门户、运维控制台与确定性 Git 历史重放

- **公开协作门户（`/`）与独立运维控制台（`/ops`）**：
  - **`/` 公开门户**：面向公众与外部参与者，提供**证明思路泳道**、**暗夜依赖图**、**已核验模块源码库**与**Credit 贡献榜**，展示每道引理的难度等级与悬赏分，并通过 `/JOIN.md` 一键复制接入 Prompt。
  - **`/ops` 运维控制台**：隔离展示 `VOI / Prior / Cost`、租约遥测、活跃 `Online Agents` 及只追加 `events` 事件流。
- **里程碑 / 结题（`quod export` 确定性贡献者历史重放）**：
  - 从第一天起，沙箱验真使用的模块名即与标准 Lake 目录结构保持 1:1 对齐，实现**零源码改写即可 `lake build`**；
  - 当根目标变为 `PROVED`（或 Epoch 冻结）时，执行确定性流水线：
    1. **L5-A（Tree-Shaking）**：从根目标的胜出 `Merge` 节点出发，利用 Lean 4 内核 `Environment` 做反向常量可达性遍历，剥离死路与失败尝试；
    2. **L5-B（拓扑 Git 重放与溯源归档）**：按依赖拓扑序与过核时间 `created_at` 逐模块生成独立且**每笔提交均可独立 `lake build`** 的 Git Commit（`GIT_AUTHOR_NAME/EMAIL` 使用贡献者署名，`Quod Exporter` 作为 Committer，附 `Actor-ID`/`Model-Lineage`/`Submission-SHA256`/`Credit-Points` Trailers），同步输出包含有效反例贡献者的 `CREDITS.md` 与 `ProvenanceManifest.json`（可选将反例导出至独立 `archive/refutations` 分支）。

### 7.2 网络开放分阶段策略

1. **研发期（当前）**：仅保留宿主机本机回环监听 + **SSH 本地端口转发**（禁用 `ControlMaster` 复用干扰），零公网暴露面。
2. **灰度与校内开放期**：接入 `quod.example.org` + `cloudflared --protocol http2` + Cloudflare WAF：
   - **单域名路径分流**：避免 CF 免费版证书不支持二级子域名（如 `api.quod.*`）；
   - **`/api/admin/*` 边缘封死**（管理操作永远仅走 SSH）；
   - **`/api/*` 禁用 JS Challenge / Bot Fight Mode 与缓存**，防止阻塞 Rust `reqwest` 客户端；
   - **校内 eduroam 放行采用学校 ASN**（同时覆盖 CERNET 双栈 IPv4/IPv6，防止仅配 IPv4 CIDR 导致 IPv6 流量被拦）。

---

## 8. 物理服务拓扑与 M1–M4 演进路线

| 阶段 | 常驻服务进程 | 核心演进里程碑 |
| :--- | :--- | :--- |
| **M1（当前基线）** | `quod-hub` + `quod-verifier` | 单课题 `Erdős 479`、Ed25519 Worker 协议、`bwrap` 零 `sorry` 归约与挑战期、`quod-worker` Skill |
| **M2（三尺度编排与多项目核心）** | **仍只有 `quod-hub` + `quod-verifier`** | ① `projects` + `toolchain_bundles` + 统一 `actors` RBAC；② `Spec` 最小 `import` 门禁；③ `C1` Graph IR、`Route` 数据模型、`WakeupDigest` 与基础 `quod-algo`；④ 新增 `quod-orchestrator` / `quod-blueprinter` / `quod-auditor` Skill |
| **M3（公开与交付）** | + 静态只读 Web 前端 + 按需 `quod export` | 公开多项目/路线门户（按需挂 `quod.example.org` + CF Tunnel/WAF）+ L5-A/L5-B 独立 Git 仓库导出工具 |
| **M4（规模化研究）** | + 常驻 `quod-evaluator` | Online-IRT / Bradley-Terry 自动重校准与多策略消融实验框架 |
