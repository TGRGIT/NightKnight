# Statistical analysis — research & roadmap

Research notes for the "Statistical analysis" roadmap item in the [README](../README.md):
deeper CGM analytics beyond the current TIR / GMI / eA1c / CV, to surface in the web
dashboard and the iOS app.

This document is **research + a prioritised plan**, not a spec. It maps each candidate
metric to (a) its clinical basis, (b) an exact formula, (c) what it needs from the
store, (d) where it fits in the API, and (e) how to show it. Everything here computes
on canonical mg/dL inside [`GlucoseValue`](../service/crates/nightknight-core/src/units.rs),
so a mixed mg/dL + mmol/L stream stays correct — the same invariant the existing
[`analytics`](../service/crates/nightknight-core/src/analytics.rs) module already holds.

> **Not a medical device.** These metrics are for personal insight. They are estimates
> with documented assumptions and limitations, not a basis for treatment decisions.

---

## 1. Where we are today

`nightknight-core::analytics` already computes, and `GET /api/v4/analytics` already
serves, the heart of the 2019 international consensus core metric set:

| Metric | Status | Notes |
|---|---|---|
| Mean glucose | ✅ | `mean_mgdl` |
| Time in Range (5 bands) | ✅ | bands `<54 / 54–69 / 70–180 / 181–250 / >250` match the consensus exactly |
| GMI (Glucose Management Indicator) | ✅ | `3.31 + 0.02392 × mean` |
| Estimated A1c (ADAG) | ✅ | `(mean + 46.7) / 28.7` — older estimate; see §2 |
| CV (coefficient of variation) | ✅ | `SD/mean × 100`; `≤ 36%` = stable |
| SD | ✅ (internal) | `std_dev_mgdl`, not yet surfaced in the API |

The band thresholds in [`TirThresholds`](../service/crates/nightknight-core/src/analytics.rs)
are the consensus values (54 / 70 / 180 / 250) and are pinned by tests. This is a strong
foundation: several proposed metrics below reuse these exact bands and are therefore
cheap to add.

**The consensus core metric set** (what a complete CGM report should contain) is:

1. Number of days CGM is worn (**recommend ≥ 14**)
2. **% time CGM is active** (recommend > 70% of the window)
3. Mean glucose
4. **GMI**
5. **Glucose variability (CV%)**, target ≤ 36%
6. **TIR** 70–180 mg/dL, target **> 70%** (≈ 17 h/day)
7. **TBR** 54–69 (target < 4%) and **< 54** (target < 1%)
8. **TAR** 181–250 (target < 25%) and **> 250** (target < 5%)

We compute 3–8 already. We do **not** yet compute or surface **#1 days of data** and
**#2 % time active** — and those two gate the trustworthiness of everything else (see
P0 below).

---

## 2. Gaps & correctness issues to fix first (P0)

These are cheap, high-value, and make every downstream metric honest.

### P0.1 — Data-sufficiency: `% time CGM active` and `days covered`

Every consensus report leads with *how much data the metrics are based on*. Without it,
"TIR 85%" over 6 hours of a 14-day window is misleading. We have the timestamps; we just
don't summarise coverage.

- **Compute:** expected readings = `window / cadence` (assume 5-min cadence ⇒ 288/day),
  `percentActive = 100 × n / expected`, clamped to 100. Also report `daysCovered` =
  `(max_date − min_date) / 86 400 000` and the count of distinct calendar days with data.
- **Data:** already have `date_ms` per reading.
- **API:** add `percentActive`, `daysCovered`, `firstReading`, `lastReading` to
  `GET /api/v4/analytics`.
- **UI:** a small "based on N readings over D days (P% active)" caption under the metric
  cards; grey the cards out / show a "limited data" hint when `percentActive < 70` or
  `daysCovered < 14`.

### P0.2 — Time-weighted TIR (vs count-weighted)

The current TIR is **count-based** (the code comment notes this assumes uniform
sampling). Real connector data (Dexcom Share ≤ 24 h, LibreLinkUp ≈ 12 h) has gaps, and
count-weighting over-counts dense stretches. The AGP standard is fine with count-based
when sampling is uniform, but we should at least *offer* time-weighting.

- **Compute:** sort readings; for each consecutive pair within a max-gap (e.g. ≤ 15 min,
  else treat as a gap and don't accrue time), attribute the interval to the earlier
  reading's band. Percent = band-time / total-attributed-time.
- **Data:** timestamps + values (have).
- **API:** keep count-based as default for backward-compat; add `timeInRangeWeighted`
  (same shape as `timeInRange`) when there's enough data.
- **Why optional:** introduces gap-handling policy; keep the simple count metric as the
  headline and treat weighted as a refinement.

### P0.3 — Surface SD; clarify GMI vs eA1c

- `std_dev_mgdl` already exists — add `sdMgdl` to the analytics response.
- The consensus prefers **GMI**; ADAG **eA1c** is the older estimate and the two can
  diverge. Keep both but label eA1c as "legacy estimate" in the UI and lead with GMI.

---

## 3. New analytics, prioritised

### P1 — High value, low effort (reuse existing bands)

#### 3.1 Glycemia Risk Index (GRI) — *strongly recommended*

A single 0–100 composite (lower = better) that blends hypo and hyper risk, weighted by
330 clinicians' rankings. It "highlights extreme excursions and aligns with clinical
perception of risk" better than TIR alone — and it uses **exactly the bands we already
have**, so it's almost free.

```
Hypo  = VLow + 0.8 × Low          # VLow = %<54,  Low = %54–69
Hyper = VHigh + 0.5 × High        # VHigh = %>250, High = %181–250
GRI   = (3.0 × Hypo) + (1.6 × Hyper)        # capped at 100
      = 3.0×VLow + 2.4×Low + 1.6×VHigh + 0.8×High
```

Zones A–E (best→worst) are quintiles; the components `(Hyper, Hypo)` also map to a 2-D
risk plot. (Klonoff et al., 2022.)

- **Data:** the five TIR percentages (have).
- **API:** `gri` (number) + `griHypoComponent` / `griHyperComponent` on
  `GET /api/v4/analytics`.
- **UI:** one prominent number with a hypo/hyper split; optionally the 2-D GRI grid
  (x = hyper component, y = hypo component) as a future chart.

#### 3.2 Episode detection (hypo / hyper events)

TIR says *how much* time out of range; episodes say *how often and how long* — the thing
people actually act on. The clinical convention: a **level-1/2 hypo event** = ≥ 15 min
below threshold, ending after ≥ 15 min back above; analogous for hyper.

- **Compute:** scan the time-ordered series; open an event when a run crosses a
  threshold for ≥ 15 min, close it after ≥ 15 min recovered. Emit `{start, end,
  durationMin, nadir/peak, level}`. Report counts, total/longest duration, and
  events/day for: `<54`, `<70`, `>180`, `>250`, plus **nocturnal** (00:00–06:00) hypos.
- **Data:** timestamps + values (have); needs gap-awareness (don't bridge a 2 h gap).
- **API:** new `GET /api/v4/events?hours=&kind=hypo|hyper` returning the list +
  summary; also add `events` counts to analytics summary.
- **UI:** an "events" card (e.g. "3 lows, 1 nocturnal, longest 45 min"); markers on the
  chart.

#### 3.3 Estimated HbA1c confidence / data-window labeling

GMI is only meaningful over ≥ 14 days with > 70% active data. Gate the GMI/eA1c display
on P0.1 and show the window it was computed over. (No new math — a presentation rule.)

### P2 — High value, more effort

#### 3.4 Ambulatory Glucose Profile (AGP)

The standard one-page CGM picture: overlay every day of the window onto a single 24-hour
axis and draw **percentile bands** (5/25/50/75/95) of glucose by time-of-day, with the
median line. This is the single most informative visual in diabetes care.

- **Compute:** bucket readings by time-of-day (e.g. 288 five-minute or 96 fifteen-minute
  bins across 24 h); per bin compute median + the 5/25/75/95 percentiles. Needs a
  percentile/quantile helper (none in core yet) and a smoothing pass for sparse bins.
- **Data:** timestamps + values; ideally ≥ 14 days. Local-time bucketing needs the
  user's timezone (not currently stored — see §4).
- **API:** new `GET /api/v4/agp?days=14` → `{ bins: [{minuteOfDay, p05,p25,p50,p75,p95,n}] }`.
- **UI:** a new canvas chart (the existing bespoke chart in `web/dist/chart.js` is a good
  base): median line + IQR band (25–75) + outer band (5–95). The flagship report view.

#### 3.5 Time-of-day & day-of-week patterns

Cheaper cousins of AGP for at-a-glance pattern spotting:

- **Time-of-day:** mean and TIR per period — overnight (00–06), morning (06–12),
  afternoon (12–18), evening (18–24). Surfaces dawn phenomenon, post-dinner highs.
- **Day-of-week:** mean / TIR / GRI per weekday — surfaces weekend vs weekday control.
- **Compute:** group by local hour / weekday, reuse `GlucoseSummary::compute` per group.
- **API:** `GET /api/v4/patterns?days=14` → `{ byPeriod: [...], byWeekday: [...] }`.
- **UI:** small multiples / a heatmap (weekday × hour mean glucose) — very legible.

#### 3.6 Glucose variability beyond CV

For users who want depth. In rough order of value/clarity:

| Metric | What it captures | Notes |
|---|---|---|
| **SD** | absolute spread | already computed; surface it (P0.3) |
| **MAGE** | size of *meaningful* swings | mean of peak↔nadir amplitudes > 1 SD; classic but algorithm-sensitive |
| **MODD** | day-to-day reproducibility | mean abs diff at same time-of-day on consecutive days |
| **CONGA(n)** | within-day variability at lag n h | SD of (value − value n hours earlier) |
| **J-index** | combined mean+SD severity | `0.001 × (mean + SD)²` (mg/dL); non-diabetic ref 4.7–23.6 |
| **eHbA1c spread / GVI / PGS** | composite indices | lower priority; niche |

- **Recommendation:** ship **SD** and **J-index** first (trivial), then **MAGE** (it's
  the one users have heard of) with a clearly documented algorithm and tests, then MODD /
  CONGA if there's demand. These belong behind an "advanced" disclosure, not the headline.
- **API:** an opt-in `GET /api/v4/analytics?advanced=true` block, so the default payload
  stays lean.

#### 3.7 Exportable reports

- **CSV / JSON export** of entries + treatments + the computed metric set, for sharing
  with a clinician or importing elsewhere. Pairs with the planned historical-import work.
- **AGP one-pager** as a printable HTML / PDF report (client-side render of the AGP +
  the consensus metric table).
- **API:** `GET /api/v4/export?from=&to=&format=csv|json`.

---

## 4. Implementation considerations specific to NightKnight

- **Unit independence.** Keep computing on `mgdl()`; never branch analytics on the
  display unit. The existing tests (`analytics_are_unit_independent`) are the pattern.
- **Empty / sparse data must never panic.** Follow the existing convention: `Option`
  scalars, all-zero TIR with `n = 0`. Every new metric returns `null`/empty rather than
  `NaN` when undersupplied, and the UI degrades gracefully.
- **The `MAX_ANALYTICS_POINTS = 20_000` cap** (in [v4.rs](../service/crates/nightknight-api/src/v4.rs))
  is ~70 days at 5-min cadence. AGP over 90 days needs either a higher cap, server-side
  downsampling, or computing percentiles in a streaming pass. Decide before building AGP.
- **Timezone.** AGP and time-of-day patterns need the user's **local** time-of-day, but
  readings are stored in epoch-ms UTC and `User` has no timezone field. Add a
  `timezone`/`utcOffset` to the user profile (and `PUT /api/v4/me`) before §3.4/§3.5, or
  bucket client-side from raw entries as an interim.
- **Gaps & cadence.** Don't assume a fixed 5-min cadence for time-weighted metrics and
  episode detection — detect gaps (e.g. > 15 min) and exclude them from accrued time and
  from bridging events. Cadence varies by source (Libre 1-min/15-min graph vs Dexcom
  5-min).
- **Where the math lives.** Put pure functions in `nightknight-core::analytics` (they're
  trivially unit-testable, the project's stated preference), keep `v4.rs` as a thin
  serializer, and pin every formula to a reference value with a test — exactly as GMI/CV
  already are.
- **Performance.** Most metrics are single-pass over the window; AGP/percentiles need a
  sort or a histogram per bin. All are fine for a per-request compute at these data sizes;
  no precomputation needed initially.

---

## 5. Suggested sequencing

1. **P0** — data-sufficiency (`percentActive`, `daysCovered`), surface `SD`, label
   GMI/eA1c. *(small; makes current numbers honest)*
2. **GRI** (§3.1) — biggest insight-per-line-of-code; reuses existing bands.
3. **Episode detection** (§3.2) — the most "actionable" addition.
4. **Timezone on the user profile** — unblocks the pattern/AGP work.
5. **Time-of-day & day-of-week patterns** (§3.5) — cheap once timezone exists.
6. **AGP** (§3.4) — the flagship report; needs the percentile helper + cap decision.
7. **Advanced variability** (§3.6) and **export** (§3.7) — depth for power users.

Each step is independently shippable and adds one card/section to the dashboard and the
iOS app's analytics view.

---

## References

- Battelino T, Danne T, Bergenstal RM, et al. **Clinical Targets for Continuous Glucose
  Monitoring Data Interpretation: Recommendations From the International Consensus on Time
  in Range.** *Diabetes Care* 2019;42(8):1593–1603.
  <https://diabetesjournals.org/care/article/42/8/1593/36184/> ·
  [PMC](https://pmc.ncbi.nlm.nih.gov/articles/PMC6973648/)
- Klonoff DC, Wang J, Rodbard D, et al. **A Glycemia Risk Index (GRI) of Hypoglycemia and
  Hyperglycemia for Continuous Glucose Monitoring Validated by Clinician Ratings.**
  *J Diabetes Sci Technol* 2023;17(5):1226–1242.
  <https://pmc.ncbi.nlm.nih.gov/articles/PMC10563532/>
- ADCES danatech. **Core Glycemic Metrics (TIR, TBR, TAR, GMI, CV, Mean Glucose).**
  <https://www.adces.org/education/danatech/glucose-monitoring/continuous-glucose-monitors-(cgm)/glycemic-metrics--an-overview>
- ADCES danatech. **Using GRI to Deepen Glycemic Data Interpretation.**
  <https://www.adces.org/education/danatech/glucose-monitoring/continuous-glucose-monitors-(cgm)/glycemic-metrics--an-overview/using-gri-to-deepen-glycemic-data-interpretation>
- Mo Y, et al. **Glycemic variability: measurement, target, impact and does it matter?**
  *J Diabetes Investig* 2024. <https://onlinelibrary.wiley.com/doi/10.1111/jdi.14112>
  (MAGE, MODD, CONGA, J-index definitions)
- Bergenstal RM, et al. **Ambulatory Glucose Profile (AGP)** — the standardised CGM report
  (percentile bands by time of day).
