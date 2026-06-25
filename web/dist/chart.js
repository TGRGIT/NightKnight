// Bespoke glucose visualisations, drawn with no dependencies — tuned for diabetes
// management and the NightKnight theme. `drawGlucoseChart` renders the live trace to a
// <canvas> (retina-aware) and returns hover geometry; `renderAgp` builds the Ambulatory
// Glucose Profile percentile chart as inline SVG.

const MGDL_PER_MMOL = 18.0156;

const COLORS = {
  grid: "rgba(255,255,255,0.05)",
  axis: "#7b8493",
  band: "rgba(54,196,107,0.07)",
  bandEdge: "rgba(54,196,107,0.28)",
  threshold: "rgba(255,255,255,0.16)",
  line: "rgba(232,235,238,0.16)",
  inrange: "#36c46b",
  warn: "#e0a23c",
  danger: "#e5484d",
};

function bandColor(mgdl, low, high) {
  if (mgdl < 54 || mgdl > 250) return COLORS.danger;
  if (mgdl < low || mgdl > high) return COLORS.warn;
  return COLORS.inrange;
}

function toUnit(mgdl, unit) {
  return unit === "mmol/l" ? mgdl / MGDL_PER_MMOL : mgdl;
}
function fmtAxis(mgdl, unit) {
  return unit === "mmol/l" ? (mgdl / MGDL_PER_MMOL).toFixed(1) : String(Math.round(mgdl));
}

// Catmull-Rom → cubic Bézier smoothing for a pleasant, non-overshooting line.
function smoothPath(ctx, pts) {
  if (pts.length < 2) return;
  ctx.moveTo(pts[0].x, pts[0].y);
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i - 1] || pts[i], p1 = pts[i], p2 = pts[i + 1], p3 = pts[i + 2] || p2;
    const c1x = p1.x + (p2.x - p0.x) / 6, c1y = p1.y + (p2.y - p0.y) / 6;
    const c2x = p2.x - (p3.x - p1.x) / 6, c2y = p2.y - (p3.y - p1.y) / 6;
    ctx.bezierCurveTo(c1x, c1y, c2x, c2y, p2.x, p2.y);
  }
}

/**
 * Draw the glucose chart and return geometry for hover handling.
 * @returns {{points:{x:number,y:number,mgdl:number,date:number}[], plotTop:number, plotBottom:number}}
 */
export function drawGlucoseChart(canvas, opts) {
  const { entries, hours, low, high, unit, now } = opts;
  const dpr = window.devicePixelRatio || 1;
  const cssW = canvas.clientWidth || 600;
  const cssH = canvas.clientHeight || 260;
  canvas.width = Math.round(cssW * dpr);
  canvas.height = Math.round(cssH * dpr);
  const ctx = canvas.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, cssW, cssH);

  const padL = 40, padR = 12, padT = 12, padB = 24;
  const plotW = cssW - padL - padR;
  const plotH = cssH - padT - padB;

  const tMax = now;
  const tMin = now - hours * 3600_000;

  const visible = entries.filter((e) => e.date >= tMin && e.date <= tMax).sort((a, b) => a.date - b.date);

  // Adaptive y-domain that hugs the data, but always keeps the low/high lines on screen.
  let dMin = Infinity, dMax = -Infinity;
  for (const e of visible) { dMin = Math.min(dMin, e.mgdl); dMax = Math.max(dMax, e.mgdl); }
  if (!isFinite(dMin)) { dMin = low; dMax = high; }
  let yMin = Math.max(40, Math.floor((dMin - 10) / 10) * 10);
  yMin = Math.min(yMin, low - 6);
  let yMax = Math.ceil((dMax + 15) / 10) * 10;
  if (yMax < high + 20) yMax = high + 20;

  const x = (t) => padL + ((t - tMin) / (tMax - tMin)) * plotW;
  const y = (mgdl) => padT + (1 - (mgdl - yMin) / (yMax - yMin)) * plotH;
  const clamp = (v) => Math.max(yMin, Math.min(yMax, v));

  // Target band + dashed edges.
  ctx.fillStyle = COLORS.band;
  const yHi = y(Math.min(high, yMax)), yLo = y(Math.max(low, yMin));
  ctx.fillRect(padL, yHi, plotW, yLo - yHi);
  ctx.setLineDash([4, 4]);
  ctx.strokeStyle = COLORS.bandEdge;
  for (const v of [low, high]) {
    if (v < yMin || v > yMax) continue;
    ctx.beginPath(); ctx.moveTo(padL, y(v)); ctx.lineTo(padL + plotW, y(v)); ctx.stroke();
  }
  ctx.setLineDash([]);

  // Y gridlines + labels at clinically meaningful values.
  ctx.font = "11px -apple-system, system-ui, sans-serif";
  ctx.textAlign = "right";
  ctx.textBaseline = "middle";
  const yTicks = new Set([54, low, high]);
  for (let v = Math.ceil(yMin / 50) * 50; v <= yMax; v += 50) yTicks.add(v);
  for (const v of yTicks) {
    if (v < yMin || v > yMax) continue;
    const yy = y(v);
    if (v !== low && v !== high) {
      ctx.strokeStyle = COLORS.grid;
      ctx.beginPath(); ctx.moveTo(padL, yy); ctx.lineTo(cssW - padR, yy); ctx.stroke();
    }
    ctx.fillStyle = COLORS.axis;
    ctx.fillText(fmtAxis(v, unit), padL - 7, yy);
  }

  // X gridlines + hour labels.
  ctx.textAlign = "center";
  ctx.textBaseline = "top";
  const step = hours <= 6 ? 1 : hours <= 12 ? 2 : 4;
  for (let h = 0; h <= hours; h += step) {
    const t = tMax - h * 3600_000;
    const xx = x(t);
    ctx.strokeStyle = COLORS.grid;
    ctx.beginPath(); ctx.moveTo(xx, padT); ctx.lineTo(xx, padT + plotH); ctx.stroke();
    const d = new Date(t);
    ctx.fillStyle = COLORS.axis;
    ctx.fillText(`${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`, xx, padT + plotH + 5);
  }

  const points = visible.map((e) => ({ x: x(e.date), y: y(clamp(e.mgdl)), mgdl: e.mgdl, date: e.date }));
  if (points.length === 0) return { points, plotTop: padT, plotBottom: padT + plotH };

  // Connecting line (smoothed), broken across >20-min gaps.
  ctx.strokeStyle = COLORS.line;
  ctx.lineWidth = 1.5;
  ctx.lineJoin = "round"; ctx.lineCap = "round";
  let run = [];
  const flush = () => { if (run.length > 1) { ctx.beginPath(); smoothPath(ctx, run); ctx.stroke(); } run = []; };
  for (let i = 0; i < points.length; i++) {
    if (i > 0 && visible[i].date - visible[i - 1].date > 20 * 60_000) flush();
    run.push(points[i]);
  }
  flush();

  // Points, coloured by band.
  for (const p of points) {
    ctx.fillStyle = bandColor(p.mgdl, low, high);
    ctx.beginPath(); ctx.arc(p.x, p.y, 2.6, 0, Math.PI * 2); ctx.fill();
  }

  const geom = { points, plotTop: padT, plotBottom: padT + plotH };
  canvas._geom = geom;
  return geom;
}

// ── AGP (Ambulatory Glucose Profile) ─────────────────────────────────────────

const SVGNS = "http://www.w3.org/2000/svg";
function svg(tag, attrs) {
  const el = document.createElementNS(SVGNS, tag);
  for (const k in attrs) el.setAttribute(k, attrs[k]);
  return el;
}

/**
 * Render the AGP percentile chart into `container`.
 * @param {{bins:{minuteOfDay:number,n:number,p05:number,p25:number,p50:number,p75:number,p95:number}[], low:number, high:number, unit:string}} opts
 */
export function renderAgp(container, opts) {
  container.innerHTML = "";
  const { low, high, unit } = opts;
  const bins = (opts.bins || []).filter((b) => b.n > 0 && b.p50 != null);
  if (bins.length < 4) {
    const empty = document.createElement("div");
    empty.className = "agp-empty";
    empty.textContent = "Not enough data yet for an AGP — needs about a day of readings.";
    container.appendChild(empty);
    return;
  }
  const w = Math.max(320, container.clientWidth || 900), h = 300;
  const padL = 42, padR = 14, padT = 14, padB = 26;
  const plotW = w - padL - padR, plotH = h - padT - padB;

  const lo = Math.min(...bins.map((b) => b.p05));
  const hi = Math.max(...bins.map((b) => b.p95));
  const yMin = Math.max(40, Math.floor((Math.min(lo, low) - 8) / 10) * 10);
  const yMax = Math.ceil((Math.max(hi, high) + 10) / 10) * 10;
  const xFor = (m) => padL + (m / 1440) * plotW;
  const yFor = (v) => padT + (1 - (v - yMin) / (yMax - yMin)) * plotH;
  const pts = (k) => bins.map((b) => ({ x: xFor(b.minuteOfDay + 7.5), y: yFor(b[k]) }));
  const bandPath = (upper, lower) => {
    let d = `M${upper[0].x.toFixed(1)},${upper[0].y.toFixed(1)}`;
    for (let i = 1; i < upper.length; i++) d += ` L${upper[i].x.toFixed(1)},${upper[i].y.toFixed(1)}`;
    for (let i = lower.length - 1; i >= 0; i--) d += ` L${lower[i].x.toFixed(1)},${lower[i].y.toFixed(1)}`;
    return d + " Z";
  };
  const linePath = (p) => "M" + p.map((q) => `${q.x.toFixed(1)},${q.y.toFixed(1)}`).join(" L");

  const root = svg("svg", { viewBox: `0 0 ${w} ${h}`, width: "100%", preserveAspectRatio: "xMidYMid meet" });

  // Target band.
  root.appendChild(svg("rect", {
    x: padL, y: yFor(Math.min(high, yMax)), width: plotW,
    height: yFor(Math.max(low, yMin)) - yFor(Math.min(high, yMax)), fill: "rgba(54,196,107,0.06)",
  }));

  // Y gridlines + labels.
  const yTicks = new Set([low, high]);
  for (let v = Math.ceil(yMin / 50) * 50; v <= yMax; v += 50) yTicks.add(v);
  yTicks.add(yMax);
  [...yTicks].filter((v) => v >= yMin && v <= yMax).sort((a, b) => a - b).forEach((v) => {
    const yy = yFor(v);
    root.appendChild(svg("line", { x1: padL, x2: padL + plotW, y1: yy, y2: yy, stroke: "rgba(255,255,255,0.045)", "stroke-width": 1 }));
    const t = svg("text", { x: padL - 8, y: yy + 4, "text-anchor": "end", fill: "#7b8493", "font-size": 11 });
    t.textContent = fmtAxis(v, unit);
    root.appendChild(t);
  });
  // X labels every 3h.
  for (let m = 0; m <= 1440; m += 180) {
    const t = svg("text", { x: xFor(m), y: h - 8, "text-anchor": "middle", fill: "#7b8493", "font-size": 11 });
    t.textContent = String(Math.floor(m / 60)).padStart(2, "0") + ":00";
    root.appendChild(t);
  }

  // Bands: outer (5–95), IQR (25–75), then the median line + dashed target edges.
  root.appendChild(svg("path", { d: bandPath(pts("p95"), pts("p05")), fill: "rgba(54,196,107,0.12)" }));
  root.appendChild(svg("path", { d: bandPath(pts("p75"), pts("p25")), fill: "rgba(54,196,107,0.26)" }));
  for (const v of [low, high]) {
    if (v < yMin || v > yMax) continue;
    root.appendChild(svg("line", { x1: padL, x2: padL + plotW, y1: yFor(v), y2: yFor(v), stroke: "rgba(54,196,107,0.45)", "stroke-width": 1, "stroke-dasharray": "4 4" }));
  }
  root.appendChild(svg("path", { d: linePath(pts("p50")), fill: "none", stroke: "#e8ebf0", "stroke-width": 2.4, "stroke-linejoin": "round", "stroke-linecap": "round" }));

  container.appendChild(root);
}
