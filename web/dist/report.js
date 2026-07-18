// NightKnight — printable AGP one-pager renderer.
//
// Standalone (does not import the dark-themed SPA chart module): it fetches the computed
// metric set from `GET /api/v4/export?format=json` (byte-identical to the Statistical
// Analysis view) plus raw readings for the daily-profile thumbnails, and lays them out as
// the standard clinical Ambulatory Glucose Profile report — statistics, a Time-in-Range
// goal bar, the AGP percentile chart, and a calendar of daily profiles — on white paper.
//
// Query params: start, end (epoch ms), tz (minutes east of UTC), unit (mg/dl|mmol/l),
// bin (AGP bin minutes), print (=1 to auto-open the print dialog once rendered).

const MGDL_PER_MMOL = 18.0156;
const DAY_MS = 86_400_000;
const SVGNS = "http://www.w3.org/2000/svg";
const DOW = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

const params = new URLSearchParams(location.search);
const numParam = (k) => {
  const n = Number(params.get(k));
  return params.has(k) && Number.isFinite(n) ? n : null;
};
const unit = params.get("unit") === "mmol/l" ? "mmol/l" : "mg/dl";
const tz = numParam("tz") ?? -new Date().getTimezoneOffset();
const now = Date.now();
const endMs = numParam("end") ?? now;
const startMs = numParam("start") ?? endMs - 14 * DAY_MS;
const bin = numParam("bin") ?? 15;

// ── formatting ──────────────────────────────────────────────────────────────
const unitName = () => (unit === "mmol/l" ? "mmol/L" : "mg/dL");
function fmtGlu(mgdl, dp) {
  if (mgdl == null) return "--";
  return unit === "mmol/l" ? (mgdl / MGDL_PER_MMOL).toFixed(dp ?? 1) : String(Math.round(mgdl));
}
const pct = (v, dp = 0) => (v == null ? "--" : v.toFixed(dp));
function localDayNumber(ms) { return Math.floor((ms + tz * 60_000) / DAY_MS); }
function localMinuteOfDay(ms) { return Math.floor(((ms + tz * 60_000) % DAY_MS) / 60_000); }
function dayLabel(dayNum) {
  const d = new Date(dayNum * DAY_MS);
  return { dow: DOW[((dayNum % 7) + 7 + 4) % 7], num: d.getUTCDate(), mon: MONTHS[d.getUTCMonth()], y: d.getUTCFullYear() };
}

function el(tag, opts = {}, children = []) {
  const node = document.createElement(tag);
  if (opts.class != null) node.className = opts.class;
  if (opts.text != null) node.textContent = opts.text;
  if (opts.style != null) node.style.cssText = opts.style;
  if (opts.html != null) node.innerHTML = opts.html; // only for trusted SVG we build ourselves
  for (const c of children) node.appendChild(c);
  return node;
}
function svg(tag, attrs) {
  const n = document.createElementNS(SVGNS, tag);
  for (const k in attrs) n.setAttribute(k, attrs[k]);
  return n;
}
function css(name) { return getComputedStyle(document.documentElement).getPropertyValue(name).trim(); }

async function fetchJson(path) {
  const res = await fetch(path, { credentials: "include" });
  if (!res.ok) throw new Error(`${path} → ${res.status}`);
  return res.json();
}

// ── entry point ──────────────────────────────────────────────────────────────
async function main() {
  document.getElementById("tb-print").addEventListener("click", () => window.print());
  document.getElementById("tb-back").addEventListener("click", () => {
    if (window.opener) window.close();
    else location.href = "/#analysis";
  });
  try {
    const q = new URLSearchParams({ format: "json", start: startMs, end: endMs, tzOffset: tz, bin });
    const hours = Math.ceil((now - startMs) / 3_600_000) + 1;
    const [rep, entriesResp] = await Promise.all([
      fetchJson(`/api/v4/export?${q}`),
      fetchJson(`/api/v4/entries?hours=${hours}&count=20000`).catch(() => ({ entries: [] })),
    ]);
    render(rep, entriesResp.entries || []);
    if (params.get("print") === "1") setTimeout(() => window.print(), 400);
  } catch (e) {
    const msg = /→ 401/.test(String(e))
      ? "Not signed in — open the report from inside the authenticated app."
      : `Could not load the report (${e}).`;
    document.getElementById("status").className = "status err";
    document.getElementById("status").textContent = msg;
  }
}

function render(rep, rawEntries) {
  const a = rep.analytics || {};
  const cov = a.coverage || {};
  const tir = a.timeInRange || {};
  const gri = a.gri || {};
  const startDay = localDayNumber(startMs);
  const endDay = localDayNumber(endMs);
  // The report-period label is the selected window length (a "14d" pick reads "14 days"),
  // independent of how many calendar days the unaligned window happens to touch.
  const periodDays = Math.max(1, Math.round((endMs - startMs) / DAY_MS));

  const rangeText = `${fmtDate(startDay)} – ${fmtDate(endDay)}`;
  document.getElementById("tb-range").textContent = rangeText;

  const sheet = document.getElementById("sheet");
  sheet.innerHTML = "";
  sheet.append(
    header(rangeText, periodDays, rep.generated),
    statsSection(a, cov, gri),
    agpSection(rep.agp, a),
    dailyProfilesSection(rawEntries, startDay, endDay),
    footer(rep),
  );
}

// ── header ────────────────────────────────────────────────────────────────────
function header(rangeText, nDays, generated) {
  const brand = el("div", { class: "rpt-brand" }, [
    logoSvg(),
    el("div", { class: "rpt-title" }, [
      el("h1", { text: "Ambulatory Glucose Profile (AGP) Report" }),
      el("p", { class: "sub", text: "NightKnight — continuous glucose monitoring summary" }),
    ]),
  ]);
  const genIso = generated && generated.iso ? generated.iso : new Date().toISOString();
  const genDate = new Date(genIso);
  const meta = el("div", { class: "rpt-meta" }, [
    el("div", { class: "range", text: rangeText }),
    el("div", { class: "row", text: `${nDays} day report period` }),
    el("div", { class: "row", text: `Local time (UTC${tz === 0 ? "" : offsetLabel(tz)})` }),
    el("div", { class: "row", text: `Generated ${genDate.toLocaleString()}` }),
  ]);
  return el("header", { class: "rpt-head" }, [brand, meta]);
}

// ── statistics + TIR goal bar ─────────────────────────────────────────────────
function statsSection(a, cov, gri) {
  const stats = el("div", { class: "stat-grid" });
  stats.append(
    stat("% Time CGM Active", pct(cov.percentActive) + "%", `${(a.n || 0).toLocaleString()} readings · ${cov.daysCovered == null ? "--" : cov.daysCovered.toFixed(1)} days`),
    stat("Average Glucose", `${fmtGlu(a.meanMgdl)}<small>${unitName()}</small>`, `SD ${fmtGlu(a.sdMgdl)}`, true),
    stat("Glucose Mgmt Indicator", pct(a.uGmiPercent, 1) + "%", `uGMI · GMI ${pct(a.gmiPercent, 1)}%`),
    stat("Variability (CV)", pct(a.cvPercent) + "%", a.cvPercent == null ? "—" : a.cvPercent <= 36 ? "stable (≤36%)" : "elevated"),
    griStat(gri),
  );

  const left = el("div", {}, [el("h2", { class: "section-title", text: "Glucose Statistics" }), stats]);
  const right = el("div", {}, [el("h2", { class: "section-title", text: "Time in Ranges" }), tirGoalBar(a.timeInRange || {})]);
  return el("section", { class: "section" }, [el("div", { class: "cols" }, [left, right])]);
}

function stat(k, valHtml, sub, wide) {
  const v = el("div", { class: "v", html: valHtml });
  return el("div", { class: "stat" + (wide ? " wide" : "") }, [
    el("div", { class: "k", text: k }),
    v,
    el("div", { class: "s", text: sub }),
  ]);
}

function griStat(gri) {
  const zoneColor = { A: "#2f9e57", B: "#7bbf3f", C: "#e0b93c", D: "#e08b2f", E: "#c73838" }[gri.zone] || "#8a94a1";
  const v = el("div", { class: "v" });
  v.append(document.createTextNode(gri.value == null ? "--" : Math.round(gri.value)));
  const chip = el("span", { class: "gri-chip", text: `Zone ${gri.zone || "—"}` });
  chip.style.background = zoneColor;
  v.appendChild(chip);
  return el("div", { class: "stat" }, [
    el("div", { class: "k", text: "Glycemia Risk Index" }),
    v,
    el("div", { class: "s", text: `hypo ${gri.hypoComponent == null ? "--" : gri.hypoComponent.toFixed(1)} · hyper ${gri.hyperComponent == null ? "--" : gri.hyperComponent.toFixed(1)}` }),
  ]);
}

// Vertical stacked Time-in-Range bar with clinical goals — the AGP report convention.
function tirGoalBar(tir) {
  const rows = [
    { key: "veryHighPct", color: css("--b-vhigh"), label: "Very High", range: ">250", goal: null },
    { key: "highPct", color: css("--b-high"), label: "High", range: "181–250", goal: "Goal for High + Very High: <25%" },
    { key: "inRangePct", color: css("--b-inrange"), label: "Target Range", range: "70–180", goal: "Goal: >70% (>16h 48m)" },
    { key: "lowPct", color: css("--b-low"), label: "Low", range: "54–69", goal: "Goal for Low + Very Low: <4%" },
    { key: "veryLowPct", color: css("--b-vlow"), label: "Very Low", range: "<54", goal: "Goal: <1%" },
  ];

  const bar = el("div", { class: "tir-bar" });
  for (const r of rows) {
    const v = tir[r.key] || 0;
    const seg = el("div", { class: "tir-seg", style: `flex-grow:${Math.max(v, 0.6)};background:${r.color}` });
    if (v >= 6) seg.appendChild(el("span", { text: `${v.toFixed(0)}%` }));
    bar.appendChild(seg);
  }

  const key = el("div", { class: "tir-key" });
  for (const r of rows) {
    const v = tir[r.key];
    const row = el("div", { class: "tir-row" }, [
      el("span", { class: "sw", style: `background:${r.color}` }),
      el("span", { class: "lab", html: `<b>${r.label}</b> <i>${r.range} ${unit === "mmol/l" ? "" : "mg/dL"}</i>` }),
      el("span", { class: "pct", text: v == null ? "--" : `${v.toFixed(0)}%` }),
    ]);
    key.appendChild(row);
    if (r.goal) key.appendChild(el("div", { class: "tir-row" }, [el("span", { class: "goal", text: r.goal })]));
  }
  return el("div", { class: "tir" }, [bar, key]);
}

// ── AGP percentile chart (light theme) ────────────────────────────────────────
function agpSection(agp, a) {
  const wrap = el("div", { class: "agp-card" });
  const bins = ((agp && agp.bins) || []).filter((b) => b.n > 0 && b.p50 != null);
  if (bins.length < 4) {
    wrap.appendChild(el("div", { class: "status", text: "Not enough data for an AGP — needs about a day of readings." }));
  } else {
    wrap.appendChild(agpSvg(bins));
    wrap.appendChild(agpLegend());
  }
  const note = el("p", { class: "s", style: "margin:6px 2px 0;color:var(--muted);font-size:11px", text: "Each day in the period overlaid on one 24-hour day. Median (50%) line, inner band 25–75%, outer band 5–95%." });
  return el("section", { class: "section" }, [el("h2", { class: "section-title", text: "Ambulatory Glucose Profile (AGP)" }), wrap, note]);
}

function agpSvg(bins) {
  const low = 70, high = 180; // analytics TIR always uses the consensus target
  const w = 960, h = 300, padL = 46, padR = 16, padT = 14, padB = 26;
  const plotW = w - padL - padR, plotH = h - padT - padB;
  const lo = Math.min(...bins.map((b) => b.p05));
  const hi = Math.max(...bins.map((b) => b.p95));
  const yMin = Math.max(40, Math.floor((Math.min(lo, low) - 8) / 10) * 10);
  const yMax = Math.ceil((Math.max(hi, high) + 10) / 10) * 10;
  const xFor = (m) => padL + (m / 1440) * plotW;
  const yFor = (v) => padT + (1 - (v - yMin) / (yMax - yMin)) * plotH;
  const pts = (k) => bins.map((b) => ({ x: xFor(b.minuteOfDay + 7.5), y: yFor(b[k]) }));
  const bandPath = (up, lo2) => {
    let d = `M${up[0].x.toFixed(1)},${up[0].y.toFixed(1)}`;
    for (let i = 1; i < up.length; i++) d += ` L${up[i].x.toFixed(1)},${up[i].y.toFixed(1)}`;
    for (let i = lo2.length - 1; i >= 0; i--) d += ` L${lo2[i].x.toFixed(1)},${lo2[i].y.toFixed(1)}`;
    return d + " Z";
  };
  const linePath = (p) => "M" + p.map((q) => `${q.x.toFixed(1)},${q.y.toFixed(1)}`).join(" L");

  const root = svg("svg", { viewBox: `0 0 ${w} ${h}`, width: "100%", preserveAspectRatio: "xMidYMid meet", role: "img", "aria-label": "Ambulatory glucose profile" });

  // Target band.
  root.appendChild(svg("rect", { x: padL, y: yFor(Math.min(high, yMax)), width: plotW, height: yFor(Math.max(low, yMin)) - yFor(Math.min(high, yMax)), fill: css("--target-band") }));

  // Y gridlines + labels.
  const yTicks = new Set([low, high, yMax]);
  for (let v = Math.ceil(yMin / 50) * 50; v <= yMax; v += 50) yTicks.add(v);
  [...yTicks].filter((v) => v >= yMin && v <= yMax).sort((x, y) => x - y).forEach((v) => {
    const yy = yFor(v);
    root.appendChild(svg("line", { x1: padL, x2: padL + plotW, y1: yy, y2: yy, stroke: "rgba(19,23,32,0.08)", "stroke-width": 1 }));
    const t = svg("text", { x: padL - 8, y: yy + 4, "text-anchor": "end", fill: "#5b6675", "font-size": 12 });
    t.textContent = unit === "mmol/l" ? (v / MGDL_PER_MMOL).toFixed(1) : String(v);
    root.appendChild(t);
  });
  // X labels every 3h.
  for (let m = 0; m <= 1440; m += 180) {
    const t = svg("text", { x: xFor(m), y: h - 8, "text-anchor": "middle", fill: "#5b6675", "font-size": 12 });
    t.textContent = String(Math.floor(m / 60)).padStart(2, "0") + ":00";
    root.appendChild(t);
  }
  // Unit caption on the Y axis.
  const yl = svg("text", { x: 12, y: padT + plotH / 2, fill: "#8a94a1", "font-size": 11, transform: `rotate(-90 12 ${padT + plotH / 2})`, "text-anchor": "middle" });
  yl.textContent = unitName();
  root.appendChild(yl);

  // Bands + dashed target edges + median line.
  root.appendChild(svg("path", { d: bandPath(pts("p95"), pts("p05")), fill: "rgba(47,158,87,0.13)" }));
  root.appendChild(svg("path", { d: bandPath(pts("p75"), pts("p25")), fill: "rgba(47,158,87,0.30)" }));
  for (const v of [low, high]) {
    root.appendChild(svg("line", { x1: padL, x2: padL + plotW, y1: yFor(v), y2: yFor(v), stroke: css("--target-line"), "stroke-width": 1, "stroke-dasharray": "5 4" }));
  }
  root.appendChild(svg("path", { d: linePath(pts("p50")), fill: "none", stroke: "#131720", "stroke-width": 2.6, "stroke-linejoin": "round", "stroke-linecap": "round" }));
  return root;
}

function agpLegend() {
  return el("div", { class: "agp-legend" }, [
    legendItem("ln", "Median (50%)", "#131720"),
    legendItem("sw", "IQR 25–75%", "rgba(47,158,87,0.55)"),
    legendItem("sw", "5–95%", "rgba(47,158,87,0.25)"),
    legendItem("ln target", "Target 70–180", null),
  ]);
}
function legendItem(kind, label, color) {
  const marker = el("span", { class: kind.split(" ")[0] === "ln" ? "ln" + (kind.includes("target") ? " target" : "") : "sw" });
  if (color) marker.style.background = kind.startsWith("sw") ? color : "";
  if (color && kind === "ln") marker.style.borderTopColor = color;
  return el("span", {}, [marker, document.createTextNode(label)]);
}

// ── daily profiles (calendar of mini day charts) ─────────────────────────────
function dailyProfilesSection(rawEntries, startDay, endDay) {
  // Group readings into local days.
  const byDay = new Map();
  for (const e of rawEntries) {
    if (e.date < startMs || e.date > endMs) continue;
    const dn = localDayNumber(e.date);
    if (!byDay.has(dn)) byDay.set(dn, []);
    byDay.get(dn).push({ min: localMinuteOfDay(e.date), mgdl: e.mgdl });
  }

  const dow = el("div", { class: "days-dow" }, DOW.map((d) => el("span", { text: d })));
  const grid = el("div", { class: "days-grid" });

  // Lead with empty cells so the first day lands under its weekday column.
  const lead = (((startDay % 7) + 7 + 4) % 7); // 0 = Sunday
  for (let i = 0; i < lead; i++) grid.appendChild(el("div", { class: "day empty" }));

  for (let dn = startDay; dn <= endDay; dn++) {
    grid.appendChild(dayCell(dn, byDay.get(dn)));
  }

  return el("section", { class: "section" }, [
    el("h2", { class: "section-title", text: "Daily Glucose Profiles" }),
    dow,
    grid,
  ]);
}

function dayCell(dayNum, readings) {
  const lbl = dayLabel(dayNum);
  const cell = el("div", { class: "day" + (readings && readings.length ? "" : " empty") });
  const top = el("div", { class: "d-top" }, [el("span", { class: "d-date", text: `${lbl.num} ${lbl.mon}` })]);
  cell.appendChild(top);

  if (!readings || readings.length < 2) {
    cell.appendChild(el("div", { class: "d-empty", text: "no data" }));
    return cell;
  }
  readings.sort((a, b) => a.min - b.min);
  const inRange = readings.filter((r) => r.mgdl >= 70 && r.mgdl <= 180).length / readings.length * 100;
  const tirColor = inRange >= 70 ? css("--b-inrange") : inRange >= 50 ? css("--b-high") : css("--b-low");
  top.appendChild(el("span", { class: "d-tir", style: `color:${tirColor}`, text: `${inRange.toFixed(0)}%` }));

  cell.appendChild(daySparkline(readings));
  return cell;
}

// A compact 24h trace over a light target band. A single dark line keeps the SVG light even
// for a 90-day report; the coloured TIR% label carries the at-a-glance verdict.
function daySparkline(readings) {
  const w = 120, h = 40, padY = 3;
  const yMin = 40, yMax = 320;
  const xFor = (m) => (m / 1440) * w;
  const yFor = (v) => padY + (1 - (Math.max(yMin, Math.min(yMax, v)) - yMin) / (yMax - yMin)) * (h - padY * 2);
  const root = svg("svg", { viewBox: `0 0 ${w} ${h}`, width: "100%", preserveAspectRatio: "none" });
  // Target band 70–180.
  root.appendChild(svg("rect", { x: 0, y: yFor(180), width: w, height: yFor(70) - yFor(180), fill: css("--target-band") }));
  root.appendChild(svg("line", { x1: 0, x2: w, y1: yFor(70), y2: yFor(70), stroke: css("--target-line"), "stroke-width": 0.5, "stroke-dasharray": "2 2" }));
  root.appendChild(svg("line", { x1: 0, x2: w, y1: yFor(180), y2: yFor(180), stroke: css("--target-line"), "stroke-width": 0.5, "stroke-dasharray": "2 2" }));
  // Break the line across gaps > 45 min so an outage isn't drawn as a straight bridge.
  let d = "";
  let prev = null;
  for (const r of readings) {
    const cmd = prev == null || r.min - prev > 45 ? "M" : "L";
    d += `${cmd}${xFor(r.min).toFixed(1)},${yFor(r.mgdl).toFixed(1)}`;
    prev = r.min;
  }
  root.appendChild(svg("path", { d, fill: "none", stroke: "#131720", "stroke-width": 1, "stroke-linejoin": "round", "stroke-linecap": "round" }));
  return root;
}

// ── footer ────────────────────────────────────────────────────────────────────
function footer(rep) {
  const foot = el("section", { class: "rpt-foot" });
  foot.appendChild(el("p", {}, [
    el("span", { class: "disc", text: "Not a medical device. " }),
    document.createTextNode(
      "These metrics are estimates for personal and clinical review, not a basis for treatment decisions. " +
      "Time-in-Range bands and goals follow the 2019 international consensus (Battelino et al., Diabetes Care 2019); " +
      "GMI = 3.31 + 0.02392 × mean; uGMI is the 2026 Diabetologia revision; the Glycemia Risk Index follows Klonoff et al. 2023."),
  ]));
  foot.appendChild(el("p", { text: "NightKnight — private, self-hosted CGM. https://github.com/TGRGIT/NightKnight" }));
  return foot;
}

// ── small helpers ──────────────────────────────────────────────────────────────
function fmtDate(dayNum) {
  const l = dayLabel(dayNum);
  return `${l.num} ${l.mon} ${l.y}`;
}
function offsetLabel(min) {
  const sign = min > 0 ? "+" : "-";
  const a = Math.abs(min);
  return `${sign}${String(Math.floor(a / 60)).padStart(2, "0")}:${String(a % 60).padStart(2, "0")}`;
}
function logoSvg() {
  // The NightKnight crescent glyph, drawn on the brand red so the header reads as ours.
  const s = svg("svg", { class: "rpt-logo", viewBox: "0 0 100 100", role: "img", "aria-label": "NightKnight" });
  s.appendChild(svg("rect", { x: 4, y: 4, width: 92, height: 92, rx: 22, fill: "#e5484d" }));
  s.appendChild(svg("path", { d: "M64 26a28 28 0 1 0 0 48 22 22 0 1 1 0-48Z", fill: "#fff" }));
  s.appendChild(svg("circle", { cx: 70, cy: 38, r: 4.4, fill: "#fff" }));
  return s;
}

main();
