# NightKnight — Platform Comparison & Feature Targets

_Feature-gap analysis of NightKnight against the CGM landscape — Nightscout, Dexcom (Clarity/G7/Follow/Stelo), Abbott (LibreLink/LibreLinkUp/LibreView/Lingo), xDrip+, the DIY closed-loop family (Loop/Trio/iAPS/AndroidAPS/OpenAPS), Tidepool, Sugarmate, Gluroo, consumer logbook apps, iOS open-source CGM apps, Eversense/Medtronic, the wearable/voice ecosystem, and clinical-interop standards. Produced 2026-06-25. NightKnight's ethos: a privacy-first, self-hosted, secure informational monitor — explicitly **not** a medical device or insulin-dosing controller._


A rigorous read of NightKnight against ten competing/adjacent platform groups. The thesis up front: NightKnight already **wins on analytics rigor** (it computes more validated glycemic metrics than any commercial portal — GRI, MAGE, MODD, CONGA, J-index alongside the consensus TIR/GMI/CV set) and **wins on privacy posture** (self-hosted, header-only credentials, per-row isolation). Its real deficits cluster in three places: (1) **alerting sophistication** — everyone in the field has moved past static thresholds to predictive + connectivity-loss + policy-driven alarms; (2) **realtime delivery** — NightKnight polls where the field pushes; and (3) **clinical packaging & sharing** — it has the AGP *math* but not the printable artifact, the share link, or any follow UI. Treatment **display** (not dosing) is the one "treatments" lane that fits its ethos and is currently a stored-but-invisible capability.

---

## 1. Feature Matrix (capabilities NightKnight lacks or partially has)

Legend for NightKnight column: ✅ present · ◐ partial · ✗ absent. "Exemplars" lists platforms that ship the capability well.

### Alarms & Alerts

| Capability | NightKnight | Exemplars |
|---|---|---|
| Static threshold high/low/urgent (on-device) | ✅ | all |
| **Predictive "urgent-low-soon" (forecast hypo before threshold)** | ✗ | Dexcom G7, Nightscout (AR2), xDrip+, Medtronic Guardian (60-min), One Drop |
| **Predictive high / forecast-threshold-crossing** | ✗ | Nightscout (predict), xDrip+ |
| **Rate-of-change (fast-rise/fast-fall) alerts** | ◐ (drop only, on-device) | Dexcom, xDrip+, LoopFollow, Spike, Sugarmate |
| **Missed-reading / data-gap / signal-loss alert** | ✗ | xDrip+, Dexcom, Libre, GlucoseDirect, LoopFollow |
| **Sensor-expiry / sensor-age reminder (SAGE)** | ✗ | Nightscout SAGE, xDrip+, Diabox |
| **Quiet-hours / time-of-day alarm profiles** | ✗ | Dexcom (2nd profile), xDrip+, Medtronic |
| **Snooze + re-raise cadence policy** | ◐ (15-min fixed throttle) | xDrip+, LoopFollow, Libre (5-min repeat) |
| **Repeating-until-acknowledged critical alert** | ✗ | LibreLinkUp, Dexcom |
| **Delay-1st-alert (sustained-threshold)** | ✗ | Dexcom |
| Override silent/DND (iOS Critical Alerts) | ✗ | Dexcom, Libre, Sugarmate |

### Realtime & Notifications

| Capability | NightKnight | Exemplars |
|---|---|---|
| **Push-based realtime (silent APNs)** | ◐ (entitlement in place, not implemented) | Dexcom, Libre, LoopFollow, Gluroo |
| **WebSocket/SSE live stream to web + app** | ✗ | Nightscout (Socket.IO) |
| **Server-side alarm evaluation (fires when phone asleep)** | ✗ | LibreLinkUp, Dexcom Follow, Nightscout |
| Phone-call / SMS escalation fallback | ✗ | Sugarmate, Gluroo |
| Third-party routing (Pushover/IFTTT/Telegram) | ✗ | Nightscout |
| Cross-device acknowledge sync | ✗ | LibreLinkUp, Dexcom Follow |

### Analytics & Reports

| Capability | NightKnight | Exemplars |
|---|---|---|
| Core glycemic metrics (TIR/GMI/CV/SD/GRI/MAGE/CONGA/MODD/J-index) | ✅ (exceeds field) | — (NightKnight leads) |
| AGP percentile bands | ✅ | Clarity, LibreView, Tidepool |
| **Standardized printable one-page AGP PDF** | ✗ | LibreView, Clarity, Tidepool, Eversense DMS, CareLink, FHIR IG |
| **Comparison report (period A vs period B)** | ✗ | CareLink (Assessment & Progress), Clarity, FHIR IG |
| **Daily/day-to-day overlay report** | ✗ | Nightscout, Clarity, Tidepool, LibreView |
| **Auto-detected pattern surfacing** | ✗ | Clarity Patterns, LibreView GPI, Sugar.IQ |
| **Weekly/nightly summary digest** | ✗ | Clarity, Dexcom, CareLink, Gluroo |
| "Best Day" / positive reinforcement | ✗ | Dexcom Clarity |

### Treatments (DISPLAY only — dosing is out of scope)

| Capability | NightKnight | Exemplars |
|---|---|---|
| Treatment storage (bolus/carb/note round-trip) | ✅ (stored, no UI) | Nightscout |
| **Treatment markers rendered on glucose chart** | ✗ | Nightscout, Tidepool, xDrip+, all loop apps |
| **Read-only IOB/COB/loop-status pill display** | ✗ | Nightscout, Loop/Trio/AAPS, LoopFollow |
| **Predicted-BG curves (read-only render of uploaded predictions)** | ✗ | Nightscout, OpenAPS family |
| **Notes / event logbook UI** | ◐ (field stored, no UI) | all logbook apps |
| **Consumable-age trackers (SAGE/CAGE display)** | ✗ | Nightscout, Gluroo, InPen |

### Sharing & Follow

| Capability | NightKnight | Exemplars |
|---|---|---|
| Multi-user isolation + read-only tokens | ✅ | Nightscout (roles/subjects) |
| **Caregiver/family follow UI (named share, not raw token)** | ✗ | Dexcom Follow, LibreLinkUp, Gluroo, LoopFollow |
| **Read-only revocable share link / snapshot** | ✗ | Clarity, LibreView, Tidepool, FHIR SMART Health Links |
| **Per-follower alarm thresholds + schedules** | ✗ | Gluroo, LibreLinkUp, LoopFollow |
| Coordinated/rollover escalation | ✗ | Gluroo |
| Multi-patient follow dashboard | ✗ | Eversense Now, LoopFollow, Dexcom Follow |

### Export & Interop

| Capability | NightKnight | Exemplars |
|---|---|---|
| v1/v3/v4 API export | ✅ | Nightscout |
| **CSV/JSON raw + computed-metric export** | ✗ | Tidepool, Gluroo, mySugr, Glucose Buddy, Sugar.Sugar |
| **HL7 FHIR CGM export (summary + AGP DiagnosticReport)** | ✗ | FHIR CGM IG, emerging vendor support |
| Cache-and-forward upload resilience (poller/importer) | ✗ | GlucoseDirect, xDrip+ |
| HealthKit read/write | ✅ | Dexcom, Libre, Tidepool |

### Wearables / Glance Surfaces

| Capability | NightKnight | Exemplars |
|---|---|---|
| iOS widget + Watch app + complications | ✅ | Dexcom, Libre |
| **iOS Live Activity / Dynamic Island** | ✗ | Gluroo, Luka |
| **Siri / App Intents spoken query** | ✗ | Dexcom, Sugarmate, Nightscout-Siri |
| **CarPlay glucose display** | ✗ (roadmap) | Sugarmate, Gluroo, Dexcom |
| Garmin Connect IQ data field | ✗ | Garmin+Dexcom, xDrip community |
| Glucose-as-iCal feed trick | ✗ | Sugarmate, xDrip4iOS |

---

## 2. Prioritized Gaps / Candidate Features

Effort is sized against NightKnight's stack (Rust core + v4 API + dependency-free web SPA + SwiftUI iOS/Watch + Cloudflare Worker). Priority reflects ethos-fit × safety/value × leverage.

### P0 — Do these first

**1. On-device predictive "urgent-low-soon" + missed-reading/stale-data alerts**
- *What:* Forecast a hypo before the threshold is crossed (short-horizon linear/AR projection of rate-of-change), plus an alarm when no reading has arrived for N minutes (the failure mode a threshold alarm structurally cannot see).
- *Exemplars:* Dexcom G7 (<55 in 20 min), Nightscout AR2, xDrip+, LoopFollow.
- *Ethos fit:* **Core.** This is the single most safety-relevant gap, it's purely informational, and it runs entirely on-device — maximally aligned with the privacy-first, non-controller ethos. NightKnight already has the trend-regression machinery and the alarm pipeline; this is mostly a new predicate.
- *Effort:* **S–M.** Forecast math reuses existing least-squares trend code; missed-reading is a timestamp check in `AlarmManager`. Wire into existing background-refresh evaluation.
- *Rationale:* Highest safety value for least work; pure monitor.

**2. Alarm policy engine: quiet-hours / day-night profiles + snooze + re-raise**
- *What:* Per-time-of-day thresholds (loud overnight, calm daytime), configurable snooze durations, and re-raise cadence for unacknowledged criticals — replacing today's single global threshold set + fixed 15-min throttle.
- *Exemplars:* xDrip+, Dexcom 2nd profile, Medtronic, LoopFollow.
- *Ethos fit:* Core. Directly addresses alert fatigue, which is the #1 reason families abandon monitors. On-device, informational.
- *Effort:* **M.** Settings model + schedule evaluation in Swift; UI in settings tab. No server change.
- *Rationale:* Makes existing alarms actually livable; unlocks aggressive overnight safety without daytime nuisance.

**3. Standardized printable one-page AGP PDF (the 10 consensus metrics + percentile profile + daily calendar)**
- *What:* The IDC/ATTD/ADA single-page artifact. NightKnight already computes every metric and the percentile bands — it lacks only the *packaging*.
- *Exemplars:* LibreView, Clarity, Tidepool, Eversense DMS, CareLink, FHIR CGM IG.
- *Ethos fit:* Strong. The de-facto interoperability artifact of the field; what you hand an endocrinologist. Informational, no medical-device claim needed (label "informational, not a diagnostic report").
- *Effort:* **M.** Server-side render in Rust (SVG→PDF) reusing existing AGP/analytics computations, exposed as `GET /api/v4/report/agp.pdf`. Or render in the web SPA and print-to-PDF for an even smaller first cut.
- *Rationale:* Converts NightKnight's analytics lead into the one clinic-shareable deliverable everyone expects; near-zero new computation.

### P1 — High value, next wave

**4. Push-based realtime: silent APNs + WebSocket/SSE**
- *What:* Server pushes new readings/alarm conditions so the iOS app and web SPA update and alarm without foreground polling; SSE/WebSocket for the live web dashboard.
- *Exemplars:* Nightscout (Socket.IO), Dexcom, Libre, LoopFollow.
- *Ethos fit:* Strong — but note the constraint. Cloudflare Workers can't hold long-lived sockets without Durable Objects; the self-hosted container can. SSE is the pragmatic cross-runtime choice for web; silent APNs handles iOS. APNs entitlement is already in place.
- *Effort:* **L.** APNs push sender in Rust, device-token registration, DO or container fan-out, SSE endpoint, SPA/iOS clients. Cross-runtime nuance adds cost.
- *Rationale:* Removes the "polling-only" constraint and makes alarms fire reliably when the app is backgrounded — the structural enabler for trustworthy alerting.

**5. Treatment DISPLAY on the glucose chart (markers + notes logbook UI)**
- *What:* Render the bolus/carb/note treatments NightKnight *already stores* as chart markers, and add a notes/event logbook view. No dosing, no IOB math — display only.
- *Exemplars:* Nightscout, Tidepool, xDrip+.
- *Ethos fit:* **Strong and deliberately bounded.** Treatments are stored and round-tripped today but invisible. Displaying them is "personal insight," explicitly not dosing — squarely inside the stated ethos. Closes the gap between "we keep your loop/xDrip treatment data" and "you can see it."
- *Effort:* **M.** Web SPA chart overlay + iOS chart overlay; logbook list view; data already in store via v3.
- *Rationale:* Unlocks latent stored data with zero ethos risk; high perceived completeness for Loop/AAPS/xDrip families.

**6. Caregiver follow UX: named shares + read-only revocable share link + per-follower alarm thresholds**
- *What:* A sharing UI that mints a named, revocable read-only grant (and an optional account-less snapshot link), where the follower can set their own high/low thresholds — instead of handing out a full account read token.
- *Exemplars:* Dexcom Follow, LibreLinkUp, Gluroo GluCrew, Clarity/LibreView share codes, FHIR SMART Health Links.
- *Ethos fit:* Good, with care. Multi-user isolation + read-only tokens already exist; this is a *UX + scoping* layer on top, plus time-boxed/revocable links. Keep it self-hosted and invitation-based to stay on-ethos.
- *Effort:* **M–L.** Token-scoping/sharing model extension in Rust, share-link issuance + expiry, follower threshold storage, web/iOS UI.
- *Rationale:* The most-requested family feature across every platform; NightKnight is multi-user but has *no* follow surface today.

**7. CSV/JSON export (raw entries + treatments + computed metric set)**
- *What:* A dedicated export endpoint for raw data and the computed metrics, downloadable from the UI.
- *Exemplars:* Tidepool, Gluroo, mySugr, Glucose Buddy.
- *Ethos fit:* Strong — data portability is a privacy-first virtue (GDPR-style "your data, exportable"). No lock-in is already a stated value.
- *Effort:* **S.** Serialize existing query results; `GET /api/v4/export?format=csv|json`; SPA download button.
- *Rationale:* Cheap, on-ethos, and a frequent gap; pairs naturally with the AGP PDF.

### P2 — Worthwhile, later

**8. Comparison report (period A vs B) + weekly/nightly digest**
- *What:* Side-by-side metric/pattern comparison across two ranges, and a low-urgency periodic summary (push or in-app).
- *Exemplars:* CareLink Assessment & Progress, Clarity, Dexcom weekly digest, Gluroo.
- *Ethos fit:* Good; informational analytics, reuses existing metric engine.
- *Effort:* **M** (comparison: two analytics runs + diff view) / **S–M** (digest, once push exists — depends on P1#4).
- *Rationale:* Strengthens the analytics story; digest is far cheaper once realtime push lands.

**9. iOS glance surfaces: Live Activity / Dynamic Island + Siri/App Intents + CarPlay**
- *What:* Live Activity with the current number, an App-Intent Siri query ("what's my blood sugar?"), and a CarPlay glance.
- *Exemplars:* Gluroo, Luka, Sugarmate, Dexcom, Nightscout-Siri.
- *Ethos fit:* Good; Apple-centric and privacy-clean. CarPlay is already a stated roadmap item.
- *Effort:* **M** total, but parallelizable: App Intents (S), Live Activity (M, needs push for live updates → couples with P1#4), CarPlay (M).
- *Rationale:* "Glanceability everywhere" is the real product in the wearables tier; NightKnight has the foundation (widget/Watch) and just needs more surfaces.

**10. Read-only loop-status / IOB-COB pill + consumable-age (SAGE) display**
- *What:* Render the `devicestatus` loop pill (Enacted/Looping/Waiting + IOB/COB/temp-basal) and sensor-age that loop apps already upload — read-only, so a family follower can see "is the loop healthy."
- *Exemplars:* Nightscout pills, LoopFollow, OpenAPS family.
- *Ethos fit:* Acceptable as **display-only**. Ingesting `devicestatus` and rendering a glanceable health pill is monitoring; it never touches control. Frame strictly as "showing what the loop reported."
- *Effort:* **M.** `devicestatus` ingestion (v3 path may already accept it) + pill render in web/iOS.
- *Rationale:* High value for the Loop/AAPS/Trio cohort with no controller risk; depends on treatment-display groundwork (P1#5).

### P3 — Nice-to-have / opportunistic

**11. HL7 FHIR CGM export (summary observations + AGP DiagnosticReport)**
- *Effort:* **L.** *Rationale:* Future-proof interop standard, but low immediate payoff for a family deployment; do after CSV + AGP PDF exist.

**12. Garmin Connect IQ data field reading NightKnight's v4 API**
- *Effort:* **M** (separate Monkey C app). *Rationale:* Nice for athletes; niche, separate toolchain.

**13. Cache-and-forward upload resilience in the poller/importers**
- *Effort:* **S–M.** *Rationale:* Robustness improvement (queue + replay on reconnect) borrowed from GlucoseDirect; internal quality, low visibility.

---

## 3. Explicitly OUT OF SCOPE (considered and rejected)

These conflict with NightKnight's "informational monitor, explicitly not a medical device / not a controller" ethos:

- **Insulin dosing, bolus calculators, BWP** (Nightscout BWP, mySugr PRO, InPen, Medtronic Guardian) — dosing recommendation is regulated medical-device territory; the explicit non-goal.
- **Closed-loop / AID control** (Loop, Trio, iAPS, AndroidAPS, OpenAPS, Tidepool Loop) — autonomous insulin delivery (SMB/UAM/autosens/dynamic-ISF). NightKnight is compatible *as an uploader sink* but must never enact. (Note the narrow carve-out: *read-only display* of loop status/predictions is in scope — see P2#10 — the **control** is not.)
- **IOB/COB *computation*** — computing active-insulin requires a dosing model. Displaying IOB/COB values *uploaded by a loop app* is fine; calculating them ourselves crosses into dosing logic.
- **Remote command relay** (Loop/Trio remote bolus, AAPS SMS commands, OTP-gated carbs) — sending dosing/carb commands to a patient device is controller behavior. A read-only "a command happened" view is the most that fits.
- **Phone-call / SMS escalation infrastructure** (Sugarmate, Gluroo) — turns a self-hosted tool into a telephony SaaS with carrier dependencies and per-message billing; conflicts with the self-hosted, no-third-party-SaaS constraint. (Silent APNs + repeating critical alerts achieve most of the safety benefit without it.)
- **Clinic population-health dashboards, RPM/CPT billing, EHR integration, TIDE triage** (Tidepool+, Glooko, CareLink Pro, LibreView Practice) — multi-patient clinical SaaS for providers; wrong product for a family deployment.
- **Direct on-device sensor reading** (NFC/BLE; Spike, GlucoseDirect, xDrip+, Glimp, Diabox; Dexcom Direct-to-Apple-Watch) — making NightKnight *be* the sensor-reading client is a different architecture; it relies on connectors/HealthKit/CSV by design.
- **AI meal logging, food databases, coaching/CDE messaging, gamification** (Gluroo, One Drop, mySugr) — service/lifestyle layers outside a minimal privacy-first monitor; cloud-AI meal estimation also conflicts with the privacy stance.
- **Cloud voice skills (Alexa, Google Assistant)** — route personal glucose through third-party clouds; conflicts with privacy-first. (Local Siri/App Intents is the on-ethos alternative — P2#9.)
- **Big-data donation / research pooling** (Tidepool) — antithetical to a private family deployment.
- **HIPAA certification, BAAs/DPAs, immutable clinical audit trails** — NightKnight is explicitly not a medical device and not a covered entity; over-engineering for a family tool. (Lightweight access logging is reasonable; formal compliance machinery is not.)

---

### One-paragraph executive take
Spend the next cycle making the **alarms trustworthy** (P0: predictive-low + missed-reading + a real quiet-hours/snooze policy engine — all on-device, all S/M) and **ship the AGP PDF** (P0: pure packaging of analytics you already compute). Then invest in the **realtime push backbone** (P1) because it's the structural unlock for reliable background alarms, Live Activities, and digests downstream. In parallel, **surface the treatment data you already store** (P1: chart markers + logbook) and **build a real follow UX** (P1: named revocable shares + per-follower thresholds) — both close glaring "we have the data / multi-user but no UI" gaps without touching the dosing third rail. Everything that turns NightKnight into a dosing engine, a loop controller, a telephony service, or a clinic SaaS stays out — and the analytics rigor and self-hosted privacy that already differentiate it remain the moat.
