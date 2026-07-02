import XCTest

// Shared (LocalStore, StandaloneSource, Models) is compiled directly into this target,
// so its types are visible without an import. Every test builds a THROWAWAY store via
// `LocalStore(path:)` at a unique temp file — never `LocalStore.shared`, whose App
// Group path doesn't resolve in a test process and would leak state across tests.

final class LocalStoreTests: XCTestCase {

    // MARK: - Helpers (class members, not top-level: all test files share one module)

    private let keyA = "dexcom:us:a"
    private let keyB = "libre:b"

    private func makeStore() -> LocalStore {
        let path = FileManager.default.temporaryDirectory
            .appendingPathComponent("LocalStoreTests-\(UUID().uuidString).sqlite3").path
        addTeardownBlock { try? FileManager.default.removeItem(atPath: path) }
        return LocalStore(path: path)
    }

    private func sample(_ dateMs: Int64, _ mgdl: Int) -> CgmSample {
        CgmSample(dateMs: dateMs, mgdl: mgdl, direction: nil, device: "test")
    }

    /// Asserts `body` throws `sourceMismatch` carrying exactly `owner`/`requested`.
    private func assertMismatch(owner expectedOwner: String?,
                                requested expectedRequested: String,
                                _ label: String,
                                file: StaticString = #filePath, line: UInt = #line,
                                _ body: () async throws -> Void) async {
        do {
            try await body()
            XCTFail("\(label): expected sourceMismatch, nothing thrown", file: file, line: line)
        } catch LocalStoreError.sourceMismatch(let owner, let requested) {
            XCTAssertEqual(owner, expectedOwner, label, file: file, line: line)
            XCTAssertEqual(requested, expectedRequested, label, file: file, line: line)
        } catch {
            XCTFail("\(label): expected sourceMismatch, got \(error)", file: file, line: line)
        }
    }

    // MARK: - Fresh store

    func testFreshStoreHasNilOwnerAndIsEmpty() async throws {
        let store = makeStore()
        let owner = try await store.owner()
        XCTAssertNil(owner)
        let empty = try await store.isEmpty()
        XCTAssertTrue(empty)
    }

    // MARK: - Ingest

    /// An empty vendor fetch (no recent readings) must NOT claim the store — otherwise
    /// a later real switch would see "already empty, no wipe needed" while a phantom
    /// owner silently blocks every subsequent write under the new key.
    func testEmptyUpsertDoesNotClaimOwnership() async throws {
        let store = makeStore()
        try await store.upsert([], sourceKey: keyA)
        let owner = try await store.owner()
        XCTAssertNil(owner, "an empty upsert must leave the store unclaimed")
        let empty = try await store.isEmpty()
        XCTAssertTrue(empty)
        // A genuinely different source can still claim it afterwards.
        try await store.upsert([sample(1_700_000_000_000, 120)], sourceKey: keyB)
        let ownerAfter = try await store.owner()
        XCTAssertEqual(ownerAfter, keyB)
    }

    /// Pruning an ownerless store is a pure no-op — it must not claim it either (same
    /// reasoning as the empty-upsert guard: a delete-only call has nothing to protect).
    func testEmptyPruneDoesNotClaimOwnership() async throws {
        let store = makeStore()
        try await store.prune(sourceKey: keyA)
        let owner = try await store.owner()
        XCTAssertNil(owner, "pruning an ownerless store must leave it unclaimed")
    }

    /// The first write claims the ownerless store and inserts.
    func testUpsertStampsOwnerAndInserts() async throws {
        let store = makeStore()
        try await store.upsert([sample(1_700_000_000_000, 120)], sourceKey: keyA)
        let owner = try await store.owner()
        XCTAssertEqual(owner, keyA)
        let empty = try await store.isEmpty()
        XCTAssertFalse(empty)
        let stats = try await store.stats(sourceKey: keyA)
        XCTAssertEqual(stats.count, 1)
    }

    /// `INSERT OR REPLACE` on the `date_ms` primary key: re-ingesting the same
    /// timestamp replaces the value instead of duplicating the row.
    func testReUpsertSameDateReplacesValueWithoutDuplicating() async throws {
        let store = makeStore()
        let t: Int64 = 1_700_000_000_000
        try await store.upsertRows([(dateMs: t, mgdl: 100)], sourceKey: keyA)
        try await store.upsertRows([(dateMs: t, mgdl: 150)], sourceKey: keyA)
        let stats = try await store.stats(sourceKey: keyA)
        XCTAssertEqual(stats.count, 1)
        let readings = try await store.entries(hours: 24, sourceKey: keyA,
                                               now: Date(timeIntervalSince1970: Double(t) / 1000))
        XCTAssertEqual(readings.map(\.mgdl), [150])
    }

    // MARK: - Windowed reads

    /// `entries` returns only the `[now - hours, now]` window, ascending, with the
    /// stored epoch-ms converted back to the right `Date`.
    func testEntriesWindowFiltersSortsAndConvertsMs() async throws {
        let store = makeStore()
        let now = Date(timeIntervalSince1970: 1_700_000_000)
        let nowMs: Int64 = 1_700_000_000_000
        let insideOld = nowMs - 23 * 3_600_000 // 23 h ago — inside a 24 h window
        let insideNew = nowMs - 3_600_000      //  1 h ago — inside
        let outside = nowMs - 25 * 3_600_000   // 25 h ago — outside
        // Inserted out of chronological order to prove the SELECT sorts, not the caller.
        try await store.upsertRows([(dateMs: insideNew, mgdl: 120),
                                    (dateMs: outside, mgdl: 200),
                                    (dateMs: insideOld, mgdl: 80)], sourceKey: keyA)
        let readings = try await store.entries(hours: 24, sourceKey: keyA, now: now)
        XCTAssertEqual(readings.map(\.mgdl), [80, 120])
        XCTAssertEqual(readings[0].date.timeIntervalSince1970,
                       Double(insideOld) / 1000, accuracy: 0.001)
        XCTAssertEqual(readings[1].date.timeIntervalSince1970,
                       Double(insideNew) / 1000, accuracy: 0.001)
    }

    /// The Rust-FFI input shape: a JSON array of `{"date": <ms>, "mgdl": <num>}`,
    /// ascending by date.
    func testAllReadingsJSONEmitsAscendingDateMgdlObjects() async throws {
        let store = makeStore()
        let now = Date(timeIntervalSince1970: 1_700_000_000)
        let nowMs: Int64 = 1_700_000_000_000
        try await store.upsertRows([(dateMs: nowMs - 600_000, mgdl: 132.5),
                                    (dateMs: nowMs - 1_200_000, mgdl: 118)], sourceKey: keyA)
        let json = try await store.allReadingsJSON(hours: 1, sourceKey: keyA, now: now)
        let rows = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(json.utf8)) as? [[String: Any]])
        XCTAssertEqual(rows.count, 2)
        XCTAssertEqual((rows[0]["date"] as? NSNumber)?.int64Value, nowMs - 1_200_000)
        XCTAssertEqual((rows[0]["mgdl"] as? NSNumber)?.doubleValue, 118)
        XCTAssertEqual((rows[1]["date"] as? NSNumber)?.int64Value, nowMs - 600_000)
        XCTAssertEqual((rows[1]["mgdl"] as? NSNumber)?.doubleValue, 132.5)
    }

    func testStatsReportsCountAndNewestDate() async throws {
        let store = makeStore()
        try await store.upsertRows([(dateMs: 1_700_000_000_000, mgdl: 100),
                                    (dateMs: 1_700_000_600_000, mgdl: 110),
                                    (dateMs: 1_700_000_300_000, mgdl: 105)], sourceKey: keyA)
        let stats = try await store.stats(sourceKey: keyA)
        XCTAssertEqual(stats.count, 3)
        XCTAssertEqual(stats.maxDateMs, 1_700_000_600_000)
    }

    // MARK: - Owner guard

    /// Every data method under the wrong key throws `sourceMismatch` carrying both
    /// the stamped owner and the rejected requester.
    func testMismatchedSourceKeyThrowsOnEveryDataMethod() async throws {
        let store = makeStore()
        try await store.upsert([sample(1_700_000_000_000, 120)], sourceKey: keyA)
        await assertMismatch(owner: keyA, requested: keyB, "upsert") {
            try await store.upsert([sample(1_700_000_100_000, 90)], sourceKey: keyB)
        }
        await assertMismatch(owner: keyA, requested: keyB, "entries") {
            _ = try await store.entries(hours: 24, sourceKey: keyB)
        }
        await assertMismatch(owner: keyA, requested: keyB, "allReadingsJSON") {
            _ = try await store.allReadingsJSON(hours: 24, sourceKey: keyB)
        }
        await assertMismatch(owner: keyA, requested: keyB, "stats") {
            _ = try await store.stats(sourceKey: keyB)
        }
        await assertMismatch(owner: keyA, requested: keyB, "prune") {
            try await store.prune(sourceKey: keyB)
        }
        // The rejected calls must not have altered the store.
        let owner = try await store.owner()
        XCTAssertEqual(owner, keyA)
        let stats = try await store.stats(sourceKey: keyA)
        XCTAssertEqual(stats.count, 1)
    }

    /// Reads against an ownerless store serve "empty" WITHOUT claiming it — a widget
    /// refresh before the app's first fetch must not stamp an owner.
    func testOwnerlessReadsReturnEmptyWithoutStamping() async throws {
        let store = makeStore()
        let readings = try await store.entries(hours: 24, sourceKey: keyA)
        XCTAssertTrue(readings.isEmpty)
        let json = try await store.allReadingsJSON(hours: 24, sourceKey: keyA)
        let parsed = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(json.utf8)) as? [Any])
        XCTAssertTrue(parsed.isEmpty)
        let stats = try await store.stats(sourceKey: keyA)
        XCTAssertEqual(stats.count, 0)
        XCTAssertNil(stats.maxDateMs)
        let owner = try await store.owner()
        XCTAssertNil(owner)
    }

    /// `reset(to:)` — the one sanctioned owner change — wipes the readings, restamps,
    /// and the new owner can then ingest where it previously mismatched.
    func testResetWipesReadingsAndRestampsOwner() async throws {
        let store = makeStore()
        try await store.upsert([sample(1_700_000_000_000, 120)], sourceKey: keyA)
        try await store.reset(to: keyB)
        let owner = try await store.owner()
        XCTAssertEqual(owner, keyB)
        let empty = try await store.isEmpty()
        XCTAssertTrue(empty)
        try await store.upsert([sample(1_700_000_300_000, 95)], sourceKey: keyB)
        let stats = try await store.stats(sourceKey: keyB)
        XCTAssertEqual(stats.count, 1)
        XCTAssertEqual(stats.maxDateMs, 1_700_000_300_000)
    }

    /// `clear()` — the full sign-out wipe — leaves the store genuinely UNCLAIMED
    /// (unlike `reset(to:)`, which immediately re-stamps a new owner). A subsequent
    /// onboarding into any source, including one that never wrote before, must not
    /// see a leftover owner from the disconnected account.
    func testClearWipesReadingsAndRemovesOwnerEntirely() async throws {
        let store = makeStore()
        try await store.upsert([sample(1_700_000_000_000, 120)], sourceKey: keyA)
        try await store.clear()
        let owner = try await store.owner()
        XCTAssertNil(owner, "clear() must leave the store unclaimed, not re-stamped")
        let empty = try await store.isEmpty()
        XCTAssertTrue(empty)
        // A different account can now claim the store as if it were brand new.
        try await store.upsert([sample(1_700_000_300_000, 95)], sourceKey: keyB)
        let ownerAfter = try await store.owner()
        XCTAssertEqual(ownerAfter, keyB)
    }

    // MARK: - Retention

    /// `prune` cuts against the CURRENT clock, so the fixture dates are relative to
    /// `Date()`: 91 d is past the 90 d default cutoff, 1 d is comfortably inside.
    func testPruneDropsRowsOlderThanCutoff() async throws {
        let store = makeStore()
        let nowMs = Int64((Date().timeIntervalSince1970 * 1000).rounded())
        let old = nowMs - 91 * 86_400_000
        let fresh = nowMs - 86_400_000
        try await store.upsertRows([(dateMs: old, mgdl: 100),
                                    (dateMs: fresh, mgdl: 110)], sourceKey: keyA)
        try await store.prune(olderThanDays: 90, sourceKey: keyA)
        let stats = try await store.stats(sourceKey: keyA)
        XCTAssertEqual(stats.count, 1)
        XCTAssertEqual(stats.maxDateMs, fresh)
    }
}
