// SPDX-License-Identifier: Apache-2.0
//
// Pieces every part of the iPhone plugin shares: the arguments as the Rust
// shell sends them, the URL-safe Base64 the Android side uses, a refusal that
// reaches the app as written, and the app's two palettes.

import Foundation
import Tauri
import UIKit

/// A refusal whose message reaches the app word for word.
struct Failure: Error {
  let message: String
  init(_ message: String) { self.message = message }
}

/// What a command answers with.
enum Reply {
  case none
  case object(JsonObject)
  case text(String)

  func send(_ invoke: Invoke) {
    switch self {
    case .none: invoke.resolve()
    case .object(let object): invoke.resolve(object)
    case .text(let text): invoke.resolve(text)
    }
  }
}

/// A command's arguments, read from the JSON the Rust shell sent.
struct Args {
  private let values: [String: Any]

  init(_ invoke: Invoke) throws {
    guard let data = invoke.getRawArgs().data(using: .utf8),
      let values = try JSONSerialization.jsonObject(with: data) as? [String: Any]
    else { throw Failure("Invalid arguments.") }
    self.values = values
  }

  func string(_ key: String) throws -> String {
    guard let value = values[key] as? String else { throw Failure("Missing \(key).") }
    return value
  }

  func optString(_ key: String) -> String? { values[key] as? String }

  func int64(_ key: String) throws -> Int64 {
    guard let value = values[key] as? NSNumber, !Args.isBool(value) else {
      throw Failure("Missing \(key).")
    }
    return value.int64Value
  }

  func bool(_ key: String, or fallback: Bool = false) -> Bool {
    guard let value = values[key] as? NSNumber, Args.isBool(value) else { return fallback }
    return value.boolValue
  }

  func array(_ key: String) -> [Any] { values[key] as? [Any] ?? [] }

  /// The value as sent, nil when absent or null.
  func raw(_ key: String) -> Any? {
    guard let value = values[key], !(value is NSNull) else { return nil }
    return value
  }

  private static func isBool(_ number: NSNumber) -> Bool {
    CFGetTypeID(number) == CFBooleanGetTypeID()
  }
}

/// URL-safe Base64 without padding, as on Android.
enum Base64URL {
  static func encode(_ data: Data) -> String {
    data.base64EncodedString()
      .replacingOccurrences(of: "+", with: "-")
      .replacingOccurrences(of: "/", with: "_")
      .replacingOccurrences(of: "=", with: "")
  }

  static func decode(_ text: String) throws -> Data {
    let allowed = text.unicodeScalars.allSatisfy {
      ("A"..."Z").contains($0) || ("a"..."z").contains($0) || ("0"..."9").contains($0)
        || $0 == "-" || $0 == "_"
    }
    guard allowed, text.count % 4 != 1 else { throw Failure("Invalid bytes.") }
    var standard = text.replacingOccurrences(of: "-", with: "+")
      .replacingOccurrences(of: "_", with: "/")
    standard += String(repeating: "=", count: (4 - standard.count % 4) % 4)
    guard let data = Data(base64Encoded: standard) else { throw Failure("Invalid bytes.") }
    return data
  }
}

extension NSLock {
  func sync<T>(_ body: () throws -> T) rethrows -> T {
    lock()
    defer { unlock() }
    return try body()
  }
}

/// Runs on the main thread, now when already there.
func onMain(_ body: @escaping () -> Void) {
  if Thread.isMainThread { body() } else { DispatchQueue.main.async(execute: body) }
}

/// A lowercase UUID, as Android's are and as the Rust shell checks.
func newId() -> String { UUID().uuidString.lowercased() }

/// Time since boot that keeps counting while the phone sleeps.
func monotonicNanos() -> UInt64 { clock_gettime_nsec_np(CLOCK_MONOTONIC) }

/// The app's colours, light and dark, as on Android.
struct Palette {
  let surface: UIColor
  let ink: UIColor
  let line: UIColor
  let accent: UIColor

  static let light = Palette(
    surface: hex(0xFAF7F0), ink: hex(0x211526), line: hex(0xDCD1DF), accent: hex(0x3B4D00))
  static let dark = Palette(
    surface: hex(0x120C0A), ink: hex(0xF1EEE6), line: hex(0x49374F), accent: hex(0xC3FF24))

  /// The palette of the theme the app last chose.
  static var current: Palette { Shell.dark ? dark : light }

  static func hex(_ value: UInt32) -> UIColor {
    UIColor(
      red: CGFloat((value >> 16) & 0xFF) / 255,
      green: CGFloat((value >> 8) & 0xFF) / 255,
      blue: CGFloat(value & 0xFF) / 255,
      alpha: 1)
  }
}

/// The screen a sheet can be shown from.
func topController(_ root: UIViewController?) -> UIViewController? {
  var top = root
  while let shown = top?.presentedViewController, !shown.isBeingDismissed { top = shown }
  return top
}
