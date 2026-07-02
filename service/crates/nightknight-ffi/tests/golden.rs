//! The cross-language golden contract: a fixed 14-day reading set and the exact
//! analytics/AGP JSON it must produce, committed under `ios/Tests/Fixtures/` and
//! asserted from BOTH sides of the FFI — this Rust test drives the real C ABI, and
//! the Swift `NightKnightSourcesTests` feed the same fixture through the linked
//! xcframework and compare against the same golden bytes. Any drift in the report
//! composition, the float formatting, or the FFI plumbing fails one side or the other.
//!
//! Regenerate after an intentional report change with:
//! `NK_REGEN_GOLDENS=1 cargo test -p nightknight-ffi --test golden` (then rebuild the
//! xcframework and re-run the Swift tests).

use std::ffi::{c_char, CStr, CString};
use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

const GOLDEN_HOURS: i64 = 336; // 14 days
const GOLDEN_TZ: i64 = 60; // minutes east of UTC — exercises local-day boundaries
const GOLDEN_DAYS: i64 = 14;
const GOLDEN_BIN: i64 = 15;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../ios/Tests/Fixtures")
}

/// The fixed reading set, generated from integer arithmetic only (no transcendental
/// functions, no RNG) so every platform reproduces the identical bytes: a triangle
/// day-curve with deterministic jitter, periodic dips below 54 and spikes above 250
/// so TIR bands, episodes and GRI all get non-trivial values.
fn golden_rows() -> Vec<(i64, f64)> {
    const START_MS: i64 = 1_760_000_000_000;
    (0..(14 * 288i64))
        .map(|i| {
            let date = START_MS + i * 300_000;
            let m = (date / 60_000).rem_euclid(1_440);
            let tri = if m < 720 { m } else { 1_440 - m };
            let mut mgdl = 80.0 + (tri as f64) * (140.0 / 720.0)
                + (((i * 2_654_435_761) % 41) as f64 - 20.0);
            if i % 289 < 6 {
                mgdl -= 35.0;
            }
            if i % 977 < 8 {
                mgdl += 60.0;
            }
            (date, mgdl.clamp(40.0, 400.0))
        })
        .collect()
}

fn golden_readings_json() -> String {
    let rows: Vec<Value> =
        golden_rows().iter().map(|(d, m)| json!({ "date": d, "mgdl": m })).collect();
    serde_json::to_string(&rows).unwrap()
}

fn call(f: unsafe fn(*const c_char) -> *mut c_char, input: &str) -> String {
    let c_in = CString::new(input).unwrap();
    let ptr = unsafe { f(c_in.as_ptr()) };
    assert!(!ptr.is_null());
    let out = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { nightknight_ffi::nk_free(ptr) };
    out
}

unsafe fn analytics(p: *const c_char) -> *mut c_char {
    nightknight_ffi::nk_analytics_json(p, GOLDEN_HOURS, GOLDEN_TZ, 54.0, 70.0, 180.0, 250.0)
}

unsafe fn agp(p: *const c_char) -> *mut c_char {
    nightknight_ffi::nk_agp_json(p, GOLDEN_DAYS, GOLDEN_BIN, GOLDEN_TZ)
}

#[test]
fn ffi_output_matches_committed_goldens() {
    let dir = fixtures_dir();
    let readings = golden_readings_json();

    if std::env::var("NK_REGEN_GOLDENS").as_deref() == Ok("1") {
        fs::write(dir.join("readings-14d.json"), &readings).unwrap();
        fs::write(dir.join("analytics-golden.json"), call(analytics, &readings)).unwrap();
        fs::write(dir.join("agp-golden.json"), call(agp, &readings)).unwrap();
        return;
    }

    // The committed fixture is exactly the deterministic generation above — so the
    // input can't drift either.
    let committed = fs::read_to_string(dir.join("readings-14d.json"))
        .expect("readings-14d.json missing — run with NK_REGEN_GOLDENS=1");
    assert_eq!(committed, readings, "readings-14d.json drifted from the generator");

    let analytics_golden = fs::read_to_string(dir.join("analytics-golden.json")).unwrap();
    assert_eq!(
        call(analytics, &committed),
        analytics_golden,
        "nk_analytics_json output drifted from analytics-golden.json — if the report \
         change is intentional, regenerate with NK_REGEN_GOLDENS=1 and rebuild the \
         xcframework"
    );

    let agp_golden = fs::read_to_string(dir.join("agp-golden.json")).unwrap();
    assert_eq!(call(agp, &committed), agp_golden, "nk_agp_json output drifted from agp-golden.json");
}
