"use strict";

const state = { events: [], detail: null, selectedCandidate: 0 };

async function api(path, options) {
  const res = await fetch(path, options);
  const body = await res.json();
  if (!res.ok) throw new Error(body.error || res.statusText);
  return body;
}

function post(path, payload) {
  return api(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(payload || {}),
  });
}

async function loadEvents(selectId) {
  state.events = await api("/api/events");
  const sel = document.getElementById("event-select");
  sel.innerHTML = "";
  for (const ev of state.events) {
    const opt = document.createElement("option");
    opt.value = ev.event_id;
    opt.textContent = `${ev.event_id} — ${ev.title}`;
    sel.appendChild(opt);
  }
  if (state.events.length) sel.value = state.events[0].event_id;
  await selectEvent();
}

async function selectEvent() {
  const id = document.getElementById("event-select").value;
  state.detail = await api(`/api/events/${encodeURIComponent(id)}`);
  state.selectedCandidate = 0;
  renderAll();
}

function fmt(v, digits = 3) {
  return typeof v === "number" && isFinite(v) ? v.toFixed(digits) : "—";
}

function renderMeta() {
  const e = state.detail.event;
  document.getElementById("speed-readout").textContent = `${e.sound_speed} ${e.speed_unit}`;
  document.getElementById("event-meta").innerHTML =
    `区间 <code>${e.t_start.toFixed(3)} .. ${e.t_end.toFixed(3)}</code> ${e.interval_semantics}<br>` +
    `观测 ${e.linked_observation_count} 条 · 坐标 ${e.coordinate_unit} · 延迟 ${e.delay_unit}`;
}

function renderEvidence() {
  const ul = document.getElementById("evidence-list");
  ul.innerHTML = "";
  for (const ev of state.detail.evidence) {
    const li = document.createElement("li");
    li.textContent = `${ev.kind}: ${JSON.stringify(ev.payload)}`;
    li.title = ev.content_id;
    ul.appendChild(li);
  }
  const jobs = document.getElementById("job-list");
  jobs.innerHTML = "";
  for (const j of state.detail.jobs) {
    const li = document.createElement("li");
    li.textContent = `${j.state}  ${j.fingerprint.slice(0, 12)}…`;
    if (j.state !== "done") li.style.color = "#a25252";
    jobs.appendChild(li);
  }
}

function stationById(id) {
  return state.detail.stations.find((s) => s.station_id === id);
}

function renderTimeline() {
  const canvas = document.getElementById("timeline");
  const ctx = canvas.getContext("2d");
  const d = state.detail;
  const W = canvas.width, H = canvas.height;
  ctx.clearRect(0, 0, W, H);
  const pad = 70;
  const t0 = d.event.t_start, t1 = d.event.t_end;
  const xOf = (t) => pad + ((t - t0) / (t1 - t0)) * (W - pad - 10);

  // half-open bracket
  ctx.strokeStyle = "#102336";
  ctx.beginPath();
  ctx.moveTo(xOf(t0), 14); ctx.lineTo(xOf(t0), 22);
  ctx.moveTo(xOf(t0), 18); ctx.lineTo(xOf(t1), 18);
  ctx.moveTo(xOf(t1), 10); ctx.lineTo(xOf(t1), 22);
  ctx.stroke();
  ctx.fillStyle = "#516173";
  ctx.fillText("[", xOf(t0) - 6, 34);
  ctx.fillText(")", xOf(t1) - 1, 34);

  const stations = d.observations.length
    ? [...new Set(d.observations.map((o) => o.station_id))].sort()
    : [];
  const all = d.stations.map((s) => s.station_id);
  const present = new Set(stations);
  const rowH = Math.min(26, (H - 50) / Math.max(all.length, 1));
  all.forEach((sid, i) => {
    const y = 50 + i * rowH;
    ctx.fillStyle = present.has(sid) ? "#1f5fa8" : "#c6cfdb";
    ctx.fillText(sid, 6, y + 4);
    ctx.strokeStyle = present.has(sid) ? "#9db7d6" : "#e3e8ef";
    ctx.beginPath();
    ctx.moveTo(pad, y); ctx.lineTo(W - 10, y);
    ctx.stroke();
    if (!present.has(sid)) {
      ctx.fillStyle = "#a25252";
      ctx.fillText("缺席（无观测）", pad + 4, y + 4);
    }
  });

  // reports, stacked duplicates slightly
  const seen = {};
  for (const o of d.observations) {
    const row = all.indexOf(o.station_id);
    const y = 50 + row * rowH;
    const t = Math.min(Math.max(o.local_onset_s, t0), t1);
    const key = o.station_id;
    seen[key] = (seen[key] || 0) + 1;
    const x = xOf(t);
    ctx.fillStyle = o.multipath ? "#c77c1a" : "#1f5fa8";
    ctx.beginPath();
    ctx.arc(x, y - 4 + (seen[key] % 2) * 7, 4.5, 0, Math.PI * 2);
    ctx.fill();
    if (seen[key] > 1) {
      ctx.fillStyle = "#8a5310";
      ctx.fillText("重复", x + 6, y + 2);
    }
  }

  document.getElementById("timeline-legend").innerHTML =
    '<span><span class="dot" style="background:#1f5fa8"></span>直达/正常</span>' +
    '<span><span class="dot" style="background:#c77c1a"></span>多径</span>' +
    '<span><span class="dot" style="background:#c6cfdb"></span>缺席站点</span>';
}

function drawEdgeHyperbola(ctx, a, b, delay, speed, toXY, multipath) {
  // delay = (r_b - r_a)/c -> foci a,b, transverse semi-axis = |d|/2
  const focal = Math.hypot(b.x - a.x, b.y - a.y) / 2;
  const aa = Math.abs(delay) * speed / 2;
  if (!(aa > 0) || aa >= focal) return;
  const bb = Math.sqrt(focal * focal - aa * aa);
  const cx = (a.x + b.x) / 2, cy = (a.y + b.y) / 2;
  const ang = Math.atan2(b.y - a.y, b.x - a.x);
  const co = Math.cos(ang), si = Math.sin(ang);
  ctx.strokeStyle = multipath ? "#d08a3c" : "#8d7ad6";
  ctx.lineWidth = 1.2;
  ctx.setLineDash(multipath ? [2, 4] : []);
  ctx.beginPath();
  let pen = false;
  for (let i = -80; i <= 80; i++) {
    const v = i * 0.08;
    const u = aa * Math.cosh(v);
    const w = bb * Math.sinh(v);
    // sign: delay > 0 -> closer to a (r_b > r_a), branch around focus a
    const sgn = delay >= 0 ? -1 : 1;
    const wx = cx + co * sgn * u - si * w;
    const wy = cy + si * sgn * u + co * w;
    const [x, y] = toXY(wx, wy);
    if (!pen) { ctx.moveTo(x, y); pen = true; } else { ctx.lineTo(x, y); }
  }
  ctx.stroke();
  ctx.setLineDash([]);
}

function candidatePoints(c) {
  if (c.kind === "point") return [{ x: c.x, y: c.y }];
  const r = c.region;
  if (!r) return [];
  return (r.coordinates || []).filter((p) => isFinite(p[0]) && isFinite(p[1]));
}

function drawHyperbola(ctx, region, toXY, stroke, width) {
  const pts = (region.coordinates || []).filter((p) => isFinite(p[0]) && isFinite(p[1]));
  ctx.strokeStyle = stroke;
  ctx.lineWidth = Math.max(1.5, (width || 4) / state.scale);
  ctx.beginPath();
  let pen = false;
  for (const p of pts) {
    const [sx, sy] = toXY(p[0], p[1]);
    if (!pen) { ctx.moveTo(sx, sy); pen = true; } else { ctx.lineTo(sx, sy); }
  }
  ctx.stroke();
}

function renderSpace() {
  const canvas = document.getElementById("space");
  const ctx = canvas.getContext("2d");
  const d = state.detail;
  const W = canvas.width, H = canvas.height;
  ctx.clearRect(0, 0, W, H);

  const positions = d.stations.map((s) => [s.x, s.y]);
  const radius = d.observable_radius_m || 2000;
  let minX = -radius, maxX = radius, minY = -radius, maxY = radius;
  for (const c of d.candidates) {
    for (const p of candidatePoints(c)) {
      minX = Math.min(minX, p.x - 20); maxX = Math.max(maxX, p.x + 20);
      minY = Math.min(minY, p.y - 20); maxY = Math.max(maxY, p.y + 20);
    }
  }
  for (const s of d.stations) {
    minX = Math.min(minX, s.x - 20); maxX = Math.max(maxX, s.x + 20);
    minY = Math.min(minY, s.y - 20); maxY = Math.max(maxY, s.y + 20);
  }
  const span = Math.max(maxX - minX, maxY - minY) * 1.08;
  const cx = (minX + maxX) / 2, cy = (minY + maxY) / 2;
  const scale = Math.min(W, H) / span;
  state.scale = scale;
  const toXY = (x, y) => [W / 2 + (x - cx) * scale, H / 2 - (y - cy) * scale];
  state.toXY = toXY;

  // grid
  ctx.strokeStyle = "#eef1f5";
  ctx.lineWidth = 1;
  const step = span / 12;
  for (let g = -6; g <= 6; g++) {
    const [gx1, gy1] = toXY(cx + g * step, cy - span);
    const [gx2, gy2] = toXY(cx + g * step, cy + span);
    ctx.beginPath(); ctx.moveTo(gx1, gy1); ctx.lineTo(gx2, gy2); ctx.stroke();
    const [hx1, hy1] = toXY(cx - span, cy + g * step);
    const [hx2, hy2] = toXY(cx + span, cy + g * step);
    ctx.beginPath(); ctx.moveTo(hx1, hy1); ctx.lineTo(hx2, hy2); ctx.stroke();
  }

  // delay hyperbolae for direct pair-wise edges (b - a delay)
  for (const edge of d.edges) {
    if (edge.peak_index !== 0) continue;
    const a = stationById(edge.station_a), b = stationById(edge.station_b);
    if (!a || !b) continue;
    drawEdgeHyperbola(ctx, a, b, edge.delay_s, d.event.sound_speed, toXY, edge.multipath);
  }

  // candidate regions
  d.candidates.forEach((c, i) => {
    if (c.kind !== "region" || !c.region) return;
    const reg = c.region;
    if (reg.shape === "line" || reg.shape === "hyperbola") {
      drawHyperbola(ctx, reg, toXY, i === state.selectedCandidate ? "#33407a" : "#7a86b8", reg.width_m);
    } else if (reg.shape === "circle") {
      const [x, y] = toXY(reg.coordinates[0][0], reg.coordinates[0][1]);
      ctx.strokeStyle = "#b9745c";
      ctx.setLineDash([3, 3]);
      ctx.beginPath();
      ctx.arc(x, y, (reg.radius_m || radius) * scale, 0, Math.PI * 2);
      ctx.stroke();
      ctx.setLineDash([]);
    }
  });

  // stations
  for (const s of d.stations) {
    const [x, y] = toXY(s.x, s.y);
    ctx.fillStyle = "#102336";
    ctx.fillRect(x - 5, y - 5, 10, 10);
    ctx.fillStyle = "#102336";
    ctx.font = "11px sans-serif";
    ctx.fillText(`${s.station_id} (${s.x}, ${s.y}) m`, x + 7, y - 7);
  }

  // point candidates
  d.candidates.forEach((c, i) => {
    if (c.kind !== "point") return;
    const [x, y] = toXY(c.x, c.y);
    const selected = i === state.selectedCandidate;
    ctx.fillStyle = selected ? "#d83a2f" : "#d8863a";
    ctx.beginPath();
    ctx.arc(x, y, selected ? 8 : 6, 0, Math.PI * 2);
    ctx.fill();
    ctx.fillStyle = "#102336";
    ctx.fillText(`#${i}`, x + 9, y + 4);
  });
}

function renderCandidates() {
  const ol = document.getElementById("candidate-list");
  ol.innerHTML = "";
  state.detail.candidates.forEach((c, i) => {
    const li = document.createElement("li");
    if (i === state.selectedCandidate) li.className = "selected";
    const loc = c.kind === "point"
      ? `(${fmt(c.x, 2)}, ${fmt(c.y, 2)}) m`
      : `${c.region ? c.region.shape : "region"} 区域`;
    li.innerHTML = `#${i} ${loc}
      <span class="tag ${c.status}">${c.status}</span>
      ${c.kind === "region" ? '<span class="tag region">region</span>' : ""}
      <div style="color:#516173;font-weight:400">${c.reason}</div>`;
    li.onclick = () => { state.selectedCandidate = i; renderCandidates(); renderResiduals(); renderSpace(); };
    ol.appendChild(li);
  });
  renderResiduals();
}

function renderResiduals() {
  const body = document.getElementById("residual-body");
  const c = state.detail.candidates[state.selectedCandidate];
  if (!c) { body.textContent = "无可审核候选"; return; }
  const m = c.metrics || {};
  let html = `
    <div style="margin-bottom:6px">
      RMS <b>${fmt(m.rms_nsigma, 2)} σ</b> ·
      最大残差 <b>${fmt(m.max_residual_s, 4)} s</b> ·
      χ² ${fmt(m.chi2, 2)}
    </div>
    <table><thead><tr>
      <th>边</th><th>实测 s</th><th>预测 s</th><th>残差 s</th><th>σ</th><th>nσ</th>
    </tr></thead><tbody>`;
  for (const r of c.residuals) {
    const cls = r.excluded ? "excluded" : (r.multipath ? "multipath" : "");
    html += `<tr class="${cls}" title="${r.kind}${r.multipath ? " · 点击可排除多径路径" : ""}">
      <td>${r.station_a}–${r.station_b}${r.multipath ? " ⚡" : ""}</td>
      <td>${fmt(r.measured_s, 4)}</td>
      <td>${fmt(r.predicted_s, 4)}</td>
      <td>${fmt(r.residual_s, 4)}</td>
      <td>${fmt(r.sigma_s, 4)}</td>
      <td>${fmt(r.n_sigma, 2)}</td>
    </tr>`;
  }
  html += "</tbody></table>";
  body.innerHTML = html;
}

function renderAll() {
  renderMeta();
  renderTimeline();
  renderSpace();
  renderCandidates();
  renderEvidence();
}

// ---- reviewer actions -----------------------------------------------------

document.getElementById("event-select").addEventListener("change", selectEvent);

document.getElementById("btn-solve").addEventListener("click", async () => {
  const id = state.detail.event.event_id;
  await post(`/api/events/${encodeURIComponent(id)}/solve`);
  await selectEvent();
});

document.getElementById("btn-lock").addEventListener("click", async () => {
  const sid = prompt("锁定哪个站点的时钟校正？(station id)");
  if (!sid) return;
  const offset = Number(prompt("校正量 offset_s（加到设备时钟，秒）"));
  if (!isFinite(offset)) return;
  const id = state.detail.event.event_id;
  await post(`/api/events/${encodeURIComponent(id)}/evidence`, {
    kind: "lock_clock",
    payload: { station_id: sid, offset_s: offset, sigma_s: 0.0002 },
  });
  await selectEvent();
});

document.getElementById("btn-keep").addEventListener("click", async () => {
  const cands = state.detail.candidates.filter((c) => c.kind === "point");
  if (cands.length < 2) { alert("需要至少两个点候选"); return; }
  const a = prompt("保留候选 A 的序号", "0");
  const b = prompt("保留候选 B 的序号", String(Math.min(1, cands.length - 1)));
  const ca = cands[Number(a)], cb = cands[Number(b)];
  if (!ca || !cb) return;
  const id = state.detail.event.event_id;
  await post(`/api/events/${encodeURIComponent(id)}/evidence`, {
    kind: "keep_pair",
    payload: { candidate_a_key: ca.cand_key, candidate_b_key: cb.cand_key },
  });
  await selectEvent();
});

// Click a multipath-suspect edge row in the residual table to distinguish the
// reflected path from direct evidence (append-only derived evidence).
document.getElementById("residual-body").addEventListener("click", async (ev) => {
  const row = ev.target.closest("tr");
  if (!row) return;
  const c = state.detail.candidates[state.selectedCandidate];
  const idx = Array.prototype.indexOf.call(row.parentNode.children, row);
  const r = c.residuals[idx];
  if (!r || !r.multipath) return;
  if (r.excluded) { alert("该反射路径已被证据排除"); return; }
  const peak = Number(prompt("排除哪个互相关峰序号 peak_index？", "0"));
  if (!isFinite(peak)) return;
  const ok = confirm(`排除 ${r.station_a}-${r.station_b} 的该多径路径（peak_index=${peak}）？`);
  if (!ok) return;
  const id = state.detail.event.event_id;
  await post(`/api/events/${encodeURIComponent(id)}/evidence`, {
    kind: "exclude_peak",
    payload: { station_a: r.station_a, station_b: r.station_b, peak_index: peak },
  });
  await selectEvent();
});

loadEvents().catch((e) => alert(e.message));
