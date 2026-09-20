"use strict";

const state = { eventId: null, data: null, activeCandidate: null, selectedLag: null, selectedSegment: null, poll: null };

async function api(path, opts) {
  const res = await fetch(path, opts);
  if (!res.ok) throw new Error(`${res.status}: ${await res.text()}`);
  return res.json();
}
async function post(path, body) {
  return api(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
}

async function loadEvents() {
  const { events } = await api("/api/events");
  const sel = document.getElementById("eventSelect");
  sel.innerHTML = "";
  for (const e of events) {
    const o = document.createElement("option");
    o.value = e.event_id;
    o.textContent = `${e.event_id} — ${e.label}`;
    sel.appendChild(o);
  }
  const wanted = location.hash.replace("#", "");
  if (events.some(e => e.event_id === wanted)) state.eventId = wanted;
  else if (events.length && !state.eventId) state.eventId = events[0].event_id;
  if (state.eventId) sel.value = state.eventId;
}

async function loadEvent() {
  state.data = await api(`/api/event?event_id=${encodeURIComponent(state.eventId)}`);
  state.activeCandidate = null;
  renderAll();
  const running = (state.data.jobs || []).some(j => j.status === "pending" || j.status === "running");
  if (running) schedulePoll();
}

function schedulePoll() {
  clearTimeout(state.poll);
  state.poll = setTimeout(async () => {
    await loadEvent();
  }, 700);
}

function renderAll() {
  const d = state.data;
  document.getElementById("status").textContent = d.event
    ? `声速 ${d.event.sound_speed_mps} m/s · 窗 [${fmt(d.event.window_start)}, ${fmt(d.event.window_end)}) s`
    : "";
  drawMap();
  drawTimeline();
  renderObservations();
  renderCandidates();
  renderEvidence();
  renderJobs();
}

const fmt = (x, n = 4) => (x == null || Number.isNaN(x)) ? "—" : Number(x).toFixed(n);

// ---------- 空间视图 ----------
function drawMap() {
  const cv = document.getElementById("map");
  const dpr = window.devicePixelRatio || 1;
  // 固定 CSS 显示尺寸并同步内部分辨率，绘图坐标与显示一一对应。
  const w = 860, h = 460;
  cv.style.width = w + "px"; cv.style.height = h + "px";
  cv.width = Math.round(w * dpr); cv.height = Math.round(h * dpr);
  const ctx = cv.getContext("2d");
  ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, w, h);
  const d = state.data, ev = d.event;
  if (!ev) return;
  const b = ev.bounds || { min_x: 0, min_y: 0, max_x: 1, max_y: 1 };
  const pad = 34;
  const sx = (w - 2 * pad) / (b.max_x - b.min_x);
  const sy = (h - 2 * pad) / (b.max_y - b.min_y);
  const s = Math.min(sx, sy);
  const X = x => pad + (x - b.min_x) * s;
  const Y = y => h - pad - (y - b.min_y) * s;

  ctx.strokeStyle = "#22303f"; ctx.lineWidth = 1;
  ctx.strokeRect(X(b.min_x), Y(b.max_y), (b.max_x - b.min_x) * s, (b.max_y - b.min_y) * s);

  // 欠定区域（选中候选）
  const cand = d.candidates.find(c => c.candidate_id === state.activeCandidate) || d.candidates[0];
  if (cand && cand.region) {
    drawPolygon(ctx, cand.region.polygon, X, Y, "rgba(239,107,107,.15)", "#ef6b6b");
  }
  // 全部 CCF 延迟双曲线：直达绿，反射紫；被排除的灰
  const excluded = new Set();
  for (const e of d.evidence) {
    if (e.kind === "exclude_reflection") excluded.add(e.payload.lag_uid);
  }
  const drawnCurves = new Set();
  for (const lag of d.lags) {
    if (drawnCurves.has(lag.lag_uid)) continue;
    drawnCurves.add(lag.lag_uid);
    const a = stationPos(d, lag.station_id), q = stationPos(d, lag.peer_id);
    if (!a || !q) continue;
    const color = excluded.has(lag.lag_uid) ? "rgba(110,120,130,.35)"
      : lag.hint === "reflection" ? "rgba(192,139,255,.55)" : "rgba(111,208,138,.65)";
    drawHyperbolaImplicit(ctx, a, q, lag.lag_sec * ev.sound_speed_mps, X, Y, color, b);
  }

  // 候选曲线
  if (cand && cand.curve && cand.curve.points) {
    drawPath(ctx, cand.curve.points, X, Y, "#6fd08a", 2.5);
  }
  // 站点
  for (const st of d.stations) {
    ctx.fillStyle = "#4ea1ff";
    ctx.beginPath(); ctx.arc(X(st.x), Y(st.y), 5, 0, Math.PI * 2); ctx.fill();
    ctx.fillStyle = "#9fb6cf";
    ctx.font = "11px sans-serif";
    ctx.fillText(st.station_id, X(st.x) + 7, Y(st.y) - 6);
  }
  // 候选点
  d.candidates.forEach((c, i) => {
    if (!c.point) return;
    const active = c.candidate_id === (cand && cand.candidate_id);
    ctx.fillStyle = c.underdetermined === "outside_observable_geometry" ? "#ef6b6b"
      : active ? "#f0b35e" : "rgba(240,179,94,.55)";
    ctx.beginPath(); ctx.arc(X(c.point[0]), Y(c.point[1]), active ? 7 : 5, 0, Math.PI * 2); ctx.fill();
    ctx.fillStyle = "#f0b35e"; ctx.font = "bold 11px sans-serif";
    ctx.fillText("#" + c.rank, X(c.point[0]) + 8, Y(c.point[1]) + 4);
  });
  if (cand && cand.kind === "region" && !cand.region) {
    ctx.fillStyle = "#ef6b6b"; ctx.font = "12px sans-serif";
    ctx.fillText("欠定：" + underLabel(cand.underdetermined), pad, 20);
  }
}

function stationPos(d, id) {
  const st = d.stations.find(s => s.station_id === id);
  return st ? [st.x, st.y] : null;
}
function drawPolygon(ctx, poly, X, Y, fill, stroke) {
  if (!poly || poly.length < 3) return;
  ctx.beginPath();
  poly.forEach((p, i) => i ? ctx.lineTo(X(p[0]), Y(p[1])) : ctx.moveTo(X(p[0]), Y(p[1])));
  ctx.closePath(); ctx.fillStyle = fill; ctx.fill();
  ctx.strokeStyle = stroke; ctx.setLineDash([5, 4]); ctx.stroke(); ctx.setLineDash([]);
}
function drawPath(ctx, pts, X, Y, color, width) {
  ctx.beginPath();
  pts.forEach((p, i) => i ? ctx.lineTo(X(p[0]), Y(p[1])) : ctx.moveTo(X(p[0]), Y(p[1])));
  ctx.strokeStyle = color; ctx.lineWidth = width; ctx.stroke(); ctx.lineWidth = 1;
}
// 直接按几何定义在网格上画 |x-a|-|x-b| ≈ diff 的等值线
function drawHyperbolaImplicit(ctx, a, q, diff, X, Y, color, b) {
  const step = 0.8;
  ctx.strokeStyle = color; ctx.lineWidth = 1.2; ctx.beginPath();
  let started = false;
  for (let yy = b.min_y; yy <= b.max_y; yy += step) {
    for (let xx = b.min_x; xx <= b.max_x; xx += step) {
      const v = Math.hypot(xx - a[0], yy - a[1]) - Math.hypot(xx - q[0], yy - q[1]) - diff;
      const vx = Math.hypot(xx + step - a[0], yy - a[1]) - Math.hypot(xx + step - q[0], yy - q[1]) - diff;
      const hit = Math.sign(v) !== Math.sign(vx) && isFinite(v);
      if (hit) {
        const px = X(xx + step / 2), py = Y(yy);
        started ? ctx.lineTo(px, py) : ctx.moveTo(px, py);
        started = true;
      }
    }
  }
  ctx.stroke();
}

// ---------- 时线 ----------
function drawTimeline() {
  const cv = document.getElementById("timeline");
  const dpr = window.devicePixelRatio || 1;
  const w = 860, h = 150;
  cv.style.width = w + "px"; cv.style.height = h + "px";
  cv.width = Math.round(w * dpr); cv.height = Math.round(h * dpr);
  const ctx = cv.getContext("2d"); ctx.scale(dpr, dpr);
  ctx.clearRect(0, 0, w, h);
  const d = state.data, ev = d.event;
  if (!ev) return;
  const t0 = ev.window_start - 0.05, t1 = ev.window_end + 0.05;
  const padL = 64, padR = 16;
  const X = t => padL + (t - t0) / (t1 - t0) * (w - padL - padR);
  const ids = [...new Set(d.observations.map(o => o.station_id))].sort();
  const rowH = (h - 24) / Math.max(ids.length, 1);

  // 半开窗：[start,end) 用左实右虚的边界
  ctx.fillStyle = "rgba(78,161,255,.06)";
  ctx.fillRect(X(ev.window_start), 6, X(ev.window_end) - X(ev.window_start), h - 20);
  ctx.strokeStyle = "#4ea1ff";
  ctx.beginPath(); ctx.moveTo(X(ev.window_start), 6); ctx.lineTo(X(ev.window_start), h - 14); ctx.stroke();
  ctx.setLineDash([4, 3]);
  ctx.beginPath(); ctx.moveTo(X(ev.window_end), 6); ctx.lineTo(X(ev.window_end), h - 14); ctx.stroke();
  ctx.setLineDash([]);

  // 时钟分段（每个 station 一行底色）
  for (const seg of d.clock_segments) {
    const ri = ids.indexOf(seg.station_id);
    if (ri < 0) continue;
    const y = 12 + ri * rowH;
    const xs = X(Math.max(seg.t_start, t0));
    const xe = X(seg.t_end == null ? t1 : Math.min(seg.t_end, t1));
    ctx.fillStyle = "rgba(240,179,94,.10)";
    ctx.fillRect(xs, y, xe - xs, rowH - 6);
    ctx.strokeStyle = "rgba(240,179,94,.35)"; ctx.strokeRect(xs, y, xe - xs, rowH - 6);
    ctx.fillStyle = "#8ea2b8"; ctx.font = "10px monospace";
    ctx.fillText(`${seg.offset_sec >= 0 ? "+" : ""}${fmt(seg.offset_sec, 4)}s`, xs + 3, y + 11);
  }

  ids.forEach((sid, ri) => {
    const y = 12 + ri * rowH;
    ctx.fillStyle = "#9fb6cf"; ctx.font = "11px sans-serif";
    ctx.fillText(sid, 8, y + rowH / 2);
  });
  for (const o of d.observations) {
    const ri = ids.indexOf(o.station_id);
    const y = 12 + ri * rowH + (o.duplicate_of ? rowH * 0.62 : rowH / 2);
    const t = o.corrected_onset_sec != null ? o.corrected_onset_sec : o.local_onset_sec;
    ctx.fillStyle = o.duplicate_of ? "#8a7a5a" : (o.clock_locked ? "#6fd08a" : "#f0b35e");
    ctx.beginPath();
    ctx.moveTo(X(t), y - 5); ctx.lineTo(X(t) + 5, y); ctx.lineTo(X(t), y + 5); ctx.lineTo(X(t) - 5, y);
    ctx.closePath(); ctx.fill();
    if (o.duplicate_of) {
      ctx.fillStyle = "#8a7a5a"; ctx.font = "9px sans-serif";
      ctx.fillText("重复", X(t) + 7, y + 3);
    }
    if (o.corrected_onset_sec == null) {
      ctx.fillStyle = "#f0b35e"; ctx.font = "9px sans-serif";
      ctx.fillText("时钟未锁定", X(t) + 7, y - 7);
    }
  }
  ctx.fillStyle = "#8ea2b8"; ctx.font = "10px sans-serif";
  ctx.fillText(`[${fmt(ev.window_start, 2)} s`, X(ev.window_start) - 4, h - 3);
  ctx.fillText(`${fmt(ev.window_end, 2)} s)`, X(ev.window_end) - 30, h - 3);
}

// ---------- 观测表 ----------
function renderObservations() {
  const d = state.data;
  const rows = [];
  rows.push("<tr><th>观测</th><th>站点</th><th>本地时钙(s)</th><th>校正(s)</th><th>频带Hz</th><th>SNR</th><th>延迟</th></tr>");
  for (const o of d.observations) {
    const lagCells = d.lags.filter(l => l.obs_uid === o.obs_uid).map(l => {
      const excluded = d.evidence.some(e => e.kind === "exclude_reflection" && e.payload.lag_uid === l.lag_uid);
      const cls = l.hint === "reflection" ? "reflection" : "direct";
      const sel = state.selectedLag === l.lag_uid ? "outline:1px solid #f0b35e" : "";
      return `<div style="cursor:pointer;${sel}" data-lag="${l.lag_uid}">
        <span class="tag ${cls}">${l.hint}</span>
        <span class="mono">${l.peer_id} Δ=${fmt(l.lag_sec*1000,2)}ms</span>
        ${excluded ? '<span class="tag bad">已排除</span>' : ""}</div>`;
    }).join("");
    rows.push(`<tr${o.duplicate_of ? ' style="opacity:.55"' : ""}>
      <td class="mono">${o.obs_uid.split(":").pop()}</td><td>${o.station_id}</td>
      <td class="mono">${fmt(o.local_onset_sec)}</td>
      <td class="mono">${o.corrected_onset_sec == null ? "未锁定" : fmt(o.corrected_onset_sec)}</td>
      <td>${o.peak_band_hz.toFixed(0)}</td><td>${o.snr_db.toFixed(1)}</td>
      <td>${lagCells || (o.duplicate_of ? '<span class="muted">重复上报</span>' : "")}</td></tr>`);
  }
  document.getElementById("obsTable").innerHTML = rows.join("");
  document.querySelectorAll("[data-lag]").forEach(el => el.onclick = () => {
    state.selectedLag = el.dataset.lag; renderObservations();
  });
}

// ---------- 候选 ----------
function underLabel(code) {
  return {
    sensor_geometry: "传感器几何不足（交会退化）",
    clock_freedom: "时钟自由度（校正段未锁定）",
    multipath: "多径冲突（存在未建模反射）",
    inconsistent_measurements: "测量互相不一致",
    unobservable_input: "输入超出可观测几何（超光速延迟，优化器不夹紧）",
    outside_observable_geometry: "解在可观测几何之外",
    ambiguous_pair: "两位置不可分",
    kept_indistinguishable: "手工保留的不可分位置",
    "": "确定点"
  }[code] || code;
}
function renderCandidates() {
  const d = state.data;
  const box = document.getElementById("candidates");
  if (!d.candidates.length) {
    const running = d.jobs.some(j => j.status !== "completed" && j.status !== "failed");
    box.innerHTML = running
      ? '<p class="muted">求解中…（未完成候选不会出现在审查列表）</p>'
      : '<p class="muted">尚无已完成候选，点击“重算候选”。</p>';
    return;
  }
  box.innerHTML = "";
  d.candidates.forEach(c => {
    const div = document.createElement("div");
    div.className = "cand" + (state.activeCandidate === c.candidate_id ? " active" : "");
    const pos = c.point ? `(${fmt(c.point[0], 2)}, ${fmt(c.point[1], 2)}) m` : "—";
    div.innerHTML = `<div class="row">
        <span><span class="kind">#${c.rank} ${shapeLabel(c.kind)}</span>
          <span class="muted mono">${c.candidate_id.split(":").pop()}</span></span>
        <span class="mono">RMS ${c.rms_sec == null ? "—" : fmt(c.rms_sec * 1000, 2) + "ms"}</span>
      </div>
      <div>${pos} · ${c.signature}</div>
      ${c.underdetermined ? `<div class="tag bad">${underLabel(c.underdetermined)}</div>` : '<div class="tag direct">点定位</div>'}
      <div class="resid"><table>${residualRows(c)}</table></div>`;
    div.onclick = () => { state.activeCandidate = c.candidate_id; renderCandidates(); drawMap(); };
    box.appendChild(div);
  });
}
function shapeLabel(k) { return { point: "点", curve: "双曲线", region: "区域" }[k] || k; }
function residualRows(c) {
  const rows = ["<tr><th>测量</th><th>实测ms</th><th>预测ms</th><th>残差ms</th><th></th></tr>"];
  for (const r of c.residuals) {
    const who = r.lag_uid ? r.lag_uid.split(":").slice(-2).join(":") : `${r.station_id}→${r.peer_id || ""}`;
    rows.push(`<tr${r.used ? "" : ' style="opacity:.5"'}>
      <td><span class="tag ${r.kind === "ccf_lag" ? "direct" : "onset"}">${r.kind === "ccf_lag" ? "CCF" : "时钙"}</span> ${who}</td>
      <td class="mono">${fmt(r.measured_sec * 1000, 2)}</td>
      <td class="mono">${r.used && r.predicted_sec != null ? fmt(r.predicted_sec * 1000, 2) : "—"}</td>
      <td class="mono">${r.used && r.residual_sec != null ? fmt(r.residual_sec * 1000, 2) : "—"}</td>
      <td>${r.excluded_reason ? `<span class="tag bad">${r.excluded_reason}</span>` : ""}</td></tr>`);
  }
  return rows.join("");
}

// ---------- 证据 ----------
function renderEvidence() {
  const d = state.data;
  const box = document.getElementById("evidence");
  if (!d.evidence.length) { box.innerHTML = '<p class="muted">尚无手工判断。</p>'; return; }
  box.innerHTML = d.evidence.map(e => {
    const p = e.payload;
    let text = e.kind;
    if (e.kind === "exclude_reflection") text = `排除反射 ${p.lag_uid}`;
    if (e.kind === "lock_clock") text = `锁定时钟段 #${p.segment_id}`;
    if (e.kind === "keep_ambiguous") text = `保留 ${p.candidate_ids.join(" / ")}`;
    return `<div><span class="mono">${e.evidence_id}</span> ${text}
      <span class="muted">— ${e.basis || "（无说明）"} @ ${e.created_at}</span></div>`;
  }).join("");
}
function renderJobs() {
  const d = state.data;
  document.getElementById("jobs").innerHTML = d.jobs.map(j =>
    `<div>${j.job_id} · ${j.status} · <span title="${j.fingerprint}">${j.fingerprint.slice(0, 16)}…</span>${j.error ? " · " + j.error : ""}</div>`
  ).join("") || '<span class="muted">无作业</span>';
}

// ---------- 操作 ----------
document.getElementById("eventSelect").onchange = async (e) => {
  state.eventId = e.target.value; state.selectedLag = null; state.selectedSegment = null;
  history.replaceState(null, "", "#" + state.eventId);
  await loadEvent();
};
document.getElementById("solveBtn").onclick = async () => {
  await post("/api/solve", { event_id: state.eventId });
  document.getElementById("status").textContent = "已入队…";
  schedulePoll();
};
document.getElementById("excludeBtn").onclick = async () => {
  if (!state.selectedLag) return alert("先在观测表中点选一条反射延迟。");
  const basis = prompt("排除该反射路径的依据：", "墙面反射峰，与直达包络不一致");
  if (basis === null) return;
  await post("/api/evidence", { event_id: state.eventId, kind: "exclude_reflection",
    payload: { lag_uid: state.selectedLag }, basis });
  state.selectedLag = null; schedulePoll();
};
document.getElementById("lockBtn").onclick = async () => {
  const segId = prompt("输入要锁定的时钟段 segment_id（见时线提示，需工程师确认）：", "");
  if (!segId) return;
  const basis = prompt("锁定依据：", "与外部 NTP 记录吻合");
  if (basis === null) return;
  await post("/api/evidence", { event_id: state.eventId, kind: "lock_clock",
    payload: { segment_id: Number(segId) }, basis });
  schedulePoll();
};
document.getElementById("keepBtn").onclick = async () => {
  const c = state.data.candidates;
  if (c.length < 2) return alert("候选不足两个。");
  const basis = prompt("保留两个不可分位置的依据：", "残差在噪声带内无显著差异");
  if (basis === null) return;
  await post("/api/evidence", { event_id: state.eventId, kind: "keep_ambiguous",
    payload: { candidate_ids: [c[0].candidate_id, c[1].candidate_id] }, basis });
  schedulePoll();
};

(async function init() {
  await loadEvents();
  await loadEvent();
})();
