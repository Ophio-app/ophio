// SPDX-License-Identifier: Apache-2.0
import Foundation
import Tauri
import UIKit
import WebKit

/// UIKit exposes hinge posture but does not expose hinge bounds. Keep geometry
/// explicitly unavailable rather than putting an imaginary crease on the page.
final class WindowLayout {
  private weak var web: WKWebView?
  private var interaction: UIInteraction?
  private var bounds: NSKeyValueObservation?
  private var posture: [String: Any]?
  private var revision = 0
  private var lastSent: String?
  private var pending: [(JsonObject) -> Void] = []
  private let emit: (JsonObject) -> Void

  init(emit: @escaping (JsonObject) -> Void) { self.emit = emit }

  func attach(_ webview: WKWebView) {
    if let interaction { web?.removeInteraction(interaction) }
    bounds = nil
    interaction = nil
    posture = nil
    web = webview
    revision += 1
    bounds = webview.observe(\.bounds, options: [.new]) { [weak self] _, _ in
      self?.refresh()
    }
    if #available(iOS 27.1, *) {
      let hinge = UIHingeInteraction { [weak self] _, update in
        guard let self else { return }
        if let hinge = update.hinge {
          let status: String
          switch hinge.status {
          case .closed: status = "closed"
          case .partiallyOpen: status = "partially-open"
          case .fullyOpen: status = "fully-open"
          default: status = "unknown"
          }
          self.posture = ["status": status, "angle": Double(hinge.angle)]
        } else {
          self.posture = nil
        }
        self.refresh()
      }
      interaction = hinge
      webview.addInteraction(hinge)
    }
    refresh()
  }

  func snapshot(_ reply: @escaping (JsonObject) -> Void) {
    pending.append(reply)
    refresh()
  }

  func replay() {
    lastSent = nil
    refresh()
  }

  func refresh() {
    revision += 1
    let expected = revision
    guard let web else {
      publish(["availability": "unavailable", "width": 0, "height": 0, "folds": []])
      return
    }
    web.evaluateJavaScript("({width: innerWidth, height: innerHeight})") { [weak self, weak web] value, _ in
      guard let self, let web, self.web === web, expected == self.revision else { return }
      let viewport = value as? [String: Any]
      var layout: JsonObject = [
        "availability": "unavailable", "width": viewport?["width"] as? Double ?? 0,
        "height": viewport?["height"] as? Double ?? 0, "folds": [],
      ]
      if let posture = self.posture { layout["posture"] = posture }
      self.publish(layout)
    }
  }

  private func publish(_ layout: JsonObject) {
    let data = try? JSONSerialization.data(withJSONObject: layout, options: [.sortedKeys])
    let encoded = data.flatMap { String(data: $0, encoding: .utf8) }
    if encoded != lastSent {
      lastSent = encoded
      emit(["type": "windowLayoutChanged", "layout": layout])
    }
    let replies = pending
    pending.removeAll()
    replies.forEach { $0(layout) }
  }
}
