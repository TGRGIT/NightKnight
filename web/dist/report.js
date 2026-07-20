// NightKnight — Ambulatory Glucose Profile report.
//
// One request carries the whole report: `GET /api/v4/export?format=json&samples=1` returns
// the computed metric set (byte-identical to the Statistical Analysis view) plus a compact
// downsampled sample series, so nothing here re-derives clinical numbers and there is no
// second unbounded fetch. Clicking a day opens a detail view that fetches THAT day on its
// own, at full stored resolution, and caches it.
//
// Colour is load-bearing: every trace is drawn as one path per glucose band, so a day's
// shape reads as very-low / low / in-range / high / very-high at a glance. All colours come
// from CSS classes rather than baked-in attributes, which is what lets `@media print` turn
// the dark on-screen document into the light clinical one-pager without re-rendering.
//
// Query params: start, end (epoch ms), tz (minutes east of UTC), unit (mg/dl|mmol/l),
// bin (AGP bin minutes), print (=1 to auto-open the print dialog once rendered).

const MGDL_PER_MMOL = 18.0156;
const DAY_MS = 86_400_000;
const SVGNS = "http://www.w3.org/2000/svg";
const DOW = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
// Break a trace across gaps longer than this (minutes) so an outage isn't drawn as a bridge.
const GAP_BREAK_MIN = 45;

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

// ── glucose bands ────────────────────────────────────────────────────────────
// Five bands in ascending order. `BAND_EDGES[i]` is the upper bound of band i, so a trace
// crossing from band i to i+1 passes exactly through it — which is where the line is split
// so the colour changes at the threshold, not at the next sample.
//
// The boundaries are NOT fixed: the ADA/ATTD consensus (54/70/180/250 mg/dL) is only the
// default. They resolve, in order of precedence, from the URL, then this page's saved
// override, then the main app's saved target range — and whatever wins is sent to the API
// so the percentages are COMPUTED against these bands, never just coloured by them.
const BAND_CLASS = ["b-vlow", "b-low", "b-inrange", "b-high", "b-vhigh"];
const BAND_LABEL = ["Very Low", "Low", "In Range", "High", "Very High"];
const CONSENSUS = { veryLow: 54, low: 70, high: 180, veryHigh: 250 };
const BANDS_KEY = "nk-report-bands"; // this page's override (mg/dL), if any

const BAND_KEYS = ["veryLow", "low", "high", "veryHigh"];

// Force a threshold set to be strictly increasing and physiologically plausible — the same
// contract the API enforces. This is NOT belt-and-braces: the app clamps its target range
// (40–180 low, 120–350 high) independently of the very-low/very-high edges, so a perfectly
// legal app setting like Target Low = 50 would otherwise yield veryLow 54 > low 50, which
// the API rejects — leaving the report permanently unloadable, with "Reset to defaults"
// re-deriving the very same broken set. Nudging the outer edges keeps the user's intent
// (their target range) and repairs only what has to move.
function normaliseBands(b) {
  const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
  const pick = (v, fallback) => (Number.isFinite(v) && v > 0 ? v : fallback);
  // How far the consensus severity edges sit outside the target range. When an edge has to
  // move, shifting it by this keeps a *usable* severity band — repairing to `low - 1` would
  // technically satisfy the ordering while leaving a 1 mg/dL sliver nothing can fall into.
  const LOW_MARGIN = CONSENSUS.low - CONSENSUS.veryLow;    // 16
  const HIGH_MARGIN = CONSENSUS.veryHigh - CONSENSUS.high; // 70
  const low = clamp(pick(b.low, CONSENSUS.low), 21, 598);
  const high = clamp(pick(b.high, CONSENSUS.high), low + 1, 599);
  const wantVeryLow = pick(b.veryLow, CONSENSUS.veryLow);
  const wantVeryHigh = pick(b.veryHigh, CONSENSUS.veryHigh);
  return {
    veryLow: clamp(wantVeryLow < low ? wantVeryLow : low - LOW_MARGIN, 20, low - 1),
    low,
    high,
    veryHigh: clamp(wantVeryHigh > high ? wantVeryHigh : high + HIGH_MARGIN, high + 1, 600),
  };
}

// The bands this page falls back to when it has no override of its own: the main app's
// saved target range over the consensus edges.
function defaultBands() {
  return normaliseBands({
    ...CONSENSUS,
    low: Number(localStorage.getItem("nk-target-low")),
    high: Number(localStorage.getItem("nk-target-high")),
  });
}

function readSavedBands() {
  try {
    const saved = JSON.parse(localStorage.getItem(BANDS_KEY) || "null");
    if (saved && BAND_KEYS.every((k) => Number.isFinite(saved[k]))) return normaliseBands(saved);
  } catch { /* corrupt override — fall through to the app defaults */ }
  return defaultBands();
}

// URL params win, so the app can deep-link a report at a specific target range.
let TH = (() => {
  const fromUrl = {};
  for (const k of BAND_KEYS) {
    const v = numParam(k);
    if (v != null) fromUrl[k] = v;
  }
  return normaliseBands({ ...readSavedBands(), ...fromUrl });
})();
let BAND_EDGES = [TH.veryLow, TH.low, TH.high, TH.veryHigh];

function bandIndex(mgdl) {
  if (mgdl < TH.veryLow) return 0;
  if (mgdl < TH.low) return 1;
  if (mgdl <= TH.high) return 2;
  if (mgdl <= TH.veryHigh) return 3;
  return 4;
}
// The threshold set, as the API expects it (always mg/dL).
const bandQuery = () => ({ veryLow: TH.veryLow, low: TH.low, high: TH.high, veryHigh: TH.veryHigh });
// "Default" means the app/consensus bands — NOT this page's saved override. Comparing
// against the override would make the indicator read "default" precisely when custom bands
// are in force, which is the inverse of what it is for.
const bandsAreDefault = () => {
  const d = defaultBands();
  return BAND_KEYS.every((k) => Math.abs(d[k] - TH[k]) < 1e-6);
};

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
function fmtDate(dayNum) {
  const l = dayLabel(dayNum);
  return `${l.num} ${l.mon} ${l.y}`;
}
// `YYYY-MM-DD` for a local day number — the value shape a native date input expects.
function isoDate(dayNum) {
  const d = new Date(dayNum * DAY_MS);
  return `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, "0")}-${String(d.getUTCDate()).padStart(2, "0")}`;
}
function dayNumFromIso(iso) {
  const [y, m, d] = iso.split("-").map(Number);
  if (!y || !m || !d) return null;
  return Math.floor(Date.UTC(y, m - 1, d) / DAY_MS);
}
function offsetLabel(min) {
  const sign = min > 0 ? "+" : "-";
  const a = Math.abs(min);
  return `${sign}${String(Math.floor(a / 60)).padStart(2, "0")}:${String(a % 60).padStart(2, "0")}`;
}

function el(tag, opts = {}, children = []) {
  const node = document.createElement(tag);
  if (opts.class != null) node.className = opts.class;
  if (opts.text != null) node.textContent = opts.text;
  if (opts.style != null) node.style.cssText = opts.style;
  if (opts.html != null) node.innerHTML = opts.html; // only for trusted markup we build ourselves
  for (const k in opts.attrs || {}) node.setAttribute(k, opts.attrs[k]);
  for (const c of children) node.appendChild(c);
  return node;
}
function svg(tag, attrs) {
  const n = document.createElementNS(SVGNS, tag);
  for (const k in attrs) n.setAttribute(k, attrs[k]);
  return n;
}

async function fetchJson(path) {
  const res = await fetch(path, { credentials: "include" });
  if (!res.ok) throw new Error(`${path} → ${res.status}`);
  return res.json();
}

// Fetch a JSON document while reporting download progress. The server aggregates the whole
// window server-side, so this one request carries the entire report; streaming the body lets
// the loading bar reflect real bytes received rather than a fake timer.
// `onProgress(receivedBytes, totalBytes|0)` fires per chunk (total 0 = unknown).
async function fetchJsonWithProgress(path, onProgress) {
  const res = await fetch(path, { credentials: "include" });
  if (!res.ok) throw new Error(`${path} → ${res.status}`);
  const total = Number(res.headers.get("content-length")) || 0;
  // No readable stream (old browser) — fall back to a plain parse; the bar stays indeterminate.
  if (!res.body || !res.body.getReader) return res.json();
  const reader = res.body.getReader();
  const chunks = [];
  let received = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
    received += value.length;
    onProgress(received, total);
  }
  const buf = new Uint8Array(received);
  let pos = 0;
  for (const c of chunks) { buf.set(c, pos); pos += c.length; }
  return JSON.parse(new TextDecoder().decode(buf));
}

// ── loading progress bar ──────────────────────────────────────────────────────
function setLoaderIndeterminate(text) {
  const bar = document.getElementById("progress");
  if (bar) bar.classList.add("indeterminate");
  const sub = document.getElementById("loader-sub");
  if (sub) sub.textContent = text;
}
function setLoaderProgress(received, total) {
  const bar = document.getElementById("progress");
  const fill = document.getElementById("progress-fill");
  const sub = document.getElementById("loader-sub");
  if (!bar || !fill) return;
  const kb = (received / 1024).toFixed(0);
  if (total > 0) {
    bar.classList.remove("indeterminate");
    fill.style.width = Math.min(100, Math.round((received / total) * 100)) + "%";
    if (sub) sub.textContent = `Loaded ${kb} KB of ${(total / 1024).toFixed(0)} KB`;
  } else if (sub) {
    sub.textContent = `Loaded ${kb} KB…`;
  }
}
function loaderError(msg) {
  const loader = document.getElementById("loader");
  if (loader) {
    loader.innerHTML = "";
    loader.appendChild(el("div", { class: "status err", text: msg }));
  }
}

// ── band-coloured trace ──────────────────────────────────────────────────────
// Build ONE path per band from a time-ordered series, splitting each segment exactly where
// it crosses a band threshold. Emitting five paths (instead of one per segment) is what
// keeps a 90-day calendar cheap: ~5 nodes per day rather than hundreds.
//
// `pts` is `[{ t, v }]` (t in the x-domain, v in mg/dL), `maxGap` breaks the line across
// outages. Returns `[{ cls, d }]`, omitting bands the series never visits.
function bandTracePaths(pts, xFor, yFor, maxGap) {
  const d = ["", "", "", "", ""];
  const cur = [null, null, null, null, null]; // where each band's path currently ends
  const add = (bi, x1, y1, x2, y2) => {
    const c = cur[bi];
    // Continue this band's subpath when it already ends exactly here, else start a new one.
    if (c && Math.abs(c.x - x1) < 0.02 && Math.abs(c.y - y1) < 0.02) {
      d[bi] += `L${x2.toFixed(1)},${y2.toFixed(1)}`;
    } else {
      d[bi] += `M${x1.toFixed(1)},${y1.toFixed(1)}L${x2.toFixed(1)},${y2.toFixed(1)}`;
    }
    cur[bi] = { x: x2, y: y2 };
  };

  for (let i = 1; i < pts.length; i++) {
    const p = pts[i - 1], q = pts[i];
    if (q.t - p.t > maxGap) continue; // outage — draw nothing across it
    const b0 = bandIndex(p.v), b1 = bandIndex(q.v);
    if (b0 === b1) {
      add(b0, xFor(p.t), yFor(p.v), xFor(q.t), yFor(q.v));
      continue;
    }
    // Crossed one or more thresholds: walk the bands in between, cutting the segment at
    // each edge so the colour changes at the clinical boundary rather than at the sample.
    const step = b1 > b0 ? 1 : -1;
    let ct = p.t, cv = p.v, bi = b0;
    const span = q.v - p.v;
    while (bi !== b1) {
      const bound = step > 0 ? BAND_EDGES[bi] : BAND_EDGES[bi - 1];
      const f = span === 0 ? 1 : (bound - p.v) / span;
      const tt = p.t + (q.t - p.t) * f;
      add(bi, xFor(ct), yFor(cv), xFor(tt), yFor(bound));
      ct = tt; cv = bound; bi += step;
    }
    add(b1, xFor(ct), yFor(cv), xFor(q.t), yFor(q.v));
  }
  const out = [];
  for (let i = 0; i < 5; i++) if (d[i]) out.push({ cls: BAND_CLASS[i], d: d[i] });
  return out;
}

function appendTrace(root, pts, xFor, yFor, maxGap, width) {
  for (const seg of bandTracePaths(pts, xFor, yFor, maxGap)) {
    root.appendChild(svg("path", { class: `trace ${seg.cls}`, d: seg.d, "stroke-width": width }));
  }
  // A lone reading has no segment to draw — show it as a dot so the day isn't blank. The
  // fill is inline (not a `.trace` class) because that class sets `fill: none`, and CSS
  // beats a presentation attribute; a `var()` still flips correctly for print.
  if (pts.length === 1) {
    root.appendChild(svg("circle", {
      cx: xFor(pts[0].t), cy: yFor(pts[0].v), r: Math.max(1.4, width),
      style: `fill:var(--${BAND_CLASS[bandIndex(pts[0].v)]})`,
    }));
  }
}

// ── entry point ──────────────────────────────────────────────────────────────
async function main() {
  document.getElementById("tb-print").addEventListener("click", () => window.print());
  document.getElementById("tb-back").addEventListener("click", () => {
    if (window.opener) window.close();
    else location.href = "/#analysis";
  });
  wireModal();
  wireBands();
  await loadReport();
}

// Fetch + render the whole report at the current thresholds. Re-runnable: changing the
// bands re-requests so the metrics are recomputed server-side, never just recoloured.
async function loadReport() {
  const sheet = document.getElementById("sheet");
  sheet.innerHTML = "";
  sheet.appendChild(el("div", { class: "loader", attrs: { id: "loader" } }, [
    el("div", { class: "loader-title", text: "Loading your glucose report…" }),
    el("div", { class: "progress indeterminate", attrs: { id: "progress" } }, [
      el("div", { class: "progress-fill", attrs: { id: "progress-fill" } }),
    ]),
    el("div", { class: "loader-sub", attrs: { id: "loader-sub" }, text: "Preparing…" }),
  ]));
  try {
    // One request: the server covers the WHOLE window (aggregating only if it must) and
    // returns the metrics plus the `samples` series the daily thumbnails need — so there's
    // no second, unbounded `/entries` fetch that could exceed the per-request budget.
    setLoaderIndeterminate("Aggregating your glucose data…");
    const q = new URLSearchParams({
      format: "json", start: startMs, end: endMs, tzOffset: tz, bin, samples: "1", ...bandQuery(),
    });
    const rep = await fetchJsonWithProgress(`/api/v4/export?${q}`, setLoaderProgress);
    // `samples` are compact `[epoch_ms, mg_dL]` pairs. Falls back to empty if an older
    // server omits them (the thumbnails then simply read "no data").
    const samples = (rep.samples || []).map(([date, mgdl]) => ({ date, mgdl }));
    render(rep, samples);
    if (params.get("print") === "1") setTimeout(() => window.print(), 400);
  } catch (e) {
    loaderError(
      /→ 401/.test(String(e))
        ? "Not signed in — open the report from inside the authenticated app."
        : `Could not load the report (${e}).`,
    );
  }
}

// ── severity-band editor ──────────────────────────────────────────────────────
// Inputs are shown in the report's display unit but always stored and sent as mg/dL.
const toMgdl = (v) => (unit === "mmol/l" ? v * MGDL_PER_MMOL : v);

function wireBands() {
  const panel = document.getElementById("bands-panel");
  const toggle = document.getElementById("tb-bands");
  document.getElementById("bands-unit").textContent = unitName();

  toggle.addEventListener("click", () => {
    const open = panel.hidden;
    panel.hidden = !open;
    toggle.setAttribute("aria-expanded", String(open));
    if (open) { fillBandInputs(); document.getElementById("band-vlow").focus(); }
  });
  document.getElementById("bands-apply").addEventListener("click", applyBands);
  document.getElementById("bands-reset").addEventListener("click", () => {
    localStorage.removeItem(BANDS_KEY);
    setBands(readSavedBands(), "Reset to defaults.");
  });
  // Enter applies, so the panel behaves like a form without being one.
  panel.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); applyBands(); } });
  fillBandInputs();
}

function fillBandInputs() {
  const dp = unit === "mmol/l" ? 1 : 0;
  for (const [id, key] of [["band-vlow", "veryLow"], ["band-low", "low"], ["band-high", "high"], ["band-vhigh", "veryHigh"]]) {
    const input = document.getElementById(id);
    input.value = unit === "mmol/l" ? (TH[key] / MGDL_PER_MMOL).toFixed(dp) : String(Math.round(TH[key]));
  }
  bandsMessage(bandsAreDefault() ? "" : "Custom bands active.", true);
}

function bandsMessage(text, ok) {
  const msg = document.getElementById("bands-msg");
  msg.textContent = text;
  msg.className = "bands-msg" + (ok ? " ok" : "");
}

function applyBands() {
  // `Number("")` is 0 — finite — so an empty field must be caught before conversion or it
  // silently becomes a zero threshold and trips the wrong error message.
  const raw = (id) => document.getElementById(id).value.trim();
  const ids = ["band-vlow", "band-low", "band-high", "band-vhigh"];
  if (ids.some((id) => raw(id) === "")) return bandsMessage("Enter a number for each band.", false);
  const read = (id) => Number(raw(id));
  const next = {
    veryLow: toMgdl(read("band-vlow")),
    low: toMgdl(read("band-low")),
    high: toMgdl(read("band-high")),
    veryHigh: toMgdl(read("band-vhigh")),
  };
  if (!Object.values(next).every(Number.isFinite)) return bandsMessage("Enter a number for each band.", false);
  if (!(next.veryLow < next.low && next.low < next.high && next.high < next.veryHigh)) {
    return bandsMessage("Bands must increase: very low < low < high < very high.", false);
  }
  if (next.veryLow < 20 || next.veryHigh > 600) {
    return bandsMessage(`Bands must lie between ${fmtGlu(20)} and ${fmtGlu(600)} ${unitName()}.`, false);
  }
  // In mmol/L the inputs are shown to 1 decimal, so re-applying an unchanged panel would
  // otherwise write back a slightly different mg/dL value each time (0.1 mmol ≈ 1.8 mg/dL)
  // and drift the bands. Treat a sub-0.5 mg/dL difference as "unchanged" and skip the reload.
  if (BAND_KEYS.every((k) => Math.abs(next[k] - TH[k]) < 0.5)) {
    return bandsMessage(bandsAreDefault() ? "" : "Custom bands active.", true);
  }
  localStorage.setItem(BANDS_KEY, JSON.stringify(normaliseBands(next)));
  setBands(next, "Applied.");
}

// Adopt a threshold set and rebuild everything that depends on it.
function setBands(next, note) {
  TH = normaliseBands(next);
  BAND_EDGES = [TH.veryLow, TH.low, TH.high, TH.veryHigh];
  bandsGeneration++;  // invalidates day fetches already in flight
  dayCache.clear();   // cached days were computed against the old bands
  fillBandInputs();
  bandsMessage(note, true);
  loadReport();
}

function render(rep, rawEntries) {
  const a = rep.analytics || {};
  const cov = a.coverage || {};
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
    statsSection(a, cov, gri, rep.sampling),
    agpSection(rep.agp),
    dailyProfilesSection(rawEntries, startDay, endDay),
    footer(rep),
  );
}

// ── header ────────────────────────────────────────────────────────────────────
function header(rangeText, nDays, generated) {
  const brand = el("div", { class: "rpt-brand" }, [
    logoSvg(),
    el("div", { class: "rpt-title" }, [
      el("h1", { text: "Ambulatory Glucose Profile" }),
      el("p", { class: "sub", text: "NightKnight — continuous glucose monitoring summary" }),
    ]),
  ]);
  const genIso = generated && generated.iso ? generated.iso : new Date().toISOString();
  const meta = el("div", { class: "rpt-meta" }, [
    el("div", { class: "range", text: rangeText }),
    el("div", { class: "row", text: `${nDays} day report period` }),
    el("div", { class: "row", text: `Local time (UTC${tz === 0 ? "" : offsetLabel(tz)})` }),
    el("div", { class: "row", text: `Generated ${new Date(genIso).toLocaleString()}` }),
  ]);
  return el("header", { class: "section rpt-head" }, [brand, meta]);
}

// ── statistics + TIR goal bar ─────────────────────────────────────────────────
function statsSection(a, cov, gri, sampling) {
  const stats = el("div", { class: "stat-grid" });
  const days = cov.daysCovered == null ? "--" : cov.daysCovered.toFixed(1);
  // Show BOTH counts when the server downsampled, so the figure is never mistaken for the
  // full stored series. Otherwise the metrics used every reading and one number is the truth.
  const used = (a.n || 0).toLocaleString();
  const readingsLine = sampling && sampling.downsampled
    ? `${used} of ${(sampling.rawReadings || 0).toLocaleString()} readings · ${days} days`
    : `${used} readings · ${days} days`;
  stats.append(
    stat("% Time CGM Active", pct(cov.percentActive) + "%", readingsLine),
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
  return el("div", { class: "stat" + (wide ? " wide" : "") }, [
    el("div", { class: "k", text: k }),
    el("div", { class: "v", html: valHtml }),
    el("div", { class: "s", text: sub }),
  ]);
}

function griStat(gri) {
  // Tokens, not hex: these chips must re-tokenise with everything else in the print face.
  const zoneColor = {
    A: "var(--b-inrange)", B: "var(--zone-b)", C: "var(--zone-c)",
    D: "var(--b-vhigh)", E: "var(--b-vlow)",
  }[gri.zone] || "var(--faint)";
  const v = el("div", { class: "v" });
  v.append(document.createTextNode(gri.value == null ? "--" : String(Math.round(gri.value))));
  v.appendChild(el("span", { class: "gri-chip", text: `Zone ${gri.zone || "—"}`, style: `background:${zoneColor}` }));
  return el("div", { class: "stat" }, [
    el("div", { class: "k", text: "Glycemia Risk Index" }),
    v,
    el("div", { class: "s", text: `hypo ${gri.hypoComponent == null ? "--" : gri.hypoComponent.toFixed(1)} · hyper ${gri.hyperComponent == null ? "--" : gri.hyperComponent.toFixed(1)}` }),
  ]);
}

// Rows are ordered high→low so the stacked bar reads like the glucose axis.
const TIR_ROWS = [
  { key: "veryHighPct", band: 4, goal: null },
  { key: "highPct", band: 3, goal: "Goal for High + Very High: <25%" },
  { key: "inRangePct", band: 2, goal: "Goal: >70% (>16h 48m)" },
  { key: "lowPct", band: 1, goal: "Goal for Low + Very Low: <4%" },
  { key: "veryLowPct", band: 0, goal: "Goal: <1%" },
];
// Band ranges are derived from the live thresholds, so the legend always matches the
// numbers the server computed.
function bandRangeText(band) {
  const g = (v) => fmtGlu(v);
  switch (band) {
    case 4: return `>${g(TH.veryHigh)}`;
    case 3: return `${g(TH.high)}–${g(TH.veryHigh)}`;
    case 2: return `${g(TH.low)}–${g(TH.high)}`;
    case 1: return `${g(TH.veryLow)}–${g(TH.low)}`;
    default: return `<${g(TH.veryLow)}`;
  }
}
const bandVar = (i) => `var(--${BAND_CLASS[i]})`;

function tirGoalBar(tir) {
  const bar = el("div", { class: "tir-bar" });
  for (const r of TIR_ROWS) {
    const v = tir[r.key] || 0;
    const seg = el("div", { class: "tir-seg", style: `flex-grow:${Math.max(v, 0.6)};background:${bandVar(r.band)}` });
    if (v >= 6) seg.appendChild(el("span", { text: `${v.toFixed(0)}%` }));
    bar.appendChild(seg);
  }
  const key = el("div", { class: "tir-key" });
  for (const r of TIR_ROWS) {
    const v = tir[r.key];
    key.appendChild(el("div", { class: "tir-row" }, [
      el("span", { class: "sw", style: `background:${bandVar(r.band)}` }),
      el("span", { class: "lab", html: `<b>${BAND_LABEL[r.band]}</b> <i>${bandRangeText(r.band)} ${unitName()}</i>` }),
      el("span", { class: "pct", text: v == null ? "--" : `${v.toFixed(0)}%` }),
    ]));
    if (r.goal) key.appendChild(el("div", { class: "tir-row" }, [el("span", { class: "goal", text: r.goal })]));
  }
  return el("div", { class: "tir" }, [bar, key]);
}

// ── AGP percentile chart ──────────────────────────────────────────────────────
function agpSection(agp) {
  const wrap = el("div", { class: "chart-card" });
  const bins = ((agp && agp.bins) || []).filter((b) => b.n > 0 && b.p50 != null);
  if (bins.length < 4) {
    wrap.appendChild(el("div", { class: "status", text: "Not enough data for an AGP — needs about a day of readings." }));
  } else {
    // The server reports the bin width it actually used; never assume the 15-minute default.
    wrap.appendChild(agpSvg(bins, (agp && agp.binMinutes) || bin));
    wrap.appendChild(agpLegend());
  }
  return el("section", { class: "section" }, [
    el("h2", { class: "section-title", text: "Ambulatory Glucose Profile (AGP)" }),
    wrap,
    el("p", { class: "chart-note", text: "Every day in the period overlaid on one 24-hour day. Median (50%) line, inner band 25–75%, outer band 5–95%." }),
  ]);
}

// Matches the `@media (max-width: 560px)` breakpoint in report.css — which APPLIES at
// exactly 560px, so this must be `<=`, not `<`, or the labels are spaced for one size and
// rendered at the other.
const isNarrow = () => window.innerWidth <= 560;
// Axis text is sized in viewBox units, so it scales with the chart; small screens use a
// larger value to stay legible. KEEP IN SYNC with the `.axis-text` rule in report.css.
const axisFontUnits = () => (isNarrow() ? 20 : 12);

// Shared y-axis scaffolding for the 24-hour charts: gridlines, value labels, the target
// band and its dashed edges, and 3-hourly time labels. Colours come from CSS classes.
function drawDayAxes(root, geom, yTickStep) {
  const { padL, padT, plotW, plotH, yMin, yMax, yFor, xFor, h } = geom;
  root.appendChild(svg("rect", {
    class: "target-band", x: padL, y: yFor(Math.min(TH.high, yMax)),
    width: plotW, height: Math.max(0, yFor(Math.max(TH.low, yMin)) - yFor(Math.min(TH.high, yMax))),
  }));

  const inRange = (v) => v >= yMin && v <= yMax;
  const round = [];
  for (let v = Math.ceil(yMin / yTickStep) * yTickStep; v <= yMax; v += yTickStep) round.push(v);
  // Gridlines for every tick; labels only where they won't collide. The clinical
  // thresholds are placed FIRST so a crowded axis keeps the target edges and drops a
  // round number instead.
  for (const v of [...new Set([TH.low, TH.high, ...round])].filter(inRange).sort((a, b) => a - b)) {
    const yy = yFor(v);
    root.appendChild(svg("line", { class: "grid-line", x1: padL, x2: padL + plotW, y1: yy, y2: yy }));
  }
  const minGap = axisFontUnits() * 1.7;
  const placed = [];
  const label = (v) => {
    if (!inRange(v)) return;
    const yy = yFor(v);
    if (placed.some((p) => Math.abs(p - yy) < minGap)) return;
    placed.push(yy);
    const t = svg("text", { class: "axis-text", x: padL - 8, y: yy + 4, "text-anchor": "end" });
    t.textContent = unit === "mmol/l" ? (v / MGDL_PER_MMOL).toFixed(1) : String(v);
    root.appendChild(t);
  };
  label(TH.low);
  label(TH.high);
  for (const v of round) label(v);
  for (const v of [TH.low, TH.high]) {
    if (v < yMin || v > yMax) continue;
    root.appendChild(svg("line", { class: "target-line", x1: padL, x2: padL + plotW, y1: yFor(v), y2: yFor(v) }));
  }
  // Time labels every 3 h, or every 6 h when the chart is drawn small. The first and last
  // are anchored inward so they can't clip outside the plot area.
  const xStep = isNarrow() ? 360 : 180;
  for (let m = 0; m <= 1440; m += xStep) {
    const t = svg("text", {
      class: "axis-text", x: xFor(m), y: h - 8,
      "text-anchor": m === 0 ? "start" : m === 1440 ? "end" : "middle",
    });
    t.textContent = String(Math.floor(m / 60)).padStart(2, "0") + ":00";
    root.appendChild(t);
  }
  // The rotated unit caption is a nicety, not information — at small sizes it would collide
  // with the value labels, and the stat tiles already carry the unit.
  if (!isNarrow()) {
    const yl = svg("text", { class: "axis-text", x: 12, y: padT + plotH / 2, transform: `rotate(-90 12 ${padT + plotH / 2})`, "text-anchor": "middle" });
    yl.textContent = unitName();
    root.appendChild(yl);
  }
}

// Geometry for a 24-hour chart with a value range covering [lo, hi] plus the target band.
function dayGeometry(w, h, lo, hi) {
  const padL = 46, padR = 16, padT = 14, padB = 26;
  const plotW = w - padL - padR, plotH = h - padT - padB;
  // Floor at 0, not 40: `yFor` clamps, so a hard 40 floor would draw a severe hypo as a
  // flat plateau along the axis and hide exactly the excursion that matters most.
  const yMin = Math.max(0, Math.floor((Math.min(lo, TH.low) - 8) / 10) * 10);
  const yMax = Math.ceil((Math.max(hi, TH.high) + 10) / 10) * 10;
  const xFor = (m) => padL + (m / 1440) * plotW;
  const yFor = (v) => padT + (1 - (Math.max(yMin, Math.min(yMax, v)) - yMin) / (yMax - yMin)) * plotH;
  return { padL, padR, padT, padB, plotW, plotH, yMin, yMax, xFor, yFor, w, h };
}

function agpSvg(bins, binMinutes) {
  const geom = dayGeometry(960, 300,
    Math.min(...bins.map((b) => b.p05)), Math.max(...bins.map((b) => b.p95)));
  const { xFor, yFor } = geom;
  // Plot each bin at its CENTRE. The server reports `minuteOfDay` as the bin's start, so the
  // offset is half the actual bin width — hardcoding 7.5 would shift the whole envelope
  // whenever `?bin=` isn't the default 15.
  const half = binMinutes / 2;

  // Bins with no readings were filtered out; drawing straight through the hole would
  // FABRICATE a percentile envelope across hours the sensor never covered. Split into runs
  // of adjacent bins and draw each run separately, so a gap reads as a gap.
  const runs = [];
  let run = [];
  for (const b of bins) {
    const prev = run[run.length - 1];
    if (prev && b.minuteOfDay - prev.minuteOfDay > binMinutes * 1.5) { runs.push(run); run = []; }
    run.push(b);
  }
  if (run.length) runs.push(run);

  const pt = (b, k) => ({ x: xFor(b.minuteOfDay + half), y: yFor(b[k]) });
  const bandPath = (seg, upKey, dnKey) => {
    const up = seg.map((b) => pt(b, upKey));
    const dn = seg.map((b) => pt(b, dnKey));
    let d = `M${up[0].x.toFixed(1)},${up[0].y.toFixed(1)}`;
    for (let i = 1; i < up.length; i++) d += ` L${up[i].x.toFixed(1)},${up[i].y.toFixed(1)}`;
    for (let i = dn.length - 1; i >= 0; i--) d += ` L${dn[i].x.toFixed(1)},${dn[i].y.toFixed(1)}`;
    return d + " Z";
  };
  const linePath = (seg, k) =>
    "M" + seg.map((b) => { const q = pt(b, k); return `${q.x.toFixed(1)},${q.y.toFixed(1)}`; }).join(" L");

  const root = svg("svg", {
    viewBox: `0 0 ${geom.w} ${geom.h}`, width: "100%", preserveAspectRatio: "xMidYMid meet",
    role: "img", "aria-label": "Ambulatory glucose profile",
  });
  drawDayAxes(root, geom, 50);
  for (const seg of runs) {
    if (seg.length < 2) continue; // a lone bin has no envelope to draw
    root.appendChild(svg("path", { class: "agp-band-outer", d: bandPath(seg, "p95", "p05") }));
    root.appendChild(svg("path", { class: "agp-band-inner", d: bandPath(seg, "p75", "p25") }));
    root.appendChild(svg("path", { class: "agp-median", d: linePath(seg, "p50") }));
  }
  return root;
}

function agpLegend() {
  const item = (markerClass, label, style) =>
    el("span", {}, [el("span", { class: markerClass, style: style || null }), document.createTextNode(label)]);
  return el("div", { class: "agp-legend" }, [
    item("ln", "Median (50%)"),
    item("sw", "IQR 25–75%", "background:var(--b-inrange);opacity:.55"),
    item("sw", "5–95%", "background:var(--b-inrange);opacity:.28"),
    item("ln target", `Target ${bandRangeText(2)}`),
  ]);
}

// ── daily profiles calendar ───────────────────────────────────────────────────
// Kept in module scope so the day modal can navigate and re-render without refetching the
// report: the window bounds define which days are reachable.
let REPORT_START_DAY = 0;
let REPORT_END_DAY = 0;

function dailyProfilesSection(rawEntries, startDay, endDay) {
  REPORT_START_DAY = startDay;
  REPORT_END_DAY = endDay;

  // Group the sample series into local days once.
  const byDay = new Map();
  for (const e of rawEntries) {
    if (e.date < startMs || e.date > endMs) continue;
    const dn = localDayNumber(e.date);
    let list = byDay.get(dn);
    if (!list) byDay.set(dn, (list = []));
    list.push({ t: localMinuteOfDay(e.date), v: e.mgdl });
  }

  const dow = el("div", { class: "days-dow" }, DOW.map((d) => el("span", { text: d })));
  const grid = el("div", { class: "days-grid" });
  const frag = document.createDocumentFragment();
  // Lead with empty cells so the first day lands under its weekday column.
  const lead = ((startDay % 7) + 7 + 4) % 7; // 0 = Sunday
  for (let i = 0; i < lead; i++) frag.appendChild(el("div", { class: "day empty", attrs: { "aria-hidden": "true" } }));
  for (let dn = startDay; dn <= endDay; dn++) frag.appendChild(dayCell(dn, byDay.get(dn)));
  grid.appendChild(frag);
  grid.addEventListener("click", onDayGridClick);

  return el("section", { class: "section" }, [
    el("h2", { class: "section-title", text: "Daily Glucose Profiles" }),
    el("p", { class: "chart-note", style: "margin:-8px 2px 12px", text: "Coloured by glucose band. Select a day for the full detail view." }),
    dow,
    grid,
  ]);
}

function onDayGridClick(ev) {
  const cell = ev.target.closest("button.day");
  if (cell && cell.dataset.day) openDayModal(Number(cell.dataset.day), cell);
}

function dayCell(dayNum, readings) {
  const lbl = dayLabel(dayNum);
  const has = readings && readings.length > 0;
  const cell = el("button", {
    class: "day" + (has ? "" : " empty"),
    attrs: { type: "button", "data-day": String(dayNum), "aria-label": `${fmtDate(dayNum)} — open detail` },
  });
  const top = el("div", { class: "d-top" }, [el("span", { class: "d-date", text: `${lbl.num} ${lbl.mon}` })]);
  cell.appendChild(top);

  if (!has) {
    cell.appendChild(el("div", { class: "d-empty", text: "no data" }));
    return cell;
  }
  readings.sort((a, b) => a.t - b.t);
  const inRange = (readings.filter((r) => r.v >= TH.low && r.v <= TH.high).length / readings.length) * 100;
  top.appendChild(el("span", {
    class: "d-tir " + (inRange >= 70 ? "good" : inRange >= 50 ? "mid" : "poor"),
    text: `${inRange.toFixed(0)}%`,
  }));
  cell.appendChild(daySparkline(readings));
  return cell;
}

// A compact 24-hour trace over the target band, coloured by glucose band.
function daySparkline(readings) {
  const w = 120, h = 42, padY = 3;
  // A fixed scale keeps every thumbnail comparable, but it has to contain the bands in use —
  // with a custom very-high above 320 the whole high range would otherwise flatten onto the
  // ceiling and every day would look identical.
  const yMin = Math.min(40, TH.veryLow - 10);
  const yMax = Math.max(320, TH.veryHigh + 30);
  const xFor = (m) => (m / 1440) * w;
  const yFor = (v) => padY + (1 - (Math.max(yMin, Math.min(yMax, v)) - yMin) / (yMax - yMin)) * (h - padY * 2);
  const root = svg("svg", { viewBox: `0 0 ${w} ${h}`, width: "100%", preserveAspectRatio: "none", "aria-hidden": "true" });
  root.appendChild(svg("rect", { class: "target-band", x: 0, y: yFor(TH.high), width: w, height: yFor(TH.low) - yFor(TH.high) }));
  for (const v of [TH.low, TH.high]) {
    // `.thin` — a presentation attribute would lose to the `.target-line` CSS rule.
    root.appendChild(svg("line", { class: "target-line thin", x1: 0, x2: w, y1: yFor(v), y2: yFor(v) }));
  }
  appendTrace(root, readings, xFor, yFor, GAP_BREAK_MIN, 1.2);
  return root;
}

// ── day detail modal ──────────────────────────────────────────────────────────
// Each day is fetched on demand at full stored resolution (a single day is small, so the
// numbers here are exact even when the overview had to downsample) and cached, so paging
// back and forth is instant.
const dayCache = new Map();
// A dense day can hold thousands of samples; keep only the most recently viewed days so
// paging through a long report can't grow without bound. Insertion order is recency: a hit
// re-inserts, so the evicted entry is the least-recently *used*, not merely the oldest.
const DAY_CACHE_MAX = 24;
function cacheDay(dayNum, data) {
  dayCache.set(dayNum, data);
  while (dayCache.size > DAY_CACHE_MAX) dayCache.delete(dayCache.keys().next().value);
}
function touchDay(dayNum) {
  const hit = dayCache.get(dayNum);
  if (hit !== undefined) { dayCache.delete(dayNum); dayCache.set(dayNum, hit); }
  return hit;
}
// Bumped whenever the thresholds change. A day fetch already in flight when the bands change
// was computed against the OLD bands, so its response must be discarded rather than cached —
// otherwise paging back to that day would show metrics that contradict the current legend.
let bandsGeneration = 0;
let modalDay = null;
let modalReturnFocus = null;
let modalToken = 0;

function wireModal() {
  const modal = document.getElementById("day-modal");
  document.getElementById("day-close").addEventListener("click", closeDayModal);
  document.getElementById("day-prev").addEventListener("click", () => stepDay(-1));
  document.getElementById("day-next").addEventListener("click", () => stepDay(1));
  document.getElementById("day-picker").addEventListener("change", (e) => {
    const dn = dayNumFromIso(e.target.value);
    if (dn != null) showDay(Math.min(REPORT_END_DAY, Math.max(REPORT_START_DAY, dn)));
  });
  // Click the backdrop (not the panel) to dismiss.
  modal.addEventListener("mousedown", (e) => { if (e.target === modal) closeDayModal(); });
  document.addEventListener("keydown", (e) => {
    if (modal.hidden) return;
    if (e.key === "Escape") { closeDayModal(); return; }
    // `aria-modal` only tells assistive tech the background is inert; it does not stop Tab.
    // Cycle focus within the panel so keyboard users can't wander into the page behind.
    if (e.key === "Tab") {
      const focusable = [...document.getElementById("day-modal-panel")
        .querySelectorAll('button:not(:disabled), input:not(:disabled), [href], [tabindex]:not([tabindex="-1"])')];
      if (!focusable.length) return;
      const first = focusable[0], last = focusable[focusable.length - 1];
      const active = document.activeElement;
      if (e.shiftKey && (active === first || !focusable.includes(active))) { e.preventDefault(); last.focus(); }
      else if (!e.shiftKey && active === last) { e.preventDefault(); first.focus(); }
      return;
    }
    // Arrows page days, unless the user is typing in the date field.
    if (document.activeElement && document.activeElement.id === "day-picker") return;
    if (e.key === "ArrowLeft") { e.preventDefault(); stepDay(-1); }
    if (e.key === "ArrowRight") { e.preventDefault(); stepDay(1); }
  });
}

function openDayModal(dayNum, trigger) {
  modalReturnFocus = trigger || null;
  const modal = document.getElementById("day-modal");
  modal.hidden = false;
  document.body.style.overflow = "hidden";
  showDay(dayNum);
  document.getElementById("day-close").focus();
}

function closeDayModal() {
  document.getElementById("day-modal").hidden = true;
  document.body.style.overflow = "";
  modalDay = null;
  if (modalReturnFocus) { modalReturnFocus.focus(); modalReturnFocus = null; }
}

function stepDay(delta) {
  if (modalDay == null) return;
  const next = modalDay + delta;
  if (next < REPORT_START_DAY || next > REPORT_END_DAY) return;
  showDay(next);
}

async function showDay(dayNum) {
  modalDay = dayNum;
  const lbl = dayLabel(dayNum);
  const title = document.getElementById("day-modal-title");
  title.innerHTML = "";
  title.append(document.createTextNode(fmtDate(dayNum)), el("span", { class: "dow", text: lbl.dow }));

  const picker = document.getElementById("day-picker");
  picker.min = isoDate(REPORT_START_DAY);
  picker.max = isoDate(REPORT_END_DAY);
  picker.value = isoDate(dayNum);
  const prevBtn = document.getElementById("day-prev");
  const nextBtn = document.getElementById("day-next");
  const wasFocused = document.activeElement;
  prevBtn.disabled = dayNum <= REPORT_START_DAY;
  nextBtn.disabled = dayNum >= REPORT_END_DAY;
  // Paging to the first/last day disables the very button under the cursor; without this,
  // focus falls to <body> and the arrow keys stop working mid-navigation.
  if ((wasFocused === prevBtn && prevBtn.disabled) || (wasFocused === nextBtn && nextBtn.disabled)) {
    (prevBtn.disabled ? nextBtn : prevBtn).focus();
  }

  const body = document.getElementById("day-modal-body");
  const token = ++modalToken; // ignore a slow response the user has already paged past
  const generation = bandsGeneration; // ...and one computed against superseded thresholds

  const cached = touchDay(dayNum);
  if (cached !== undefined) {
    renderDayDetail(body, cached);
    return;
  }
  body.innerHTML = "";
  body.appendChild(el("div", { class: "modal-status", text: "Loading day…" }));
  try {
    // The day's own window, in local time.
    const dayStart = dayNum * DAY_MS - tz * 60_000;
    const q = new URLSearchParams({
      format: "json", start: dayStart, end: dayStart + DAY_MS - 1, tzOffset: tz, samples: "1",
      ...bandQuery(),
    });
    const data = await fetchJson(`/api/v4/export?${q}`);
    // Only keep it if the bands haven't changed under us — otherwise this response describes
    // bands the report is no longer showing.
    if (generation !== bandsGeneration) return;
    cacheDay(dayNum, data);
    if (token !== modalToken) return; // superseded
    renderDayDetail(body, data);
  } catch (e) {
    if (token !== modalToken || generation !== bandsGeneration) return;
    body.innerHTML = "";
    body.appendChild(el("div", { class: "modal-status err", text: `Could not load this day (${e}).` }));
  }
}

function renderDayDetail(body, data) {
  const a = data.analytics || {};
  const tir = a.timeInRange || {};
  const pts = (data.samples || []).map(([ms, mgdl]) => ({ t: localMinuteOfDay(ms), v: mgdl })).sort((x, y) => x.t - y.t);

  body.innerHTML = "";
  if (!pts.length) {
    body.appendChild(el("div", { class: "modal-status", text: "No readings stored for this day." }));
    return;
  }

  const values = pts.map((p) => p.v);
  const lo = Math.min(...values), hi = Math.max(...values);
  body.appendChild(el("div", { class: "chart-card" }, [dayDetailChart(pts, lo, hi)]));

  // Key stats. The average is tinted by its own band so the day reads at a glance.
  const mean = a.meanMgdl;
  const metrics = el("div", { class: "modal-metrics" }, [
    mstat("Average", `${fmtGlu(mean)}<small>${unitName()}</small>`, `SD ${fmtGlu(a.sdMgdl)}`, mean == null ? null : BAND_CLASS[bandIndex(mean)].slice(2)),
    mstat("Time in Range", `${pct(tir.inRangePct)}<small>%</small>`, `${bandRangeText(2)} ${unitName()}`, "inrange"),
    mstat("Lowest", `${fmtGlu(lo)}<small>${unitName()}</small>`, BAND_LABEL[bandIndex(lo)], BAND_CLASS[bandIndex(lo)].slice(2)),
    mstat("Highest", `${fmtGlu(hi)}<small>${unitName()}</small>`, BAND_LABEL[bandIndex(hi)], BAND_CLASS[bandIndex(hi)].slice(2)),
    mstat("Variability", `${pct(a.cvPercent)}<small>%</small>`, a.cvPercent == null ? "—" : a.cvPercent <= 36 ? "stable" : "elevated"),
    mstat("Readings", (a.n || pts.length).toLocaleString(), `${pct((a.coverage || {}).percentActive)}% active`),
  ]);
  body.appendChild(metrics);

  // Horizontal TIR bar + key.
  const bar = el("div", { class: "mtir" });
  const key = el("div", { class: "mtir-key" });
  for (const r of TIR_ROWS) {
    const v = tir[r.key] || 0;
    if (v > 0) bar.appendChild(el("i", { style: `flex:${v};background:${bandVar(r.band)}` }));
    key.appendChild(el("span", {}, [
      el("span", { class: "sw", style: `background:${bandVar(r.band)}` }),
      document.createTextNode(`${BAND_LABEL[r.band]} ${v.toFixed(0)}%`),
    ]));
  }
  if (!bar.children.length) bar.appendChild(el("i", { style: "flex:1;background:var(--rim)" }));
  body.append(bar, key);
}

function mstat(k, valHtml, sub, band) {
  const node = el("div", { class: "mstat" }, [
    el("div", { class: "k", text: k }),
    el("div", { class: "v", html: valHtml }),
    el("div", { class: "s", text: sub }),
  ]);
  if (band) node.setAttribute("data-band", band);
  return node;
}

function dayDetailChart(pts, lo, hi) {
  const geom = dayGeometry(960, 300, lo, hi);
  const root = svg("svg", {
    viewBox: `0 0 ${geom.w} ${geom.h}`, width: "100%", preserveAspectRatio: "xMidYMid meet",
    role: "img", "aria-label": "Glucose for the selected day",
  });
  drawDayAxes(root, geom, 50);
  appendTrace(root, pts, geom.xFor, geom.yFor, GAP_BREAK_MIN, 2.2);
  return root;
}

// ── footer ────────────────────────────────────────────────────────────────────
function footer(rep) {
  const foot = el("section", { class: "section rpt-foot" });
  foot.appendChild(el("p", {}, [
    el("span", { class: "disc", text: "Not a medical device. " }),
    document.createTextNode(
      "These metrics are estimates for personal and clinical review, not a basis for treatment decisions. " +
      "Time-in-Range bands and goals follow the 2019 international consensus (Battelino et al., Diabetes Care 2019); " +
      "GMI = 3.31 + 0.02392 × mean; uGMI is the 2026 Diabetologia revision; the Glycemia Risk Index follows Klonoff et al. 2023."),
  ]));
  // When the window was dense enough to need downsampling, say so plainly — including the
  // one thing it can actually affect: an excursion shorter than the sample interval may be
  // under-represented, so a brief nadir/peak can read less extreme than it truly was.
  const s = rep.sampling;
  if (s && s.downsampled) {
    const mins = Math.round((s.bucketMs || 0) / 60000);
    foot.appendChild(el("p", {}, [
      el("span", { class: "disc", text: "Sampling. " }),
      document.createTextNode(
        `Overview computed from ${(s.usedReadings || 0).toLocaleString()} samples at ${mins}-minute resolution, ` +
        `taken evenly across the period from ${(s.rawReadings || 0).toLocaleString()} stored readings. ` +
        "Averages, Time in Range, GMI/CV and the AGP percentile bands are unaffected at this density; " +
        `excursions shorter than ${mins} minutes may be under-represented, so a brief low or high can read less extreme than it was. ` +
        "Opening a single day loads that day at full stored resolution."),
    ]));
  }
  foot.appendChild(el("p", { text: "NightKnight — private, self-hosted CGM. https://github.com/TGRGIT/NightKnight" }));
  return foot;
}

function logoSvg() {
  // The NightKnight crescent glyph on the brand red, so the header reads as ours.
  const s = svg("svg", { class: "rpt-logo", viewBox: "0 0 100 100", role: "img", "aria-label": "NightKnight" });
  s.appendChild(svg("rect", { x: 0, y: 0, width: 100, height: 100, rx: 24, fill: "#e5484d" }));
  s.appendChild(svg("path", { d: "M64 26a28 28 0 1 0 0 48 22 22 0 1 1 0-48Z", fill: "#fff" }));
  s.appendChild(svg("circle", { cx: 70, cy: 38, r: 4.4, fill: "#fff" }));
  return s;
}

main();
