(() => {
  "use strict";

  const isOpsPage = document.body.dataset.page === "ops";
  const initialProjectParam = new URLSearchParams(window.location.search).get("project");

  const state = {
    hubStatus: null,
    projects: [],
    currentProjectId: initialProjectParam || null,
    currentEpochId: 1,
    epochStatus: null,
    graph: null,
    events: [],
    leaderboard: [],
    leaderboardScope: "project",
    maxEventSeq: 0,
    activeView: isOpsPage ? "ops-tasks" : "lanes",
    hidePruned: false,
    searchQuery: "",
    selectedTaskId: null,
    inspectorBackTaskId: null,
    dagZoom: 1.0,
    promptRole: "worker",
    promptTargetRouteId: null,
    promptTargetTaskId: null,
    refreshTimer: null,
  };

  const els = {
    tickerEventType: document.getElementById("ticker-event-type"),
    tickerEventText: document.getElementById("ticker-event-text"),
    tickerOnlineAgents: document.getElementById("ticker-online-agents"),
    tickerTime: document.getElementById("ticker-time"),
    projectSelect: document.getElementById("project-select"),
    projectStatusBadge: document.getElementById("project-status-badge"),
    navOpsLink: document.getElementById("nav-ops-link"),
    footerOpsLink: document.getElementById("footer-ops-link"),
    navPublicLink: document.getElementById("nav-public-link"),
    footerPublicLink: document.getElementById("footer-public-link"),
    devEpochBadge: document.getElementById("dev-epoch-badge"),
    devToolchain: document.getElementById("dev-toolchain"),
    devPolicy: document.getElementById("dev-policy"),
    devGraveyard: document.getElementById("dev-graveyard"),
    devJoinMode: document.getElementById("dev-join-mode"),
    devOnlineAgents: document.getElementById("dev-online-agents"),
    autoRefreshToggle: document.getElementById("auto-refresh-toggle"),
    refreshBtn: document.getElementById("refresh-btn"),
    openJoinBtn: document.getElementById("open-join-btn"),
    auditBanner: document.getElementById("pending-audit-banner"),
    bannerAuditPromptBtn: document.getElementById("banner-audit-prompt-btn"),
    heroProjectSlug: document.getElementById("hero-project-slug"),
    heroSpecModule: document.getElementById("hero-spec-module"),
    heroTitle: document.getElementById("hero-title"),
    opsTitle: document.getElementById("ops-title"),
    heroVerdict: document.getElementById("hero-verdict"),
    heroDesc: document.getElementById("hero-desc"),
    heroStmtCode: document.getElementById("hero-stmt-code"),
    heroViewSpecBtn: document.getElementById("hero-view-spec-btn"),
    heroJoinBtn: document.getElementById("hero-join-btn"),
    heroProgressPct: document.getElementById("hero-progress-pct"),
    heroProgressTrack: document.getElementById("hero-progress-track"),
    heroProgressFill: document.getElementById("hero-progress-fill"),
    statProved: document.getElementById("stat-proved"),
    statRoutes: document.getElementById("stat-routes"),
    statOverlays: document.getElementById("stat-overlays"),
    statLeases: document.getElementById("stat-leases"),
    statLeasesSub: document.getElementById("stat-leases-sub"),
    searchInput: document.getElementById("search-input"),
    viewTabs: document.querySelector('[role="tablist"]'),
    viewLanes: document.getElementById("view-lanes"),
    viewOpsTasks: document.getElementById("view-ops-tasks"),
    viewDag: document.getElementById("view-dag"),
    viewEvents: document.getElementById("view-events"),
    viewOverlays: document.getElementById("view-overlays"),
    viewLeaderboard: document.getElementById("view-leaderboard"),
    dagSvg: document.getElementById("dag-svg"),
    dagZoomLabel: document.getElementById("dag-zoom-label"),
    inspEyebrow: document.getElementById("insp-eyebrow"),
    inspTitle: document.getElementById("insp-title"),
    inspCloseBtn: document.getElementById("insp-close-btn"),
    inspectorBody: document.getElementById("inspector-body"),
    eventsCount: document.getElementById("events-count"),
    eventsCursorLabel: document.getElementById("events-cursor-label"),
    eventsTbody: document.getElementById("events-tbody"),
    joinDialog: document.getElementById("join-dialog"),
    closeJoinBtn: document.getElementById("close-join-btn"),
    joinStatusPill: document.getElementById("join-status-pill"),
    simpleInviteRow: document.getElementById("simple-invite-row"),
    inpInviteSimple: document.getElementById("inp-invite-simple"),
    advancedPromptDetails: document.getElementById("advanced-prompt-details"),
    targetContextBar: document.getElementById("target-context-bar"),
    targetContextLabel: document.getElementById("target-context-label"),
    clearTargetBtn: document.getElementById("clear-target-btn"),
    inpInvite: document.getElementById("inp-invite"),
    inviteHintTag: document.getElementById("invite-hint-tag"),
    promptHubUrlLabel: document.getElementById("prompt-hub-url-label"),
    promptOutput: document.getElementById("prompt-output"),
    copyPromptBtn: document.getElementById("copy-prompt-btn"),
    copyStatusLive: document.getElementById("copy-status-live"),
    toast: document.getElementById("toast"),
  };

  function esc(str) {
    return String(str ?? "")
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  function showToast(msg) {
    if (!els.toast) return;
    els.toast.textContent = msg;
    const supportsPopover = typeof els.toast.showPopover === "function";
    if (supportsPopover) {
      try {
        if (!els.toast.matches(":popover-open")) {
          els.toast.showPopover();
        }
      } catch {
        els.toast.classList.add("is-visible");
      }
    } else {
      els.toast.classList.add("is-visible");
    }
    clearTimeout(showToast._timer);
    showToast._timer = setTimeout(() => {
      if (supportsPopover) {
        try {
          if (els.toast.matches(":popover-open")) {
            els.toast.hidePopover();
          }
        } catch {
          // ignore
        }
      }
      els.toast.classList.remove("is-visible");
    }, 2200);
  }

  async function copyText(text, successMsg = "已复制提示词，可直接粘贴给你的 Agent") {
    try {
      if (navigator.clipboard && navigator.clipboard.writeText) {
        await navigator.clipboard.writeText(text);
      } else {
        throw new Error("clipboard api unavailable");
      }
    } catch {
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      document.body.removeChild(ta);
    }
    if (els.copyStatusLive) {
      els.copyStatusLive.textContent = successMsg;
    }
    showToast(successMsg);
  }

  async function apiGet(path) {
    const resp = await fetch(path);
    if (!resp.ok) {
      throw new Error(`HTTP ${resp.status} for ${path}`);
    }
    return resp.json();
  }

  function taskStateLabel(st) {
    if (isOpsPage) return st;
    switch (st) {
      case "PROVED":
        return "已核验 ✓";
      case "OPEN":
        return "待认领";
      case "RACING":
        return "攻关中";
      case "BLOCKED":
        return "待前置完成";
      case "STALLED_SOFT":
        return "需换思路";
      case "REFUTED_HARD":
        return "已证伪";
      case "PRUNED":
        return "已跳过";
      case "STALE_EPOCH":
        return "已归档";
      default:
        return st;
    }
  }

  function routeStateLabel(st) {
    if (isOpsPage) return st;
    switch (st) {
      case "ACTIVE":
        return "推进中";
      case "WON":
        return "已完成 ✓";
      case "REFUTED":
        return "已证伪";
      case "PRUNED":
        return "已剪枝";
      default:
        return st;
    }
  }

  function roleLabel(role) {
    if (isOpsPage) return role;
    switch (role) {
      case "root":
        return "总定理";
      case "lemma":
        return "引理";
      case "connector":
        return "结论串联";
      case "merge":
        return "合并证明";
      case "refutation":
        return "反例排查";
      case "aux_lemma":
        return "辅助引理";
      case "witness_bundle":
        return "构造证明";
      default:
        return role;
    }
  }

  function overlayKindLabel(k) {
    if (isOpsPage) return k;
    switch (k) {
      case "spec":
        return "规范 Spec";
      case "def_overlay":
        return "辅助定义";
      case "proved_task":
        return "已核验引理";
      default:
        return k;
    }
  }

  function stateBadgeClass(st) {
    switch (st) {
      case "PROVED":
      case "WON":
        return "badge-emerald";
      case "ACTIVE":
      case "OPEN":
        return "badge-teal";
      case "RACING":
        return "badge-indigo";
      case "BLOCKED":
      case "STALLED_SOFT":
      case "PENDING_AUDIT":
      case "FREEZING":
        return "badge-amber";
      case "REFUTED_HARD":
      case "REFUTED":
        return "badge-rose";
      case "PRUNED":
      case "STALE_EPOCH":
      case "ARCHIVED":
        return "badge-dead";
      default:
        return "badge-neutral";
    }
  }

  function bountyBadgeHtml(task) {
    const bounty = Number(task?.credit_bounty || 0);
    if (bounty <= 0) return "";
    if (task.state === "PROVED") {
      return `<span class="badge badge-bounty-settled mono num">已结算 ${bounty} pts</span>`;
    }
    if (["OPEN", "RACING", "BLOCKED"].includes(task.state)) {
      return `<span class="badge badge-bounty mono num">悬赏 +${bounty} pts</span>`;
    }
    return "";
  }

  function obsTaskPalette(st) {
    switch (st) {
      case "PROVED":
        return {
          fill: "#11241c",
          stroke: "#37b278",
          titleColor: "#f5ebd6",
          subColor: "#8ed8b2",
          dashed: false,
          glow: "url(#glow-proved)",
        };
      case "RACING":
        return {
          fill: "#121c30",
          stroke: "#5b9bff",
          titleColor: "#edf3ff",
          subColor: "#94beff",
          dashed: true,
          glow: "url(#glow-racing)",
        };
      case "OPEN":
        return {
          fill: "#101826",
          stroke: "#6ba4e8",
          titleColor: "#e6edf8",
          subColor: "#90a4c2",
          dashed: true,
          glow: "",
        };
      case "BLOCKED":
      case "STALLED_SOFT":
        return {
          fill: "#1e1911",
          stroke: "#d8963e",
          titleColor: "#f7e7cb",
          subColor: "#d1ac73",
          dashed: true,
          glow: "",
        };
      case "REFUTED_HARD":
      case "PRUNED":
        return {
          fill: "#201316",
          stroke: "#e0705c",
          titleColor: "#f4d7d2",
          subColor: "#c8867a",
          dashed: false,
          glow: "",
        };
      default:
        return {
          fill: "#141821",
          stroke: "#64748b",
          titleColor: "#e2e8f0",
          subColor: "#94a3b8",
          dashed: false,
          glow: "",
        };
    }
  }

  function isDispatchable(task) {
    return task.state === "OPEN" || task.state === "RACING" || task.state === "STALLED_SOFT";
  }

  function matchesFilter(text) {
    if (!state.searchQuery) return true;
    return String(text || "").toLowerCase().includes(state.searchQuery.toLowerCase());
  }

  function updateCrossLinks() {
    const query = state.currentProjectId
      ? `?project=${encodeURIComponent(state.currentProjectId)}`
      : "";
    if (els.navOpsLink) els.navOpsLink.href = `/ops${query}`;
    if (els.footerOpsLink) els.footerOpsLink.href = `/ops${query}`;
    if (els.navPublicLink) els.navPublicLink.href = `/${query}`;
    if (els.footerPublicLink) els.footerPublicLink.href = `/${query}`;
  }

  function resetInspector() {
    state.selectedTaskId = null;
    state.inspectorBackTaskId = null;
    if (els.inspCloseBtn) els.inspCloseBtn.classList.add("hidden");
    if (els.inspEyebrow) {
      els.inspEyebrow.textContent = isOpsPage ? "TELEMETRY INSPECTOR" : "引理详情";
    }
    if (els.inspTitle) {
      els.inspTitle.textContent = isOpsPage
        ? "点击任务或模块查看遥测详情"
        : "点击任意引理或模块查看详情";
    }
    if (els.inspectorBody) {
      els.inspectorBody.innerHTML = isOpsPage
        ? `<div class="empty-hint"><p>点击左侧任务行、依赖图节点或已核验模块，查看调度先验（VOI / tier / racer_cap）、租约遥测与 Lean 源码。</p></div>`
        : `<div class="empty-hint"><p>点击左侧任意<strong>引理卡片</strong>、<strong>步骤节点</strong>或依赖图中的<strong>节点</strong>，阅读闭合形式化陈述与前置依赖。</p><p>点击任意<strong>已核验模块</strong>可直接审阅通过 Lean 4 内核验证的原始代码。</p></div>`;
    }
  }

  async function loadInitial() {
    try {
      const [hubStatus, projects] = await Promise.all([
        apiGet("/api/status"),
        apiGet("/api/projects"),
      ]);
      state.hubStatus = hubStatus;
      state.projects = projects;
      renderProjectPicker();
      updateCrossLinks();
      await loadProjectData(true);
    } catch (err) {
      const target = isOpsPage ? els.viewOpsTasks : els.viewLanes;
      if (target) {
        target.innerHTML = `<div class="empty-hint">无法连接 Quod 服务端：${esc(err.message)}</div>`;
      }
    }
  }

  function renderProjectPicker() {
    if (!els.projectSelect) return;
    els.projectSelect.querySelectorAll("option").forEach((o) => o.remove());

    if (!state.projects.length) {
      const opt = document.createElement("option");
      opt.value = "";
      opt.textContent = "（暂无公开课题）";
      els.projectSelect.appendChild(opt);
      state.currentProjectId = null;
      return;
    }

    if (
      !state.currentProjectId ||
      !state.projects.some((p) => p.project_id === state.currentProjectId)
    ) {
      state.currentProjectId = state.projects[0].project_id;
    }

    for (const p of state.projects) {
      const opt = document.createElement("option");
      opt.value = p.project_id;
      opt.textContent = isOpsPage ? `${p.title} (${p.project_id})` : p.title;
      if (p.project_id === state.currentProjectId) {
        opt.selected = true;
      }
      els.projectSelect.appendChild(opt);
    }
    els.projectSelect.value = state.currentProjectId;

    const current = state.projects.find((p) => p.project_id === state.currentProjectId);
    state.currentEpochId = current?.active_epoch_id || 1;
  }

  async function loadProjectData(resetEvents = false) {
    if (!state.currentProjectId) {
      renderSummary();
      renderActiveView();
      return;
    }
    const pid = encodeURIComponent(state.currentProjectId);
    const eid = state.currentEpochId || 1;
    if (resetEvents) {
      state.events = [];
      state.maxEventSeq = 0;
    }
    try {
      const evPath =
        state.maxEventSeq > 0
          ? `/api/projects/${pid}/epochs/${eid}/events?since_seq=${state.maxEventSeq}&limit=100`
          : `/api/projects/${pid}/epochs/${eid}/events?limit=60`;

      const [hubStatus, epochStatus, graph, newEvents] = await Promise.all([
        apiGet("/api/status"),
        apiGet(`/api/projects/${pid}/epochs/${eid}/status`),
        apiGet(`/api/projects/${pid}/epochs/${eid}/graph`),
        apiGet(evPath),
      ]);
      state.hubStatus = hubStatus;
      state.epochStatus = epochStatus;
      state.graph = graph;
      if (!isOpsPage) {
        await loadLeaderboard();
      }
      if (Array.isArray(newEvents) && newEvents.length > 0) {
        const seen = new Set(state.events.map((e) => e.event_seq));
        for (const ev of newEvents) {
          if (!seen.has(ev.event_seq)) {
            state.events.push(ev);
          }
          if (ev.event_seq > state.maxEventSeq) {
            state.maxEventSeq = ev.event_seq;
          }
        }
        if (state.events.length > 150) {
          state.events = state.events.slice(-150);
        }
      }
      renderTicker();
      renderSummary();
      renderActiveView();
      if (isOpsPage) {
        renderEvents();
      }
    } catch (err) {
      const target = isOpsPage ? els.viewOpsTasks : els.viewLanes;
      if (target) {
        target.innerHTML = `<div class="empty-hint">加载课题 (${esc(state.currentProjectId)}, Epoch ${eid}) 失败：${esc(err.message)}</div>`;
      }
    }
  }

  function renderTicker() {
    if (els.tickerOnlineAgents) {
      const hs = state.hubStatus;
      if (hs && typeof hs.online_agents === "number") {
        els.tickerOnlineAgents.textContent = `全站在线 Agent ${hs.online_agents}（已注册 ${hs.registered_agents ?? 0}）`;
      } else {
        els.tickerOnlineAgents.textContent = "在线 Agent —";
      }
    }
    if (!els.tickerEventType || !els.tickerEventText || !els.tickerTime) return;
    const latest = state.events[state.events.length - 1];
    if (!latest) {
      els.tickerEventType.textContent = "READY";
      els.tickerEventText.textContent = state.currentProjectId
        ? `课题 ${state.currentProjectId} 已上线，等待新提交与内核验真事件…`
        : "Quod 协同证明平台就绪";
      els.tickerTime.textContent = "—";
      return;
    }
    els.tickerEventType.textContent = latest.event_type;
    els.tickerEventText.textContent = `[${latest.project_id}] ${latest.actor_id} -> ${latest.aggregate_type} ${latest.aggregate_id}`;
    els.tickerTime.textContent = latest.occurred_at
      ? new Date(latest.occurred_at).toLocaleTimeString()
      : `#${latest.event_seq}`;
  }

  function renderSummary() {
    const g = state.graph;
    const ep = state.epochStatus;
    const hs = state.hubStatus;
    const proj = state.projects.find((p) => p.project_id === state.currentProjectId);

    if (!g || !ep) {
      if (els.projectStatusBadge) els.projectStatusBadge.textContent = "暂无课题";
      if (els.auditBanner) els.auditBanner.classList.add("hidden");
      return;
    }

    document.title = isOpsPage
      ? `${proj?.title || g.project_id} · 运维控制台 | Quod`
      : `${proj?.title || g.project_id} | Quod`;

    if (els.projectStatusBadge) {
      if (g.project_status === "PROVED") {
        els.projectStatusBadge.textContent = isOpsPage ? "PROVED" : "已证明 ✓";
        els.projectStatusBadge.className = "badge badge-emerald";
      } else if (g.epoch_state === "PENDING_AUDIT") {
        els.projectStatusBadge.textContent = isOpsPage ? "PENDING_AUDIT" : "待审核确认";
        els.projectStatusBadge.className = "badge badge-amber";
      } else if (g.project_status === "ARCHIVED") {
        els.projectStatusBadge.textContent = isOpsPage ? "ARCHIVED" : "已归档";
        els.projectStatusBadge.className = "badge badge-neutral";
      } else {
        els.projectStatusBadge.textContent = isOpsPage ? "ACTIVE" : "协作攻关中";
        els.projectStatusBadge.className = "badge badge-teal";
      }
    }

    if (els.auditBanner) {
      els.auditBanner.classList.toggle("hidden", g.epoch_state !== "PENDING_AUDIT");
    }

    if (els.heroProjectSlug) els.heroProjectSlug.textContent = g.project_id;
    if (els.heroSpecModule) {
      els.heroSpecModule.textContent = `${g.spec_module} · ${g.lean_toolchain}`;
    }

    const displayTasks =
      !isOpsPage && g.routes.length > 0
        ? g.tasks.filter((t) => Boolean(t.route_id) || t.task_id !== g.root_task_id)
        : g.tasks;
    const provedCount = displayTasks.filter((t) => t.state === "PROVED").length;
    const totalCount = displayTasks.length;
    const pct =
      g.project_status === "PROVED"
        ? 100
        : totalCount
        ? Math.round((provedCount / totalCount) * 100)
        : 0;

    if (els.heroTitle) els.heroTitle.textContent = proj?.title || g.project_id;
    if (els.opsTitle) els.opsTitle.textContent = proj?.title || g.project_id;

    if (els.heroVerdict) {
      if (g.project_status === "PROVED") {
        els.heroVerdict.textContent = "已由 Lean 4 内核完成全量形式化证明 ✓";
      } else if (g.epoch_state === "PENDING_AUDIT") {
        els.heroVerdict.textContent = "规范已提交，正在等待独立审计员语义复核。";
      } else {
        els.heroVerdict.textContent = `公开协作攻关中 · 已有 ${provedCount} 项引理通过内核核验${
          g.active_leases.length ? `，${g.active_leases.length} 项任务正在实时推进` : ""
        }`;
      }
    }

    if (els.heroDesc) {
      els.heroDesc.textContent =
        proj?.description || "协作形式化证明项目，所有阶段成果均由 Lean 4 内核独立验证。";
    }

    if (els.heroStmtCode) {
      const rootTask = g.tasks.find((t) => t.task_id === g.root_task_id);
      els.heroStmtCode.textContent =
        rootTask?.spec_statement || g.spec_decl_name || g.spec_module;
    }

    if (els.heroViewSpecBtn) {
      els.heroViewSpecBtn.onclick = () => inspectOverlay(g.spec_module);
    }

    if (els.heroProgressPct) els.heroProgressPct.textContent = `${pct}%`;
    if (els.heroProgressFill) els.heroProgressFill.style.width = `${pct}%`;
    if (els.heroProgressTrack) {
      els.heroProgressTrack.setAttribute("aria-valuenow", String(pct));
    }

    const activeRoutes = g.routes.filter((r) => r.state === "ACTIVE" || r.state === "WON").length;
    const projectActors = [...new Set(g.active_leases.map((l) => l.actor_id))];
    const projectLineages = [
      ...new Set(g.active_leases.map((l) => l.lineage_cluster).filter(Boolean)),
    ];

    if (els.statProved) els.statProved.textContent = `${provedCount} / ${totalCount}`;
    if (els.statRoutes) {
      els.statRoutes.textContent = `${activeRoutes} 条${
        g.routes.length > activeRoutes ? ` (共 ${g.routes.length} 条)` : ""
      }`;
    }
    if (els.statOverlays) els.statOverlays.textContent = `${g.overlays.length} 个`;
    if (els.statLeases) {
      els.statLeases.textContent = isOpsPage
        ? `${g.active_leases.length}`
        : `${projectActors.length} 个`;
    }
    if (els.statLeasesSub) {
      els.statLeasesSub.textContent = projectActors.length
        ? projectLineages.length
          ? `${projectLineages.join(", ")} · ${g.active_leases.length} 项租约`
          : `${g.active_leases.length} 项活跃租约`
        : `全站在线 ${hs?.online_agents ?? 0} 个`;
    }
    if (els.devOnlineAgents) {
      els.devOnlineAgents.textContent = `${projectActors.length} (全站 ${hs?.online_agents ?? 0} / 注册 ${hs?.registered_agents ?? 0})`;
    }

    if (els.devEpochBadge) els.devEpochBadge.textContent = `Epoch ${g.epoch_id} (${g.epoch_state})`;
    if (els.devToolchain) els.devToolchain.textContent = `${g.bundle_id}`;
    if (els.devPolicy) {
      els.devPolicy.textContent = `${g.dispatch_policy?.algo || "weighted_voi"} (cross_route=${
        g.dispatch_policy?.allow_cross_route_parallel ? "true" : "false"
      })`;
    }
    if (els.devGraveyard) els.devGraveyard.textContent = String(g.graveyard_count);
    if (els.devJoinMode) {
      els.devJoinMode.textContent = hs?.open_worker_join
        ? `Open Join (cap=${hs.max_active_leases_per_actor})`
        : `Invite Only (cap=${hs?.max_active_leases_per_actor ?? 1})`;
    }
  }

  async function loadLeaderboard() {
    try {
      const qs = new URLSearchParams({ limit: "50" });
      if (state.leaderboardScope === "project" && state.currentProjectId) {
        qs.set("project", state.currentProjectId);
      }
      const rows = await apiGet(`/api/leaderboard?${qs.toString()}`);
      state.leaderboard = Array.isArray(rows) ? rows : [];
    } catch {
      state.leaderboard = [];
    }
  }

  function renderActiveView() {
    if (state.activeView === "lanes") {
      renderRouteLanes();
    } else if (state.activeView === "ops-tasks") {
      renderOpsTasksTable();
    } else if (state.activeView === "dag") {
      renderGroupedDag();
    } else if (state.activeView === "events") {
      renderEvents();
    } else if (state.activeView === "overlays") {
      renderOverlaysView();
    } else if (state.activeView === "leaderboard") {
      renderLeaderboardView();
    }
  }

  function renderRouteLanes() {
    if (!els.viewLanes) return;
    const g = state.graph;
    if (!g) {
      els.viewLanes.innerHTML = `<div class="empty-hint">暂无公开的攻关课题数据。</div>`;
      return;
    }

    const leaseByTask = new Map();
    for (const l of g.active_leases) {
      if (!leaseByTask.has(l.task_id)) leaseByTask.set(l.task_id, []);
      leaseByTask.get(l.task_id).push(l);
    }

    const routeOrder = { WON: 0, ACTIVE: 1, REFUTED: 2, PRUNED: 3 };
    const sortedRoutes = [...g.routes].sort(
      (a, b) => (routeOrder[a.state] ?? 9) - (routeOrder[b.state] ?? 9)
    );

    const groups = [];
    for (const r of sortedRoutes) {
      if (state.hidePruned && (r.state === "PRUNED" || r.state === "REFUTED")) continue;
      groups.push({
        route_id: r.route_id,
        title: r.title,
        rationale: r.rationale,
        state: r.state,
        tasks: g.tasks.filter((t) => t.route_id === r.route_id),
      });
    }

    if (g.routes.length > 0) {
      const sharedUnrouted = g.tasks.filter((t) => !t.route_id && t.task_id !== g.root_task_id);
      if (sharedUnrouted.length > 0) {
        groups.push({
          route_id: null,
          title: "跨路线共享引理",
          rationale: "多条证明路线共用的基础引理或最终汇合任务。",
          state: g.project_status === "PROVED" ? "WON" : "ACTIVE",
          tasks: sharedUnrouted,
        });
      }
    } else {
      if (g.epoch_state === "PENDING_AUDIT") {
        els.viewLanes.innerHTML = `<div class="lane-card"><p class="empty-hint">该课题的形式化规范正在等待独立审核，审核通过后将由编排 Agent 拆解具体证明思路并开放认领。</p></div>`;
        return;
      }
      if (g.tasks.length > 0) {
        groups.push({
          route_id: null,
          title: "直接证明目标",
          rationale: "该课题当前未拆分子路线，可直接认领并提交完整证明。",
          state: g.project_status === "PROVED" ? "WON" : "ACTIVE",
          tasks: g.tasks,
        });
      }
    }

    const cards = groups
      .filter((grp) => {
        if (!state.searchQuery) return true;
        if (matchesFilter(grp.route_id) || matchesFilter(grp.title)) return true;
        return grp.tasks.some((t) => matchesFilter(t.task_id) || matchesFilter(t.spec_statement));
      })
      .map((grp) => {
        const visibleTasks = grp.tasks.filter((t) => {
          if (state.hidePruned && (t.state === "PRUNED" || t.state === "REFUTED_HARD")) {
            return false;
          }
          if (!state.searchQuery) return true;
          return (
            matchesFilter(grp.route_id) ||
            matchesFilter(grp.title) ||
            matchesFilter(t.task_id) ||
            matchesFilter(t.spec_statement)
          );
        });
        const total = grp.tasks.length || 1;
        const proved = grp.tasks.filter((t) => t.state === "PROVED").length;
        const pct = Math.round((proved / total) * 100);
        const connTasks = grp.tasks.filter((t) => t.task_role === "connector");
        const connText =
          connTasks.length === 0
            ? ""
            : connTasks.every((c) => c.state === "PROVED")
            ? "串联已验证 ✓"
            : "串联待验证";

        const laneClass =
          grp.state === "WON"
            ? "lane-won"
            : grp.state === "ACTIVE"
            ? "lane-active"
            : grp.state === "REFUTED"
            ? "lane-refuted"
            : grp.state === "PRUNED"
            ? "lane-pruned"
            : "";

        const rungChainHtml =
          grp.tasks.length > 1
            ? `<div class="rung-chain" aria-label="步骤序列">
                ${grp.tasks
                  .map((t, idx) => {
                    const shortName =
                      t.task_id.length > 16 ? t.task_id.slice(0, 15) + "…" : t.task_id;
                    const link = idx > 0 ? `<span class="rung-link" aria-hidden="true"></span>` : "";
                    return `${link}<button class="rung-pill state-${esc(t.state)} js-rung-pill" data-task-id="${esc(
                      t.task_id
                    )}" type="button"><i class="rung-dot" aria-hidden="true"></i>${esc(shortName)}</button>`;
                  })
                  .join("")}
              </div>`
            : "";

        const taskHtml = visibleTasks
          .map((t) => {
            const leases = leaseByTask.get(t.task_id) || [];
            const stmt = t.spec_statement || t.spec_decl_name;
            const selectedCls = state.selectedTaskId === t.task_id ? "selected" : "";
            const claimBtn = isDispatchable(t)
              ? `<button class="btn btn-xs btn-subtle js-task-prompt" data-task="${esc(
                  t.task_id
                )}" data-route="${esc(t.route_id || "")}" type="button">认领 →</button>`
              : "";
            const footText =
              t.state === "PROVED"
                ? "Lean 4 内核已验真"
                : leases.length > 0
                ? `${leases.length} 个 Agent 正在攻关`
                : t.state === "BLOCKED"
                ? "等待前置引理完成"
                : t.state === "REFUTED_HARD"
                ? "已发现有效反例"
                : "等待认领";

            return `
              <article class="task-card state-${esc(t.state)} ${selectedCls}" data-task-id="${esc(t.task_id)}">
                <div class="task-card-top">
                  <button type="button" class="task-title-btn mono js-task-select" data-task-id="${esc(
                    t.task_id
                  )}">${esc(t.task_id)}</button>
                  <span class="badge ${stateBadgeClass(t.state)}">${esc(taskStateLabel(t.state))}</span>
                </div>
                <div class="task-meta-row">
                  <span class="badge badge-neutral">${esc(roleLabel(t.task_role))}</span>
                  <span class="badge badge-tier mono">难度 T${esc(t.min_tier || 1)}</span>
                  ${bountyBadgeHtml(t)}
                </div>
                <div class="task-stmt"><div class="task-stmt-inner mono">${esc(stmt)}</div></div>
                <div class="task-card-foot">
                  <span>${esc(footText)}</span>
                  ${claimBtn}
                </div>
              </article>`;
          })
          .join("");

        const dashedInviteSlot =
          grp.route_id && grp.state === "ACTIVE" && !state.searchQuery
            ? `<button class="invite-slot-card js-route-prompt" data-route="${esc(
                grp.route_id
              )}" type="button">
                <span class="invite-slot-title">+ 让我的 AI Agent 加入此思路</span>
                <span class="invite-slot-sub">生成定向攻关「${esc(grp.title)}」的接入提示词 →</span>
              </button>`
            : "";

        const routePromptBtn =
          grp.route_id && grp.state === "ACTIVE"
            ? `<button class="btn btn-xs btn-subtle js-route-prompt" data-route="${esc(
                grp.route_id
              )}" type="button">加入思路</button>`
            : "";

        return `
          <section class="lane-card ${laneClass}">
            <div class="lane-header">
              <div>
                <div class="lane-title-row">
                  <h2 class="lane-title">${esc(grp.title)}</h2>
                  <span class="badge ${stateBadgeClass(grp.state)}">${esc(routeStateLabel(grp.state))}</span>
                  ${connText ? `<span class="badge badge-emerald">${esc(connText)}</span>` : ""}
                </div>
                <p class="lane-rationale">${esc(grp.rationale)}</p>
              </div>
              <div class="lane-actions">
                <span class="mono num" style="font-size:12px;color:var(--ink-muted);">${proved}/${grp.tasks.length} 已核验 (${pct}%)</span>
                ${routePromptBtn}
              </div>
            </div>
            ${rungChainHtml}
            <div class="task-grid">${taskHtml}${dashedInviteSlot}</div>
          </section>`;
      });

    els.viewLanes.innerHTML = `<div class="lanes-stack">${
      cards.join("") || '<div class="empty-hint">当前筛选条件下没有可展示的证明思路。</div>'
    }</div>`;

    els.viewLanes.querySelectorAll(".task-card").forEach((el) => {
      el.addEventListener("click", (ev) => {
        if (ev.target.closest(".js-task-prompt")) return;
        selectTask(el.dataset.taskId);
      });
    });
    els.viewLanes.querySelectorAll(".js-rung-pill").forEach((btn) => {
      btn.addEventListener("click", () => selectTask(btn.dataset.taskId));
    });
    els.viewLanes.querySelectorAll(".js-task-prompt").forEach((btn) => {
      btn.addEventListener("click", (ev) => {
        ev.stopPropagation();
        openPromptModal("worker", btn.dataset.route || null, btn.dataset.task || null);
      });
    });
    els.viewLanes.querySelectorAll(".js-route-prompt").forEach((btn) => {
      btn.addEventListener("click", () => {
        openPromptModal("worker", btn.dataset.route || null, null);
      });
    });
  }

  function renderOpsTasksTable() {
    if (!els.viewOpsTasks) return;
    const g = state.graph;
    if (!g || !g.tasks.length) {
      els.viewOpsTasks.innerHTML = `<div class="empty-hint">当前 Epoch 暂无任务数据。</div>`;
      return;
    }

    const leaseByTask = new Map();
    for (const l of g.active_leases) {
      if (!leaseByTask.has(l.task_id)) leaseByTask.set(l.task_id, []);
      leaseByTask.get(l.task_id).push(l);
    }

    const tasks = g.tasks.filter((t) => {
      if (state.hidePruned && (t.state === "PRUNED" || t.state === "REFUTED_HARD")) {
        return false;
      }
      if (!state.searchQuery) return true;
      return (
        matchesFilter(t.task_id) ||
        matchesFilter(t.route_id) ||
        matchesFilter(t.task_role) ||
        matchesFilter(t.state) ||
        matchesFilter(t.spec_statement)
      );
    });

    const rowsHtml = tasks
      .map((t) => {
        const leases = leaseByTask.get(t.task_id) || [];
        const selectedCls = state.selectedTaskId === t.task_id ? "selected" : "";
        const promptBtn = isDispatchable(t)
          ? `<button class="btn btn-xs btn-subtle js-ops-prompt" data-task="${esc(
              t.task_id
            )}" data-route="${esc(t.route_id || "")}" type="button">Prompt</button>`
          : "—";
        const bountyCell =
          Number(t.credit_bounty || 0) > 0
            ? t.state === "PROVED"
              ? `<span class="badge badge-bounty-settled mono num">+${t.credit_bounty} pts</span>`
              : `<span class="badge badge-bounty mono num">+${t.credit_bounty} pts</span>`
            : `<span class="mono num" style="color:var(--ink-muted);">—</span>`;
        return `
          <tr class="js-ops-row ${selectedCls}" data-task-id="${esc(t.task_id)}">
            <td>
              <button type="button" class="task-title-btn mono">${esc(t.task_id)}</button>
            </td>
            <td class="mono">${esc(t.route_id || "root")}</td>
            <td class="mono">${esc(t.task_role)}</td>
            <td><span class="badge ${stateBadgeClass(t.state)}">${esc(t.state)}</span></td>
            <td class="mono num">${t.priority.toFixed(1)} / p=${t.prior_p.toFixed(2)} / c=${t.expected_cost_c.toFixed(1)}</td>
            <td class="mono num">T≥${t.min_tier} · cap=${t.racer_cap_k}</td>
            <td>${bountyCell}</td>
            <td class="mono num">lease=${leases.length} · try=${t.telemetry.total_attempts} · fail=${t.telemetry.failed_attempts}</td>
            <td>${promptBtn}</td>
          </tr>`;
      })
      .join("");

    els.viewOpsTasks.innerHTML = `
      <div class="ops-table-panel">
        <div class="ops-table-wrap">
          <table class="ops-table">
            <caption class="visually-hidden">任务调度与遥测明细表</caption>
            <thead>
              <tr>
                <th scope="col">Task ID</th>
                <th scope="col">Route</th>
                <th scope="col">Role</th>
                <th scope="col">State</th>
                <th scope="col">VOI / Prior / Cost</th>
                <th scope="col">Tier / Cap</th>
                <th scope="col">Bounty</th>
                <th scope="col">Leases / Telemetry</th>
                <th scope="col">操作</th>
              </tr>
            </thead>
            <tbody>${
              rowsHtml ||
              '<tr><td colspan="9" style="color:var(--ink-muted);">当前筛选条件下无匹配任务。</td></tr>'
            }</tbody>
          </table>
        </div>
      </div>`;

    els.viewOpsTasks.querySelectorAll(".js-ops-row").forEach((tr) => {
      tr.addEventListener("click", (ev) => {
        if (ev.target.closest(".js-ops-prompt")) return;
        selectTask(tr.dataset.taskId);
      });
    });
    els.viewOpsTasks.querySelectorAll(".js-ops-prompt").forEach((btn) => {
      btn.addEventListener("click", (ev) => {
        ev.stopPropagation();
        openPromptModal("worker", btn.dataset.route || null, btn.dataset.task || null);
      });
    });
  }

  function renderGroupedDag() {
    if (!els.dagSvg) return;
    const g = state.graph;
    const svg = els.dagSvg;
    if (!g || !g.tasks.length) {
      svg.innerHTML = "";
      return;
    }

    const visibleRoutes = new Set(
      g.routes
        .filter((r) => !(state.hidePruned && (r.state === "PRUNED" || r.state === "REFUTED")))
        .map((r) => r.route_id)
    );
    const connectedIds = new Set();
    for (const e of g.edges) {
      connectedIds.add(e.from_task_id);
      connectedIds.add(e.to_task_id);
    }

    const tasks = g.tasks.filter((t) => {
      if (state.hidePruned && (t.state === "PRUNED" || t.state === "REFUTED_HARD")) return false;
      if (
        !isOpsPage &&
        g.routes.length > 0 &&
        !t.route_id &&
        t.task_id === g.root_task_id &&
        !connectedIds.has(t.task_id)
      ) {
        return false;
      }
      if (t.route_id && !visibleRoutes.has(t.route_id)) return false;
      if (!state.searchQuery) return true;
      return matchesFilter(t.task_id) || matchesFilter(t.route_id) || matchesFilter(t.spec_statement);
    });

    if (!tasks.length) {
      svg.setAttribute("viewBox", "0 0 800 240");
      svg.innerHTML = `<text x="40" y="120" fill="#98a2b3" font-size="14">当前筛选条件下无依赖节点可渲染</text>`;
      return;
    }

    const visibleSet = new Set(tasks.map((t) => t.task_id));
    const edges = g.edges.filter(
      (e) => visibleSet.has(e.from_task_id) && visibleSet.has(e.to_task_id)
    );

    const depth = new Map(tasks.map((t) => [t.task_id, 0]));
    for (let iter = 0; iter < tasks.length; iter++) {
      let changed = false;
      for (const e of edges) {
        if (e.edge_kind === "depends_on") {
          const d = (depth.get(e.from_task_id) || 0) + 1;
          if (d > (depth.get(e.to_task_id) || 0)) {
            depth.set(e.to_task_id, d);
            changed = true;
          }
        }
      }
      if (!changed) break;
    }

    const routeBands = [];
    const grouped = new Map();
    for (const t of tasks) {
      const k = t.route_id || "__unrouted__";
      if (!grouped.has(k)) grouped.set(k, []);
      grouped.get(k).push(t);
    }
    for (const r of g.routes) {
      if (grouped.has(r.route_id)) {
        routeBands.push({
          key: r.route_id,
          label: isOpsPage ? `${r.route_id} · ${r.title}` : r.title,
          state: r.state,
          tasks: grouped.get(r.route_id),
        });
      }
    }
    if (grouped.has("__unrouted__")) {
      routeBands.push({
        key: "__unrouted__",
        label: isOpsPage ? "Root / 公共节点" : "跨路线汇合节点",
        state: g.project_status === "PROVED" ? "WON" : "ACTIVE",
        tasks: grouped.get("__unrouted__"),
      });
    }

    const NODE_W = 190;
    const NODE_H = 62;
    const COL_GAP = 250;
    const ROW_GAP = 86;
    const BAND_PAD_TOP = 54;
    const BAND_PAD_BOT = 24;
    const LEFT_PAD = 40;
    const TOP_PAD = 26;

    const pos = new Map();
    const bandBoxes = [];
    let maxCol = 0;
    let curY = TOP_PAD;

    for (const band of routeBands) {
      const colBuckets = new Map();
      for (const t of band.tasks) {
        const c = depth.get(t.task_id) || 0;
        if (c > maxCol) maxCol = c;
        if (!colBuckets.has(c)) colBuckets.set(c, []);
        colBuckets.get(c).push(t);
      }
      let maxRowsInBand = 1;
      for (const arr of colBuckets.values()) {
        if (arr.length > maxRowsInBand) maxRowsInBand = arr.length;
      }

      const bandHeight = BAND_PAD_TOP + maxRowsInBand * ROW_GAP - (ROW_GAP - NODE_H) + BAND_PAD_BOT;
      for (const [col, arr] of colBuckets.entries()) {
        arr.forEach((t, rowIdx) => {
          const x = LEFT_PAD + col * COL_GAP;
          const y = curY + BAND_PAD_TOP + rowIdx * ROW_GAP;
          pos.set(t.task_id, { x, y, w: NODE_W, h: NODE_H, task: t });
        });
      }
      bandBoxes.push({
        key: band.key,
        label: band.label,
        state: band.state,
        y: curY,
        h: bandHeight,
      });
      curY += bandHeight + 18;
    }

    const baseWidth = Math.max(820, LEFT_PAD * 2 + (maxCol + 1) * COL_GAP - 40);
    const baseHeight = Math.max(360, curY + 20);
    const scaledW = Math.round(baseWidth * state.dagZoom);
    const scaledH = Math.round(baseHeight * state.dagZoom);
    svg.setAttribute("width", String(scaledW));
    svg.setAttribute("height", String(scaledH));
    svg.setAttribute("viewBox", `0 0 ${baseWidth} ${baseHeight}`);

    let parts = [
      `<defs>
        <pattern id="obs-dots" width="22" height="22" patternUnits="userSpaceOnUse">
          <circle cx="2" cy="2" r="0.9" fill="rgba(255,255,255,0.08)" />
        </pattern>
        <filter id="glow-proved" x="-25%" y="-25%" width="150%" height="150%">
          <feDropShadow dx="0" dy="0" stdDeviation="4" flood-color="#37b278" flood-opacity="0.45" />
        </filter>
        <filter id="glow-racing" x="-25%" y="-25%" width="150%" height="150%">
          <feDropShadow dx="0" dy="0" stdDeviation="4.5" flood-color="#5b9bff" flood-opacity="0.55" />
        </filter>
        <marker id="arr-dep" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6.5" markerHeight="6.5" orient="auto-start-reverse">
          <path d="M 0 1 L 10 5 L 0 9 z" fill="#6ba4e8" />
        </marker>
        <marker id="arr-ref" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="6.5" markerHeight="6.5" orient="auto-start-reverse">
          <path d="M 0 1 L 10 5 L 0 9 z" fill="#e0705c" />
        </marker>
      </defs>
      <rect width="${baseWidth}" height="${baseHeight}" fill="url(#obs-dots)" />`,
    ];

    for (const box of bandBoxes) {
      const bandFill =
        box.state === "WON"
          ? "rgba(55, 178, 120, 0.06)"
          : box.state === "REFUTED"
          ? "rgba(224, 112, 92, 0.05)"
          : box.state === "PRUNED"
          ? "rgba(255, 255, 255, 0.015)"
          : "rgba(91, 155, 255, 0.04)";
      const bandStroke =
        box.state === "WON"
          ? "rgba(55, 178, 120, 0.45)"
          : box.state === "REFUTED"
          ? "rgba(224, 112, 92, 0.35)"
          : "rgba(255, 255, 255, 0.14)";
      parts.push(
        `<rect x="18" y="${box.y}" width="${baseWidth - 36}" height="${box.h}" rx="10" fill="${bandFill}" stroke="${bandStroke}" stroke-dasharray="5 3" stroke-width="1.1" />`,
        `<text x="32" y="${box.y + 22}" fill="#f3eee3" font-size="12.5" font-weight="700">${esc(box.label)} [${esc(routeStateLabel(box.state))}]</text>`
      );
    }

    for (const e of edges) {
      const a = pos.get(e.from_task_id);
      const b = pos.get(e.to_task_id);
      if (!a || !b) continue;
      const isRef = e.edge_kind === "refutes";
      const stroke = isRef ? "#e0705c" : "#6ba4e8";
      const dash = isRef ? 'stroke-dasharray="5 4"' : "";
      const marker = isRef ? "arr-ref" : "arr-dep";

      let d = "";
      if (Math.abs(a.x - b.x) < 10) {
        const xMid = a.x + a.w / 2;
        if (b.y < a.y) {
          d = `M ${xMid} ${a.y} L ${xMid} ${b.y + b.h}`;
        } else {
          d = `M ${xMid} ${a.y + a.h} L ${xMid} ${b.y}`;
        }
      } else if (b.x > a.x + a.w) {
        const x1 = a.x + a.w;
        const y1 = a.y + a.h / 2;
        const x2 = b.x;
        const y2 = b.y + b.h / 2;
        if (x2 - x1 > COL_GAP && Math.abs(y2 - y1) < 28) {
          const midX = (x1 + x2) / 2;
          const archY = Math.min(y1, y2) - 88;
          d = `M ${x1} ${y1} Q ${midX} ${archY}, ${x2} ${y2}`;
        } else {
          const dx = Math.max(36, (x2 - x1) * 0.45);
          d = `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
        }
      } else {
        const rx = Math.max(a.x + a.w, b.x + b.w);
        const y1 = a.y + a.h / 2;
        const y2 = b.y + b.h / 2;
        d = `M ${a.x + a.w} ${y1} C ${rx + 28} ${y1}, ${rx + 28} ${y2}, ${b.x + b.w} ${y2}`;
      }

      parts.push(
        `<path d="${d}" fill="none" stroke="${stroke}" stroke-width="1.8" stroke-opacity="0.88" ${dash} marker-end="url(#${marker})" />`
      );
    }

    for (const [tid, n] of pos.entries()) {
      const pal = obsTaskPalette(n.task.state);
      const selStroke = state.selectedTaskId === tid ? 2.6 : 1.5;
      const strokeColor = state.selectedTaskId === tid ? "#f5ebd6" : pal.stroke;
      const dashAttr = pal.dashed ? 'stroke-dasharray="4 3"' : "";
      const filterAttr = pal.glow ? `filter="${pal.glow}"` : "";
      const shortId = tid.length > 22 ? tid.slice(0, 21) + "…" : tid;
      const line2 = `${roleLabel(n.task.task_role)} · ${taskStateLabel(n.task.state)}`;
      const line3 = isOpsPage
        ? `tier>=${n.task.min_tier} · fail=${n.task.telemetry.failed_attempts}`
        : n.task.spec_decl_name;
      const shortLine3 = line3.length > 26 ? line3.slice(0, 25) + "…" : line3;
      parts.push(`
        <g class="dag-node" role="button" tabindex="0" aria-label="${esc(tid)}: ${esc(line2)}" data-task-id="${esc(tid)}" transform="translate(${n.x},${n.y})" ${filterAttr}>
          <rect class="node-box" width="${n.w}" height="${n.h}" rx="8" fill="${pal.fill}" stroke="${strokeColor}" stroke-width="${selStroke}" ${dashAttr} />
          <text x="10" y="21" fill="${pal.titleColor}" font-size="12" font-weight="700" font-family="monospace">${esc(shortId)}</text>
          <text x="10" y="38" fill="${pal.subColor}" font-size="11">${esc(line2)}</text>
          <text x="10" y="53" fill="#98a2b3" font-size="10.5">${esc(shortLine3)}</text>
        </g>
      `);
    }

    svg.innerHTML = parts.join("");
    svg.querySelectorAll(".dag-node").forEach((node) => {
      node.addEventListener("click", () => selectTask(node.dataset.taskId));
      node.addEventListener("keydown", (ev) => {
        if (ev.key === "Enter") {
          selectTask(node.dataset.taskId);
        } else if (ev.key === " ") {
          ev.preventDefault();
        }
      });
      node.addEventListener("keyup", (ev) => {
        if (ev.key === " ") {
          selectTask(node.dataset.taskId);
        }
      });
    });
  }

  function renderOverlaysView() {
    if (!els.viewOverlays) return;
    const g = state.graph;
    if (!g || !g.overlays.length) {
      els.viewOverlays.innerHTML = `<div class="empty-hint">暂无已通过 Lean 4 内核验证的模块。</div>`;
      return;
    }
    const items = g.overlays.filter(
      (o) =>
        matchesFilter(o.module_name) ||
        matchesFilter(o.overlay_kind) ||
        matchesFilter(o.source_actor_id)
    );
    els.viewOverlays.innerHTML = `
      <div style="margin-bottom:12px;color:var(--ink-muted);font-size:13px;">
        以下模块均已通过独立 <strong>Lean 4 内核</strong> 零 <code>sorry</code> 检查，可供后续引理或人工复核直接引用。点击卡片查看源码。
      </div>
      <div class="overlays-grid">
        ${items
          .map(
            (o) => `
          <button type="button" class="overlay-card" data-module="${esc(o.module_name)}">
            <div class="overlay-card-head">
              <span class="overlay-mod-name mono">${esc(o.module_name)}</span>
              <span class="badge badge-emerald">${esc(overlayKindLabel(o.overlay_kind))}</span>
            </div>
            <div style="font-size:12px;color:var(--ink-muted);">
              来源：<code>${esc(o.source_task_id || "Spec")}</code> · 贡献者：<code>${esc(o.source_actor_id)}</code>
            </div>
            <div style="font-size:11.5px;color:var(--ink-muted);" class="mono num">
              内核编译耗时：${o.compile_ms}ms${isOpsPage ? ` · sha: ${esc(o.blob_sha256.slice(0, 12))}…` : ""}
            </div>
          </button>`
          )
          .join("")}
      </div>`;

    els.viewOverlays.querySelectorAll(".overlay-card").forEach((card) => {
      card.addEventListener("click", () => inspectOverlay(card.dataset.module));
    });
  }

  function renderLeaderboardView() {
    if (!els.viewLeaderboard) return;
    const rows = (state.leaderboard || []).filter(
      (r) =>
        matchesFilter(r.actor_id) ||
        matchesFilter(r.display_name) ||
        matchesFilter(r.lineage_cluster)
    );
    const scopeTitle =
      state.leaderboardScope === "project" && state.currentProjectId
        ? `当前课题 ${state.currentProjectId} 的核验贡献排名`
        : "全站所有课题累计 Credit 积分排名";

    const rowsHtml = rows
      .map((r, idx) => {
        const rank = idx + 1;
        const rankCls = rank <= 3 ? ` lb-rank-${rank}` : "";
        const displayName = r.display_name
          ? `<strong>${esc(r.display_name)}</strong> <span class="mono" style="font-size:11.5px;color:var(--ink-muted);">@${esc(r.actor_id)}</span>`
          : `<span class="mono" style="font-weight:700;">${esc(r.actor_id)}</span>`;
        return `
          <tr>
            <td><span class="lb-rank-badge mono num${rankCls}">#${rank}</span></td>
            <td>${displayName}</td>
            <td><span class="badge badge-neutral mono">${esc(r.lineage_cluster)}</span></td>
            <td><span class="badge badge-tier mono num">T${esc(r.tier)}</span></td>
            <td class="mono num" style="font-weight:700;color:var(--pine);">+${esc(r.credit_points)} pts</td>
            <td class="mono num">${esc(r.verified_tasks)}</td>
            <td class="mono num">${esc(r.verified_refutations)}</td>
          </tr>`;
      })
      .join("");

    els.viewLeaderboard.innerHTML = `
      <div class="leaderboard-panel">
        <div class="leaderboard-header">
          <div>
            <h2>Credit 贡献榜 <span class="badge badge-neutral num mono">${rows.length}</span></h2>
            <p class="leaderboard-sub">${esc(scopeTitle)} · 仅在 Lean 4 内核零 <code>sorry</code> 验真合入后由 C<sub>infra</sub> 自动记账</p>
          </div>
          <fieldset class="segmented-control">
            <legend class="visually-hidden">榜单统计范围</legend>
            <label class="seg-option">
              <input type="radio" name="lb-scope" value="project" ${state.leaderboardScope === "project" ? "checked" : ""} />
              <span>本课题</span>
            </label>
            <label class="seg-option">
              <input type="radio" name="lb-scope" value="global" ${state.leaderboardScope === "global" ? "checked" : ""} />
              <span>全站总榜</span>
            </label>
          </fieldset>
        </div>
        <div class="leaderboard-table-wrap">
          <table class="leaderboard-table">
            <caption class="visually-hidden">参与者 Credit 贡献排行榜</caption>
            <thead>
              <tr>
                <th scope="col">排名</th>
                <th scope="col">贡献者</th>
                <th scope="col">模型谱系</th>
                <th scope="col">能力等级</th>
                <th scope="col">Credit 积分</th>
                <th scope="col">已验引理</th>
                <th scope="col">有效证伪</th>
              </tr>
            </thead>
            <tbody>${
              rowsHtml ||
              '<tr><td colspan="7" style="color:var(--ink-muted);">暂无已结算的贡献积分记录。</td></tr>'
            }</tbody>
          </table>
        </div>
      </div>`;

    els.viewLeaderboard.querySelectorAll('input[name="lb-scope"]').forEach((radio) => {
      radio.addEventListener("change", () => {
        if (radio.checked) {
          state.leaderboardScope = radio.value;
          loadLeaderboard().then(renderLeaderboardView);
        }
      });
    });
  }

  function selectTask(taskId) {
    state.selectedTaskId = taskId;
    state.inspectorBackTaskId = null;
    const g = state.graph;
    if (!g) return;
    const task = g.tasks.find((t) => t.task_id === taskId);
    if (!task) return;

    renderActiveView();
    if (els.inspCloseBtn) els.inspCloseBtn.classList.remove("hidden");
    if (els.inspEyebrow) els.inspEyebrow.textContent = roleLabel(task.task_role);
    if (els.inspTitle) els.inspTitle.textContent = task.task_id;

    const leases = g.active_leases.filter((l) => l.task_id === task.task_id);
    const deps = g.edges.filter((e) => e.to_task_id === task.task_id).map((e) => e.from_task_id);
    const unblocks = g.edges.filter((e) => e.from_task_id === task.task_id).map((e) => e.to_task_id);
    const relatedOverlays = g.overlays.filter((o) => o.source_task_id === task.task_id);

    const claimAction = isDispatchable(task)
      ? `<button id="insp-claim-prompt-btn" class="btn btn-primary" type="button" style="width:100%;margin-bottom:12px;">
           让我的 Agent 认领这项引理
         </button>`
      : "";

    const devBlock = isOpsPage
      ? `<div class="insp-section">
           <h3>调度与客观遥测 (C0/C1)</h3>
           <div class="kv-list">
             <span class="kv-key">Candidate Decl</span><span class="kv-val mono">${esc(task.candidate_decl)}</span>
             <span class="kv-key">Tier / Cap</span><span class="kv-val mono num">min_tier=${task.min_tier}, racer_cap=${task.racer_cap_k}, prio=${task.priority}</span>
             <span class="kv-key">Prior / Cost</span><span class="kv-val mono num">p=${task.prior_p.toFixed(2)}, c=${task.expected_cost_c.toFixed(2)}</span>
             <span class="kv-key">Telemetry</span><span class="kv-val mono num">attempts=${task.telemetry.total_attempts}, fail=${task.telemetry.failed_attempts}, timeout=${task.telemetry.timeout_count}</span>
           </div>
         </div>`
      : "";

    if (!els.inspectorBody) return;
    els.inspectorBody.innerHTML = `
      ${claimAction}
      <div class="insp-section">
        <h3>当前状态与进展</h3>
        <div class="kv-list">
          <span class="kv-key">状态</span><span class="kv-val"><span class="badge ${stateBadgeClass(task.state)}">${esc(taskStateLabel(task.state))}</span></span>
          <span class="kv-key">所属思路</span><span class="kv-val mono">${esc(task.route_id || "主线汇合")}</span>
          <span class="kv-key">难度等级</span><span class="kv-val"><span class="badge badge-tier mono">T${esc(task.min_tier || 1)}</span></span>
          <span class="kv-key">Credit 悬赏</span><span class="kv-val">${bountyBadgeHtml(task) || '<span class="mono num" style="color:var(--ink-muted);">0 pts</span>'}</span>
          <span class="kv-key">正在攻关</span><span class="kv-val mono">${
            leases.length
              ? leases
                  .map((l) => (l.lineage_cluster ? `${l.actor_id} (${l.lineage_cluster})` : l.actor_id))
                  .join(", ")
              : "暂无"
          }</span>
        </div>
      </div>
      ${devBlock}
      <div class="insp-section">
        <h3>形式化命题陈述 (Prop)</h3>
        <pre class="code-block">${esc(task.spec_statement || `-- 见 ${task.spec_decl_name}`)}</pre>
      </div>
      <div class="insp-section">
        <h3>前置可导入模块（点击阅读源码）</h3>
        <div>${
          task.curated_imports.length
            ? task.curated_imports
                .map((m) => `<button type="button" class="import-pill mono js-insp-overlay" data-module="${esc(m)}">${esc(m)}</button>`)
                .join("")
            : '<span class="empty-hint">无额外导入</span>'
        }</div>
      </div>
      <div class="insp-section">
        <h3>依赖关系 & 已核验产物</h3>
        <div class="kv-list">
          <span class="kv-key">依赖前置</span><span class="kv-val">${
            deps.length
              ? deps
                  .map((d) => `<button type="button" class="task-ref-pill mono js-insp-task" data-task="${esc(d)}">${esc(d)}</button>`)
                  .join("")
              : "无"
          }</span>
          <span class="kv-key">完成后解锁</span><span class="kv-val">${
            unblocks.length
              ? unblocks
                  .map((u) => `<button type="button" class="task-ref-pill mono js-insp-task" data-task="${esc(u)}">${esc(u)}</button>`)
                  .join("")
              : "无"
          }</span>
          <span class="kv-key">已核验模块</span><span class="kv-val">${
            relatedOverlays.length
              ? relatedOverlays
                  .map((o) => `<button type="button" class="import-pill mono js-insp-overlay" data-module="${esc(o.module_name)}">${esc(o.module_name)}</button>`)
                  .join("")
              : "尚未完成"
          }</span>
        </div>
      </div>
    `;

    const claimBtn = document.getElementById("insp-claim-prompt-btn");
    if (claimBtn) {
      claimBtn.addEventListener("click", () => {
        openPromptModal("worker", task.route_id || null, task.task_id);
      });
    }
    els.inspectorBody.querySelectorAll(".js-insp-overlay").forEach((pill) => {
      pill.addEventListener("click", () => inspectOverlay(pill.dataset.module, task.task_id));
    });
    els.inspectorBody.querySelectorAll(".js-insp-task").forEach((pill) => {
      pill.addEventListener("click", () => selectTask(pill.dataset.task));
    });
  }

  async function inspectOverlay(moduleName, fromTaskId = null) {
    if (!state.currentProjectId || !moduleName || !els.inspectorBody) return;
    state.inspectorBackTaskId = fromTaskId || state.selectedTaskId || null;
    if (els.inspCloseBtn) els.inspCloseBtn.classList.remove("hidden");
    if (els.inspEyebrow) {
      els.inspEyebrow.textContent = isOpsPage ? "VERIFIED OVERLAY" : "模块源码";
    }
    if (els.inspTitle) els.inspTitle.textContent = moduleName;
    const backBarHtml = state.inspectorBackTaskId
      ? `<div class="insp-back-bar">
           <button id="insp-back-task-btn" class="btn btn-xs btn-secondary" type="button">
             ← 返回引理 <span class="mono">${esc(state.inspectorBackTaskId)}</span>
           </button>
         </div>`
      : "";
    els.inspectorBody.innerHTML = `${backBarHtml}<div class="empty-hint">正在加载 Lean 4 源码…</div>`;
    const drawer = document.getElementById("inspector-drawer");
    if (drawer) drawer.scrollTop = 0;

    const bindBackBtn = () => {
      const backBtn = document.getElementById("insp-back-task-btn");
      if (backBtn && state.inspectorBackTaskId) {
        const targetId = state.inspectorBackTaskId;
        backBtn.addEventListener("click", () => {
          selectTask(targetId);
          if (drawer) drawer.scrollTop = 0;
        });
      }
    };
    bindBackBtn();

    try {
      const pid = encodeURIComponent(state.currentProjectId);
      const eid = state.currentEpochId || 1;
      const mod = encodeURIComponent(moduleName);
      const src = await apiGet(`/api/projects/${pid}/epochs/${eid}/overlays/${mod}/source`);
      els.inspectorBody.innerHTML = `
        ${backBarHtml}
        <div class="insp-section">
          <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:8px;">
            <span class="badge badge-emerald">${
              isOpsPage
                ? `SHA: ${esc(src.blob_sha256.slice(0, 16) || "inline")}`
                : "Lean 4 内核验真通过 ✓"
            }</span>
            <button id="copy-overlay-src-btn" class="btn btn-xs btn-secondary" type="button">复制源码</button>
          </div>
          <pre class="code-block">${esc(src.code_utf8)}</pre>
        </div>`;
      bindBackBtn();
      const copyBtn = document.getElementById("copy-overlay-src-btn");
      if (copyBtn) {
        copyBtn.addEventListener("click", () => {
          copyText(src.code_utf8, "已复制 Lean 源码");
        });
      }
    } catch (err) {
      els.inspectorBody.innerHTML = `${backBarHtml}<div class="empty-hint">读取源码失败：${esc(err.message)}</div>`;
      bindBackBtn();
    }
  }

  function renderEvents() {
    if (!els.eventsTbody) return;
    const filtered = state.events.filter((ev) => {
      if (!state.searchQuery) return true;
      return (
        matchesFilter(ev.event_type) ||
        matchesFilter(ev.aggregate_id) ||
        matchesFilter(ev.actor_id) ||
        matchesFilter(JSON.stringify(ev.payload || {}))
      );
    });
    if (els.eventsCount) els.eventsCount.textContent = String(filtered.length);
    if (els.eventsCursorLabel) {
      els.eventsCursorLabel.textContent = `since_seq: ${state.maxEventSeq}`;
    }
    const rows = [...filtered].reverse().slice(0, 80);
    els.eventsTbody.innerHTML = rows
      .map((ev) => {
        const ts = ev.occurred_at ? new Date(ev.occurred_at).toLocaleTimeString() : "—";
        const payload = JSON.stringify(ev.payload || {});
        const shortPayload = payload.length > 90 ? payload.slice(0, 89) + "…" : payload;
        return `
          <tr>
            <td class="mono num">#${ev.event_seq}</td>
            <td class="mono num">${esc(ts)}</td>
            <td><span class="badge badge-neutral">${esc(ev.event_type)}</span></td>
            <td class="mono">${esc(ev.aggregate_type)}:${esc(ev.aggregate_id)}</td>
            <td class="mono">${esc(ev.actor_id)}</td>
            <td class="mono" style="color:var(--ink-muted);">${esc(shortPayload)}</td>
          </tr>`;
      })
      .join("");
  }

  function openPromptModal(role = "worker", routeId = null, taskId = null, preserveDetails = false) {
    state.promptRole = role;
    state.promptTargetRouteId = routeId;
    state.promptTargetTaskId = taskId;

    if (!preserveDetails && els.advancedPromptDetails) {
      els.advancedPromptDetails.open = isOpsPage || role !== "worker";
    }

    document.querySelectorAll('input[name="prompt-role"]').forEach((radio) => {
      radio.checked = radio.value === role;
    });

    updatePromptModalUi();
    if (els.joinDialog && !els.joinDialog.open) {
      if (typeof els.joinDialog.showModal === "function") {
        els.joinDialog.showModal();
      } else {
        els.joinDialog.setAttribute("open", "");
      }
    }
  }

  function updatePromptModalUi() {
    const hubUrl = window.location.origin;
    const project = state.currentProjectId || "erdos-479";
    const epochId = state.currentEpochId || 1;
    const role = state.promptRole;
    const openWorker = Boolean(state.hubStatus?.open_worker_join);
    const invite = els.inpInvite?.value.trim() || "";

    if (els.promptHubUrlLabel) els.promptHubUrlLabel.textContent = hubUrl;
    if (els.joinStatusPill && els.simpleInviteRow && els.inviteHintTag) {
      if (role === "worker" && openWorker) {
        els.joinStatusPill.className = "join-status-pill is-open";
        els.joinStatusPill.textContent =
          "✓ 当前平台已开放免码参与：直接复制下方提示词发给我的 Agent，它会阅读 JOIN.md 并交互式确认公开代号后自动领题提交。";
        els.simpleInviteRow.classList.add("hidden");
        els.inviteHintTag.textContent = "开放模式免填";
      } else if (role === "worker") {
        els.joinStatusPill.className = "join-status-pill is-invite";
        els.joinStatusPill.textContent =
          "需要邀请码：请在下方填入课题管理员发放的邀请码（Invite Code），然后复制提示词给 Agent。";
        els.simpleInviteRow.classList.remove("hidden");
        els.inviteHintTag.textContent = "需要邀请码";
      } else {
        els.joinStatusPill.className = "join-status-pill is-invite";
        els.joinStatusPill.textContent =
          "高权限维护角色：需要有效邀请码或管理员授权的密钥。";
        els.simpleInviteRow.classList.add("hidden");
        els.inviteHintTag.textContent = "需要授权";
      }
    }

    if (els.targetContextBar && els.targetContextLabel) {
      if (state.promptTargetTaskId || state.promptTargetRouteId) {
        els.targetContextBar.classList.remove("hidden");
        els.targetContextLabel.textContent = state.promptTargetTaskId
          ? `已定向锁定引理：${state.promptTargetTaskId}${
              state.promptTargetRouteId ? ` (${state.promptTargetRouteId})` : ""
            }`
          : `已定向锁定思路：${state.promptTargetRouteId}`;
      } else {
        els.targetContextBar.classList.add("hidden");
      }
    }

    const lines = [];
    if (role === "worker") {
      lines.push(`Read ${hubUrl}/JOIN.md and follow its instructions to onboard and join Quod as a worker.`);
      lines.push(`Target Hub: ${hubUrl}`);
      lines.push(`Target Project: ${project} (Epoch ${epochId})`);
      if (state.promptTargetRouteId) {
        lines.push(`Target Route: ${state.promptTargetRouteId}`);
      }
      if (state.promptTargetTaskId) {
        lines.push(`Target Task: ${state.promptTargetTaskId}`);
      }
      if (invite) {
        lines.push(`Invite Code: ${invite}`);
      } else if (!openWorker) {
        lines.push("Invite Code: <ASK_USER_FOR_INVITE_CODE>");
      }
    } else {
      lines.push(
        `Read ${hubUrl}/JOIN.md and follow its instructions to onboard as ${role}, then run \`quod skill ${role}\` and work on project "${project}" (Epoch ${epochId}).`
      );
      lines.push(`Target Hub: ${hubUrl}`);
      if (invite) {
        lines.push(`Invite Code: ${invite}`);
      } else {
        lines.push("Invite Code: <ASK_USER_FOR_INVITE_CODE>");
      }
    }

    if (els.promptOutput) els.promptOutput.value = lines.join("\n");
  }

  function switchView(viewName, focusTab = false) {
    state.activeView = viewName;
    document.querySelectorAll('.tab-btn[role="tab"]').forEach((tab) => {
      const isActive = tab.dataset.view === viewName;
      tab.classList.toggle("active", isActive);
      tab.setAttribute("aria-selected", isActive ? "true" : "false");
      tab.setAttribute("tabindex", isActive ? "0" : "-1");
      if (isActive && focusTab) {
        tab.focus();
      }
    });
    document.querySelectorAll('.view-panel[role="tabpanel"]').forEach((panel) => {
      const isMatch = panel.id === `view-${viewName}`;
      panel.classList.toggle("active", isMatch);
      panel.hidden = !isMatch;
    });
    if (viewName === "leaderboard") {
      loadLeaderboard().then(renderLeaderboardView);
    } else {
      renderActiveView();
    }
  }

  if (els.projectSelect) {
    els.projectSelect.addEventListener("change", () => {
      state.currentProjectId = els.projectSelect.value;
      const current = state.projects.find((p) => p.project_id === state.currentProjectId);
      state.currentEpochId = current?.active_epoch_id || 1;
      updateCrossLinks();
      resetInspector();
      loadProjectData(true);
    });
  }

  if (els.refreshBtn) {
    els.refreshBtn.addEventListener("click", () => loadProjectData(false));
  }

  if (els.autoRefreshToggle) {
    els.autoRefreshToggle.addEventListener("change", () => {
      clearInterval(state.refreshTimer);
      if (els.autoRefreshToggle.checked) {
        state.refreshTimer = setInterval(() => loadProjectData(false), 10000);
      }
    });
  }

  if (els.viewTabs) {
    const tabs = Array.from(els.viewTabs.querySelectorAll('.tab-btn[role="tab"]'));
    tabs.forEach((btn) => {
      btn.addEventListener("click", () => switchView(btn.dataset.view, false));
    });
    els.viewTabs.addEventListener("keydown", (ev) => {
      const currentIdx = tabs.findIndex((t) => t === document.activeElement);
      if (currentIdx === -1) return;
      let nextIdx = -1;
      if (ev.key === "ArrowRight") nextIdx = (currentIdx + 1) % tabs.length;
      else if (ev.key === "ArrowLeft") nextIdx = (currentIdx - 1 + tabs.length) % tabs.length;
      else if (ev.key === "Home") nextIdx = 0;
      else if (ev.key === "End") nextIdx = tabs.length - 1;
      if (nextIdx !== -1) {
        ev.preventDefault();
        switchView(tabs[nextIdx].dataset.view, true);
      }
    });
  }

  document.querySelectorAll('input[name="route-filter"]').forEach((radio) => {
    radio.addEventListener("change", () => {
      state.hidePruned = radio.value === "active";
      renderActiveView();
    });
  });

  if (els.searchInput) {
    els.searchInput.addEventListener("input", () => {
      state.searchQuery = els.searchInput.value.trim();
      renderActiveView();
    });
  }

  const zoomInBtn = document.getElementById("dag-zoom-in");
  const zoomOutBtn = document.getElementById("dag-zoom-out");
  const zoomResetBtn = document.getElementById("dag-zoom-reset");
  if (zoomInBtn) {
    zoomInBtn.addEventListener("click", () => {
      state.dagZoom = Math.min(2.0, +(state.dagZoom + 0.15).toFixed(2));
      if (els.dagZoomLabel) els.dagZoomLabel.textContent = `${Math.round(state.dagZoom * 100)}%`;
      renderGroupedDag();
    });
  }
  if (zoomOutBtn) {
    zoomOutBtn.addEventListener("click", () => {
      state.dagZoom = Math.max(0.55, +(state.dagZoom - 0.15).toFixed(2));
      if (els.dagZoomLabel) els.dagZoomLabel.textContent = `${Math.round(state.dagZoom * 100)}%`;
      renderGroupedDag();
    });
  }
  if (zoomResetBtn) {
    zoomResetBtn.addEventListener("click", () => {
      state.dagZoom = 1.0;
      if (els.dagZoomLabel) els.dagZoomLabel.textContent = "100%";
      renderGroupedDag();
    });
  }

  if (els.inspCloseBtn) {
    els.inspCloseBtn.addEventListener("click", () => {
      resetInspector();
      renderActiveView();
    });
  }

  if (els.openJoinBtn) {
    els.openJoinBtn.addEventListener("click", () => {
      openPromptModal(isOpsPage ? "orchestrator" : "worker", null, null);
    });
  }
  if (els.heroJoinBtn) {
    els.heroJoinBtn.addEventListener("click", () => {
      const tipId = els.heroJoinBtn.getAttribute("interestfor");
      const tip = tipId ? document.getElementById(tipId) : null;
      if (tip) {
        try {
          if (tip.matches(":popover-open") && typeof tip.hidePopover === "function") {
            tip.hidePopover();
          }
        } catch {
          // ignore
        }
        tip.classList.remove("is-visible");
      }
      openPromptModal("worker", null, null);
    });
  }

  // Self-contained WCAG 1.4.13 fallback for `[interestfor]` tooltips when native support is absent
  if (!("interestForElement" in HTMLButtonElement.prototype)) {
    const supportsAnchorPos =
      typeof CSS !== "undefined" &&
      typeof CSS.supports === "function" &&
      CSS.supports("position-area: block-end");
    document.querySelectorAll("[interestfor]").forEach((trigger) => {
      const tipId = trigger.getAttribute("interestfor");
      const tip = tipId ? document.getElementById(tipId) : null;
      if (!tip) return;
      let showTimer = null;
      let hideTimer = null;

      const positionFallback = () => {
        if (supportsAnchorPos) return;
        const rect = trigger.getBoundingClientRect();
        tip.style.position = "fixed";
        tip.style.margin = "0";
        tip.style.inset = "auto";
        tip.style.left = `${Math.max(12, Math.min(rect.left, window.innerWidth - 372))}px`;
        const spaceBelow = window.innerHeight - rect.bottom;
        if (spaceBelow < 110 && rect.top > 110) {
          tip.style.top = "auto";
          tip.style.bottom = `${window.innerHeight - rect.top + 8}px`;
        } else {
          tip.style.bottom = "auto";
          tip.style.top = `${rect.bottom + 8}px`;
        }
      };

      const openTip = () => {
        clearTimeout(hideTimer);
        positionFallback();
        if (typeof tip.showPopover === "function") {
          try {
            if (!tip.matches(":popover-open")) tip.showPopover();
          } catch {
            tip.classList.add("is-visible");
          }
        } else {
          tip.classList.add("is-visible");
        }
      };

      const closeTip = () => {
        clearTimeout(showTimer);
        clearTimeout(hideTimer);
        if (typeof tip.hidePopover === "function") {
          try {
            if (tip.matches(":popover-open")) tip.hidePopover();
          } catch {
            tip.classList.remove("is-visible");
          }
        } else {
          tip.classList.remove("is-visible");
        }
      };

      const scheduleOpen = (delayMs) => {
        clearTimeout(hideTimer);
        clearTimeout(showTimer);
        showTimer = setTimeout(openTip, delayMs);
      };

      const scheduleClose = () => {
        clearTimeout(showTimer);
        clearTimeout(hideTimer);
        hideTimer = setTimeout(closeTip, 160);
      };

      trigger.addEventListener("mouseenter", () => scheduleOpen(140));
      trigger.addEventListener("mouseleave", scheduleClose);
      trigger.addEventListener("focusin", openTip);
      trigger.addEventListener("focusout", scheduleClose);
      tip.addEventListener("mouseenter", () => clearTimeout(hideTimer));
      tip.addEventListener("mouseleave", scheduleClose);
      document.addEventListener("keydown", (ev) => {
        if (ev.key === "Escape") closeTip();
      });
    });
  }
  if (els.bannerAuditPromptBtn) {
    els.bannerAuditPromptBtn.addEventListener("click", () => openPromptModal("auditor", null, null));
  }
  if (els.closeJoinBtn && els.joinDialog) {
    els.closeJoinBtn.addEventListener("click", () => {
      if (typeof els.joinDialog.close === "function") {
        els.joinDialog.close();
      } else {
        els.joinDialog.removeAttribute("open");
      }
    });
  }

  // Mandatory light-dismiss fallback for browsers without <dialog closedby="any">
  if (els.joinDialog && !("closedBy" in HTMLDialogElement.prototype)) {
    els.joinDialog.addEventListener("click", (event) => {
      if (event.target !== els.joinDialog) return;
      const rect = els.joinDialog.getBoundingClientRect();
      const isDialogContent =
        rect.top <= event.clientY &&
        event.clientY <= rect.top + rect.height &&
        rect.left <= event.clientX &&
        event.clientX <= rect.left + rect.width;
      if (isDialogContent) return;
      els.joinDialog.close();
    });
  }

  document.querySelectorAll('input[name="prompt-role"]').forEach((radio) => {
    radio.addEventListener("change", () => {
      if (radio.checked) {
        openPromptModal(
          radio.value,
          state.promptTargetRouteId,
          state.promptTargetTaskId,
          true
        );
      }
    });
  });

  if (els.clearTargetBtn) {
    els.clearTargetBtn.addEventListener("click", () => {
      state.promptTargetRouteId = null;
      state.promptTargetTaskId = null;
      updatePromptModalUi();
    });
  }

  if (els.inpInviteSimple && els.inpInvite) {
    els.inpInviteSimple.addEventListener("input", () => {
      els.inpInvite.value = els.inpInviteSimple.value;
      updatePromptModalUi();
    });
    els.inpInvite.addEventListener("input", () => {
      els.inpInviteSimple.value = els.inpInvite.value;
      updatePromptModalUi();
    });
  }

  if (els.copyPromptBtn && els.promptOutput) {
    els.copyPromptBtn.addEventListener("click", async () => {
      await copyText(els.promptOutput.value);
      const original = "一键复制提示词";
      els.copyPromptBtn.textContent = "✓ 已复制";
      els.copyPromptBtn.classList.add("is-copied");
      clearTimeout(els.copyPromptBtn._resetTimer);
      els.copyPromptBtn._resetTimer = setTimeout(() => {
        els.copyPromptBtn.textContent = original;
        els.copyPromptBtn.classList.remove("is-copied");
      }, 1800);
    });
  }

  state.refreshTimer = setInterval(() => loadProjectData(false), 10000);
  loadInitial();
})();
