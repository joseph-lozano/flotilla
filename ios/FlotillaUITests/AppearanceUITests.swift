import XCTest

/// Settings → Appearance changes the Settings sheet it's picked in at once, not after it's
/// closed, and Phone follows the phone while it's open. Against `flotilla serve --offline` on
/// `ios/script/dev-db.py`; `test1Pair` pairs from `$FLOTILLA_LINK` first. Screenshots go to
/// `$FLOTILLA_SHOTS`.
final class AppearanceUITests: FlotillaUITestCase {
    func test1Pair() throws {
        try startPairing()
        app.buttons["pairConnect"].tap()
        waitForThreads()
        allowNotifications(timeout: 5)
    }

    /// Settings opened in Night, then Day, Night and Phone picked in it, and the phone's own
    /// look changed under Phone: the open sheet follows each at once.
    func test2SettingsSheetFollowsAppearance() {
        continueAfterFailure = true
        let was = XCUIDevice.shared.appearance
        XCUIDevice.shared.appearance = .light
        addTeardownBlock { XCUIDevice.shared.appearance = was }
        launchToThreads()
        let bar = app.navigationBars["Settings"]
        let picker = app.segmentedControls["appearance"]
        func open() {
            settings()
            XCTAssertTrue(bar.waitForExistence(timeout: 10), "Settings")
            for _ in 0..<5 where !picker.isHittable { app.swipeUp() }
        }
        open()
        picker.buttons["Night"].tap()
        bar.buttons["Done"].tap()
        XCTAssertTrue(bar.waitForNonExistence(timeout: 5))
        open()
        expect(dark: true, after: "opening in Night", shot: "opened", bar: bar)

        for (look, dark) in [("Day", false), ("Night", true), ("Phone", false)] {
            picker.buttons[look].tap()
            expect(dark: dark, after: look, shot: look.lowercased(), bar: bar)
        }
        XCUIDevice.shared.appearance = .dark
        expect(dark: true, after: "Phone, the phone turned dark", shot: "phone-dark", bar: bar)
        XCUIDevice.shared.appearance = .light
        expect(dark: false, after: "Phone, the phone turned light", shot: "phone-light", bar: bar)
    }

    /// The open sheet, light or dark, within a few seconds of `what`.
    func expect(dark: Bool, after what: String, shot name: String, bar: XCUIElement) {
        var l = lightness(under: bar)
        let deadline = Date().addingTimeInterval(3)
        while (l < 0.5) != dark, Date() < deadline {
            usleep(200_000)
            l = lightness(under: bar)
        }
        note(String(format: "%@: %.2f", what, l), tag: "appearance", file: "appearance.txt")
        shot("appearance-\(name)")
        XCTAssertTrue(bar.exists, "Settings, still open after \(what)")
        XCTAssertEqual(l < 0.5, dark, "after \(what), the open Settings sheet should be \(dark ? "dark" : "light") (lightness \(l))")
    }

    /// How light the sheet's top is, 0 to 1: the mean of the 200 points under Settings' bar,
    /// mostly background and rows.
    func lightness(under bar: XCUIElement) -> Double {
        let image = XCUIScreen.main.screenshot().image
        guard let cg = image.cgImage else { return -1 }
        let f = bar.frame
        let s = image.scale
        guard let part = cg.cropping(to: CGRect(x: f.minX * s, y: f.maxY * s, width: f.width * s, height: 200 * s)) else { return -1 }
        let (w, h) = (64, 32)
        var px = [UInt8](repeating: 0, count: w * h * 4)
        let drawn: Bool = px.withUnsafeMutableBytes { buf in
            guard let ctx = CGContext(data: buf.baseAddress, width: w, height: h, bitsPerComponent: 8, bytesPerRow: w * 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            ctx.interpolationQuality = .high
            ctx.draw(part, in: CGRect(x: 0, y: 0, width: w, height: h))
            return true
        }
        guard drawn else { return -1 }
        var sum = 0.0
        for i in stride(from: 0, to: px.count, by: 4) {
            sum += 0.2126 * Double(px[i]) + 0.7152 * Double(px[i + 1]) + 0.0722 * Double(px[i + 2])
        }
        return sum / Double(w * h) / 255
    }
}
