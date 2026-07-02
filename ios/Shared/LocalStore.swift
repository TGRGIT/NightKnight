import Foundation
import SQLite3

// The C macro `SQLITE_TRANSIENT` ((sqlite3_destructor_type)-1) doesn't import into
// Swift. It tells sqlite3_bind_text to copy the bytes immediately, which is required
// here because the Swift String's C buffer only lives for the duration of the call.
private let SQLITE_TRANSIENT = unsafeBitCast(-1, to: sqlite3_destructor_type.self)

enum LocalStoreError: Error, LocalizedError {
    /// The store holds another source's readings: `owner` is the `Settings.sourceKey`
    /// stamped into the DB, `requested` is the caller's. Deliberately not auto-healed —
    /// wiping one account's history to serve another is exactly the accident this
    /// guard exists to prevent, so the switch must be confirmed in Settings (resync).
    case sourceMismatch(owner: String?, requested: String)
    case io(String)

    var errorDescription: String? {
        switch self {
        case .sourceMismatch(let owner, let requested):
            return "Local readings belong to \(owner ?? "no source") but \(requested) "
                + "requested them — confirm the source switch in Settings to resync."
        case .io(let message):
            return "Local store error: \(message)"
        }
    }
}

/// The on-device reading archive for the local-analytics sources: one SQLite table of
/// `(date_ms, mgdl)` in the App Group container so the widget and watch extensions can
/// read what the app accumulated. Writes happen only from the app process by
/// convention; extensions read — the owner guard below is the only enforcement needed.
///
/// Being an ACTOR is load-bearing: it serialises access so the app's 60 s poll can't
/// interleave with a long Nightscout backfill (tens of thousands of rows) — no torn
/// SQLite state, no double-ingest.
///
/// THE DB-LEVEL SINGLE-SOURCE GUARD is the point of this type: the `meta` row `owner`
/// records which `Settings.sourceKey` the readings belong to, and every method except
/// `owner()`/`isEmpty()` checks the caller's key against it. Even a code path that
/// skips the settings UI cannot serve or append another account's data — it must go
/// through `reset(to:)` (the authoritative wipe, and the ONLY method allowed to change
/// a non-nil owner) first.
actor LocalStore {
    static let shared = LocalStore()

    /// Non-nil only for tests; production resolves the App Group path lazily so a
    /// missing entitlement surfaces as a diagnosable `.io` error at first use, not a
    /// silent per-process database the extensions would never see.
    private let explicitPath: String?
    private var db: OpaquePointer?

    init(path: String? = nil) {
        explicitPath = path
    }

    deinit {
        if let db { sqlite3_close(db) }
    }

    // MARK: - Public API

    /// The `sourceKey` whose data the store currently holds; nil for a fresh store.
    func owner() throws -> String? {
        try readOwner(try handle())
    }

    func isEmpty() throws -> Bool {
        let db = try handle()
        let stmt = try prepare(db, "SELECT COUNT(*) FROM readings")
        defer { sqlite3_finalize(stmt) }
        guard sqlite3_step(stmt) == SQLITE_ROW else { throw ioError(db) }
        return sqlite3_column_int64(stmt, 0) == 0
    }

    func upsert(_ samples: [CgmSample], sourceKey: String) throws {
        try upsertRows(samples.map { (dateMs: $0.dateMs, mgdl: Double($0.mgdl)) },
                       sourceKey: sourceKey)
    }

    /// The CSV-import path (and the shared insert core). `INSERT OR REPLACE` on the
    /// `date_ms` primary key is the dedup: re-fetching an overlapping vendor window is
    /// idempotent.
    func upsertRows(_ rows: [(dateMs: Int64, mgdl: Double)], sourceKey: String) throws {
        // Nothing to write: return before the owner guard, so an empty vendor fetch
        // (no recent readings) can never CLAIM the store — that would make a later
        // real switch look like "the store is already empty, no wipe needed" while
        // actually leaving a phantom owner that blocks every subsequent write.
        guard !rows.isEmpty else { return }
        let db = try handle()
        try guardWrite(db, sourceKey)
        // One transaction for the whole batch: a Nightscout backfill inserts tens of
        // thousands of rows, and per-row fsyncs would take minutes. BEGIN IMMEDIATE
        // takes the write lock up front instead of deadlocking on a mid-batch upgrade.
        try exec(db, "BEGIN IMMEDIATE")
        do {
            let stmt = try prepare(db, "INSERT OR REPLACE INTO readings(date_ms, mgdl) VALUES(?, ?)")
            defer { sqlite3_finalize(stmt) }
            for row in rows {
                sqlite3_bind_int64(stmt, 1, row.dateMs)
                sqlite3_bind_double(stmt, 2, row.mgdl)
                guard sqlite3_step(stmt) == SQLITE_DONE else { throw ioError(db) }
                sqlite3_reset(stmt)
            }
            try exec(db, "COMMIT")
        } catch {
            _ = sqlite3_exec(db, "ROLLBACK", nil, nil, nil)
            throw error
        }
    }

    /// The readings in the inclusive window `[now - hours, now]`, ascending by date.
    func entries(hours: Int, sourceKey: String, now: Date = Date()) throws -> [GlucoseReading] {
        let db = try handle()
        guard try guardRead(db, sourceKey) else { return [] }
        let window = Self.windowMs(hours: hours, now: now)
        return try selectWindow(db, fromMs: window.from, toMs: window.to).map {
            GlucoseReading(date: Date(timeIntervalSince1970: Double($0.dateMs) / 1000),
                           value: GlucoseValue(mgdl: $0.mgdl))
        }
    }

    /// The `[{"date":<ms>,"mgdl":<num>}]` array the Rust FFI consumes, ascending.
    func allReadingsJSON(hours: Int, sourceKey: String, now: Date = Date()) throws -> String {
        let db = try handle()
        guard try guardRead(db, sourceKey) else { return "[]" }
        let window = Self.windowMs(hours: hours, now: now)
        let rows = try selectWindow(db, fromMs: window.from, toMs: window.to)
            .map { Row(date: $0.dateMs, mgdl: $0.mgdl) }
        return String(decoding: try JSONEncoder().encode(rows), as: UTF8.self)
    }

    /// The analytics memo-key ingredients: row count + newest reading. Cheaper than
    /// hashing the data — any ingest changes at least one of the two.
    func stats(sourceKey: String) throws -> (count: Int, maxDateMs: Int64?) {
        let db = try handle()
        guard try guardRead(db, sourceKey) else { return (0, nil) }
        let stmt = try prepare(db, "SELECT COUNT(*), MAX(date_ms) FROM readings")
        defer { sqlite3_finalize(stmt) }
        guard sqlite3_step(stmt) == SQLITE_ROW else { throw ioError(db) }
        let maxMs: Int64? = sqlite3_column_type(stmt, 1) == SQLITE_NULL
            ? nil : sqlite3_column_int64(stmt, 1)
        return (Int(sqlite3_column_int64(stmt, 0)), maxMs)
    }

    /// The authoritative wipe on a source switch: empty the table and stamp the new
    /// owner in ONE transaction, so a crash can't leave the new key owning the old
    /// account's readings.
    func reset(to newKey: String) throws {
        let db = try handle()
        try exec(db, "BEGIN IMMEDIATE")
        do {
            try exec(db, "DELETE FROM readings")
            try stampOwner(db, newKey)
            try exec(db, "COMMIT")
        } catch {
            _ = sqlite3_exec(db, "ROLLBACK", nil, nil, nil)
            throw error
        }
    }

    /// The full sign-out wipe: empty the table and remove the owner entirely (unlike
    /// `reset(to:)`, which immediately re-stamps a new owner). Leaves the store in the
    /// same ownerless state as a fresh install, so a later onboarding through
    /// `WelcomeView` — which assumes an empty, unclaimed store — is never lied to by
    /// data or an owner stamp a disconnected account left behind.
    func clear() throws {
        let db = try handle()
        try exec(db, "BEGIN IMMEDIATE")
        do {
            try exec(db, "DELETE FROM readings")
            try exec(db, "DELETE FROM meta WHERE key = 'owner'")
            try exec(db, "COMMIT")
        } catch {
            _ = sqlite3_exec(db, "ROLLBACK", nil, nil, nil)
            throw error
        }
    }

    /// Trailing-90-day stats only need 90 d of history — drop anything older so the
    /// store stays bounded under a permanent 5-minute cadence.
    func prune(olderThanDays: Int = 90, sourceKey: String) throws {
        let db = try handle()
        // A delete-only operation must never CLAIM an ownerless store either — same
        // reasoning as the empty-upsert guard above. `guardRead` gives exactly that:
        // no-op (and no stamp) when there's nothing to prune, throw on a real
        // mismatch, proceed only when the caller already owns this data.
        guard try guardRead(db, sourceKey) else { return }
        let cutoff = Int64((Date().timeIntervalSince1970 * 1000).rounded())
            - Int64(olderThanDays) * 86_400_000
        let stmt = try prepare(db, "DELETE FROM readings WHERE date_ms < ?")
        defer { sqlite3_finalize(stmt) }
        sqlite3_bind_int64(stmt, 1, cutoff)
        guard sqlite3_step(stmt) == SQLITE_DONE else { throw ioError(db) }
    }

    // MARK: - Owner guard

    /// Write-side check: same owner proceeds; an ownerless (fresh) store is claimed by
    /// the first writer; anything else is a hard mismatch.
    private func guardWrite(_ db: OpaquePointer, _ sourceKey: String) throws {
        switch try readOwner(db) {
        case nil: try stampOwner(db, sourceKey)
        case sourceKey?: break
        case let other: throw LocalStoreError.sourceMismatch(owner: other, requested: sourceKey)
        }
    }

    /// Read-side check: returns false for an ownerless store — the caller serves
    /// "empty" WITHOUT claiming it, so a widget's read before the app's first fetch
    /// can never stamp an owner the user hasn't ingested data for.
    private func guardRead(_ db: OpaquePointer, _ sourceKey: String) throws -> Bool {
        switch try readOwner(db) {
        case nil: return false
        case sourceKey?: return true
        case let other: throw LocalStoreError.sourceMismatch(owner: other, requested: sourceKey)
        }
    }

    private func readOwner(_ db: OpaquePointer) throws -> String? {
        let stmt = try prepare(db, "SELECT value FROM meta WHERE key = 'owner'")
        defer { sqlite3_finalize(stmt) }
        switch sqlite3_step(stmt) {
        case SQLITE_ROW: return sqlite3_column_text(stmt, 0).map { String(cString: $0) }
        case SQLITE_DONE: return nil
        default: throw ioError(db)
        }
    }

    private func stampOwner(_ db: OpaquePointer, _ key: String) throws {
        let stmt = try prepare(db, "INSERT OR REPLACE INTO meta(key, value) VALUES('owner', ?)")
        defer { sqlite3_finalize(stmt) }
        sqlite3_bind_text(stmt, 1, key, -1, SQLITE_TRANSIENT)
        guard sqlite3_step(stmt) == SQLITE_DONE else { throw ioError(db) }
    }

    // MARK: - SQLite plumbing

    /// Lazy open + schema. `db` is only assigned once the schema exists, so a failure
    /// here retries from scratch on the next call instead of serving a half-made DB.
    private func handle() throws -> OpaquePointer {
        if let db { return db }
        let path = try resolvedPath()
        var opened: OpaquePointer?
        guard sqlite3_open_v2(path, &opened, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, nil) == SQLITE_OK,
              let handle = opened else {
            let message = opened.map { String(cString: sqlite3_errmsg($0)) } ?? "cannot open database"
            sqlite3_close(opened)
            throw LocalStoreError.io(message)
        }
        // The extensions read this file while the app writes it: wait out a writer's
        // lock briefly instead of surfacing spurious SQLITE_BUSY to a widget reload.
        sqlite3_busy_timeout(handle, 2000)
        for ddl in ["CREATE TABLE IF NOT EXISTS readings(date_ms INTEGER PRIMARY KEY, mgdl REAL)",
                    "CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT)"] {
            guard sqlite3_exec(handle, ddl, nil, nil, nil) == SQLITE_OK else {
                let message = String(cString: sqlite3_errmsg(handle))
                sqlite3_close(handle)
                throw LocalStoreError.io(message)
            }
        }
        db = handle
        return handle
    }

    private func resolvedPath() throws -> String {
        if let explicitPath { return explicitPath }
        // The App Group container is the ONLY correct home: it's the one directory the
        // app, widget, and watch extensions all see (same reasoning as Settings). If
        // the entitlement is missing, fail loudly rather than fork a per-process DB.
        guard let container = FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: Settings.appGroup) else {
            throw LocalStoreError.io("App Group container unavailable — check the "
                + "\(Settings.appGroup) entitlement")
        }
        return container.appendingPathComponent("LocalStore.sqlite3").path
    }

    private func exec(_ db: OpaquePointer, _ sql: String) throws {
        guard sqlite3_exec(db, sql, nil, nil, nil) == SQLITE_OK else { throw ioError(db) }
    }

    private func prepare(_ db: OpaquePointer, _ sql: String) throws -> OpaquePointer {
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK, let stmt else {
            throw ioError(db)
        }
        return stmt
    }

    private func selectWindow(_ db: OpaquePointer, fromMs: Int64, toMs: Int64)
        throws -> [(dateMs: Int64, mgdl: Double)] {
        let stmt = try prepare(db, "SELECT date_ms, mgdl FROM readings "
            + "WHERE date_ms >= ? AND date_ms <= ? ORDER BY date_ms ASC")
        defer { sqlite3_finalize(stmt) }
        sqlite3_bind_int64(stmt, 1, fromMs)
        sqlite3_bind_int64(stmt, 2, toMs)
        var rows: [(dateMs: Int64, mgdl: Double)] = []
        while true {
            switch sqlite3_step(stmt) {
            case SQLITE_ROW:
                rows.append((sqlite3_column_int64(stmt, 0), sqlite3_column_double(stmt, 1)))
            case SQLITE_DONE:
                return rows
            default:
                throw ioError(db)
            }
        }
    }

    private func ioError(_ db: OpaquePointer) -> LocalStoreError {
        .io(String(cString: sqlite3_errmsg(db)))
    }

    /// The inclusive `[now - hours, now]` window in epoch-ms — pure, so the boundary
    /// arithmetic is testable without a database.
    static func windowMs(hours: Int, now: Date) -> (from: Int64, to: Int64) {
        let to = Int64((now.timeIntervalSince1970 * 1000).rounded())
        return (to - Int64(hours) * 3_600_000, to)
    }

    /// The exact shape the Rust FFI consumes: `{"date":<ms>,"mgdl":<num>}`.
    private struct Row: Encodable {
        let date: Int64
        let mgdl: Double
    }
}
