There's a `STATISTICAL-ANALYSIS.md` already. I have what I need to align with conventions. Here is the merged, de-duplicated, correction-applied reference document.

---

# CGM Analytics — Research Reference

> Authoritative implementation spec for provably-correct CGM analytics in Rust. Every formula is given with exact coefficients, units, and inclusive/exclusive edges. **Canonical internal unit is mg/dL**; convert at the edges only (`mg/dL = mmol/L × 18.0182`; the consensus uses rounded ×18 for its band *labels*, which is why 70 mg/dL ↔ 3.9 mmol/L, 180 ↔ 10.0). All corrections from the verification reports are already applied. Where sources disagreed, the **recommended** value is stated with a one-line rationale and any parity variant noted.

**Cross-cutting design rules (apply everywhere):**
- Compute every metric on canonical mg/dL; pick unit-dependent constants (e.g. J-index) by the *canonical* unit, never the display unit.
- Empty / insufficient data → return `None`/`Option`, never `0` or `NaN→0`. A metric value of 0 means "perfect/none," which is a false signal for "no data."
- Standard data-sufficiency gate (consensus): **≥14 days** of wear AND **≥70%** of possible readings present. Compute on less, but flag as low-confidence; suppress GMI and hypo metrics below threshold.
- `%active` denominator keys off the sensor's native cadence (288/day @ 5-min Dexcom; 96/day @ 15-min Libre historic), never a hardcoded 288.

---

## 1. Trend Arrows (Rate of Change)

### 1.1 Rate-of-change estimator
RoC is computed in **mg/dL per minute**. Two estimators:

**Two-point slope** (xDrip parity; noise-sensitive):
```
slope (mg/dL/min) = (g₂ − g₁) / (t₂ − t₁)        t in minutes, g in mg/dL
```
xDrip internally stores `calculated_value_slope` in mg/dL **per millisecond** and multiplies by `60000` (ms/min) to get mg/dL/min.

**Recommended robust estimator — least-squares slope over the trailing 15-min window `[t−15min, t]`:**
```
slope (mg/dL/min) = Σ((tᵢ − t̄)·(gᵢ − ḡ)) / Σ((tᵢ − t̄)²)
```
with `tᵢ` in minutes, `gᵢ` in mg/dL. Reduces to the two-point slope when only two points exist. The **15-minute window** is the clinically documented basis for both Dexcom (Aleppo) and Abbott (Kudva) arrows ("emphasis on the most recent ~15 minutes").

### 1.2 Threshold table — RECOMMENDED (Dexcom/clinical, outer edge 3.0)

Classifier on `slope` in mg/dL/min. Edges: **lower-inclusive / upper-exclusive around the magnitude**, so `|slope| ≤ 1 → Flat`, `1 < |slope| ≤ 2 → FortyFive`, `2 < |slope| ≤ 3 → Single`, `|slope| > 3 → Double`. This keeps 1.0/2.0/3.0 deterministic. (The exact `<`-vs-`≤` choice is a documented **design decision**, not a vendor-published fact — Dexcom only publishes the phrasing "less than 1 / 1 to 2 / 2 to 3 / more than 3." Mark it as such in code.)

| Direction | Glyph | RoC (mg/dL/min) | ~Δ over 30 min | Human label |
|---|---|---|---|---|
| DoubleUp | ⬆⬆ | `> 3` | > 90 | Rising fast |
| SingleUp | ⬆ | `2 < s ≤ 3` | 60–90 | Rising |
| FortyFiveUp | ↗ | `1 < s ≤ 2` | 30–60 | Drifting up |
| Flat | → | `−1 ≤ s ≤ 1` | < 30 | Steady |
| FortyFiveDown | ↘ | `−2 ≤ s < −1` | 30–60 | Drifting down |
| SingleDown | ⬇ | `−3 ≤ s < −2` | 60–90 | Falling |
| DoubleDown | ⬇⬇ | `< −3` | > 90 | Falling fast |
| NotComputable / NONE | — | sparse/stale/invalid | — | Unknown |
| RateOutOfRange | ⇕ | `\|s\| > ~30–40` (impossible) | — | Changing rapidly |

`Δ30 = RoC × 30`. The **Flat band ±1 mg/dL/min is fully agreed across all vendors** — the one universally-confirmed number. The user's "10 mg/dL over 10 min = 1.0" lands exactly on the Flat↔FortyFive edge (→ Flat under the rule above); 12 mg/dL/10 min = 1.2 is unambiguously FortyFiveUp.

### 1.3 xDrip / Nightscout-ecosystem bands (parity variant)
xDrip `BgReading.slopeName()` — verified verbatim against source. Edges are **upper-inclusive** (`slope_by_minute <= X`), evaluated most-negative first; outer edge is **3.5**, not 3.0:

| Band (mg/dL/min) | Direction |
|---|---|
| `≤ −3.5` | DoubleDown |
| `(−3.5, −2]` | SingleDown |
| `(−2, −1]` | FortyFiveDown |
| `(−1, +1]` | Flat |
| `(+1, +2]` | FortyFiveUp |
| `(+2, +3.5]` | SingleUp |
| `(+3.5, +40]` | DoubleUp |
| `> +40` | NONE (noise/invalid) |

**The one real disagreement: Single↔Double outer edge — Dexcom/clinical = 3.0, xDrip/Nightscout = 3.5.** Inner edges (±1, ±2) and the Flat band (±1) agree everywhere.
**Recommendation: use 3.0** for NightKnight's own computation — matches the first-party Dexcom clinical definition and the Klonoff/Kerr peer-reviewed insulin-dosing breakpoints (1/2/3), keeps a clean arithmetic series, and our computed value is only a fallback (we prefer the sensor's native arrow). Add an `xdrip_parity` test variant at 3.5 (upper-inclusive cascade) if byte-exact xDrip output is ever required.

### 1.4 First-party sensor trend mapping — PREFER native arrow over our computation

**Priority order:** (1) Dexcom v3 `trendRate` numeric → classify with §1.2; (2) native `direction`/`Trend`/`TrendArrow` string-or-int → pass through; (3) else compute from history with the §1.1 estimator.

**Dexcom Share** (`ReadPublisherLatestGlucoseValues`, `Trend` field):
- Newer transmitters: **string** — `Flat, FortyFiveUp, FortyFiveDown, SingleUp, SingleDown, DoubleUp, DoubleDown, NotComputable, RateOutOfRange, None`. Maps 1:1 to Nightscout `direction`.
- Legacy transmitters: **integer (verified against pydexcom)** — `0=None, 1=DoubleUp, 2=SingleUp, 3=FortyFiveUp, 4=Flat, 5=FortyFiveDown, 6=SingleDown, 7=DoubleDown, 8=NotComputable, 9=RateOutOfRange`. (Note: up→down order, opposite sign convention to xDrip.)

**Dexcom v3 developer API** (`/v3/users/self/egvs`): `trend` enum (lowerCamelCase) `doubleUp, singleUp, fortyFiveUp, flat, fortyFiveDown, singleDown, doubleDown, none, notComputable, rateOutOfRange`; plus `trendRate` = signed mg/dL/min (negative = falling). **Prefer `trendRate` when present.**

**LibreLinkUp** `glucoseMeasurement.TrendArrow` integer **1..5** (verified against PyLibreLinkUp): `0=NotDetermined, 1=Falling quickly (⬇), 2=Falling (↘), 3=Stable (→), 4=Rising (↗), 5=Rising quickly (⬆)`. Libre is a **5-state scale — no 45° tier, no double arrows** (breakpoints at 1 and 2 mg/dL/min). Libre→our 7-state mapping is lossy: `1→SingleDown, 2→FortyFiveDown, 3→Flat, 4→FortyFiveUp, 5→SingleUp` (map Libre's fast tiers to **Single**, not Double, since Libre's >2 threshold ≈ Dexcom's SingleUp; reserve Double for our own ≥3 computation). LibreLinkUp glucose arrives as `ValueInMgPerDl` (already mg/dL).

Internally, normalize **all** API conventions to the Nightscout `direction` string.

### 1.5 Trend edge cases / gap tolerance
- **< 2 readings in window** → `NotComputable`. Never emit Flat from a single point (Flat = "measured ~0 slope," not "unknown").
- **Stale latest reading** (newest > ~11 min old, i.e. 2× a 5-min cadence) → suppress / mark unknown.
- **Gap inside window**: require ≥2 readings AND total span ≤ 16 min AND newest ≤ 11 min old; otherwise do not classify.
- **Variable cadence**: always divide by actual elapsed minutes; never assume a fixed 5-min delta.
- **Noise / impossible slopes**: clamp/reject `|slope| > ~30–40 mg/dL/min` → `RateOutOfRange`.
- **Sensor warm-up (first ~1–2 h)**: suppress arrows if data sparse, as vendors do.
- **Unit safety**: keep ONE classifier in mg/dL/min; if ever classifying in mmol/L, thresholds scale ×(1/18.0182) (1 mg/dL/min ≈ 0.0555 mmol/L/min).

---

## 2. Core Metrics

All use the same set of valid mg/dL readings over the window. Exclude non-physiologic sentinels (≤0, calibration/error codes) from numerator and denominator.

**Mean glucose:** arithmetic mean of valid readings (mg/dL).

**Standard deviation (SD)** — use the **sample (N−1)** denominator for parity with iglu / EasyGV / base-R `sd()` and Nightscout-adjacent tooling:
```
SD = sqrt( (1/(N−1)) · Σ (Gₜ − X̄)² )      [mg/dL]
```
(Difference vs population N-denominator is <0.2% at CGM series lengths, but matters for byte-exact reference tests. Requires N ≥ 2.)

**Coefficient of Variation (%CV)** — unit-independent (doubles as a cross-unit invariant test):
```
%CV = 100 × (SD / X̄)
```
Target **≤ 36%** (inclusive); tighter alt **< 33%** for insulin/sulfonylurea users. Binary stable/labile flag: treat `≥ 36` as labile (2017 cutoff).

**GMI — Glucose Management Indicator** (Bergenstal 2018, consensus metric; **primary A1c estimate**):
```
GMI(%)        = 3.31 + 0.02392 × mean_glucose_mgdl
GMI(mmol/mol) = 12.71 + 4.70587 × mean_glucose_mmolL
```
+25 mg/dL of mean ⇒ +0.6 pp GMI (25 × 0.02392 = 0.598).

**eA1c — inverted-ADAG** (Nathan 2008; legacy/Nightscout-compat figure only):
```
eA1c(%)               = (mean_glucose_mgdl + 46.7) / 28.7
eA1c(mmol/mol, IFCC)  = ((mean_mgdl + 46.7)/28.7 − 2.15) × 10.929
```
Native ADAG (A1C → glucose): `eAG_mgdl = 28.7 × A1C − 46.7` (R²=0.84, N=507).

**GMI vs eA1c diverge** — they coincide near mean ≈ 154 mg/dL (both → ~6.99% ≈ 7.0%) and fan apart elsewhere (different slopes: GMI 0.02392/mg-dL vs ADAG implied 1/28.7 = 0.03484/mg-dL, fit on different populations). **Report GMI as primary** (consensus-endorsed, FDA nomenclature, validated against contemporaneous lab A1c); treat eA1c as a compatibility figure only. Never present as interchangeable — a lab A1c of 8.0% maps to a CGM mean anywhere from ~155 to ~218 mg/dL.

**Percent time CGM active** (data-sufficiency metric):
```
%active            = 100 × n_valid_readings / expected_readings
expected_readings  = days_in_window × (1440 / cadence_minutes)    # 288/day @5-min, 96/day @15-min
```

---

## 3. Time in Range (TIR / TBR / TAR)

Battelino 2019 consensus 5-band partition. The consensus reports **10 core metrics**.

### 3.1 Bands & exact edges
Bands are printed on **integer mg/dL** with a deliberate +1 step (…69 | 70…180 | 181…). To make a true partition of the real line for canonical (possibly fractional) mg/dL, implement as **half-open intervals**:

| Band | Label / color | Printed range | Computational rule (mg/dL) | mmol/L |
|---|---|---|---|---|
| TBR Level 2 (Very Low) | maroon | < 54 | `g < 54` → `[0, 54)` | < 3.0 |
| TBR Level 1 (Low) | red | 54–69 | `54 ≤ g < 70` → `[54, 70)` | 3.0–3.8 |
| **TIR (Target)** | green | 70–180 | `70 ≤ g ≤ 180` → `[70, 181)` | 3.9–10.0 |
| TAR Level 1 (High) | yellow | 181–250 | `180 < g ≤ 250` → `[181, 251)` | 10.1–13.9 |
| TAR Level 2 (Very High) | mustard | > 250 | `g > 250` → `[251, ∞)` | > 13.9 |

Edge consequences: 54.0 → Low; 70.0 → TIR; 180.0 → TIR; 180.5 → TAR-L1; 250.0 → TAR-L1; 250.5 → TAR-L2. The five bands are mutually exclusive and exhaustive.

### 3.2 Per-level vs cumulative reporting
- The two TBR/TAR **levels are reported SEPARATELY (non-nested) in the AGP/metric table** — metric "<54" and metric "54–69" are distinct rows; they do **not** double-count.
- **Clinical target compliance uses CUMULATIVE thresholds.** Compute both:
  `%<70 = %<54 + %54–69`; `%>180 = %181–250 + %>250`.

### 3.3 Targets
**General T1D/T2D** (vs cumulative thresholds):
- TIR 70–180: **> 70%** (> 16 h 48 min/day)
- TBR < 70: **< 4%** (< ~1 h); TBR < 54: **< 1%** (< ~15 min)
- TAR > 180: **< 25%** (< 6 h); TAR > 250: **< 5%**
- %CV: **≤ 36%** (alt < 33%)

**Older / high-risk:** TIR > 50%; TBR < 70: **< 1%** (no separate <54 target); TAR > 250: **< 10%**.
**Pregnancy (T1D):** target range **63–140 mg/dL** (3.5–7.8); TIR > 70%; TBR <63 <4%, <54 <1%; TAR >140 <25% (different low cut at 63, no high-level-2).

### 3.4 Count-weighted vs time-weighted + gap rule
**Recommendation: count-weighted is canonical** — `pct = 100 × readings_in_band / total_readings`. Matches the consensus "% of readings" wording and Nightscout/AGP output, and is robust to irregular cadence. At a fixed gap-free cadence, count and time weighting are identical.

If you ever **time-weight** (e.g. mixing sensors of differing cadence in one window):
- **Max-gap rule** — do not accrue interval time across gaps. Nightscout constant: `maxGap = 5×60×1000 + 10000 = 310000 ms (≈5.17 min)`; intervals with `Δt == 0` or `Δt > maxGap` are skipped.
- Attribute each reading's interval as `min(Δt_to_next, max_attributable)` with `max_attributable = 2 × cadence` (10 min Dexcom, 30 min Libre); drop intervals exceeding a hard `MAX_GAP = 15 min`.
- Dedupe by timestamp (the `Δt == 0` guard) before any accrual.

For **count-weighting**, gaps simply reduce `%active` and need no special handling.
**Empty input** (`total_readings == 0`) → null for every percentage, never 0% or NaN→0.

---

## 4. Glycemia Risk Index (GRI)

Primary source: Klonoff et al., *J Diabetes Sci Technol* 2023;17(5):1226–1242 (e-pub 2022; PMID 35348391; print 2023 vs e-pub 2022 is the same paper). All formula items confirmed verbatim incl. the paper's own worked example.

### 4.1 Formula
Inputs are the **percentages of valid readings** (each as a number, "5%" = 5) in the five bands; only four enter the formula (TIR is **not** used). Bands are the §3 edges (single source of truth for 54/70/180/250).

**Component form:**
```
HypoComponent  = VLow + (0.8 × Low)
HyperComponent = VHigh + (0.5 × High)
GRI            = (3.0 × HypoComponent) + (1.6 × HyperComponent)
```
**Expanded form (algebraically identical; best single Rust expression):**
```
GRI = (3.0 × VLow) + (2.4 × Low) + (1.6 × VHigh) + (0.8 × High)
```
Coefficient provenance: 3.0 = overall hypo weight, 1.6 = overall hyper weight; within-hypo Low weight 0.8 (VLow=1.0), within-hyper High weight 0.5 (VHigh=1.0). Distributed: VLow 3.0, Low 2.4, VHigh 1.6, High 0.8.

**Cap (after summation only):** `GRI = min(GRI_raw, 100.0)`. Floor 0 by construction. Final range **[0, 100]**. Keep `f64` internally; apply cap once; assign zone from the **unrounded** GRI.

### 4.2 Zones A–E
Absolute GRI bins of width 20. **Attribute the fixed 20-wide bins to the applied study (PMC11418469), not the primary paper** — Klonoff defines zones as empirical population **quintiles** ("best 1st–20th … worst 81st–100th percentile") and is silent on exact-boundary behavior for continuous GRI.

| Zone | GRI range | Meaning |
|---|---|---|
| A | 0–20 | lowest risk (best) |
| B | 21–40 | |
| C | 41–60 | |
| D | 61–80 | |
| E | 81–100 | highest risk (worst) |

**Boundary inclusivity is the only genuine source disagreement** (primary prose "0–20…81–100" implies integer bins with gaps; secondary calculators use "0–20, 20–40, …"). **Recommended self-imposed convention for continuous GRI:** `A=[0,20), B=[20,40), C=[40,60), D=[60,80), E=[80,100]` — clean 20-wide partition; document it as a *local decision*, not a literature fact. It only affects scores landing exactly on 20/40/60/80.

### 4.3 GRI Grid axes
**x-axis = Hypoglycemia Component (0–100%); y-axis = Hyperglycemia Component (0–100%)** (confirmed verbatim from the paper). Iso-GRI lines: `3.0·x + 1.6·y = const`, slope `dy/dx = −1.875`. (Note: if a task brief states hyper-on-x / hypo-on-y, that reverses the paper — flag to whoever specified it. The numeric GRI is unaffected.)

### 4.4 GRI edge cases
- **Empty input** → `None`, not 0 (GRI 0 = "perfect glycemia").
- Percentages from **valid readings only**.
- **Cadence:** band percentages by count when cadence is uniform; switch to duration-weighted when intervals vary by >2×. Do not interpolate across gaps for band assignment — a gap just reduces the denominator.

---

## 5. Ambulatory Glucose Profile (AGP)

- **Percentiles: 5, 25, 50, 75, 95.** Median = 50th line; **IQR band = 25–75** (50% of readings); **outer band = 5–95** (90%; 5% each tail).
  - **Disagreement:** original 1987 Mazze AGP used 10/90 for the outer band; the **2013 Bergenstal standardization and 2019 consensus settled on 5/95**. **Use 5/25/50/75/95** — current standard, matches Dexcom Clarity / Abbott LibreView.
- **Time-of-day bucketing:** collapse onto one modal 24-h day. **Recommended: 96 × 15-min bins** (good resolution/density at 14 d × 288/day ≈ 4032 readings ⇒ ~42/bin). Compute the 5 percentiles per bin, then **LOESS-smooth** the five curves across bins.
- **Per-bin minimum data (stated per hourly bin):** draw the **median if ≥ 3 readings**; draw the **IQR (25–75) if ≥ 5 readings**. Sparse bins below these minimums are dropped and bridged by the smoother, not plotted raw.
- **Minimum data for an AGP:** 14 consecutive days.

---

## 6. Episodes (Hypo/Hyper Event Detection)

Governing standard: 2017 ATTD consensus (Danne et al.), restated 2019 (Battelino) and verbatim in ONWARDS 2024. Implement as a **state machine over a time-ordered series**, parameterized by threshold `T`, onset `D_on = 15 min`, recovery `D_off = 15 min`. **Use timestamp-based spans, not sample counts** (cadence-independent; 15 min = 3 samples at 5-min cadence).

### 6.1 Onset / recovery rule
**Hypo (below `T`):**
```
ONSET : glucose < T continuously for ≥ 15 min  → event starts at first sample < T
END   : glucose ≥ T continuously for ≥ 15 min  → event ends at start of that first ≥T sample
```
**Hyper (above `T`)** — mirror:
```
ONSET : glucose > T continuously for ≥ 15 min  → event starts at first sample > T
END   : glucose ≤ T continuously for ≥ 15 min  → event ends at start of that first ≤T sample
```
**Nadir** = min glucose during a hypo event; **peak** = max during a hyper event, captured over `[onset, last sample before recovery]`. **Duration** = `t(end) − t(onset)` in minutes.

### 6.2 Thresholds & edge inclusivity

| Event class | `T` (mg/dL) | mmol/L | In-event test | Recovery test |
|---|---|---|---|---|
| Hypo Level 1 (alert) | 70 | 3.9 | `g < 70` | `g ≥ 70` |
| Hypo Level 2 (clinically significant) | 54 | 3.0 | `g < 54` | `g ≥ 54` |
| Hyper Level 1 | 180 | 10.0 | `g > 180` | `g ≤ 180` |
| Hyper Level 2 | 250 | 13.9 | `g > 250` | `g ≤ 250` |

**Edge convention (event detection):** in-event is **strict `<` / `>`** (exclusive); recovery is **inclusive `≥` / `≤`**. Therefore a value exactly equal to the threshold (70/54/180/250) is **recovery / in-range, never in-event**. This strict-`<` event edge is sourced to Danne 2017 / ONWARDS (the hypo phrasing is verbatim); the IHSG *alert-value classification* uses inclusive `≤3.9` but that is a sample-level band, a different thing — do not cite IHSG for the strict event edge. The **hyper recovery edge (`≤` vs `<`) is under-specified in primary sources**; the symmetric mirror here is the implementer's convention — keep it documented/configurable.

### 6.3 Level 1 vs Level 2 (hypo) — nested, report both
Level 1 (alert) = ≥15 min `<70` **without any ≥15-min sub-period spent `<54`**. Level 2 = ≥15 min `<54`, **nested inside** the broader `<70` excursion. Report (a) all `<70` events and (b) all `<54` events as **two overlapping series** (both counts) — matching Danne/ONWARDS — rather than mutually-exclusive bins. Overall hypo count uses the `<70` threshold.

### 6.4 Episode edge cases / gap handling
- **Sub-15-min excursions are not events** (a 10-min dip then recovery → no event).
- **Recovery must itself last ≥15 min.** A brief blip back across the threshold for <15 min does **not** end the event (anti-sawtooth) — it stays the same event. (Cessation is considered 15 min after glucose returns outside the range.)
- **Do NOT bridge a long sensor gap into one event.** Treat a gap `> 2 × expected_cadence` (**default 30 min for 5-min CGM; configurable**) as a discontinuity: close any open event at the last valid pre-gap sample (count it only if `≥15 min` already satisfied), require a fresh `≥15 min` onset after the gap.
- **Event open at series end** → report ongoing/right-censored; count it if onset criterion met (recovery not required to count).
- **Nocturnal subset:** events whose **onset** falls in `[00:00, 06:00)` local time (half-open, configurable).
- **Rate normalization:**
  ```
  events_per_day  = event_count / total_observation_days       # = (valid-data time)/24h, NOT calendar span
  events_per_week = event_count / total_observation_weeks
  ```
  Normalize by **active sensor time**, not calendar window, so gaps don't deflate the rate. (Both per-day and per-week are plain arithmetic conversions — the consensus does not *mandate* a "per week" unit; Nightscout reports per-day.)
- Sort and de-duplicate by timestamp before scanning. Empty/single-sample/all-in-range → 0 events; guard division by zero observation time.

---

## 7. Advanced Variability (MAGE, MODD, CONGA, J-index)

Canonical reference implementation: the **iglu** R package (Gaynanova lab) + its formula companion (Olawsky/Gaynanova GV PDF), which faithfully reproduce the original papers. All in mg/dL. Notation: `Gₜ` reading at `t`; `N` count; `X̄` mean; `SD` = §2 sample (N−1) SD.

### 7.1 J-index (Wojcicki 1995) — **unit-dependent constant; highest-risk bug**
```
mg/dL:   J = 0.001 × (X̄ + SD)²            # constant = 1/1000
mmol/L:  J = 0.324 × (X̄ + SD)²            # constant = 18²/1000 = 324/1000
```
**0.324 is the mmol/L constant — using it on mg/dL is wrong.** NightKnight computes on mg/dL → **use 0.001**. Non-diabetic reference range (Hill 2011, mg/dL): **4.7–23.6**.

### 7.2 MAGE (Service et al. 1970) — mean amplitude of excursions whose amplitude **> 1 SD**

**(a) Difference method (EasyGV / iglu "naive"):**
```
Dₜ = Xₜ − Xₜ₋₁
E  = { Dₜ : |Dₜ| > SD of all glucose readings from the day Dₜ occurred }
MAGE+ = (1/#E⁺) Σ_{E⁺} Dₜ      MAGE− = (1/#E⁻) Σ_{E⁻} Dₜ   (negative)
```
The 1-SD filter is **strictly greater** — an excursion exactly equal to SD is excluded. `sd_multiplier` (default 1) scales the threshold.

**(b) Moving-average method (Fernandes & Gaynanova 2022) — RECOMMENDED, most accurate (median ~1% error vs manual):**
- Interpolate to a uniform **5-min grid** first (so window durations are consistent across sensors).
- Short SMA window **α = 5** readings; long SMA window **β = 32** readings.
- Peaks/nadirs only on intervals bounded by **crossings** of the short and long MA.
- Measure amplitude on the **original (un-smoothed)** glucose between turning points; keep only excursions **> 1 SD**.

**Direction selection** (`direction=`): `"plus"`, `"minus"`, `"avg" = (MAGE+ + |MAGE−|)/2` **(iglu default)**, `"max"`, `"service"` (first qualifying excursion dictates the set).

Document α, β, the 5-min grid, and `avg` default. Treat published MAGE "normal" values as approximate — they diverge by algorithm/cohort (Hill 0–~50 mg/dL; Shah median ~27.7; CAD cutoff 65). Do not hard-code a single normal; if forced, use a soft <50–65 mg/dL ceiling and surface the algorithm used.

### 7.3 MODD (Molnar, Taylor & Ho 1972) — mean of |daily differences|
For `Xₜ`, `Dₜ` = `Xₜ` minus glucose **24 h earlier** (matched within ±s slack). `T` = times with a valid partner, `k = |T|`.
```
Manuscript (RECOMMENDED, iglu default):  MODD_M  = (1/k)     Σ_T  |Dₜ|        [mg/dL]
EasyGV variant:                          MODD_GV = (1/(k−1)) Σ_{T′} |Dₜ|       (T′ = T minus its last time)
```
Absolute values, then mean. Default lag = 1 day. **Use manuscript (1/k).**

### 7.4 CONGA(n) (McDonnell et al. 2005) — SD of n-hour differences
For `Xₜ`, `Dₜ` = `Xₜ` minus glucose **n hours earlier** (within ±s slack). `T` = times with a partner, `k = |T|`, `D̄ = (Σ Dₜ)/k`.
```
CONGA_n = sqrt( Σ_T (Dₜ − D̄)² / (k − 1) )      [mg/dL]   # sample SD, k−1 denominator
```
Use iglu `method="manuscript"`. **Default lag differs between reference implementations — pin whichever NightKnight adopts and state it explicitly:**
- **Olawsky/Gaynanova GV companion: n = 1 hour** (its null value).
- **iglu R package `conga()`: n = 24 hours** (its default).
(The dossier's original "n=1 (iglu null value)" was a mislabel; corrected here.) Common reported lags: 1, 2, 4, 6, 24 h.

### 7.5 Variability thresholds & non-diabetic bands (Hill 2011, n=70, mean±2SD)
- %CV ≤ 36% target (§2). MAGE filter `> 1 × SD` strict. J-index 4.7–23.6.
- MAGE (CGM): ~0–2.8 mmol/L (~0–50 mg/dL) · MODD: 0–3.5 mmol/L (~0–63 mg/dL) · CONGA(1h): 3.6–5.5 mmol/L (~65–99 mg/dL) · SD: 0–3.0 mmol/L (~0–54 mg/dL). Use as sanity bounds only.

### 7.6 Variability edge cases / partner slack
- **Empty / single reading:** SD/CV/J undefined (need N≥2); MAGE/MODD/CONGA need ≥1 qualifying pair → `None`, never 0. Guard `mean > 0` for CV.
- **Partner slack `s`:** iglu default **1 min is too tight** for real CGM and silently drops most pairs. **Recommend `±½ sampling interval` (≈ ±5 min Dexcom, ±7–8 min Libre)**; record `s` in output metadata for reproducibility. When several readings fall in the ±s window, use the **mean of all partners** n hours prior (Molnar/McDonnell wording; iglu manuscript follows this).
- **Interpolation:** interpolate only short gaps (≤ slack window or ≤ 2 sampling intervals, e.g. ≤ 15 min); never interpolate across multi-hour gaps (fabricates variability).
- **MAGE per-day SD threshold:** compute SD per calendar day (local midnight, configured tz); exclude days with too few readings (e.g. <50% expected or <24 readings).
- The difference-method `Dₜ = Xₜ − Xₜ₋₁` is cadence-sensitive (a 5-min and a 60-min gap give the same "consecutive" difference) — prefer the `ma` method for MAGE.

---

## 8. Reference Values (input → expected output) — pin as tests

### 8.1 Trend arrows (mg/dL/min classifier)
**Recommended (Dexcom 3.0 outer edge, `|s|≤1` Flat convention):**

| Input | slope | Expected | Note |
|---|---|---|---|
| +10 mg/dL / 10 min | +1.0 | **Flat** | Flat↔45° edge (this convention) |
| +12 mg/dL / 10 min | +1.2 | FortyFiveUp | clearly 1–2 |
| +15 mg/dL / 15 min | +1.0 | **Flat** | same edge, different cadence |
| +25 mg/dL / 10 min | +2.5 | SingleUp | 2–3 |
| +35 mg/dL / 10 min | +3.5 | DoubleUp | >3 |
| 0 / 5 min | 0.0 | Flat | |
| +4 mg/dL / 5 min | +0.8 | Flat | <1 |
| −20 mg/dL / 10 min | −2.0 | **SingleDown** | edge value (this convention) |
| −45 mg/dL / 15 min | −3.0 | **DoubleDown** | Dexcom edge (xDrip would say SingleDown) |
| 1 reading only | n/a | NotComputable | sparse guard |
| latest reading 20 min old | n/a | NotComputable | stale guard |

**xDrip-parity variant** (`<=` upper-inclusive, 3.5 outer): `1.0→Flat`, `2.0→FortyFiveUp`, `3.5→SingleUp`, `3.6→DoubleUp`, `−2.0→SingleDown`, `−3.5→DoubleDown`.

**First-party passthrough:** Dexcom Share string `"FortyFiveDown"`→FortyFiveDown; legacy int `5`→FortyFiveDown, `4`→Flat; Dexcom v3 `"singleUp"`/`trendRate:2.4`→SingleUp; LibreLinkUp `5`→fast-rising tier, `3`→Flat, `1`→fast-falling tier.

### 8.2 GMI / eA1c
| mean mg/dL | GMI exact | GMI 1dp | eA1c (mean+46.7)/28.7 |
|---|---|---|---|
| 100 | 5.7020 | 5.7 | 5.11 |
| 125 | 6.3000 | 6.3 | |
| 150 | 6.8980 | 6.9 | |
| **154** | **6.9937** | **7.0** | **6.99** (coincidence point) |
| 175 | 7.4960 | 7.5 | |
| 183 | 7.6874 | 7.7 | **8.00** (divergence test) |
| 200 | 8.0940 | 8.1 | 8.60 |
| 225 | 8.6920 | 8.7 | |
| 250 | 9.2900 | 9.3 | 10.34 |
| 275 | 9.8880 | 9.9 | |
| 300 | 10.4860 | 10.5 | 12.08 |
| 350 | 11.6820 | 11.7 | |

GMI–eA1c divergence Δ at mean 100 = **0.59** (5.702 − 5.112; pin to 2 dp as 0.59, not 0.60). GMI mmol/mol @154 = **52.93** (using ×18.018; would be 52.97 with ×18.0 — pin to the factor you ship). Native ADAG eAG: A1C 6→125.5, 7→154.2, 8→182.9, 9→211.6, 10→240.3 mg/dL.

### 8.3 Core / TIR
%CV: SD 50, mean 160 → **31.25%**. Band edges: 53.9→VeryLow, 54.0→Low, 70.0→TIR, 180.0→TIR, 180.5→TAR-L1, 250.0→TAR-L1, 250.1→TAR-L2. %active: 4032/4032 (14 d @5-min) → **100%**; 2880/4032 → **71.4%** (passes); 2800/4032 → **69.4%** (fails 70%).

### 8.4 GRI (Eq. 4; cap after summation)
| # | VLow | Low | InRange | High | VHigh | Hypo | Hyper | raw | capped | Zone |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 (paper) | 5 | 10 | 50 | 20 | 15 | 13.0 | 25.0 | 79.0 | **79.0** | D |
| 2 (perfect) | 0 | 0 | 100 | 0 | 0 | 0 | 0 | 0.0 | **0.0** | A |
| 3 (all VHigh) | 0 | 0 | 0 | 0 | 100 | 0 | 100 | 160.0 | **100.0** | E |
| 4 (all VLow) | 100 | 0 | 0 | 0 | 0 | 100 | 0 | 300.0 | **100.0** | E |
| 5 (all Low) | 0 | 100 | 0 | 0 | 0 | 80 | 0 | 240.0 | **100.0** | E |
| 6 (all High) | 0 | 0 | 0 | 100 | 0 | 0 | 50 | 80.0 | **80.0** | D / E* |
| 7 (mild hyper) | 0 | 0 | 80 | 20 | 0 | 0 | 10 | 16.0 | **16.0** | A |
| 8 (mild hypo) | 0 | 5 | 95 | 0 | 0 | 4 | 0 | 12.0 | **12.0** | A |
| 9 (boundary) | 0 | 0 | 90 | 10 | 5 | 0 | 13 | 28.8 | **28.8** | B |

Test #1 is the **primary regression anchor** (paper's own example; both code paths must give bit-identical 79.0). *Test #6 (GRI=80.0) is the deliberate zone-boundary probe: Zone E under recommended `E=[80,100]`, Zone D under `D=[60,80]` inclusive — pin to your documented convention.

### 8.5 Variability (synthetic, hand-verifiable)
| # | Series (mg/dL) | Expected |
|---|---|---|
| 1 | [100,100,100,100] | SD 0, CV 0%, **J = 10.0**, MAGE 0, MODD/CONGA 0 |
| 2 | [80, 120] | X̄ 100, **SD 28.284**, **CV 28.28%**, **J 16.457** |
| 3 | [100,110,90,100] | X̄ 100, **SD 8.165**, CV 8.165%, **J 11.700** |
| 4 (unit check) | mean 140, SD 40 mg/dL | **J(mg/dL)=32.4**; same data in mmol/L (7.777…, 2.222…) → **J(mmol/L)=32.4** (must match) |
| 5 | [100,130,120] at t=0,60,120 min | D=[30,−10], D̄=10, **CONGA(1) = 28.284** |
| 6 | day1 [100]@08:00, day2 [140]@08:00 | **MODD_M = 40** |

J-index plausibility: healthy trace mean~100/SD~18 → J ≈ 13.9 (mid non-diabetic 4.7–23.6).

### 8.6 Episodes (5-min cadence; times in min, values mg/dL)
| # | Series | T | Expected |
|---|---|---|---|
| 1 (clean min valid) | [(0,80),(5,65),(10,60),(15,62),(20,68),(25,90),(30,95)] | 70 | 1 hypo L1; onset t=5; nadir 60; **duration 20 min** (onset t5 → recovery t25; the four beyond-samples span t5–t20) |
| 2 (too short) | [(0,80),(5,65),(10,60),(15,75),(20,80)] | 70 | **0 events** (10-min dip) |
| 3 (sawtooth) | [(0,90),(5,60),(10,60),(15,60),(20,72),(25,60),(30,60),(35,60),(40,90),(45,95),(50,100)] | 70 | **1 event**, t5–t40, nadir 60 (the lone 72 is <15-min recovery) |
| 4 (gap, tol 30) | [(0,60),(5,60),(10,60), gap, (60,60),(65,60),(70,60),(75,90)] | 70 | **1 event** — only the second run reaches the 15-min bar (the first is a 10-min run); the 50-min gap is **not** bridged into one 75-min event |
| 5 (L2 nested) | [(0,80),(5,68),(10,60),(15,50),(20,50),(25,50),(30,66),(35,75),(40,80)] | hypo | L1 count 1, L2 count 1, nadir 50 |
| 6 (hyper L1) | [(0,150),(5,200),(10,210),(15,220),(20,170),(25,160)] | 180 | 1 hyper L1, peak 220, ends t=20 |
| 7 (on-threshold) | [(0,80),(5,70),(10,70),(15,70),(20,80)] | 70 | **0 hypo events** (70 is ≥70, in-range) |

> Note: test #1 was corrected from the original dossier fixture, whose `<70` run spanned only 10 min and therefore would not have qualified.

---

## 9. Implementation Notes — gap/cadence tolerance & unit-independence

1. **One canonical unit.** All math on mg/dL `f64`. Convert mmol/L→mg/dL once at ingest (`×18.0182`). Select unit-dependent constants (J-index 0.001) by the canonical unit. **%CV and J-index are unit-independence regression invariants** — assert mg/dL and mmol/L code paths agree (J test #4 = 32.4 both ways; CV identical).
2. **Cadence-independence.** Trend slope and episode durations are computed from timestamps, never sample counts. `%active` and MAGE-`ma` interpolation key off native cadence (5-min Dexcom, 15-min Libre historic, 1-min Libre real-time). MAGE `ma` interpolates to a fixed 5-min grid so α/β windows have consistent duration.
3. **Gap thresholds (single table, all tunable, documented in output metadata):**
   - Trend staleness: newest reading ≤ ~11 min old (2× 5-min cadence); window span ≤ 16 min.
   - TIR time-weighting (only if used): Nightscout `maxGap ≈ 5.17 min`; cap interval attribution at `2×cadence`; hard `MAX_GAP = 15 min`.
   - Episode discontinuity: gap `> 2×cadence`, **default 30 min**; never bridge into one event.
   - Variability partner slack `s`: `±½ sampling interval` (±5 min Dexcom); interpolate only gaps ≤ ~15 min.
4. **Valid-reading denominator everywhere.** Exclude nulls, calibration/error sentinels, non-physiologic codes (≤0) from both numerator and denominator. Dedupe by timestamp (the `Δt==0` guard) and sort before any scan.
5. **Empty/insufficient → `Option::None`,** never 0/NaN. Gate GMI and hypo metrics on ≥14 days + ≥70% active; flag (don't fabricate) below threshold.
6. **Determinism.** Apply the GRI 100-cap once after summation; assign zones from unrounded values; pin integer mg/dL band comparisons where input is integer; round only for display. Both GRI code paths (component vs expanded) must be bit-identical.
7. **Prefer native sensor trend** over computed slope (priority: v3 `trendRate` → native arrow string/int → computed §1.1). Normalize all API conventions to the Nightscout `direction` string internally.
8. **Reuse one band-assignment function** (the §3 edges) across TIR, GRI, and episode classification — single source of truth for 54/70/180/250.
9. **Document every self-imposed convention as such in code comments:** the trend `<`-vs-`≤` edges, the GRI zone-boundary scheme, the hyper recovery `≤` edge, episode gap/nocturnal parameters, and the CONGA default lag you adopt. These are engineering decisions, not vendor-published facts.

---

## 10. Citations

**Consensus / clinical targets**
- Battelino T, Danne T, Bergenstal RM, et al. *Clinical Targets for Continuous Glucose Monitoring Data Interpretation: Recommendations From the International Consensus on Time in Range.* Diabetes Care 2019;42(8):1593–1603. https://pmc.ncbi.nlm.nih.gov/articles/PMC6973648/ — TIR bands, targets, CV ≤36%/<33%, 14-day/70% sufficiency, AGP context.
- Danne T, Nimri R, Battelino T, et al. *International Consensus on Use of Continuous Glucose Monitoring.* Diabetes Care 2017;40(12):1631–1640. https://pmc.ncbi.nlm.nih.gov/articles/PMC6467165/ — 15-min onset/15-min recovery event rule; 70/54/180/250 levels.
- International Hypoglycaemia Study Group. *Glucose Concentrations <3.0 mmol/L (54 mg/dL) Should Be Reported…* Diabetes Care 2017;40(1):155–157. https://pubmed.ncbi.nlm.nih.gov/27872155/ — 70 (3.9) alert / 54 (3.0) clinically-significant.
- Bhargava A, et al. (ONWARDS 2/4 post-hoc). Diabetes Care 2024. https://pmc.ncbi.nlm.nih.gov/articles/PMC10973898/ — verbatim restatement of L1/L2 hypo event definitions (nesting + "ending at first 15-min period ≥ threshold").
- ADA. *Glycemic Goals and Hypoglycemia: Standards of Care—2025.* Diabetes Care 2025;48(Suppl 1):S128. https://diabetesjournals.org/care/article/48/Supplement_1/S128/157561

**GMI / eA1c**
- Bergenstal RM, Beck RW, Close KL, et al. *Glucose Management Indicator (GMI)…* Diabetes Care 2018;41(11):2275–2280. https://pmc.ncbi.nlm.nih.gov/articles/PMC6196826/ — GMI = 3.31 + 0.02392×mean (N=528, G4).
- Nathan DM, Kuenen J, Borg R, et al. (ADAG). *Translating the A1C Assay Into Estimated Average Glucose Values.* Diabetes Care 2008;31(8):1473–1478. https://pubmed.ncbi.nlm.nih.gov/18540046/ — eAG = 28.7×A1C − 46.7 (R²=0.84, N=507).

**AGP**
- Bergenstal RM, Ahmann AJ, Bailey T, et al. *Recommendations for Standardizing Glucose Reporting and Analysis… (AGP).* 2013. https://pubmed.ncbi.nlm.nih.gov/23448694/ — 5/25/50/75/95, LOESS, per-bin minimums.
- Czupryniak L, et al. *AGP Report in Daily Care… Practical Tips.* Diabetes Ther 2022;13:811–821. https://pmc.ncbi.nlm.nih.gov/articles/PMC8991298/

**GRI**
- Klonoff DC, Wang J, Rodbard D, et al. *A Glycemia Risk Index (GRI)…* J Diabetes Sci Technol 2023;17(5):1226–1242 (e-pub 2022-03-29). PMID 35348391. https://pmc.ncbi.nlm.nih.gov/articles/PMC10563532/ · https://journals.sagepub.com/doi/10.1177/19322968221085273 — formula, weights, cap, axes, worked example (5/10/50/20/15 → 79).
- *Glycemic Risk Index Profiles and Predictors Among Diverse Adults With T1D.* J Diabetes Sci Technol 2023. PMID 36999215. https://pmc.ncbi.nlm.nih.gov/articles/PMC11418469/ — source for the fixed 20-wide A–E bins.

**Variability**
- Service FJ, Molnar GD, et al. *Mean amplitude of glycemic excursions…* Diabetes 1970;19(9):644–655. https://diabetesjournals.org/diabetes/article/19/9/644/3599
- Molnar GD, Taylor WF, Ho MM. *Day-to-day variation of continuously monitored glycaemia…* Diabetologia 1972;8:342–348. https://link.springer.com/article/10.1007/BF01218495
- McDonnell CM, Donath SM, et al. *A novel approach to continuous glucose analysis… (CONGA).* Diabetes Technol Ther 2005;7(2):253–263. https://www.liebertpub.com/doi/10.1089/dia.2005.7.253
- Wojcicki JM. *'J'-index…* Horm Metab Res 1995;27(1):41–42. https://pubmed.ncbi.nlm.nih.gov/7729793
- Fernandes NJ, et al. *Open-Source Algorithm to Calculate MAGE Using Short and Long Moving Averages.* J Diabetes Sci Technol 2022;16(2):576–577. https://pmc.ncbi.nlm.nih.gov/articles/PMC8861796/
- Olawsky E, Zhang Y, … Gaynanova I. *iglu GV metric definitions (formula companion).* https://shiny.biostat.umn.edu/GV/README2.pdf · iglu reference: https://irinagain.github.io/iglu/reference/ (conga, mage, modd, j_index, sd_glucose, cv_glucose) — **CONGA default n=1 (companion) vs n=24 (iglu package)**.
- Hill NR, Oliver NS, et al. *Normal reference range for mean tissue glucose and glycemic variability from CGM…* Diabetes Technol Ther 2011;13(9):921–928. https://pmc.ncbi.nlm.nih.gov/articles/PMC3160264/ — J-index 4.7–23.6; non-diabetic GV bands.

**Trend arrows**
- Klonoff DC, Kerr D. *A Simplified Approach Using Rate of Change Arrows…* J Diabetes Sci Technol 2017;11(6):1063–1069 (PMC5951054). https://journals.sagepub.com/doi/10.1177/1932296817723260 — breakpoints 1/2/3 mg/dL/min.
- Aleppo G, et al. *A Practical Approach to Using Trend Arrows on the Dexcom G5…* J Endocr Soc 2017;1(12):1445–1460. https://pmc.ncbi.nlm.nih.gov/articles/PMC5760209/ — arrow definitions, ~10–15 min window.
- Kudva YC, et al. *Approach to Using Trend Arrows in FreeStyle Libre…* J Endocr Soc 2018 (PMC6243139). https://pmc.ncbi.nlm.nih.gov/articles/PMC6243139/ — Libre 5-arrow set, no double arrows.
- Dexcom (provider). *G6 Trend Arrows and Treatment Decisions.* https://provider.dexcom.com/education-research/cgm-education-use/cgm-basics/dexcom-g6-trend-arrows-and-treatment-decisions — official <1/1–2/2–3/>3 bands, 15-min window.
- Dexcom Developer v3 API (egvs): `trend` enum + `trendRate` mg/dL/min. https://developer.dexcom.com/get-egvs
- pydexcom (`const.py`, `DEXCOM_TREND_DIRECTIONS`) — legacy Share integer 0–9 mapping. (StephenBlack gist for Share envelope: https://gist.github.com/StephenBlackWasAlreadyTaken/adb0525344bedade1e25)
- xDrip `BgReading.slopeName()` — `slope_by_minute = calculated_value_slope×60000`; edges ±1/±2/±3.5/40. https://github.com/StephenBlackWasAlreadyTaken/xDrip/blob/master/app/src/main/java/com/eveningoutpost/dexdrip/Models/BgReading.java
- PyLibreLinkUp (`TrendArrow` 0–5). https://pylibrelinkup.readthedocs.io/ · Abbott *Trend arrows in depth.* https://www.freestyle.abbott/us-en/discover-freestyle-libre/understanding-reports-and-data/trend-arrows-in-depth.html
- Time in Range Coalition. *Reading trend arrows.* https://www.timeinrange.org/get-started/reading-trend-arrows/

**Reference implementation**
- Nightscout `cgm-remote-monitor`, `lib/report_plugins/glucosedistribution.js` — count-weighted TIR (`100 × rangeRecords.length / data.length`), eA1c = (mean+46.7)/28.7, `maxGap = 5×60×1000 + 10000 = 310000 ms`. https://github.com/nightscout/cgm-remote-monitor ; `lib/plugins/direction.js` — `dir2Char` glyph map (display-only).

---

### Summary of applied corrections & flagged disagreements
- **CONGA default lag corrected:** n=1 (Gaynanova GV companion) vs n=24 (iglu package) — was mislabeled "n=1 (iglu null value)." §7.4.
- **GRI 20-wide A–E bins re-attributed** to the applied study (PMC11418469); primary paper defines zones as population quintiles. Zone-boundary inclusivity = self-imposed convention. §4.2.
- **GMI–eA1c Δ@mean100 pinned to 0.59** (not 0.60); GMI mmol/mol @154 = 52.93 with ×18.018. §8.2.
- **Episode test #1 fixture corrected** to a clean ≥15-min span. §8.6.
- **Trend outer edge: 3.0 recommended** (xDrip 3.5 as parity variant); `<`/`≤` edge marked a design decision. §1.2–1.3.
- **Hyper recovery edge (`≤`)** and **gap/nocturnal parameters** are implementer conventions, not vendor facts. §6.2, §6.4.
- **J-index 0.001 (mg/dL)** — 0.324 is the mmol/L constant; highest-risk bug. §7.1.

Source dossiers/reports merged from the task input (trend, gri, tir-gmi-agp, variability, episodes) plus their adversarial fact-check reports; all CORRECTIONS applied. Related repo file for alignment: `/Users/fergus/repos/NightKnight/docs/STATISTICAL-ANALYSIS.md`.