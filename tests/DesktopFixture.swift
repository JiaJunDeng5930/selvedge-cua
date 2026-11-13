import AppKit
import CoreGraphics

if CommandLine.arguments.dropFirst() == ["--desktop-state"] {
    let point = CGEvent(source: nil)!.location
    let state: [String: Any] = [
        "frontmost": NSWorkspace.shared.frontmostApplication?.processIdentifier ?? 0,
        "x": point.x, "y": point.y
    ]
    print(String(data: try JSONSerialization.data(withJSONObject: state), encoding: .utf8)!)
    exit(0)
}

let output = URL(fileURLWithPath: CommandLine.arguments[1])
var observations: [[String: Any]] = []
func record(_ kind: String, _ values: [String: Any] = [:]) {
    var item = values
    item["kind"] = kind
    item["time"] = ProcessInfo.processInfo.systemUptime
    observations.append(item)
    let data = try! JSONSerialization.data(withJSONObject: observations)
    try! data.write(to: output, options: .atomic)
}

final class Drawing: NSView {
    var strokes: [[NSPoint]] = []
    var current: [NSPoint] = []
    override var isFlipped: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override func draw(_ rect: NSRect) {
        NSColor.white.setFill()
        bounds.fill()
        NSColor.systemBlue.setStroke()
        for points in strokes + [current] {
            guard let start = points.first else { continue }
            let path = NSBezierPath()
            path.lineWidth = 4
            path.move(to: start)
            for point in points.dropFirst() { path.line(to: point) }
            path.stroke()
        }
    }
    func local(_ event: NSEvent) -> NSPoint { convert(event.locationInWindow, from: nil) }
    override func mouseDown(with event: NSEvent) {
        current = [local(event)]
        record("down", ["event": event.eventNumber, "shift": event.modifierFlags.contains(.shift)])
    }
    override func mouseDragged(with event: NSEvent) {
        current.append(local(event))
        needsDisplay = true
        record("drag", ["x": current.last!.x, "y": current.last!.y, "event": event.eventNumber, "shift": event.modifierFlags.contains(.shift)])
    }
    override func mouseUp(with event: NSEvent) {
        strokes.append(current)
        current = []
        needsDisplay = true
        record("up", ["event": event.eventNumber, "strokes": strokes.count])
    }
    @objc func undo(_ sender: Any?) {
        if !strokes.isEmpty { strokes.removeLast() }
        needsDisplay = true
        record("undo", ["strokes": strokes.count])
    }
}

final class Tiles: NSView {
    override var isFlipped: Bool { true }
    override func draw(_ rect: NSRect) {
        NSColor.white.setFill()
        bounds.fill()
        for row in 0..<40 {
            for column in 0..<8 {
                let x = CGFloat(column * 135 + (row * 11) % 41)
                let y = CGFloat(row * 31)
                NSColor(hue: CGFloat((column * 17 + row * 31) % 100) / 100,
                        saturation: 0.8, brightness: 0.8, alpha: 1).setFill()
                NSRect(x: x, y: y, width: 115, height: 21).fill()
                ("tile \(row),\(column)" as NSString).draw(at: NSPoint(x: x+6, y: y+3))
            }
        }
    }
    override func scrollWheel(with event: NSEvent) {
        record("scroll", ["dx": event.scrollingDeltaX, "dy": event.scrollingDeltaY])
        super.scrollWheel(with: event)
    }
}

final class Delegate: NSObject, NSApplicationDelegate, NSTextFieldDelegate {
    let window = NSWindow(contentRect: NSRect(x: 200, y: 180, width: 710, height: 530),
                          styleMask: [.titled, .closable], backing: .buffered, defer: false)
    let drawing = Drawing(frame: NSRect(x: 12, y: 12, width: 360, height: 454))
    let field = NSTextField(frame: NSRect(x: 12, y: 483, width: 235, height: 26))
    let button = NSButton(frame: NSRect(x: 250, y: 483, width: 124, height: 28))
    let slider = NSSlider(value: 2, minValue: 0, maxValue: 10, target: nil, action: nil)

    func applicationDidFinishLaunching(_ notification: Notification) {
        let root = window.contentView!
        window.title = "Selvedge CUA Test Fixture"
        root.addSubview(drawing)
        let scroll = NSScrollView(frame: NSRect(x: 385, y: 12, width: 310, height: 454))
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = false
        scroll.documentView = Tiles(frame: NSRect(x: 0, y: 0, width: 1100, height: 1300))
        root.addSubview(scroll)
        field.placeholderString = "Fixture input"
        field.setAccessibilityIdentifier("fixture-input")
        field.delegate = self
        root.addSubview(field)
        button.title = "Fixture button"
        button.target = self
        button.action = #selector(pressed)
        root.addSubview(button)
        slider.frame = NSRect(x: 394, y: 485, width: 220, height: 28)
        slider.setAccessibilityIdentifier("fixture-slider")
        root.addSubview(slider)
        let edit = NSMenu(title: "Edit")
        let undo = NSMenuItem(title: "Undo Stroke", action: #selector(Drawing.undo(_:)), keyEquivalent: "z")
        undo.target = drawing
        edit.addItem(undo)
        let main = NSMenu()
        let editItem = NSMenuItem(title: "Edit", action: nil, keyEquivalent: "")
        editItem.submenu = edit
        main.addItem(editItem)
        NSApplication.shared.mainMenu = main
        window.makeKeyAndOrderFront(nil)
        window.makeFirstResponder(field)
        window.orderFrontRegardless()
        let location = window.convertToScreen(root.bounds)
        let screenHeight = NSScreen.screens[0].frame.height
        record("ready", ["x": location.minX, "y": screenHeight - location.maxY,
                         "width": location.width, "height": location.height,
                         "pid": ProcessInfo.processInfo.processIdentifier, "windowId": window.windowNumber])
    }
    func controlTextDidChange(_ notification: Notification) {
        record("text", ["value": field.stringValue])
    }
    @objc func pressed() {
        record("pressed")
    }
}

let app = NSApplication.shared
let delegate = Delegate()
app.delegate = delegate
app.setActivationPolicy(.regular)
app.run()
