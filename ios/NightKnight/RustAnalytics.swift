import Foundation
import NightKnightFFI

/// The app-side face of the Rust analytics FFI (`nightknight-ffi` staticlib, linked as
/// `NightKnightFFI.xcframework` into the APP TARGET ONLY — extensions are cache-only
/// and never carry the static lib; they reach analytics through the nil-able
/// `LocalAnalytics.engine` seam and simply don't compute in a local source).
///
/// Memory contract with the FFI (binding): every pointer an `nk_*` function returns is
/// owned by us and must be released with `nk_free` — never Swift/C `free()` (mismatched
/// allocators corrupt the heap). The bytes are copied out BEFORE the free. Failures
/// come back in-band as `{"error":"…"}` JSON and are rethrown as `EngineError.ffi`.
struct RustAnalytics: AnalyticsEngine {
    /// Must equal `nk_abi_version()` — bumped together with the Rust side on any
    /// contract change. `assertABI()` runs at launch so a stale checked-in xcframework
    /// fails loudly instead of surfacing as silent DTO-decode blanks.
    static let expectedABIVersion: UInt32 = 1

    static func assertABI() {
        let got = nk_abi_version()
        precondition(got == expectedABIVersion,
                     "NightKnightFFI ABI \(got) ≠ expected \(expectedABIVersion) — rebuild ios/Rust (ios/scripts/build-rust-ffi.sh)")
    }

    enum EngineError: Error, LocalizedError {
        case ffi(String)
        var errorDescription: String? {
            switch self {
            case .ffi(let m): return "On-device analytics failed: \(m)"
            }
        }
    }

    // The consensus TIR thresholds (mg/dL) — the server's TirThresholds::default();
    // passing the same constants keeps local analytics byte-identical to server output.
    private static let veryLow = 54.0, low = 70.0, high = 180.0, veryHigh = 250.0

    func analyticsJSON(readingsJSON: String, hours: Int, tzOffsetMin: Int) throws -> Data {
        try Self.callJSON {
            readingsJSON.withCString {
                nk_analytics_json($0, Int64(hours), Int64(tzOffsetMin),
                                  Self.veryLow, Self.low, Self.high, Self.veryHigh)
            }
        }
    }

    func agpJSON(readingsJSON: String, days: Int, binMinutes: Int, tzOffsetMin: Int) throws -> Data {
        try Self.callJSON {
            readingsJSON.withCString {
                nk_agp_json($0, Int64(days), Int64(binMinutes), Int64(tzOffsetMin))
            }
        }
    }

    func importGlucoseCSV(text: String, tzOffsetMin: Int) throws -> Data {
        try Self.callJSON {
            text.withCString { nk_import_clarity_csv($0, Int64(tzOffsetMin)) }
        }
    }

    /// The shared shell for every call: copy the result out, free the Rust pointer,
    /// and fold the in-band `{"error":…}` convention into a thrown error.
    private static func callJSON(_ body: () -> UnsafeMutablePointer<CChar>?) throws -> Data {
        guard let ptr = body() else {
            throw EngineError.ffi("result allocation failed")
        }
        defer { nk_free(ptr) }
        let data = Data(bytes: ptr, count: strlen(ptr))
        // serde emits errors as exactly one-key compact JSON, so a prefix check spares
        // re-parsing the (much larger) success payloads.
        if data.starts(with: Data(#"{"error":"#.utf8)),
           let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let message = obj["error"] as? String {
            throw EngineError.ffi(message)
        }
        return data
    }
}
