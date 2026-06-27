// NightKnight SPA. Talks to the v4 API (same-origin, credentials included so the
// Cloudflare Access / proxy session cookie rides along). Three views — Dashboard,
// Analysis, Settings — switched client-side; the visible view is polled for fresh data.

import { drawGlucoseChart, renderAgp } from "/chart.js";

const MGDL_PER_MMOL = 18.0156;

// Plain-language trend labels matching the CGM ecosystem (Dexcom/Libre wording). The
// server now sends `trendLabel`; this map is a fallback for older servers and keeps
// the wire `direction` (DoubleUp, …) out of the UI.
const TREND_LABELS = {
  DoubleUp: "Rising rapidly", SingleUp: "Rising", FortyFiveUp: "Rising slowly",
  Flat: "Steady", FortyFiveDown: "Falling slowly", SingleDown: "Falling",
  DoubleDown: "Falling rapidly", "RATE OUT OF RANGE": "Rate out of range",
};

// Glucose **level** status (separate dimension from trend). Fallback for older servers
// that don't send `levelLabel`.
function levelLabelFromMgdl(mgdl) {
  if (mgdl < 54) return "Urgent low";
  if (mgdl < state.targetLow) return "Low";
  if (mgdl <= state.targetHigh) return "In range";
  if (mgdl <= 250) return "High";
  return "Urgent high";
}

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
  calMode: localStorage.getItem("nk-cal-mode") || "coverage", // data calendar colouring
  daysData: null, // last /days payload, so the calendar toggle re-renders without a fetch
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
  ugmi: "Updated GMI (uGMI) — the 2026 consensus revision of GMI that aligns more closely with lab A1c, especially at lower averages. uGMI% = 1 / (15.36 / mean mg/dL + 0.0425) (Bergenstal et al., Diabetologia 2026). This is the A1c estimate NightKnight leads with.",
  gmi: "Glucose Management Indicator (2018) — the original estimate of lab A1c from average glucose (GMI% = 3.31 + 0.02392 × mean mg/dL). Shown for comparison; prefer uGMI.",
  ea1c: "The older ADAG estimate of A1c from average glucose ((mean + 46.7) ÷ 28.7). Kept for compatibility; it can differ from GMI/uGMI.",
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

function metricTile({ label, value, suffix = "", sub = "", tip, hero, exact }) {
  // `exact` keeps the label's own casing (for uGMI / GMI / eA1c, where the precise
  // spelling is what tells the reader which A1c estimate they're looking at).
  const head = el("span", { class: "metric-label" + (exact ? " exact" : ""), text: label });
  if (tip) head.appendChild(infoIcon(tip));
  const val = el("span", { class: "metric-value" });
  val.appendChild(document.createTextNode(value));
  if (suffix) val.appendChild(el("small", { text: suffix }));
  return el("div", { class: "metric" + (hero ? " metric-hero" : "") }, [head, val, el("span", { class: "metric-sub", text: sub })]);
}

// ── routing ────────────────────────────────────────────────────────────────
function setView(view) {
  if (!["dashboard", "analysis", "data", "settings"].includes(view)) view = "dashboard";
  state.view = view;
  if (location.hash !== `#${view}`) location.hash = view;
  $$("[data-page]").forEach((p) => (p.hidden = p.dataset.page !== view));
  $$(".tab").forEach((t) => t.classList.toggle("is-active", t.dataset.view === view));
  if (view === "dashboard") { refreshLive(); refreshSummary(); }
  else if (view === "analysis") refreshAnalysis();
  else if (view === "data") refreshData();
  else if (view === "settings") { loadConnectors(); loadTokens(); syncTargetInputs(); }
}

// ── dashboard: current reading ───────────────────────────────────────────────
function renderCurrent(current) {
  const hero = $("#hero");
  if (!current) {
    $("#bg-value").textContent = "--"; $("#bg-trend").textContent = "·";
    $("#bg-age").textContent = "no data"; $("#bg-delta").textContent = "";
    $("#bg-level").textContent = ""; $("#bg-level").removeAttribute("data-band");
    hero.removeAttribute("data-band"); return;
  }
  const mgdl = current.mgdl;
  const band = bandClass(mgdl);
  $("#bg-value").textContent = fmtGlu(mgdl);
  $("#bg-unit").textContent = unitName();
  $("#bg-trend").textContent = current.trend && current.trend !== "–" ? current.trend : "";
  hero.setAttribute("data-band", band);
  const ageMin = Math.round((Date.now() - current.date) / 60000);
  $("#bg-age").textContent = ageMin <= 0 ? "just now" : `${ageMin} min ago`;
  // Level (Urgent low … Urgent high) and trend (Rising/Steady/Falling) are two
  // distinct things; show both, server-provided when available.
  const level = $("#bg-level");
  if (level) {
    level.textContent = current.levelLabel || levelLabelFromMgdl(mgdl);
    level.setAttribute("data-band", band);
  }
  $("#bg-delta").textContent = current.trendLabel && current.trendLabel !== "No trend" ? current.trendLabel : "";
}

// Colour a value by its glucose band (matches the chart + hero glow).
function bandColorVar(mgdl) {
  return mgdl < 54 || mgdl > 250 ? "var(--g-danger)"
    : mgdl < state.targetLow || mgdl > state.targetHigh ? "var(--g-warn)" : "var(--g-inrange)";
}

// Centred moving average — calms per-sample sensor noise so the spotlight line reads as a
// trend, not a scribble. The window narrows at the ends, so the final point stays faithful
// to the latest reading (no lag at "now").
function movingAverage(vals, win) {
  const n = vals.length, half = Math.floor(win / 2);
  return vals.map((_, i) => {
    let sum = 0, count = 0;
    for (let j = Math.max(0, i - half); j <= Math.min(n - 1, i + half); j++) { sum += vals[j]; count++; }
    return sum / count;
  });
}

// Catmull-Rom → cubic-bézier smoothing as an SVG path string (the same non-overshooting
// curve the main chart uses), so the spotlight line reads as a calm sweep, not jagged noise.
function smoothSvgPath(coords) {
  const f = (n) => n.toFixed(1);
  if (coords.length < 2) return coords.length ? `M${f(coords[0][0])} ${f(coords[0][1])}` : "";
  let d = `M${f(coords[0][0])} ${f(coords[0][1])}`;
  for (let i = 0; i < coords.length - 1; i++) {
    const p0 = coords[i - 1] || coords[i], p1 = coords[i], p2 = coords[i + 1], p3 = coords[i + 2] || p2;
    const c1x = p1[0] + (p2[0] - p0[0]) / 6, c1y = p1[1] + (p2[1] - p0[1]) / 6;
    const c2x = p2[0] - (p3[0] - p1[0]) / 6, c2y = p2[1] - (p3[1] - p1[1]) / 6;
    d += ` C${f(c1x)} ${f(c1y)} ${f(c2x)} ${f(c2y)} ${f(p2[0])} ${f(p2[1])}`;
  }
  return d;
}

// The spotlight sparkline: ONLY the last hour of readings — a calm smoothed line over a
// gradient that fades to nothing, emerging from a soft left edge (no hard cliff) and
// ending in a haloed dot at "now". Coloured by the latest band; hidden under two points.
function renderHeroSpark(entries) {
  const svg = $("#hero-spark");
  const cutoff = Date.now() - 3600000; // last 60 minutes only
  const pts = (entries || []).filter((e) => e.date >= cutoff).sort((a, b) => a.date - b.date);
  if (pts.length < 2) { svg.setAttribute("hidden", ""); svg.replaceChildren(); return; }
  svg.removeAttribute("hidden");
  // Draw in the element's own pixel space so the endpoint dot stays a circle (no stretch);
  // inset horizontally so the end dot + its halo have room and aren't clipped at the edge.
  const W = svg.clientWidth || 240, H = svg.clientHeight || 50, padX = 8, padY = 9;
  svg.setAttribute("viewBox", `0 0 ${W} ${H}`);
  const t0 = pts[0].date, t1 = Math.max(pts[pts.length - 1].date, t0 + 1);
  // Smooth the values (window scales with density: ~1 every 12 points), then plot — so a
  // dense 1-min stream reads as calmly as a sparse 5-min one.
  const smoothed = movingAverage(pts.map((p) => p.mgdl), Math.max(3, Math.round(pts.length / 12) | 1));
  const lo = Math.min(...smoothed) - 6, hi = Math.max(...smoothed) + 6, span = Math.max(1, hi - lo);
  const X = (t) => padX + ((t - t0) / (t1 - t0)) * (W - padX * 2);
  const Y = (v) => padY + (1 - (v - lo) / span) * (H - padY * 2);
  const coords = pts.map((p, i) => [X(p.date), Y(smoothed[i])]);
  const lineD = smoothSvgPath(coords);
  const f = (n) => n.toFixed(1);
  const [x0] = coords[0], last = coords[coords.length - 1];
  const color = bandColorVar(pts[pts.length - 1].mgdl);
  // Inline `style` (not presentation attrs) so the CSS var() colours resolve. The line +
  // fill ride a horizontal mask that fades them in from the left; the dot sits on top.
  svg.innerHTML =
    `<defs>` +
      `<linearGradient id="heroSparkGrad" x1="0" y1="0" x2="0" y2="1">` +
        `<stop offset="0" style="stop-color:${color};stop-opacity:.26"/>` +
        `<stop offset="1" style="stop-color:${color};stop-opacity:0"/>` +
      `</linearGradient>` +
      `<linearGradient id="heroSparkFade" x1="0" y1="0" x2="1" y2="0">` +
        `<stop offset="0" stop-color="#fff" stop-opacity="0"/>` +
        `<stop offset="0.22" stop-color="#fff" stop-opacity="1"/>` +
      `</linearGradient>` +
      `<mask id="heroSparkMask"><rect width="${W}" height="${H}" fill="url(#heroSparkFade)"/></mask>` +
    `</defs>` +
    `<g mask="url(#heroSparkMask)">` +
      `<path d="${lineD} L${f(last[0])} ${H} L${f(x0)} ${H} Z" style="fill:url(#heroSparkGrad)"/>` +
      `<path d="${lineD}" style="fill:none;stroke:${color};stroke-width:2.2px;opacity:.92" stroke-linecap="round" stroke-linejoin="round"/>` +
    `</g>` +
    `<circle cx="${f(last[0])}" cy="${f(last[1])}" r="6" style="fill:${color};opacity:.18"/>` +
    `<circle cx="${f(last[0])}" cy="${f(last[1])}" r="3" style="fill:${color}"/>`;
}

function renderSummary(a) {
  const grid = $("#summary-grid");
  grid.innerHTML = "";
  const cv = a.cvPercent;
  grid.append(
    metricTile({ label: "uGMI", exact: true, value: a.uGmiPercent == null ? "--" : a.uGmiPercent.toFixed(1), suffix: "%", sub: a.gmiPercent == null ? "updated GMI" : `GMI ${a.gmiPercent.toFixed(1)} · eA1c ${a.estimatedA1cPercent.toFixed(1)}`, tip: TIPS.ugmi, hero: true }),
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
    metricTile({ label: "uGMI", exact: true, value: a.uGmiPercent == null ? "--" : a.uGmiPercent.toFixed(1), suffix: "%", sub: "updated · preferred", tip: TIPS.ugmi }),
    metricTile({ label: "GMI", exact: true, value: a.gmiPercent == null ? "--" : a.gmiPercent.toFixed(1), suffix: "%", sub: "2018 estimate", tip: TIPS.gmi }),
    metricTile({ label: "eA1c", exact: true, value: a.estimatedA1cPercent == null ? "--" : a.estimatedA1cPercent.toFixed(1), suffix: "%", sub: "ADAG (legacy)", tip: TIPS.ea1c }),
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
    renderHeroSpark(state.entries);
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

// ── data coverage page ─────────────────────────────────────────────────────────
const DAY_MS = 86_400_000;
const MONTHS = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
const DOW = ["Sun","Mon","Tue","Wed","Thu","Fri","Sat"];

// Local weekday (0=Sun…6=Sat) of a day-number, matching the server's day_number basis
// (day 0 = 1970-01-01 = Thursday). The +700 keeps the modulo positive for any input.
const calWeekday = (idx) => (idx + 4 + 700) % 7;
// A day-number's civil date (UTC midnight of that local day — fine for labelling).
const dateOfIndex = (idx) => new Date(idx * DAY_MS);

async function refreshData() {
  if (state.view !== "data") return;
  try {
    const d = await api(`/api/v4/days?tzOffset=${state.tzOffset}`);
    clearBanner();
    state.daysData = d;
    renderData(d);
  } catch (e) { if (!/unauthorized|forbidden/.test(String(e))) console.error(e); }
}

// Coverage against the day's OWN expected reading count when the server provides it
// (`day.expectedPerDay`), falling back to the global rate for older servers. Per-day
// expectation stops a complete day from a slower-sensor era reading as under-covered.
function coverageFrac(day, expectedPerDay) {
  const expected = (day && day.expectedPerDay) || expectedPerDay;
  return Math.min(1, day.n / Math.max(1, expected));
}

function renderData(d) {
  renderDataSummary(d);
  renderCalendar(d);
  renderDayList(d);
  const cadMin = Math.max(1, Math.round((d.cadenceMs || 300000) / 60000));
  $("#data-note").textContent = (d.statsCapped
    ? `Per-day glucose stats cover the most recent ${(d.statsWindowReadings || 0).toLocaleString()} readings; older days list their reading count only. `
    : "") + `Coverage is measured against each day's own sensor cadence (most recently ≈${cadMin}-minute), so days from a different-cadence era aren't shown as under-covered. Not a medical device.`;
}

function renderDataSummary(d) {
  const grid = $("#data-summary");
  grid.innerHTML = "";
  if (!d.totalDays) {
    grid.appendChild(el("p", { class: "muted", text: "No readings imported yet. Add a CGM connection or import a CSV in Settings." }));
    return;
  }
  const days = d.days || [];
  const first = Math.min(...days.map((x) => x.dayIndex));
  const last = Math.max(...days.map((x) => x.dayIndex));
  const spanDays = last - first + 1;
  const gaps = spanDays - d.totalDays;
  const avgCov = days.reduce((s, x) => s + coverageFrac(x, d.expectedPerDay), 0) / days.length * 100;
  const w = d.windowStats || {};
  grid.append(
    metricTile({ label: "Days with data", value: d.totalDays.toLocaleString(), sub: `over ${spanDays} days · ${gaps} empty`, hero: true }),
    metricTile({ label: "Total readings", value: d.totalReadings.toLocaleString(), sub: `${d.firstDay || "—"} → ${d.lastDay || "—"}`, hero: true }),
    metricTile({ label: "Avg coverage", value: avgCov.toFixed(0), suffix: "%", sub: "of expected/day", tip: TIPS.active }),
    metricTile({ label: "uGMI", exact: true, value: w.uGmiPercent == null ? "--" : w.uGmiPercent.toFixed(1), suffix: "%", sub: "recent window", tip: TIPS.ugmi }),
    metricTile({ label: "Avg glucose", value: fmtGlu(w.meanMgdl), suffix: unitName(), sub: "recent window", tip: TIPS.avg }),
    metricTile({ label: "In range", value: w.timeInRange && w.timeInRange.inRangePct != null ? w.timeInRange.inRangePct.toFixed(0) : "--", suffix: "%", sub: "70–180 · recent", tip: TIPS.tir }),
  );
}

// Colour for a calendar cell, by the active mode. `day` may be undefined (no data).
function calCellColor(day, expectedPerDay) {
  if (!day) return "var(--cal-empty)";
  if (state.calMode === "glucose") {
    if (day.meanMgdl == null) return "var(--cal-nostat)";
    return bandColorVar(day.meanMgdl);
  }
  const f = coverageFrac(day, expectedPerDay);
  if (f <= 0.25) return "rgba(54,196,107,0.24)";
  if (f <= 0.5) return "rgba(54,196,107,0.45)";
  if (f <= 0.75) return "rgba(54,196,107,0.68)";
  return "rgba(54,196,107,0.95)";
}

// A GitHub-style contribution calendar as inline SVG: weeks are columns, weekdays rows.
// Scales cleanly to thousands of days (it just grows columns and scrolls horizontally).
function renderCalendar(d) {
  const wrap = $("#data-calendar");
  wrap.innerHTML = "";
  const days = d.days || [];
  if (!days.length) { wrap.appendChild(el("p", { class: "muted", text: "No readings to map yet." })); renderCalLegend(d); return; }
  const byIndex = new Map(days.map((x) => [x.dayIndex, x]));
  const first = Math.min(...days.map((x) => x.dayIndex));
  const last = Math.max(...days.map((x) => x.dayIndex));
  const gridStart = first - calWeekday(first);       // back up to that week's Sunday
  const cols = Math.floor((last - gridStart) / 7) + 1;
  const cell = 12, gap = 3, step = cell + gap, padL = 30, padT = 18;
  const W = padL + cols * step + 4, H = padT + 7 * step;

  let cells = "", months = "", prevMonth = -1, lastLabelCol = -3;
  for (let day = gridStart; day <= last; day++) {
    const col = Math.floor((day - gridStart) / 7), row = calWeekday(day);
    const x = padL + col * step, y = padT + row * step;
    const dd = byIndex.get(day);
    const color = calCellColor(dd, d.expectedPerDay);
    const dt = dateOfIndex(day);
    const label = dd
      ? `${DOW[dt.getUTCDay()]} ${dt.getUTCDate()} ${MONTHS[dt.getUTCMonth()]} ${dt.getUTCFullYear()} · ${dd.n} readings` +
        (dd.meanMgdl != null ? ` · avg ${fmtGlu(dd.meanMgdl)} ${unitName()} · uGMI ${dd.uGmiPercent.toFixed(1)}%` : "")
      : `${DOW[dt.getUTCDay()]} ${dt.getUTCDate()} ${MONTHS[dt.getUTCMonth()]} ${dt.getUTCFullYear()} · no data`;
    cells += `<rect x="${x}" y="${y}" width="${cell}" height="${cell}" rx="2.5" fill="${color}"><title>${label}</title></rect>`;
    // Month label at the first column of each new month, but never crowding the previous
    // label (≥3 columns apart) so the start of the strip doesn't print "JulAug".
    if (row === 0) {
      const m = dt.getUTCMonth();
      if (m !== prevMonth && col - lastLabelCol >= 3) {
        prevMonth = m;
        lastLabelCol = col;
        const txt = m === 0 ? `${MONTHS[m]} ’${String(dt.getUTCFullYear()).slice(2)}` : MONTHS[m];
        months += `<text x="${x}" y="12" class="cal-month">${txt}</text>`;
      } else if (m !== prevMonth) {
        prevMonth = m; // advance the month tracker without drawing a crowded label
      }
    }
  }
  // Weekday guides (Mon/Wed/Fri).
  let dows = "";
  for (const r of [1, 3, 5]) dows += `<text x="0" y="${padT + r * step + cell - 2}" class="cal-dow">${DOW[r]}</text>`;

  wrap.innerHTML = `<svg viewBox="0 0 ${W} ${H}" width="${W}" height="${H}" role="img" aria-label="Calendar of days with glucose data">${months}${dows}${cells}</svg>`;
  renderCalLegend(d);
}

function renderCalLegend(d) {
  const lg = $("#cal-legend");
  lg.innerHTML = "";
  const sw = (color, label) => {
    const s = el("span", { class: "cal-leg-item" });
    const box = el("span", { class: "cal-leg-sw" }); box.style.background = color;
    s.append(box, el("span", { text: label }));
    return s;
  };
  if (state.calMode === "glucose") {
    lg.append(
      sw("var(--g-inrange)", "In range"),
      sw("var(--g-warn)", "High / low"),
      sw("var(--g-danger)", "Very high / low"),
      sw("var(--cal-nostat)", "No stats"),
      sw("var(--cal-empty)", "No data"),
    );
  } else {
    lg.append(el("span", { class: "cal-leg-label", text: "Less" }),
      sw("var(--cal-empty)", ""), sw("rgba(54,196,107,0.24)", ""), sw("rgba(54,196,107,0.45)", ""),
      sw("rgba(54,196,107,0.68)", ""), sw("rgba(54,196,107,0.95)", ""),
      el("span", { class: "cal-leg-label", text: "More" }));
  }
}

function renderDayList(d) {
  const list = $("#data-days");
  list.innerHTML = "";
  const days = d.days || [];
  $("#data-days-caption").textContent = days.length ? `${days.length.toLocaleString()} days, newest first` : "";
  if (!days.length) { list.appendChild(el("p", { class: "muted", text: "No days with data yet." })); return; }

  list.appendChild(el("div", { class: "day-row day-head" }, [
    el("span", { text: "Date" }), el("span", { text: "Coverage" }), el("span", { text: "Avg" }),
    el("span", { text: "Time in range" }), el("span", { text: "uGMI" }), el("span", { text: "Min–max" }),
  ]));

  for (const day of days) {
    const dt = new Date(day.date + "T00:00:00");
    const dateBox = el("div", { class: "day-date" }, [
      el("span", { class: "day-dow", text: DOW[dt.getDay()] }),
      el("span", { class: "day-num", text: `${dt.getDate()} ${MONTHS[dt.getMonth()]} ${dt.getFullYear()}` }),
    ]);

    // Coverage bar + count.
    const frac = coverageFrac(day, d.expectedPerDay);
    const fill = el("div", { class: "cov-fill" }); fill.style.width = `${frac * 100}%`;
    const covBar = el("div", { class: "cov-bar" }, [fill]);
    const covBox = el("div", { class: "day-cov" }, [
      covBar,
      el("span", { class: "day-cov-pct", text: `${Math.round(frac * 100)}%` }),
      el("span", { class: "day-n", text: `${day.n.toLocaleString()} rdgs` }),
    ]);

    // Mean glucose (band-coloured) or em-dash for days outside the stats window.
    const meanBox = el("div", { class: "day-mean" });
    if (day.meanMgdl != null) {
      const v = el("span", { class: "day-mean-v", text: fmtGlu(day.meanMgdl) });
      v.style.color = bandColorVar(day.meanMgdl);
      meanBox.append(v, el("small", { text: unitName() }));
    } else meanBox.appendChild(el("span", { class: "muted", text: "—" }));

    // Mini TIR bar.
    const tirBox = el("div", { class: "day-tir" });
    if (day.timeInRange) {
      const segs = [["veryLowPct","var(--g-danger)"],["lowPct","var(--g-warn)"],["inRangePct","var(--g-inrange)"],["highPct","var(--g-warn)"],["veryHighPct","var(--g-danger)"]];
      const bar = el("div", { class: "day-tir-bar" });
      for (const [k, c] of segs) { const p = day.timeInRange[k] || 0; if (p > 0) { const s = el("div"); s.style.width = `${p}%`; s.style.background = c; bar.appendChild(s); } }
      tirBox.appendChild(bar);
    } else tirBox.appendChild(el("span", { class: "muted", text: "—" }));

    const ugmiBox = el("div", { class: "day-ugmi", text: day.uGmiPercent != null ? `${day.uGmiPercent.toFixed(1)}%` : "—" });
    const rangeBox = el("div", { class: "day-range", text: day.minMgdl != null ? `${fmtGlu(day.minMgdl)}–${fmtGlu(day.maxMgdl)}` : "—" });

    list.appendChild(el("div", { class: "day-row" }, [dateBox, covBox, meanBox, tirBox, ugmiBox, rangeBox]));
  }
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
  syncActive($(".cal-mode-toggle"), "mode", state.calMode);
  $$(".cal-mode-toggle .seg-btn").forEach((b) => b.addEventListener("click", () => {
    state.calMode = b.dataset.mode; localStorage.setItem("nk-cal-mode", state.calMode);
    syncActive($(".cal-mode-toggle"), "mode", state.calMode);
    if (state.daysData) { renderCalendar(state.daysData); }
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
