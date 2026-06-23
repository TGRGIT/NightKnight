// NightKnight dashboard. Talks to the v4 API (same-origin, credentials included so
// the Cloudflare Access / proxy session cookie rides along). Polls for fresh data.

import { drawGlucoseChart } from "/chart.js";

const MGDL_PER_MMOL = 18.0156;
const LOW = 70, HIGH = 180;

const state = {
  unit: localStorage.getItem("nk-unit") || "mg/dl",
  periodDays: 7,
  chartHours: 24,
  provider: "dexcom",
  entries: [],
};

const $ = (s) => document.querySelector(s);
const $$ = (s) => document.querySelectorAll(s);

async function api(path, opts = {}) {
  const res = await fetch(path, {
    credentials: "include",
    headers: { "content-type": "application/json" },
    ...opts,
  });
  if (res.status === 401) { showBanner("Not signed in. Open this app through the authenticated URL."); throw new Error("unauthorized"); }
  if (res.status === 403) { showBanner("Access denied — you're not in the night_knight_users group."); throw new Error("forbidden"); }
  if (!res.ok) throw new Error(`${path} → ${res.status}`);
  return res.status === 204 ? null : res.json();
}

function showBanner(msg) { const b = $("#banner"); b.textContent = msg; b.hidden = false; }
function clearBanner() { $("#banner").hidden = true; }

function fmt(mgdl, unit) { return unit === "mmol/l" ? (mgdl / MGDL_PER_MMOL).toFixed(1) : String(Math.round(mgdl)); }
function bandClass(mgdl) { if (mgdl < 54 || mgdl > 250) return "danger"; if (mgdl < LOW || mgdl > HIGH) return "warn"; return "inrange"; }

// ---- rendering ----------------------------------------------------------

function renderCurrent(current) {
  const hero = $("#hero");
  if (!current) {
    $("#bg-value").textContent = "--"; $("#bg-trend").textContent = "·";
    $("#bg-age").textContent = "no data"; $("#bg-delta").textContent = "";
    hero.removeAttribute("data-band"); return;
  }
  const mgdl = current.mgdl;
  $("#bg-value").textContent = fmt(mgdl, state.unit);
  $("#bg-unit").textContent = state.unit === "mmol/l" ? "mmol/L" : "mg/dL";
  $("#bg-trend").textContent = current.trend || "·";
  hero.setAttribute("data-band", bandClass(mgdl));
  const ageMin = Math.round((Date.now() - current.date) / 60000);
  $("#bg-age").textContent = ageMin <= 0 ? "just now" : `${ageMin} min ago`;
  $("#bg-delta").textContent = current.direction ? current.direction.replace(/([A-Z])/g, " $1").trim() : "";
}

function renderChart() {
  $("#chart-empty").hidden = state.entries.length > 0;
  drawGlucoseChart($("#chart"), {
    entries: state.entries, hours: state.chartHours,
    low: LOW, high: HIGH, unit: state.unit, now: Date.now(),
  });
}

function renderAnalytics(a) {
  const setHero = (id, v, suffix = "") =>
    ($(id).innerHTML = v == null ? "--" : `${v}<small>${suffix}</small>`);
  setHero("#m-a1c", a.estimatedA1cPercent == null ? null : a.estimatedA1cPercent.toFixed(1), "%");
  $("#m-gmi").textContent = a.gmiPercent == null ? "GMI —" : `GMI ${a.gmiPercent.toFixed(1)}%`;
  $("#m-avg").innerHTML = a.meanMgdl == null ? "--" : fmt(a.meanMgdl, state.unit);
  $("#m-avg-unit").textContent = state.unit === "mmol/l" ? "mmol/L" : "mg/dL";
  const tir = a.timeInRange;
  setHero("#m-tir", tir.inRangePct == null ? null : tir.inRangePct.toFixed(0), "%");
  $("#m-n").textContent = `${a.n} readings`;
  setHero("#m-cv", a.cvPercent == null ? null : a.cvPercent.toFixed(0), "%");
  $("#m-cv-note").textContent = a.cvPercent == null ? "—" : a.cvPercent <= 36 ? "stable" : "variable";

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
      const seg = document.createElement("div");
      seg.className = "tir-seg"; seg.style.width = `${pct}%`; seg.style.background = color;
      seg.title = `${label}: ${pct.toFixed(0)}%`; bar.appendChild(seg);
    }
    const li = document.createElement("span");
    li.innerHTML = `<span class="dot" style="background:${color}"></span>${label}: ${pct.toFixed(0)}%`;
    legend.appendChild(li);
  }
}

// ---- data flow ----------------------------------------------------------

async function refreshLive() {
  try {
    const [current, entries] = await Promise.all([
      api("/api/v4/current"),
      api(`/api/v4/entries?hours=${state.chartHours}`),
    ]);
    clearBanner();
    state.entries = (entries.entries || []).map((e) => ({ date: e.date, mgdl: e.mgdl }));
    renderCurrent(current.current);
    renderChart();
  } catch (e) { if (!/unauthorized|forbidden/.test(String(e))) console.error(e); }
}

async function refreshAnalytics() {
  try { renderAnalytics(await api(`/api/v4/analytics?hours=${state.periodDays * 24}`)); }
  catch (e) { if (!/unauthorized|forbidden/.test(String(e))) console.error(e); }
}

async function loadMe() {
  try {
    const me = await api("/api/v4/me");
    $("#user-email").textContent = me.subject || "";
    if (me.preferredUnit) { state.unit = me.preferredUnit; localStorage.setItem("nk-unit", state.unit); }
  } catch (_) {}
  syncActive(".unit-toggle", "unit", state.unit);
}

// ---- connectors ---------------------------------------------------------

function fmtAgo(ms) {
  if (!ms) return "never";
  const m = Math.round((Date.now() - ms) / 60000);
  if (m < 1) return "just now"; if (m < 60) return `${m} min ago`;
  return `${Math.round(m / 60)} h ago`;
}

async function loadConnectors() {
  try {
    const { connectors } = await api("/api/v4/connectors");
    const list = $("#connector-list");
    list.innerHTML = "";
    const names = { dexcom: "Dexcom Share", librelinkup: "LibreLinkUp" };
    for (const c of connectors) {
      const ok = (c.lastStatus || "").startsWith("ok");
      const dot = c.lastStatus ? (ok ? "ok" : "err") : "";
      const li = document.createElement("li");
      li.innerHTML =
        `<span><span class="s-dot ${dot}"></span><span class="s-title">${names[c.provider] || c.provider}</span>` +
        `<div class="s-sub">${c.lastStatus || "awaiting first sync"} · ${fmtAgo(c.lastSyncAt)}</div></span>`;
      const btn = document.createElement("button");
      btn.textContent = "Remove";
      btn.onclick = async () => { await api(`/api/v4/connectors/${c.provider}`, { method: "DELETE" }); loadConnectors(); };
      li.appendChild(btn);
      list.appendChild(li);
    }
  } catch (_) {}
}

async function submitConnector(e) {
  e.preventDefault();
  const provider = state.provider;
  const user = $("#c-user").value.trim();
  const pass = $("#c-pass").value;
  const body = provider === "dexcom"
    ? { username: user, password: pass, region: $("#c-region").value }
    : { email: user, password: pass };
  try {
    await api(`/api/v4/connectors/${provider}`, { method: "PUT", body: JSON.stringify(body) });
    $("#c-pass").value = "";
    showBanner(`${provider === "dexcom" ? "Dexcom" : "LibreLinkUp"} connected — first sync within a minute.`);
    loadConnectors();
  } catch (err) { showBanner(`Could not connect: ${err}`); }
}

// ---- tokens -------------------------------------------------------------

async function loadTokens() {
  try {
    const { tokens } = await api("/api/v4/tokens");
    const list = $("#token-list"); list.innerHTML = "";
    for (const t of tokens.filter((t) => !t.revoked)) {
      const li = document.createElement("li");
      li.innerHTML = `<span><span class="s-title">${t.name}</span><div class="s-sub">${t.scopes.join(", ")}</div></span>`;
      const btn = document.createElement("button");
      btn.textContent = "Revoke";
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
  const reveal = $("#token-reveal");
  reveal.hidden = false;
  reveal.textContent = `Token for "${name}" (copy now — shown once): ${res.token}`;
  $("#token-name").value = "";
  loadTokens();
}

// ---- wiring -------------------------------------------------------------

function syncActive(container, dataKey, value) {
  $$(`${container} .seg-btn`).forEach((b) => b.classList.toggle("is-active", b.dataset[dataKey] === String(value)));
}

function wire() {
  $$(".unit-toggle .seg-btn").forEach((b) => b.addEventListener("click", async () => {
    state.unit = b.dataset.unit; localStorage.setItem("nk-unit", state.unit);
    syncActive(".unit-toggle", "unit", state.unit);
    renderChart();
    try { await api("/api/v4/me", { method: "PUT", body: JSON.stringify({ preferredUnit: state.unit }) }); } catch (_) {}
    refreshLive(); refreshAnalytics();
  }));
  $$(".period-toggle .seg-btn").forEach((b) => b.addEventListener("click", () => {
    state.periodDays = Number(b.dataset.days);
    syncActive(".period-toggle", "days", state.periodDays);
    refreshAnalytics();
  }));
  $$(".range-toggle .seg-btn").forEach((b) => b.addEventListener("click", () => {
    state.chartHours = Number(b.dataset.hours);
    syncActive(".range-toggle", "hours", state.chartHours);
    refreshLive();
  }));
  $$(".provider-toggle .seg-btn").forEach((b) => b.addEventListener("click", () => {
    state.provider = b.dataset.provider;
    syncActive(".provider-toggle", "provider", state.provider);
    const dex = state.provider === "dexcom";
    $(".dexcom-only").style.display = dex ? "" : "none";
    $("#c-user").placeholder = dex ? "Username" : "Email";
  }));
  $("#connector-form").addEventListener("submit", submitConnector);
  $("#token-form").addEventListener("submit", createToken);
  window.addEventListener("resize", renderChart);
}

async function main() {
  wire();
  await loadMe();
  await Promise.all([refreshLive(), refreshAnalytics(), loadConnectors(), loadTokens()]);
  setInterval(() => { refreshLive(); refreshAnalytics(); loadConnectors(); }, 60_000);
}

main();
