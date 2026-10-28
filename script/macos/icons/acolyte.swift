// Acolyte's app icon: Moloch's glyph's attendant, in the same Maya style — a face in a rounded
// stone cartouche with downcast, serving eyes, a headband with one jade bead and feathered curls,
// and a speech scroll curling from the mouth (the messenger carrying work back and forth). Black Moss
// colours, edge to edge like Krust's icon.
//
//   swift script/macos/icons/acolyte.swift out.png

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

// Drawn on the 824px grid, scaled up about the centre to fill the canvas, as Moloch's.
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

// The app squircle.
let squircle = rounded(100, 100, 824, 824, 185)
fill(squircle, .black)
stroke(squircle, moss, 14)

// Ear-spools, under the cartouche's edge.
for cx in [CGFloat(218), 806] {
  fill(circle(cx, 470, 50), .black)
  stroke(circle(cx, 470, 50), stone, 16)
  fill(circle(cx, 470, 16), mossLight)
}

// The glyph block.
stroke(rounded(205, 190, 614, 610, 150), stone, 26)

// Headband with one jade bead, and a feathered curl to each side.
stroke(rounded(300, 655, 424, 54, 27), stone, 18)
fill(circle(512, 682, 30), mossLight)
stroke(spiral(345, 735, 30, 1.2, .pi * 1.5, true), stone, 12)
stroke(spiral(679, 735, 30, 1.2, .pi * 1.5, false), stone, 12)

// Downcast eyes: lids closed into low arcs, lashes of a line beneath.
for cx in [CGFloat(388), 636] {
  let lid = NSBezierPath()
  lid.move(to: NSPoint(x: cx - 72, y: 540))
  lid.curve(to: NSPoint(x: cx + 72, y: 540),
            controlPoint1: NSPoint(x: cx - 40, y: 490),
            controlPoint2: NSPoint(x: cx + 40, y: 490))
  stroke(lid, stone, 20)
  stroke(NSBezierPath(rect: NSRect(x: cx - 46, y: 470, width: 92, height: 0.5)), stone, 10)
}

// Nose ridge with a jade bead.
stroke(rounded(491, 410, 42, 130, 21), stone, 16)
fill(circle(512, 384, 16), mossLight)

// A small closed mouth, and a speech scroll curling out from its corner.
stroke(rounded(400, 290, 224, 56, 28), stone, 18)
let breath = NSBezierPath()
breath.move(to: NSPoint(x: 624, y: 318))
breath.curve(to: NSPoint(x: 700, y: 300),
             controlPoint1: NSPoint(x: 660, y: 330),
             controlPoint2: NSPoint(x: 690, y: 330))
stroke(breath, mossLight, 12)
stroke(spiral(712, 280, 30, 1.4, .pi * 0.5, true), mossLight, 12)

// Three dots, as Moloch's glyph has.
for cx in [CGFloat(452), 512, 572] {
  fill(circle(cx, 236, 11), stone)
}

NSGraphicsContext.restoreGraphicsState()
try! rep.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: out))
