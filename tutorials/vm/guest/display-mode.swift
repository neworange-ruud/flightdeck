// Switch the guest's main display to a HiDPI mode of W x H points, for good:
//   swift display-mode.swift 1440 810
// Tart's `--display` sets the virtual panel, but macOS keeps the mode it
// saved for that display, so a base image made at another size needs this.
import CoreGraphics
import Foundation

let args = CommandLine.arguments
guard args.count == 3, let w = Int(args[1]), let h = Int(args[2]) else {
    print("usage: display-mode W H"); exit(2)
}
let display = CGMainDisplayID()
let opts = [kCGDisplayShowDuplicateLowResolutionModes: true] as CFDictionary
let modes = CGDisplayCopyAllDisplayModes(display, opts) as! [CGDisplayMode]
guard let mode = modes.first(where: { $0.width == w && $0.height == h && $0.pixelWidth == 2 * w }) else {
    print("no \(w)x\(h) HiDPI mode"); exit(1)
}
var config: CGDisplayConfigRef?
CGBeginDisplayConfiguration(&config)
CGConfigureDisplayWithDisplayMode(config, display, mode, nil)
let err = CGCompleteDisplayConfiguration(config, .permanently)
let now = CGDisplayCopyDisplayMode(display)!
print("display: \(now.width)x\(now.height) points, \(now.pixelWidth)x\(now.pixelHeight) pixels (\(err.rawValue))")
