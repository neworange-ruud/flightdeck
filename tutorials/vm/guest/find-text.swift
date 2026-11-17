// Find text on the guest's screen with Vision OCR:
//   find-text "Oldest first"     -> "x y w h" in screen points, one line per match,
//                                   top to bottom (exit 1 when there is none)
// For a scene step whose target moves (an agent's button, a dialog that
// depends on the take): vnc_drive.py's clicktext/highlighttext use it.
// Compiled by tutorials/vm/up.sh to /tmp/find-text; the screenshot goes
// through screencapture, which this SSH session may use.
import AppKit
import Vision

let target = CommandLine.arguments.dropFirst().joined(separator: " ").lowercased()
let shot = "/tmp/fd-find-text.png"
let capture = Process()
capture.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
capture.arguments = ["-x", "-t", "png", shot]
try capture.run()
capture.waitUntilExit()

guard let image = NSImage(contentsOfFile: shot),
      let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { exit(2) }
let scale = 2.0  // Retina: pixels per point
let width = Double(cg.width), height = Double(cg.height)

let request = VNRecognizeTextRequest()
request.recognitionLevel = .accurate
request.usesLanguageCorrection = false
try VNImageRequestHandler(cgImage: cg).perform([request])

var found: [(Int, Int, Int, Int)] = []
for observation in request.results ?? [] {
    guard let candidate = observation.topCandidates(1).first else { continue }
    let text = candidate.string.lowercased()
    guard let range = text.range(of: target),
          let box = try? candidate.boundingBox(for: range)?.boundingBox else { continue }
    // Vision's box is normalised with its origin at the bottom left.
    found.append((Int(box.minX * width / scale), Int((1 - box.maxY) * height / scale),
                  Int(box.width * width / scale), Int(box.height * height / scale)))
}
if found.isEmpty { exit(1) }
for (x, y, w, h) in found.sorted(by: { $0.1 < $1.1 }) { print(x, y, w, h) }
