// A bespoke glucose chart, tuned for diabetes management and the Hub System theme.
// No dependencies — drawn directly to a <canvas>, retina-aware. Designed to be both
// pretty and clinically useful: a shaded target band, hi/lo threshold lines, and
// points coloured by glucose band so out-of-range time is obvious at a glance.

const MGDL_PER_MMOL = 18.0156;

const COLORS = {
  grid: "rgba(255,255,255,0.06)",
  axis: "#9aa4ae",
  band: "rgba(63,185,80,0.10)",
  threshold: "rgba(255,255,255,0.18)",
  line: "rgba(232,235,238,0.55)",
  inrange: "#3fb950",
  warn: "#d9a23b",
  danger: "#e5484d",
};

function bandColor(mgdl) {
  if (mgdl < 54 || mgdl > 250) return COLORS.danger;
  if (mgdl < 70 || mgdl > 180) return COLORS.warn;
  return COLORS.inrange;
}

function toUnit(mgdl, unit) {
  return unit === "mmol/l" ? mgdl / MGDL_PER_MMOL : mgdl;
}

function fmtAxis(mgdl, unit) {
  return unit === "mmol/l" ? (mgdl / MGDL_PER_MMOL).toFixed(1) : String(Math.round(mgdl));
}

/**
 * Draw the glucose chart.
 * @param {HTMLCanvasElement} canvas
 * @param {{entries:{date:number,mgdl:number}[],hours:number,low:number,high:number,unit:string,now:number}} opts
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

  const padL = 38, padR = 10, padT = 12, padB = 22;
  const plotW = cssW - padL - padR;
  const plotH = cssH - padT - padB;

  const tMax = now;
  const tMin = now - hours * 3600_000;
  // Y range: cover 40..(max reading or 260), with headroom.
  let maxR = 260;
  for (const e of entries) maxR = Math.max(maxR, e.mgdl);
  const yMin = 40, yMax = Math.min(maxR + 20, 600);

  const x = (t) => padL + ((t - tMin) / (tMax - tMin)) * plotW;
  const y = (mgdl) => padT + (1 - (mgdl - yMin) / (yMax - yMin)) * plotH;

  // Target band.
  ctx.fillStyle = COLORS.band;
  const yb = y(high), yb2 = y(low);
  ctx.fillRect(padL, yb, plotW, yb2 - yb);

  // Y gridlines + labels at clinically meaningful values.
  ctx.font = "11px -apple-system, system-ui, sans-serif";
  ctx.fillStyle = COLORS.axis;
  ctx.textAlign = "right";
  ctx.textBaseline = "middle";
  const yTicks = [54, low, high, 250].filter((v) => v >= yMin && v <= yMax);
  for (const v of yTicks) {
    const yy = y(v);
    ctx.strokeStyle = v === low || v === high ? COLORS.threshold : COLORS.grid;
    ctx.beginPath();
    ctx.moveTo(padL, yy);
    ctx.lineTo(cssW - padR, yy);
    ctx.stroke();
    ctx.fillText(fmtAxis(v, unit), padL - 6, yy);
  }

  // X gridlines + hour labels.
  ctx.textAlign = "center";
  ctx.textBaseline = "top";
  const step = hours <= 6 ? 1 : hours <= 12 ? 2 : 4; // hours between labels
  for (let h = 0; h <= hours; h += step) {
    const t = tMax - h * 3600_000;
    const xx = x(t);
    ctx.strokeStyle = COLORS.grid;
    ctx.beginPath();
    ctx.moveTo(xx, padT);
    ctx.lineTo(xx, padT + plotH);
    ctx.stroke();
    const d = new Date(t);
    const label = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
    ctx.fillStyle = COLORS.axis;
    ctx.fillText(label, xx, padT + plotH + 5);
  }

  if (entries.length === 0) return;

  // Connecting line (thin, neutral) — only between points close in time.
  const pts = entries
    .filter((e) => e.date >= tMin && e.date <= tMax)
    .sort((a, b) => a.date - b.date);
  ctx.strokeStyle = COLORS.line;
  ctx.lineWidth = 1.5;
  ctx.beginPath();
  let started = false;
  for (let i = 0; i < pts.length; i++) {
    const p = pts[i];
    const xx = x(p.date), yy = y(Math.max(yMin, Math.min(yMax, p.mgdl)));
    const gap = i > 0 && p.date - pts[i - 1].date > 20 * 60_000; // >20 min gap → break
    if (!started || gap) {
      ctx.moveTo(xx, yy);
      started = true;
    } else {
      ctx.lineTo(xx, yy);
    }
  }
  ctx.stroke();

  // Points, coloured by band.
  for (const p of pts) {
    const xx = x(p.date), yy = y(Math.max(yMin, Math.min(yMax, p.mgdl)));
    ctx.fillStyle = bandColor(p.mgdl);
    ctx.beginPath();
    ctx.arc(xx, yy, 2.6, 0, Math.PI * 2);
    ctx.fill();
  }
}
