// SPDX-License-Identifier: Apache-2.0
import Foundation
import MetricKit
import Tauri
import UIKit
import WebKit

final class Reports: NSObject, MXMetricManagerSubscriber {
  private struct Crash: Codable {
    let at: Int64
    let code: String
    let build: String

    var json: JsonObject {
      ["at": at, "side": "phone", "kind": "system", "code": code, "build": build, "frames": []]
    }
  }

  private struct Saved: Codable {
    var through: Int64 = 0
    var seen: [String] = []
    var pending: [Crash] = []
  }

  private let lock = NSLock()
  private let file = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
    .appendingPathComponent("report-diagnostics.json")
  private weak var web: WKWebView?
  private var allowed: () -> Bool = { false }
  private var observer: NSObjectProtocol?
  private var events: Channel?
  private var subscribed = false

  func attach(_ web: WKWebView, allowed: @escaping () -> Bool) {
    self.web = web
    self.allowed = allowed
    #if !targetEnvironment(simulator)
      guard !subscribed else { return }
      subscribed = true
      MXMetricManager.shared.add(self)
      didReceive(MXMetricManager.shared.pastDiagnosticPayloads)
    #endif
  }

  deinit {
    if let observer = observer { NotificationCenter.default.removeObserver(observer) }
    if subscribed { MXMetricManager.shared.remove(self) }
  }

  func supported(_ invoke: Invoke) {
    #if targetEnvironment(simulator)
      invoke.resolve(["supported": false])
    #else
      invoke.resolve(["supported": true])
    #endif
  }

  func start(_ invoke: Invoke, events: Channel) {
    #if !targetEnvironment(simulator)
      self.events = events
      if observer == nil {
        observer = NotificationCenter.default.addObserver(
          forName: UIApplication.userDidTakeScreenshotNotification, object: nil, queue: .main
        ) { [weak self] _ in
          guard let self = self, self.canCapture else { return }
          let event: JsonObject = ["type": "screenshot"]
          self.events?.send(event)
        }
      }
    #endif
    supported(invoke)
  }

  func stop(_ invoke: Invoke) {
    if let observer = observer { NotificationCenter.default.removeObserver(observer) }
    observer = nil
    events = nil
    invoke.resolve()
  }

  private var canCapture: Bool {
    UIApplication.shared.applicationState == .active && allowed() && web?.window != nil
  }

  func capture(_ invoke: Invoke) {
    #if targetEnvironment(simulator)
      invoke.resolve(["screenshot": NSNull()])
    #else
      guard canCapture, let web = web, web.bounds.width > 0, web.bounds.height > 0 else {
        return invoke.resolve(["screenshot": NSNull()])
      }
      let config = WKSnapshotConfiguration()
      config.rect = web.bounds
      config.snapshotWidth = NSNumber(value: Double(min(web.bounds.width, 1200)))
      // Snapshot only this app's web view, never a display or another window.
      web.takeSnapshot(with: config) { [weak self] image, _ in
        guard let self = self, self.canCapture, let image = image else {
          return invoke.resolve(["screenshot": NSNull()])
        }
        var picture = image
        for _ in 0..<8 {
          for quality: CGFloat in [0.85, 0.65, 0.45] {
            if let bytes = picture.jpegData(compressionQuality: quality), bytes.count <= 512 * 1024 {
              guard self.canCapture else { return invoke.resolve(["screenshot": NSNull()]) }
              let value: JsonObject = [
                "media_type": "image/jpeg", "data": bytes.base64EncodedString(),
                "width": picture.cgImage?.width ?? Int(picture.size.width * picture.scale),
                "height": picture.cgImage?.height ?? Int(picture.size.height * picture.scale),
              ]
              return invoke.resolve(["screenshot": value])
            }
          }
          let size = CGSize(width: max(1, picture.size.width * 0.75), height: max(1, picture.size.height * 0.75))
          let format = UIGraphicsImageRendererFormat()
          format.scale = 1
          format.opaque = true
          let previous = picture
          picture = UIGraphicsImageRenderer(size: size, format: format).image { _ in
            previous.draw(in: CGRect(origin: .zero, size: size))
          }
        }
        invoke.resolve(["screenshot": NSNull()])
      }
    #endif
  }

  func takeCrashes() throws -> JsonObject {
    #if targetEnvironment(simulator)
      return ["crashes": []]
    #else
      return try lock.sync {
        var saved = try read()
        let result = saved.pending.sorted { $0.at > $1.at }.map { $0.json }
        saved.pending.removeAll()
        try write(saved)
        var taken: JsonObject = ["crashes": result]
        // An update replaces the app bundle; nothing from before it is a crash to send.
        if let installed = try? FileManager.default.attributesOfItem(atPath: Bundle.main.bundlePath)[.creationDate] as? Date {
          taken["updatedAt"] = Int64(installed.timeIntervalSince1970 * 1000)
        }
        return taken
      }
    #endif
  }

  func didReceive(_ payloads: [MXMetricPayload]) {}

  func didReceive(_ payloads: [MXDiagnosticPayload]) {
    #if !targetEnvironment(simulator)
      try? lock.sync {
        var saved = try read()
        for payload in payloads.sorted(by: { $0.timeStampEnd < $1.timeStampEnd }) {
          let at = Int64(payload.timeStampEnd.timeIntervalSince1970 * 1000)
          guard at >= saved.through else { continue }
          if at > saved.through {
            saved.through = at
            saved.seen.removeAll()
          }
          let groups: [(String, [MXDiagnostic])] = [
            ("crash", (payload.crashDiagnostics ?? []).map { $0 as MXDiagnostic }),
            ("hang", (payload.hangDiagnostics ?? []).map { $0 as MXDiagnostic }),
          ]
          for (kind, diagnostics) in groups {
            for (index, diagnostic) in diagnostics.enumerated() {
              let build = String(diagnostic.applicationVersion.prefix(160))
              let key = "\(payload.timeStampBegin.timeIntervalSince1970):\(kind):\(index):\(build)"
              guard !saved.seen.contains(key) else { continue }
              saved.seen.append(key)
              // Keep only structured fields. Never read MetricKit's stacks or text.
              saved.pending.append(Crash(at: at, code: "metric:\(kind)", build: build))
            }
          }
        }
        saved.pending = Array(saved.pending.sorted { $0.at > $1.at }.prefix(32))
        try write(saved)
      }
    #endif
  }

  private func read() throws -> Saved {
    guard FileManager.default.fileExists(atPath: file.path) else { return Saved() }
    return try JSONDecoder().decode(Saved.self, from: Data(contentsOf: file))
  }

  private func write(_ saved: Saved) throws {
    try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
    try JSONEncoder().encode(saved).write(to: file, options: [.atomic, .completeFileProtectionUntilFirstUserAuthentication])
  }
}
