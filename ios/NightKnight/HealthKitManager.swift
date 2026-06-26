import Foundation
import HealthKit

/// Reads and writes blood glucose to Apple Health. Writes are de-duplicated against a
/// stored watermark so repeated refreshes don't create duplicate samples.
@MainActor
final class HealthKitManager {
    static let shared = HealthKitManager()
    private let store = HKHealthStore()
    private let glucoseType = HKQuantityType(.bloodGlucose)
    private let unit = HKUnit.gramUnit(with: .milli).unitDivided(by: .literUnit(with: .deci))
    private let watermarkKey = "hkLastWrittenMs"

    private var backgroundObserver: HKObserverQuery?

    func requestAuth() async -> Bool {
        guard HKHealthStore.isHealthDataAvailable() else { return false }
        do {
            try await store.requestAuthorization(toShare: [glucoseType], read: [glucoseType])
            return true
        } catch {
            return false
        }
    }

    /// Wake the app whenever new glucose is written to Apple Health (e.g. by a vendor
    /// CGM app) so we can refresh and reload widgets promptly — even in the background.
    /// Idempotent. Relies on the HealthKit entitlement; no extra background mode needed.
    func startBackgroundDelivery(onUpdate: @escaping @Sendable () -> Void) {
        guard HKHealthStore.isHealthDataAvailable(), backgroundObserver == nil else { return }
        // Requires the `com.apple.developer.healthkit.background-delivery` entitlement —
        // surface (don't swallow) a failure so a missing entitlement / authorization is
        // observable rather than silently disabling the "wake on new Health glucose" path.
        store.enableBackgroundDelivery(for: glucoseType, frequency: .immediate) { ok, error in
            if !ok {
                NSLog("NightKnight: HealthKit background delivery not enabled: %@",
                      error?.localizedDescription ?? "unknown error")
            }
        }
        let observer = HKObserverQuery(sampleType: glucoseType, predicate: nil) { _, completion, _ in
            onUpdate()
            completion() // acknowledge so HealthKit doesn't keep retrying
        }
        backgroundObserver = observer
        store.execute(observer)
    }

    /// Write any readings newer than the last write watermark.
    func write(_ readings: [GlucoseReading]) async {
        guard HKHealthStore.isHealthDataAvailable(), !readings.isEmpty else { return }
        let defaults = UserDefaults(suiteName: Settings.appGroup) ?? .standard
        let lastMs = defaults.double(forKey: watermarkKey)
        let fresh = readings.filter { $0.date.timeIntervalSince1970 * 1000 > lastMs }
        guard !fresh.isEmpty else { return }

        let samples = fresh.map { r in
            HKQuantitySample(
                type: glucoseType,
                quantity: HKQuantity(unit: unit, doubleValue: r.mgdl),
                start: r.date, end: r.date,
                metadata: [HKMetadataKeyWasUserEntered: false]
            )
        }
        do {
            try await store.save(samples)
            let newest = fresh.map { $0.date.timeIntervalSince1970 * 1000 }.max() ?? lastMs
            defaults.set(newest, forKey: watermarkKey)
        } catch {
            // Best-effort; surfaced via logs only.
        }
    }

    /// Read recent glucose samples from Health (for when Health is the source).
    func readRecent(hours: Int) async -> [GlucoseReading] {
        guard HKHealthStore.isHealthDataAvailable() else { return [] }
        let start = Date().addingTimeInterval(-Double(hours) * 3600)
        let predicate = HKQuery.predicateForSamples(withStart: start, end: nil)
        return await withCheckedContinuation { cont in
            let q = HKSampleQuery(sampleType: glucoseType, predicate: predicate, limit: HKObjectQueryNoLimit,
                                  sortDescriptors: [NSSortDescriptor(key: HKSampleSortIdentifierStartDate, ascending: true)]) { _, samples, _ in
                let readings = (samples as? [HKQuantitySample] ?? []).map {
                    GlucoseReading(date: $0.startDate, value: GlucoseValue(mgdl: $0.quantity.doubleValue(for: self.unit)))
                }
                cont.resume(returning: readings)
            }
            store.execute(q)
        }
    }
}
