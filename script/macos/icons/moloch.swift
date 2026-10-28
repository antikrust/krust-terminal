// Moloch's app icon: a Maya-style glyph block — a frontal idol face in a rounded stone cartouche,
// with the sun god K'inich Ajaw's features (square eyes with crossed pupils, a T-shaped filed tooth),
// ear-spools at the sides and a beaded headdress band. Drawn in Krust's Black Moss colours: stone
// grey on black, moss-green inlays.
//
//   swift script/macos/icons/moloch.swift out.png
//
// Coordinates are on the 1024px macOS icon grid (the squircle is 824px), origin bottom-left.

import AppKit

let out = CommandLine.arguments[1]
let size: CGFloat = 1024
let stone = NSColor(srgbRed: 0xD6 / 255.0, green: 0xD6 / 255.0, blue: 0xD6 / 255.0, alpha: 1)
let moss = NSColor(srgbRed: 0x2F / 255.0, green: 0x6F / 255.0, blue: 0x44 / 255.0, alpha: 1)
let mossLight = NSColor(srgbRed: 0x48 / 255.0, green: 0xA8 / 255.0, blue: 0x68 / 255.0, alpha: 1)

let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(size), pixelsHigh: Int(size),
                           bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                           colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)

// Krust's icon runs edge to edge on its canvas, so this one does too: everything below is drawn on
// the 824px grid and scaled up about the centre until the squircle meets the canvas edge.
let fill = NSAffineTransform()
fill.translateX(by: size / 2, yBy: size / 2)
fill.scale(by: size / 824)
fill.translateX(by: -size / 2, yBy: -size / 2)
fill.concat()

func rounded(_ x: CGFloat, _ y: CGFloat, _ w: CGFloat, _ h: CGFloat, _ r: CGFloat) -> NSBezierPath {
  NSBezierPath(roundedRect: NSRect(x: x, y: y, width: w, height: h), xRadius: r, yRadius: r)
}
func circle(_ cx: CGFloat, _ cy: CGFloat, _ r: CGFloat) -> NSBezierPath {
  NSBezierPath(ovalIn: NSRect(x: cx - r, y: cy - r, width: r * 2, height: r * 2))
}
func stroke(_ path: NSBezierPath, _ color: NSColor, _ width: CGFloat) {
  color.setStroke()
  path.lineWidth = width
  path.lineJoinStyle = .round
  path.lineCapStyle = .round
  path.stroke()
}
func fill(_ path: NSBezierPath, _ color: NSColor) {
  color.setFill()
  path.fill()
}
/// A scroll: an Archimedean spiral from `cx, cy` outward to `radius`, over `turns`, starting at
/// `start` radians; `clockwise` mirrors it. The curl Maya carving uses for eyes, cheeks and breath.
func spiral(_ cx: CGFloat, _ cy: CGFloat, _ radius: CGFloat, _ turns: CGFloat, _ start: CGFloat,
            _ clockwise: Bool) -> NSBezierPath {
  let path = NSBezierPath()
  let steps = 120
  for i in 0...steps {
    let t = CGFloat(i) / CGFloat(steps)
    let angle = start + (clockwise ? -1 : 1) * t * turns * 2 * .pi
    let r = radius * (0.12 + 0.88 * t)
    let point = NSPoint(x: cx + r * cos(angle), y: cy + r * sin(angle))
    if i == 0 { path.move(to: point) } else { path.line(to: point) }
  }
  return path
}

// The app squircle: black, with an accent rim.
let squircle = rounded(100, 100, 824, 824, 185)
fill(squircle, .black)
stroke(squircle, moss, 14)

// Ear-flares, drawn first so the cartouche's edge runs over them like a flare through the ear.
for cx in [CGFloat(218), 806] {
  fill(circle(cx, 470, 58), .black)
  stroke(circle(cx, 470, 58), stone, 18)
  fill(circle(cx, 470, 20), mossLight)
}

// The glyph block: a rounded, pillow-like stone cartouche.
stroke(rounded(205, 190, 614, 610, 150), stone, 26)

// Crest: three scalloped lobes (the cloud / stone scallops of a headdress), a jade bead in each.
let crest = NSBezierPath()
crest.move(to: NSPoint(x: 290, y: 650))
for i in 0..<3 {
  let left = 290 + CGFloat(i) * 148
  crest.curve(to: NSPoint(x: left + 148, y: 650),
              controlPoint1: NSPoint(x: left + 10, y: 752),
              controlPoint2: NSPoint(x: left + 138, y: 752))
}
crest.line(to: NSPoint(x: 290, y: 650))
stroke(crest, stone, 18)
for i in 0..<3 {
  fill(circle(290 + 74 + CGFloat(i) * 148, 690, 20), mossLight)
}

// Square "god eyes", each with a spiral pupil curling toward the nose (the crossed gaze of the sun
// god, carved as a scroll).
stroke(rounded(298, 470, 156, 128, 42), stone, 20)
stroke(rounded(570, 470, 156, 128, 42), stone, 20)
stroke(spiral(410, 530, 34, 1.6, .pi, false), stone, 11)
stroke(spiral(614, 530, 34, 1.6, 0, true), stone, 11)

// Cheek scrolls under the outer corners of the eyes.
stroke(spiral(318, 405, 34, 1.3, .pi * 0.5, false), mossLight, 10)
stroke(spiral(706, 405, 34, 1.3, .pi * 0.5, true), mossLight, 10)

// Nose: a ridge with a jade bead below it.
stroke(rounded(491, 420, 42, 140, 21), stone, 16)
fill(circle(512, 394, 16), mossLight)

// Mouth, its corners curling up into small scrolls, with the T-shaped filed tooth.
stroke(rounded(352, 268, 320, 86, 40), stone, 20)
stroke(spiral(330, 330, 24, 1.1, 0, true), stone, 10)
stroke(spiral(694, 330, 24, 1.1, .pi, false), stone, 10)
fill(rounded(474, 314, 76, 22, 6), stone)
fill(rounded(501, 277, 22, 46, 6), stone)

// Bar-and-dot under the mouth, as the numeral marks carved beside glyph blocks.
for cx in [CGFloat(452), 512, 572] {
  fill(circle(cx, 236, 11), stone)
}
fill(rounded(420, 208, 184, 12, 6), stone)

NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
