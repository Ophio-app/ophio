// SPDX-License-Identifier: Apache-2.0
// Disposable native fixture only. Never launched by ordinary unit tests.
// Build as an AppKit .app executable using swiftc -framework AppKit.
import AppKit
import Foundation

final class Fixture: NSObject, NSApplicationDelegate, NSWindowDelegate {
    private var window: NSWindow?
    private var count = 0
    private let directory: URL

    init(directory: URL) {
        self.directory = directory
        super.init()
    }

    private func mark(_ name: String, _ value: String) {
        do {
            try Data(value.utf8).write(to: directory.appendingPathComponent(name), options: .atomic)
        } catch {
            fputs("Fixture marker write failed\n", stderr)
            NSApplication.shared.terminate(nil)
        }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let window = NSWindow(
            contentRect: NSRect(x: 120, y: 120, width: 420, height: 180),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Ophio native fixture"
        window.delegate = self
        window.isReleasedWhenClosed = false
        let button = NSButton(title: "Increment fixture counter", target: self, action: #selector(increment))
        button.frame = NSRect(x: 70, y: 65, width: 280, height: 40)
        button.setAccessibilityIdentifier("ophio-native-fixture-increment")
        button.setAccessibilityLabel("Increment fixture counter")
        window.contentView?.addSubview(button)
        self.window = window
        mark("button-count.txt", "0\n")
        mark("pid.txt", "\(ProcessInfo.processInfo.processIdentifier)\n")
        mark("bundle-path.txt", "\(Bundle.main.bundleURL.path)\n")
        window.makeKeyAndOrderFront(nil)
        NSApplication.shared.activate(ignoringOtherApps: true)
        mark("ready.txt", "Ophio native fixture\n")
    }

    @objc private func increment() {
        count += 1
        mark("button-count.txt", "\(count)\n")
    }

    func windowWillClose(_ notification: Notification) {
        mark("window-closed.txt", "closed\n")
        NSApplication.shared.terminate(nil)
    }
}

let environment = ProcessInfo.processInfo.environment
var isDirectory: ObjCBool = false
guard environment["OPHIO_MAC_NATIVE_FIXTURE_APPROVED"] == "1",
      let path = environment["OPHIO_MAC_NATIVE_FIXTURE_MARKERS"],
      path.hasPrefix("/"),
      FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory),
      isDirectory.boolValue else {
    fputs("Explicit fixture opt-in and an existing absolute marker directory are required\n", stderr)
    exit(2)
}
let application = NSApplication.shared
let delegate = Fixture(directory: URL(fileURLWithPath: path, isDirectory: true))
application.delegate = delegate
application.setActivationPolicy(.regular)
application.run()
