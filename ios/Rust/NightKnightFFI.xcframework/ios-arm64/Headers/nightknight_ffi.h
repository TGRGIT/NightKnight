// NightKnight Rust FFI — the ONLY Rust↔Swift binary contract.
//
// Mirrors `service/crates/nightknight-ffi/src/lib.rs`. Boundary rules:
//   * Every input is a NUL-terminated UTF-8 JSON C string.
//   * Every `char *` result is heap-allocated, OWNED BY THE CALLER, and must be
//     freed with `nk_free` (never the C `free` — one allocator, one free fn).
//   * Failures come back in-band as `{"error":"…"}` JSON; a panic becomes
//     `{"error":"panic"}`. NULL is returned only when the result string itself
//     could not be allocated.
//   * Functions are pure: no global state, no threads, no IO.
//
// Bump `nk_abi_version()` (in lib.rs) on ANY change here; the app asserts it at
// launch so a stale checked-in xcframework fails loudly.

#ifndef NIGHTKNIGHT_FFI_H
#define NIGHTKNIGHT_FFI_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

// The FFI contract version. The app compares this against its compiled-in
// expectation at launch.
uint32_t nk_abi_version(void);

// Free a string returned by any nk_* function. NULL is a no-op.
void nk_free(char *ptr);

// The full Statistical-Analysis payload (the /api/v4/analytics body) computed
// on-device. `readings_json` is `[{"date": <epoch ms>, "mgdl": <number>}, ...]`
// already restricted to the last `hours` hours; thresholds are mg/dL
// (54/70/180/250 for the consensus defaults).
char *nk_analytics_json(const char *readings_json,
                        int64_t hours,
                        int64_t tz_offset_min,
                        double very_low,
                        double low,
                        double high,
                        double very_high);

// The Ambulatory Glucose Profile payload (the /api/v4/agp body) computed
// on-device. `readings_json` as above, already restricted to the last `days` days.
char *nk_agp_json(const char *readings_json,
                  int64_t days,
                  int64_t bin_minutes,
                  int64_t tz_offset_min);

// Parse a glucose CSV export (Dexcom Clarity or LibreView — auto-detected) for
// the instant history backfill. Returns
// {"source":…,"unit":…,"rows":…,"imported":…,"skipped":…,
//  "entries":[{"date":<epoch ms>,"mgdl":<number>},…]} with entries normalised
// to mg/dL regardless of the export's unit.
char *nk_import_clarity_csv(const char *csv_text, int64_t tz_offset_min);

#ifdef __cplusplus
}
#endif

#endif // NIGHTKNIGHT_FFI_H
