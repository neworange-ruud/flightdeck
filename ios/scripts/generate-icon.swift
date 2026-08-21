#!/usr/bin/env swift
//
//  generate-icon.swift
//  FlightDeckRemote
//
//  Standalone macOS-host generator for the app's 1024x1024 App Store icon.
//  Run with: swift ios/scripts/generate-icon.swift
//
//  Draws "Radar & blip" (PRD §5.1): a midnight-green field with a subtle
//  radial/vertical gradient, three concentric thin radar rings, an orange
//  angular-gradient sweep wedge (~60deg), a glowing orange blip off-center
//  in the swept area, and faint cross-hair lines. No wordmark, two brand
//  colors (orange #FF6601 field #061417->#0D353B, muted teal #4FB3C4 for
//  the rings). Output is flattened onto the field color (no alpha channel)
//  for App Store compliance, since iOS applies its own rounded-rect mask.
//
//  Writes: FlightDeckRemote/Resources/Assets.xcassets/AppIcon.appiconset/AppIcon-1024.png
//

import AppKit
import CoreGraphics
import Foundation

// MARK: - Paths

let scriptURL = URL(fileURLWithPath: #filePath)
// ios/scripts/generate-icon.swift -> ios/
let iosRoot = scriptURL.deletingLastPathComponent().deletingLastPathComponent()
let appIconSetURL = iosRoot
    .appendingPathComponent("FlightDeckRemote")
    .appendingPathComponent("Resources")
    .appendingPathComponent("Assets.xcassets")
    .appendingPathComponent("AppIcon.appiconset")
let outputURL = appIconSetURL.appendingPathComponent("AppIcon-1024.png")

guard FileManager.default.fileExists(atPath: appIconSetURL.path) else {
    FileHandle.standardError.write("error: appiconset not found at \(appIconSetURL.path)\n".data(using: .utf8)!)
    exit(1)
}

// MARK: - Colors (PRD §5.1 — "Radar & blip")

/// Midnight-green field, dark end (top-left / outer).
let fieldDark = CGColor(red: 0x06 / 255.0, green: 0x14 / 255.0, blue: 0x17 / 255.0, alpha: 1)
/// Midnight-green field, lighter end (center / inner) — subtle depth only.
let fieldLight = CGColor(red: 0x0D / 255.0, green: 0x35 / 255.0, blue: 0x3B / 255.0, alpha: 1)
/// Brand orange — radar sweep + blip.
let orange = CGColor(red: 0xFF / 255.0, green: 0x66 / 255.0, blue: 0x01 / 255.0, alpha: 1)
/// Muted teal — faint radar rings / cross-hairs so they read as instrumentation,
/// not a second competing accent.
let teal = CGColor(red: 0x4F / 255.0, green: 0xB3 / 255.0, blue: 0xC4 / 255.0, alpha: 1)

// MARK: - Canvas

let size = 1024
let colorSpace = CGColorSpaceCreateDeviceRGB()

guard let ctx = CGContext(
    data: nil,
    width: size,
    height: size,
    bitsPerComponent: 8,
    bytesPerRow: 0,
    space: colorSpace,
    // No alpha — App Store icons must be fully opaque. premultipliedLast with
    // the alpha channel fixed at 1 lets us draw normally and still export flat.
    bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
) else {
    FileHandle.standardError.write("error: could not create CGContext\n".data(using: .utf8)!)
    exit(1)
}

let bounds = CGRect(x: 0, y: 0, width: size, height: size)
let center = CGPoint(x: bounds.midX, y: bounds.midY)
let radius = CGFloat(size) * 0.5

// MARK: - 1. Field: flat fill + subtle radial depth

// Flatten background first (belt-and-suspenders against any transparency).
ctx.setFillColor(fieldDark)
ctx.fill(bounds)

// Subtle radial gradient, lighter near center, dark toward the corners —
// "subtle radial/vertical depth" per spec. Kept low-contrast so it reads
// as depth, not a visible ring.
if let bgGradient = CGGradient(
    colorsSpace: colorSpace,
    colors: [fieldLight, fieldDark] as CFArray,
    locations: [0, 1]
) {
    ctx.saveGState()
    ctx.drawRadialGradient(
        bgGradient,
        startCenter: center, startRadius: 0,
        endCenter: center, endRadius: radius * 1.05,
        options: [.drawsAfterEndLocation]
    )
    ctx.restoreGState()
}

// MARK: - 2. Faint cross-hair lines (instrumentation feel)

ctx.saveGState()
ctx.setStrokeColor(teal)
ctx.setAlpha(0.16)
ctx.setLineWidth(CGFloat(size) * 0.0035)
let crossInset = CGFloat(size) * 0.06
ctx.move(to: CGPoint(x: bounds.minX + crossInset, y: center.y))
ctx.addLine(to: CGPoint(x: bounds.maxX - crossInset, y: center.y))
ctx.move(to: CGPoint(x: center.x, y: bounds.minY + crossInset))
ctx.addLine(to: CGPoint(x: center.x, y: bounds.maxY - crossInset))
ctx.strokePath()
ctx.restoreGState()

// MARK: - 3. Three concentric thin radar rings (low-alpha, muted teal)

let ringRadii: [CGFloat] = [0.30, 0.42, 0.54].map { $0 * CGFloat(size) }
ctx.saveGState()
ctx.setStrokeColor(teal)
for (index, r) in ringRadii.enumerated() {
    // Outer rings fade slightly more so the eye lands on the sweep, not the rings.
    ctx.setAlpha(0.28 - CGFloat(index) * 0.06)
    ctx.setLineWidth(CGFloat(size) * 0.0045)
    let ringRect = CGRect(x: center.x - r, y: center.y - r, width: r * 2, height: r * 2)
    ctx.strokeEllipse(in: ringRect)
}
ctx.restoreGState()

// MARK: - 4. Radar sweep — ~60deg orange angular-gradient wedge from center

// Sweep points up-and-right out of the center, matching the blip placement
// at ~10 o'clock being "ahead of" the leading edge of the sweep visually —
// the wedge itself sits toward 1-2 o'clock so the blip (10 o'clock) reads
// as something the sweep just passed over / is about to catch.
let sweepRadius = radius * 1.02
let sweepCenterAngle = CGFloat.pi / 2 - .pi / 6      // ~ -60deg from +x axis (up-right), in standard math angle
let sweepHalfWidth = (60.0 * .pi / 180.0) / 2

ctx.saveGState()
ctx.beginPath()
ctx.move(to: center)
ctx.addArc(
    center: center,
    radius: sweepRadius,
    startAngle: sweepCenterAngle - sweepHalfWidth,
    endAngle: sweepCenterAngle + sweepHalfWidth,
    clockwise: false
)
ctx.closePath()
ctx.clip()

// Angular fade: bright near the leading edge, dissolving toward center-trail,
// approximated with a linear gradient along the sweep's bisector so it reads
// as a "sweep" rather than a solid pie slice.
let leadingEdge = CGPoint(
    x: center.x + sweepRadius * cos(sweepCenterAngle),
    y: center.y + sweepRadius * sin(sweepCenterAngle)
)
if let sweepGradient = CGGradient(
    colorsSpace: colorSpace,
    colors: [
        orange.copy(alpha: 0.85)!,
        orange.copy(alpha: 0.32)!,
        orange.copy(alpha: 0.0)!
    ] as CFArray,
    locations: [0, 0.55, 1]
) {
    ctx.drawLinearGradient(
        sweepGradient,
        start: leadingEdge,
        end: center,
        options: []
    )
}
ctx.restoreGState()

// Crisp leading-edge line so the sweep has a defined "now" edge.
ctx.saveGState()
ctx.setStrokeColor(orange)
ctx.setAlpha(0.9)
ctx.setLineWidth(CGFloat(size) * 0.006)
ctx.move(to: center)
ctx.addLine(to: leadingEdge)
ctx.strokePath()
ctx.restoreGState()

// MARK: - 5. Blip — glowing orange dot, ~10 o'clock, ~60% radius

// 10 o'clock ~= 300deg clockwise-from-12, i.e. 60deg counter-clockwise past
// 12 going left. In standard math angle (0 = +x, CCW positive) that's 150deg.
let blipAngle = 150.0 * .pi / 180.0
let blipDistance = radius * 0.60
let blipCenter = CGPoint(
    x: center.x + blipDistance * cos(blipAngle),
    y: center.y + blipDistance * sin(blipAngle)
)
let blipCoreRadius = CGFloat(size) * 0.028
let blipGlowRadius = blipCoreRadius * 5.5

ctx.saveGState()
// Soft outer glow via radial gradient, additive-feeling falloff to transparent.
if let glowGradient = CGGradient(
    colorsSpace: colorSpace,
    colors: [
        orange.copy(alpha: 0.55)!,
        orange.copy(alpha: 0.18)!,
        orange.copy(alpha: 0.0)!
    ] as CFArray,
    locations: [0, 0.4, 1]
) {
    ctx.drawRadialGradient(
        glowGradient,
        startCenter: blipCenter, startRadius: 0,
        endCenter: blipCenter, endRadius: blipGlowRadius,
        options: []
    )
}
ctx.restoreGState()

// Solid core dot on top of the glow — this is what must stay legible at 60px.
ctx.saveGState()
ctx.setFillColor(orange)
let coreRect = CGRect(
    x: blipCenter.x - blipCoreRadius, y: blipCenter.y - blipCoreRadius,
    width: blipCoreRadius * 2, height: blipCoreRadius * 2
)
ctx.fillEllipse(in: coreRect)
// A slightly brighter near-white-orange hot center reads as "glowing" even
// when scaled down to a 60px home-screen icon.
ctx.setFillColor(CGColor(red: 1.0, green: 0.85, blue: 0.55, alpha: 0.9))
let hotRect = CGRect(
    x: blipCenter.x - blipCoreRadius * 0.45, y: blipCenter.y - blipCoreRadius * 0.45,
    width: blipCoreRadius * 0.9, height: blipCoreRadius * 0.9
)
ctx.fillEllipse(in: hotRect)
ctx.restoreGState()

// MARK: - Export

guard let image = ctx.makeImage() else {
    FileHandle.standardError.write("error: could not render CGImage\n".data(using: .utf8)!)
    exit(1)
}

let bitmapRep = NSBitmapImageRep(cgImage: image)
guard let pngData = bitmapRep.representation(using: .png, properties: [:]) else {
    FileHandle.standardError.write("error: could not encode PNG\n".data(using: .utf8)!)
    exit(1)
}

do {
    try pngData.write(to: outputURL)
    print("Wrote \(outputURL.path) (\(size)x\(size), no alpha)")
} catch {
    FileHandle.standardError.write("error: could not write PNG: \(error)\n".data(using: .utf8)!)
    exit(1)
}
