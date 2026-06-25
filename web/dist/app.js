// NightKnight SPA. Talks to the v4 API (same-origin, credentials included so the
// Cloudflare Access / proxy session cookie rides along). Three views — Dashboard,
// Analysis, Settings — switched client-side; the visible view is polled for fresh data.

import { drawGlucoseChart, renderAgp } from "/chart.js";

const MGDL_PER_MMOL = 18.0156;

const state = {
  view: (location.hash || "#dashboard").slice(1),
  unit: localStorage.getItem("nk-unit") || "mg/dl",
  periodDays: 7,          // dashboard trailing summary
  aPeriodDays: 14,        // analysis page
  chartHours: 24,
  provider: "dexcom",
  targetLow: Number(localStorage.getItem("nk-target-low")) || 70,   // mg/dL (chart band)
  targetHigh: Number(localStorage.getItem("nk-target-high")) || 180, // mg/dL
  tzOffset: -new Date().getTimezoneOffset(), // minutes east of UTC
  entries: [],
};

const $ = (s) => document.querySelector(s);
const $$ = (s) => document.querySelectorAll(s);

// Build an element safely. `text` is set via textContent (never parsed as HTML), so
// user/vendor-controlled strings can't inject markup.
function el(tag, { class: cls, text } = {}, children = []) {
  const node = document.createElement(tag);
  if (cls != null) node.className = cls;
  if (text != null) node.textContent = text;
  for (const c of children) node.appendChild(c);
  return node;
}

async function api(path, opts = {}) {
  const res = await fetch(path, { credentials: "include", headers: { "content-type": "application/json" }, ...opts });
  if (res.status === 401) { showBanner("Not signed in. Open this app through the authenticated URL."); throw new Error("unauthorized"); }
  if (res.status === 403) { showBanner("Access denied — you're not in the night_knight_users group."); throw new Error("forbidden"); }
  if (!res.ok) throw new Error(`${path} → ${res.status}`);
  return res.status === 204 ? null : res.json();
}

function showBanner(msg) { const b = $("#banner"); b.textContent = msg; b.hidden = false; }
function clearBanner() { $("#banner").hidden = true; }

// ── formatting ─────────────────────────────────────────────────────────────
const unitName = () => (state.unit === "mmol/l" ? "mmol/L" : "mg/dL");
function fmtGlu(mgdl) {
  if (mgdl == null) return "--";
  return state.unit === "mmol/l" ? (mgdl / MGDL_PER_MMOL).toFixed(1) : String(Math.round(mgdl));
}
function bandClass(mgdl) { if (mgdl < 54 || mgdl > 250) return "danger"; if (mgdl < state.targetLow || mgdl > state.targetHigh) return "warn"; return "inrange"; }
function fmtAgo(ms) {
  if (!ms) return "never";
  const m = Math.round((Date.now() - ms) / 60000);
  if (m < 1) return "just now"; if (m < 60) return `${m} min ago`;
  return `${Math.round(m / 60)} h ago`;
}
function fmtClock(ms) { const d = new Date(ms); return `${["Sun","Mon","Tue","Wed","Thu","Fri","Sat"][d.getDay()]} ${String(d.getHours()).padStart(2,"0")}:${String(d.getMinutes()).padStart(2,"0")}`; }
function fmtDur(min) { const m = Math.round(min); return m >= 60 ? `${Math.floor(m / 60)}h ${m % 60}m` : `${m}m`; }

// Plain-language explanations surfaced through the (?) tooltips.
const TIPS = {
  gmi: "Glucose Management Indicator — a modern, consensus-preferred estimate of lab A1c from your average glucose (GMI% = 3.31 + 0.02392 × mean mg/dL).",
  ea1c: "The older ADAG estimate of A1c from average glucose ((mean + 46.7) ÷ 28.7). Kept for compatibility; it can differ from GMI.",
  mean: "Your average sensor glucose over the period.",
  sd: "Standard deviation — the absolute spread of glucose around the mean. Lower is steadier.",
  cv: "Coefficient of variation (SD ÷ mean). The standard stability measure; ≤ 36% is considered stable.",
  tir: "Share of readings in the 70–180 mg/dL target range — the most actionable CGM metric. Aim for > 70%.",
  active: "Percent of the period with CGM data. Metrics are most reliable over ≥ 14 days with > 70% active.",
  jindex: "J-index — one severity score combining mean and SD: 0.001 × (mean + SD)². Non-diabetic reference ≈ 4.7–23.6.",
  mage: "Mean Amplitude of Glycemic Excursions — the average size of your meaningful swings (those larger than 1 SD).",
  conga: "CONGA(2h) — Continuous Overall Net Glycemic Action: within-day variability, the spread of glucose differences 2 hours apart.",
  avg: "Your average sensor glucose over the trailing period.",
};

function infoIcon(text) {
  const s = el("span", { class: "info", text: "?" });
  s.tabIndex = 0; s.setAttribute("role", "button"); s.setAttribute("aria-label", text);
  s.appendChild(el("span", { class: "tip", text }));
  return s;
}

function metricTile({ label, value, suffix = "", sub = "", tip, hero }) {
  const head = el("span", { class: "metric-label", text: label });
  if (tip) head.appendChild(infoIcon(tip));
  const val = el("span", { class: "metric-value" });
  val.appendChild(document.createTextNode(value));
  if (suffix) val.appendChild(el("small", { text: suffix }));
  return el("div", { class: "metric" + (hero ? " metric-hero" : "") }, [head, val, el("span", { class: "metric-sub", text: sub })]);
}

// ── routing ────────────────────────────────────────────────────────────────
function setView(view) {
  if (!["dashboard", "analysis", "settings"].includes(view)) view = "dashboard";
  state.view = view;
  if (location.hash !== `#${view}`) location.hash = view;
  $$("[data-page]").forEach((p) => (p.hidden = p.dataset.page !== view));
  $$(".tab").forEach((t) => t.classList.toggle("is-active", t.dataset.view === view));
  if (view === "dashboard") { refreshLive(); refreshSummary(); }
  else if (view === "analysis") refreshAnalysis();
  else if (view === "settings") { loadConnectors(); loadTokens(); syncTargetInputs(); }
}

// ── dashboard: current reading ───────────────────────────────────────────────
function renderCurrent(current) {
  const hero = $("#hero");
  if (!current) {
    $("#bg-value").textContent = "--"; $("#bg-trend").textContent = "·";
    $("#bg-age").textContent = "no data"; $("#bg-delta").textContent = "";
    hero.removeAttribute("data-band"); return;
  }
  const mgdl = current.mgdl;
  $("#bg-value").textContent = fmtGlu(mgdl);
  $("#bg-unit").textContent = unitName();
  $("#bg-trend").textContent = current.trend && current.trend !== "–" ? current.trend : "";
  hero.setAttribute("data-band", bandClass(mgdl));
  const ageMin = Math.round((Date.now() - current.date) / 60000);
  $("#bg-age").textContent = ageMin <= 0 ? "just now" : `${ageMin} min ago`;
  $("#bg-delta").textContent = current.trendLabel && current.trendLabel !== "No trend" ? current.trendLabel : "";
}

function renderSummary(a) {
  const grid = $("#summary-grid");
  grid.innerHTML = "";
  const cv = a.cvPercent;
  grid.append(
    metricTile({ label: "Est. A1c", value: a.gmiPercent == null ? "--" : a.gmiPercent.toFixed(1), suffix: "%", sub: a.estimatedA1cPercent == null ? "GMI —" : `eA1c ${a.estimatedA1cPercent.toFixed(1)}%`, tip: TIPS.gmi, hero: true }),
    metricTile({ label: "Avg glucose", value: fmtGlu(a.meanMgdl), suffix: unitName(), sub: `${a.n} readings`, tip: TIPS.avg, hero: true }),
    metricTile({ label: "In range", value: a.timeInRange.inRangePct == null ? "--" : a.timeInRange.inRangePct.toFixed(0), suffix: "%", sub: "70–180 target", tip: TIPS.tir }),
    metricTile({ label: "Variability (CV)", value: cv == null ? "--" : cv.toFixed(0), suffix: "%", sub: cv == null ? "—" : cv <= 36 ? "stable" : "variable", tip: TIPS.cv }),
  );
  renderTirBar(a.timeInRange);
}

function renderTirBar(tir) {
  const segs = [
    ["veryLowPct", "var(--g-danger)", "Very low <54"],
    ["lowPct", "var(--g-warn)", "Low 54–69"],
    ["inRangePct", "var(--g-inrange)", "In range 70–180"],
    ["highPct", "var(--g-warn)", "High 181–250"],
    ["veryHighPct", "var(--g-danger)", "Very high >250"],
  ];
  const bar = $("#tir-bar"), legend = $("#tir-legend");
  bar.innerHTML = ""; legend.innerHTML = "";
  for (const [key, color, label] of segs) {
    const pct = tir[key] || 0;
    if (pct > 0) {
      const seg = el("div", { class: "tir-seg" });
      seg.style.width = `${pct}%`; seg.style.background = color; seg.title = `${label}: ${pct.toFixed(0)}%`;
      bar.appendChild(seg);
    }
    const li = el("span");
    const dot = el("span", { class: "dot" }); dot.style.background = color;
    li.append(dot, document.createTextNode(`${label}: ${pct.toFixed(0)}%`));
    legend.appendChild(li);
  }
}

// ── dashboard: chart + hover ─────────────────────────────────────────────────
let crosshair, hoverDot;
function ensureChartOverlay() {
  const wrap = $("#chart-wrap");
  if (crosshair) return;
  crosshair = el("div"); crosshair.className = "chart-cross"; crosshair.hidden = true;
  crosshair.style.cssText = "position:absolute;top:0;bottom:0;width:1px;background:rgba(255,255,255,0.25);pointer-events:none;";
  hoverDot = el("div"); hoverDot.hidden = true;
  hoverDot.style.cssText = "position:absolute;width:9px;height:9px;border-radius:50%;transform:translate(-50%,-50%);box-shadow:0 0 0 2px var(--ink);pointer-events:none;";
  wrap.append(crosshair, hoverDot);
  wrap.addEventListener("mousemove", onChartHover);
  wrap.addEventListener("mouseleave", () => { for (const e of [crosshair, hoverDot, $("#chart-tip")]) e.hidden = true; });
}
function onChartHover(e) {
  const canvas = $("#chart"); const geom = canvas._geom; const tip = $("#chart-tip");
  if (!geom || !geom.points.length) return;
  const rect = canvas.getBoundingClientRect();
  const mx = e.clientX - rect.left;
  let best = null, bd = Infinity;
  for (const p of geom.points) { const d = Math.abs(p.x - mx); if (d < bd) { bd = d; best = p; } }
  if (!best) return;
  crosshair.hidden = false; crosshair.style.left = `${best.x}px`;
  hoverDot.hidden = false; hoverDot.style.left = `${best.x}px`; hoverDot.style.top = `${best.y}px`;
  hoverDot.style.background = best.mgdl < 54 || best.mgdl > 250 ? "var(--g-danger)" : best.mgdl < state.targetLow || best.mgdl > state.targetHigh ? "var(--g-warn)" : "var(--g-inrange)";
  tip.hidden = false; tip.innerHTML = "";
  tip.append(el("span", { class: "ct-val", text: `${fmtGlu(best.mgdl)} ${unitName()}` }), el("span", { class: "ct-time", text: fmtClock(best.date) }));
  let tx = best.x; const tw = tip.offsetWidth;
  tx = Math.max(tw / 2 + 2, Math.min(rect.width - tw / 2 - 2, tx));
  tip.style.left = `${tx}px`; tip.style.top = `${Math.max(26, best.y - 6)}px`;
}
function renderChart() {
  $("#chart-empty").hidden = state.entries.length > 0;
  ensureChartOverlay();
  drawGlucoseChart($("#chart"), { entries: state.entries, hours: state.chartHours, low: state.targetLow, high: state.targetHigh, unit: state.unit, now: Date.now() });
}

// ── analysis page ────────────────────────────────────────────────────────────
function renderAnalysis(a) {
  const cov = a.coverage || {};
  $("#a-caption").textContent = `Based on ${a.n.toLocaleString()} readings over ${a.aPeriodDays || state.aPeriodDays} days · ${cov.percentActive == null ? "—" : cov.percentActive.toFixed(0)}% active${cov.sufficient ? "" : " · limited data"}`;

  // GRI card.
  const gri = a.gri || {};
  const zoneColor = { A: "#34d36b", B: "#9bd34a", C: "#e6c93c", D: "#e6a93c", E: "#f0555a" }[gri.zone] || "#7b8493";
  const griBody = $("#gri-body"); griBody.innerHTML = "";
  const num = el("span", { class: "gri-num", text: gri.value == null ? "--" : gri.value.toFixed(0) });
  num.style.color = zoneColor;
  const badgeDot = el("span", { class: "zone-dot" }); badgeDot.style.background = zoneColor;
  const badge = el("span", { class: "zone-badge" }, [badgeDot, el("span", { text: `Zone ${gri.zone || "—"}` })]);
  griBody.append(el("div", { class: "gri-top" }, [num, badge]), el("div", { class: "gri-hint", text: "0 = lowest risk · lower is better" }));
  const comp = el("div", { class: "gri-comp" });
  comp.append(
    griCompRow("Hypoglycemia", gri.hypoComponent, "#e6a93c"),
    griCompRow("Hyperglycemia", gri.hyperComponent, "#f0555a"),
  );
  griBody.appendChild(comp);

  // Core metrics.
  const core = $("#core-grid"); core.innerHTML = "";
  const cv = a.cvPercent;
  core.append(
    metricTile({ label: "Mean Glucose", value: fmtGlu(a.meanMgdl), suffix: unitName(), sub: `${a.n.toLocaleString()} readings`, tip: TIPS.mean }),
    metricTile({ label: "GMI", value: a.gmiPercent == null ? "--" : a.gmiPercent.toFixed(1), suffix: "%", sub: "mgmt indicator", tip: TIPS.gmi }),
    metricTile({ label: "eA1c (legacy)", value: a.estimatedA1cPercent == null ? "--" : a.estimatedA1cPercent.toFixed(1), suffix: "%", sub: "ADAG estimate", tip: TIPS.ea1c }),
    metricTile({ label: "SD", value: fmtGlu(a.sdMgdl), suffix: unitName(), sub: "std deviation", tip: TIPS.sd }),
    metricTile({ label: "CV", value: cv == null ? "--" : cv.toFixed(0), suffix: "%", sub: cv == null ? "—" : cv <= 36 ? "stable" : "unstable", tip: TIPS.cv }),
    metricTile({ label: "Time Active", value: cov.percentActive == null ? "--" : cov.percentActive.toFixed(0), suffix: "%", sub: cov.sufficient ? "sufficient" : "limited", tip: TIPS.active }),
  );

  // Time-of-day patterns.
  const todNames = ["Overnight", "Morning", "Afternoon", "Evening"];
  const tod = $("#tod-grid"); tod.innerHTML = "";
  (a.patterns || []).forEach((p, i) => {
    const sub = `${String(p.startHour).padStart(2, "0")}–${String(p.endHour).padStart(2, "0")}`;
    const card = el("div", { class: "metric" }, [
      el("span", { class: "metric-label", text: `${todNames[i]} · ${sub}` }),
      (() => { const v = el("span", { class: "metric-value" }); v.append(document.createTextNode(fmtGlu(p.meanMgdl)), el("small", { text: unitName() })); return v; })(),
      el("span", { class: "metric-sub", text: p.meanMgdl == null ? "no data" : `${p.inRangePct.toFixed(0)}% in range` }),
    ]);
    tod.appendChild(card);
  });

  // Episodes.
  const ep = a.episodes || {};
  const days = (a.coverage && a.coverage.distinctDays) || (a.aPeriodDays || state.aPeriodDays) || 1;
  const body = $("#episodes-body"); body.innerHTML = "";
  const stats = el("div", { class: "ep-stats" });
  stats.append(
    epStat(ep.low?.count ?? 0, `low events · ${(ep.low?.perDay ?? 0).toFixed(1)}/day`, "#e6a93c"),
    epStat(ep.low?.nocturnal ?? 0, "nocturnal lows", "#6f8cff"),
    epStat(ep.high?.count ?? 0, `high events · ${(ep.high?.perDay ?? 0).toFixed(1)}/day`, "#f0555a"),
  );
  body.appendChild(stats);
  body.appendChild(el("p", { class: "ep-summary", text: `Longest low ${fmtDur(ep.low?.longestMin ?? 0)} · ${ep.veryLow?.count ?? 0} severe (<54)` }));
  const list = el("div", { class: "ep-list" });
  for (const e of ep.recent || []) {
    const c = e.kind === "low" ? "#e6a93c" : "#f0555a";
    const dot = el("span", { class: "ep-dot" }); dot.style.background = c;
    const detail = `${e.kind === "low" ? "down to" : "up to"} ${fmtGlu(e.extremeMgdl)} ${unitName()} · ${fmtDur(e.durationMin)}`;
    list.appendChild(el("div", { class: "ep-row" }, [
      dot,
      el("span", { class: "ep-kind", text: e.kind === "low" ? "Low" : "High" }),
      el("span", { class: "ep-time", text: fmtClock(e.start) }),
      el("span", { class: "ep-detail", text: detail }),
    ]));
  }
  if (!(ep.recent || []).length) list.appendChild(el("p", { class: "muted", text: "No episodes in this period — nice." }));
  body.appendChild(list);

  // Advanced variability.
  const v = a.variability || {};
  const adv = $("#adv-grid"); adv.innerHTML = "";
  adv.append(
    metricTile({ label: "J-Index", value: v.jIndex == null ? "--" : v.jIndex.toFixed(1), sub: "mean + SD severity", tip: TIPS.jindex }),
    metricTile({ label: "MAGE", value: fmtGlu(v.mage), suffix: unitName(), sub: "mean large swing", tip: TIPS.mage }),
    metricTile({ label: `CONGA-${v.congaHours ?? 2}h`, value: fmtGlu(v.conga), suffix: unitName(), sub: "within-day variability", tip: TIPS.conga }),
    metricTile({ label: "SD", value: fmtGlu(a.sdMgdl), suffix: unitName(), sub: "absolute spread", tip: TIPS.sd }),
  );
}
function griCompRow(label, value, color) {
  const head = el("div", { class: "gri-comp-head" }, [
    el("span", { class: "lbl", text: label }),
    el("span", { class: "val", text: value == null ? "—" : value.toFixed(1) }),
  ]);
  head.querySelector(".val").style.color = color;
  const fill = el("div"); fill.style.width = `${Math.min(100, (value || 0) * 2.5)}%`; fill.style.background = color;
  const bar = el("div", { class: "gri-bar" }, [fill]);
  return el("div", { class: "gri-comp-row" }, [head, bar]);
}
function epStat(n, cap, color) {
  const num = el("div", { class: "ep-n", text: String(n) }); num.style.color = color;
  return el("div", { class: "ep-stat" }, [num, el("div", { class: "ep-cap", text: cap })]);
}

// ── data flow ────────────────────────────────────────────────────────────────
async function refreshLive() {
  if (state.view !== "dashboard") return;
  try {
    const [current, entries] = await Promise.all([api("/api/v4/current"), api(`/api/v4/entries?hours=${state.chartHours}`)]);
    clearBanner();
    state.entries = (entries.entries || []).map((e) => ({ date: e.date, mgdl: e.mgdl }));
    renderCurrent(current.current);
    renderChart();
  } catch (e) { if (!/unauthorized|forbidden/.test(String(e))) console.error(e); }
}
async function refreshSummary() {
  if (state.view !== "dashboard") return;
  try { renderSummary(await api(`/api/v4/analytics?hours=${state.periodDays * 24}&tzOffset=${state.tzOffset}`)); }
  catch (e) { if (!/unauthorized|forbidden/.test(String(e))) console.error(e); }
}
async function refreshAnalysis() {
  if (state.view !== "analysis") return;
  try {
    const a = await api(`/api/v4/analytics?hours=${state.aPeriodDays * 24}&tzOffset=${state.tzOffset}`);
    a.aPeriodDays = state.aPeriodDays;
    renderAnalysis(a);
    const agp = await api(`/api/v4/agp?days=${state.aPeriodDays}&tzOffset=${state.tzOffset}`);
    $("#agp-caption").textContent = `${state.aPeriodDays} days overlaid on a 24-hour day`;
    renderAgp($("#agp-wrap"), { bins: agp.bins, low: state.targetLow, high: state.targetHigh, unit: state.unit });
  } catch (e) { if (!/unauthorized|forbidden/.test(String(e))) console.error(e); }
}

async function loadMe() {
  try {
    const me = await api("/api/v4/me");
    $("#user-email").textContent = me.displayName || me.subject || "";
    if (me.preferredUnit) { state.unit = me.preferredUnit; localStorage.setItem("nk-unit", state.unit); }
  } catch (_) {}
  $$(".unit-toggle").forEach((c) => syncActive(c, "unit", state.unit));
  syncTargetInputs();
}

// ── settings: connectors & tokens ─────────────────────────────────────────────
async function loadConnectors() {
  try {
    const { connectors } = await api("/api/v4/connectors");
    const list = $("#connector-list"); list.innerHTML = "";
    const names = { dexcom: "Dexcom Share", librelinkup: "LibreLinkUp", nightscout: "Nightscout" };
    for (const c of connectors) {
      const ok = (c.lastStatus || "").startsWith("ok");
      const dot = c.lastStatus ? (ok ? "ok" : "err") : "";
      const wrap = el("span", {}, [
        el("span", { class: `s-dot ${dot}` }),
        el("span", { class: "s-title", text: names[c.provider] || c.provider }),
        el("div", { class: "s-sub", text: `${c.lastStatus || "awaiting first sync"} · ${fmtAgo(c.lastSyncAt)}` }),
      ]);
      const li = el("li", {}, [wrap]);
      const btn = el("button", { text: "Remove" });
      btn.onclick = async () => { await api(`/api/v4/connectors/${c.provider}`, { method: "DELETE" }); loadConnectors(); };
      li.appendChild(btn); list.appendChild(li);
    }
  } catch (_) {}
}
async function submitConnector(e) {
  e.preventDefault();
  const provider = state.provider;
  const user = $("#c-user").value.trim(); const pass = $("#c-pass").value;
  const body = provider === "dexcom"
    ? { username: user, password: pass, region: $("#c-region").value }
    : provider === "nightscout"
    ? { url: user, secret: pass }
    : { email: user, password: pass };
  const label = { dexcom: "Dexcom", librelinkup: "LibreLinkUp", nightscout: "Nightscout" }[provider] || provider;
  try {
    await api(`/api/v4/connectors/${provider}`, { method: "PUT", body: JSON.stringify(body) });
    $("#c-pass").value = "";
    showBanner(`${label} connected — first sync within a minute.`);
    loadConnectors();
  } catch (err) { showBanner(`Could not connect: ${err}`); }
}

// ---- Glucose CSV import (LibreView / Dexcom Clarity, auto-detected) -------
async function importLibreView(file) {
  const status = $("#import-status");
  status.textContent = `Importing ${file.name}…`;
  try {
    const text = await file.text();
    const res = await api(`/api/v4/import/csv?tzOffset=${state.tzOffset}`, {
      method: "POST", headers: { "content-type": "text/csv" }, body: text,
    });
    const plural = (n) => (n === 1 ? "" : "s");
    const from = res.source === "dexcom" ? "Dexcom Clarity" : "LibreView";
    status.textContent = `Imported ${res.imported} reading${plural(res.imported)} (${res.unit}) from ${res.rows} ${from} rows — ${res.duplicates} already present${res.rejected ? `, ${res.rejected} rejected` : ""}.`;
    if (state.view === "dashboard") { refreshLive(); refreshSummary(); }
  } catch (e) {
    status.textContent = `Import failed: ${e}`;
  } finally {
    $("#import-file").value = ""; // allow re-importing the same file
  }
}
async function loadTokens() {
  try {
    const { tokens } = await api("/api/v4/tokens");
    const list = $("#token-list"); list.innerHTML = "";
    for (const t of tokens.filter((t) => !t.revoked)) {
      const wrap = el("span", {}, [el("span", { class: "s-title", text: t.name }), el("div", { class: "s-sub", text: t.scopes.join(", ") })]);
      const li = el("li", {}, [wrap]);
      const btn = el("button", { text: "Revoke" });
      btn.onclick = async () => { await api(`/api/v4/tokens/${t.id}`, { method: "DELETE" }); loadTokens(); };
      li.appendChild(btn); list.appendChild(li);
    }
  } catch (_) {}
}
async function createToken(e) {
  e.preventDefault();
  const name = $("#token-name").value.trim() || "device";
  const scopes = $("#token-scope").value === "upload"
    ? ["api:entries:create", "api:entries:read", "api:treatments:create", "api:treatments:read", "api:devicestatus:create"]
    : ["api:entries:read", "api:treatments:read"];
  const res = await api("/api/v4/tokens", { method: "POST", body: JSON.stringify({ name, scopes }) });
  const reveal = $("#token-reveal"); reveal.hidden = false;
  reveal.textContent = `Token for "${name}" (copy now — shown once): ${res.token}`;
  $("#token-name").value = ""; loadTokens();
}

// ── settings: display & targets ───────────────────────────────────────────────
function syncTargetInputs() {
  $$(".unit-name").forEach((s) => (s.textContent = unitName()));
  $("#target-low").value = fmtGlu(state.targetLow);
  $("#target-high").value = fmtGlu(state.targetHigh);
  $("#target-note").textContent = `In-range time on the dashboard uses the consensus 70–180 mg/dL range; your target here shades the chart band.`;
}
function readTarget(input, fallbackMgdl) {
  const v = parseFloat(input.value);
  if (!isFinite(v)) return fallbackMgdl;
  return state.unit === "mmol/l" ? v * MGDL_PER_MMOL : v;
}

// ── wiring ─────────────────────────────────────────────────────────────────
function syncActive(container, dataKey, value) {
  container.querySelectorAll(".seg-btn").forEach((b) => b.classList.toggle("is-active", b.dataset[dataKey] === String(value)));
}
function onUnitChange() {
  $$(".unit-toggle").forEach((c) => syncActive(c, "unit", state.unit));
  syncTargetInputs();
  renderChart();
  if (state.view === "dashboard") { refreshLive(); refreshSummary(); }
  else if (state.view === "analysis") refreshAnalysis();
}
function wire() {
  $$(".tab").forEach((t) => t.addEventListener("click", () => setView(t.dataset.view)));
  window.addEventListener("hashchange", () => setView((location.hash || "#dashboard").slice(1)));

  $$(".unit-toggle .seg-btn").forEach((b) => b.addEventListener("click", async () => {
    state.unit = b.dataset.unit; localStorage.setItem("nk-unit", state.unit);
    onUnitChange();
    try { await api("/api/v4/me", { method: "PUT", body: JSON.stringify({ preferredUnit: state.unit }) }); } catch (_) {}
  }));
  $$(".period-toggle .seg-btn").forEach((b) => b.addEventListener("click", () => {
    state.periodDays = Number(b.dataset.days); syncActive($(".period-toggle"), "days", state.periodDays); refreshSummary();
  }));
  $$(".aperiod-toggle .seg-btn").forEach((b) => b.addEventListener("click", () => {
    state.aPeriodDays = Number(b.dataset.days); syncActive($(".aperiod-toggle"), "days", state.aPeriodDays); refreshAnalysis();
  }));
  $$(".range-toggle .seg-btn").forEach((b) => b.addEventListener("click", () => {
    state.chartHours = Number(b.dataset.hours); syncActive($(".range-toggle"), "hours", state.chartHours); refreshLive();
  }));
  $$(".provider-toggle .seg-btn").forEach((b) => b.addEventListener("click", () => {
    state.provider = b.dataset.provider; syncActive($(".provider-toggle"), "provider", state.provider);
    const dex = state.provider === "dexcom", ns = state.provider === "nightscout";
    $(".dexcom-only").style.display = dex ? "" : "none";
    $("#nightscout-hint").hidden = !ns;
    $("#c-user").placeholder = ns ? "https://your-nightscout.example.com" : dex ? "Username" : "Email";
    $("#c-pass").placeholder = ns ? "API secret" : "Password";
  }));
  $("#connector-form").addEventListener("submit", submitConnector);
  $("#import-file").addEventListener("change", (e) => { const f = e.target.files[0]; if (f) importLibreView(f); });
  $("#token-form").addEventListener("submit", createToken);

  const saveTargets = () => {
    state.targetLow = Math.round(Math.max(40, Math.min(180, readTarget($("#target-low"), state.targetLow))));
    state.targetHigh = Math.round(Math.max(120, Math.min(350, readTarget($("#target-high"), state.targetHigh))));
    localStorage.setItem("nk-target-low", state.targetLow);
    localStorage.setItem("nk-target-high", state.targetHigh);
    renderChart();
  };
  $("#target-low").addEventListener("change", saveTargets);
  $("#target-high").addEventListener("change", saveTargets);

  window.addEventListener("resize", () => { if (state.view === "dashboard") renderChart(); else if (state.view === "analysis") refreshAnalysis(); });
}

async function main() {
  wire();
  await loadMe();
  setView(state.view);
  setInterval(() => {
    if (state.view === "dashboard") { refreshLive(); refreshSummary(); }
    else if (state.view === "analysis") refreshAnalysis();
    if (state.view === "settings") loadConnectors();
  }, 60_000);
}

main();
