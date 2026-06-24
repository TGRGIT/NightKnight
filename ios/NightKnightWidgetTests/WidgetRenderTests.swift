import XCTest
import SwiftUI
import WidgetKit
import ImageIO
import UniformTypeIdentifiers

// The widget view sources (NightKnightWidget.swift) and the Shared models are compiled
// directly into this test target, so their types are visible without an import.

/// Renders the real widget view (the same `NightKnightWidgetView` the extension uses)
/// off-screen and asserts it actually draws something. This is the automated form of
/// "does the widget render anything?": we rasterise each supported widget family with
/// `ImageRenderer` and count non-transparent pixels — a blank render means the widget
/// would show nothing on the home/lock screen.
@MainActor
final class WidgetRenderTests: XCTestCase {

    /// Where to drop the rendered PNGs for visual inspection. On the *simulator* the
    /// test process shares the host filesystem, so we can write straight into the repo
    /// build dir. Falls back to the temp dir if that path isn't writable.
    private static let outDir: URL = {
        let repo = URL(fileURLWithPath: "/Users/fergus/repos/NightKnight/ios/build/widget-renders", isDirectory: true)
        try? FileManager.default.createDirectory(at: repo, withIntermediateDirectories: true)
        if FileManager.default.isWritableFile(atPath: repo.path) { return repo }
        return FileManager.default.temporaryDirectory
    }()

    private struct Scenario {
        let name: String
        let family: WidgetFamily
        let size: CGSize
        let entry: GlucoseEntry
    }

    private func entry(_ mgdl: Double?, _ trend: TrendDirection = .flat) -> GlucoseEntry {
        GlucoseEntry(date: .now,
                     value: mgdl.map { GlucoseValue(mgdl: $0) },
                     trend: trend,
                     unit: .mgdl)
    }

    private var scenarios: [Scenario] {
        // Approx point sizes of the real containers on iPhone 16.
        [
            Scenario(name: "systemSmall-inRange", family: .systemSmall, size: .init(width: 158, height: 158), entry: entry(110, .flat)),
            Scenario(name: "systemSmall-high",    family: .systemSmall, size: .init(width: 158, height: 158), entry: entry(240, .singleUp)),
            Scenario(name: "systemSmall-noData",  family: .systemSmall, size: .init(width: 158, height: 158), entry: entry(nil, .none)),
            Scenario(name: "accessoryCircular",   family: .accessoryCircular, size: .init(width: 72, height: 72), entry: entry(96, .fortyFiveDown)),
            Scenario(name: "accessoryInline",     family: .accessoryInline, size: .init(width: 200, height: 24), entry: entry(110, .flat)),
            Scenario(name: "accessoryRectangular", family: .accessoryRectangular, size: .init(width: 160, height: 72), entry: entry(180, .flat)),
        ]
    }

    func testEveryFamilyRendersNonBlank() throws {
        var failures: [String] = []
        for s in scenarios {
            let view = NightKnightWidgetContent(family: s.family, entry: s.entry)
                .frame(width: s.size.width, height: s.size.height)

            let renderer = ImageRenderer(content: view)
            renderer.scale = 2
            guard let cg = renderer.cgImage else {
                failures.append("\(s.name): ImageRenderer produced no image")
                continue
            }
            write(cg, name: s.name)

            let drawn = nonTransparentPixels(cg)
            let total = cg.width * cg.height
            // A real render of text/arrows covers a meaningful fraction of pixels.
            // "Renders nothing" shows up as ~0 drawn pixels.
            if drawn < 30 {
                failures.append("\(s.name): only \(drawn)/\(total) non-transparent pixels — looks blank")
            } else {
                print("✅ \(s.name): \(drawn)/\(total) pixels drawn")
            }
        }
        XCTAssertTrue(failures.isEmpty, "Widget rendered blank:\n" + failures.joined(separator: "\n"))
        print("ℹ️ Rendered PNGs written to \(Self.outDir.path)")
    }

    // MARK: - Helpers

    private func write(_ image: CGImage, name: String) {
        let url = Self.outDir.appendingPathComponent("\(name).png")
        guard let dest = CGImageDestinationCreateWithURL(url as CFURL, "public.png" as CFString, 1, nil) else { return }
        CGImageDestinationAddImage(dest, image, nil)
        CGImageDestinationFinalize(dest)
    }

    /// Count pixels with non-zero alpha — i.e. pixels the view actually painted over the
    /// (transparent) backdrop. The widget view draws no background of its own (the system
    /// supplies `containerBackground`), so anything > 0 is real content.
    private func nonTransparentPixels(_ image: CGImage) -> Int {
        let w = image.width, h = image.height
        var data = [UInt8](repeating: 0, count: w * h * 4)
        let cs = CGColorSpaceCreateDeviceRGB()
        guard let ctx = CGContext(data: &data, width: w, height: h, bitsPerComponent: 8,
                                  bytesPerRow: w * 4, space: cs,
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return 0 }
        ctx.draw(image, in: CGRect(x: 0, y: 0, width: w, height: h))
        var count = 0
        var i = 3
        while i < data.count {
            if data[i] != 0 { count += 1 }
            i += 4
        }
        return count
    }
}
