// SPDX-License-Identifier: Apache-2.0
//
// Phone-local authentication. No companion command or authority enters this
// class. As on Android: the lock follows the person's choice of when to lock
// again, an opaque cover hides the app until its own lock screen has painted,
// and the app switcher never shows what was on screen while the lock is on.

import Foundation
import LocalAuthentication
import Tauri
import UIKit

final class AppLock {
  private static let enabledKey = "app-lock.enabled"
  private static let minutesKey = "app-lock.minutes"
  private static let choices = [0, 1, 5, 15]
  private static let unlockTitle = "Unlock app"

  /// Whether the lock is on, for alerts raised off the main thread.
  static var enabledNow: Bool { UserDefaults.standard.bool(forKey: enabledKey) }

  private let emit: (JsonObject) -> Void
  private let turnedOn: () -> Void
  private weak var webView: UIView?
  private var enabled: Bool
  private var minutes: Int
  private var locked: Bool
  private var leftAt: UInt64?
  private var prompting = false
  /// The app's own lock screen has painted since the app last locked.
  private var screenShowsLock = false
  /// Hidden from the app switcher while the app isn't active.
  private var covered = false
  private var curtain: UIView?
  private var words: UIStackView?
  private let mirror = NSLock()
  private var lockedNow: Bool

  /// On the main thread. `emit` sends each change to the app; `turnedOn` runs
  /// once the lock is turned on, to hide what alerts already show.
  init(emit: @escaping (JsonObject) -> Void, turnedOn: @escaping () -> Void) {
    self.emit = emit
    self.turnedOn = turnedOn
    let defaults = UserDefaults.standard
    enabled = defaults.bool(forKey: AppLock.enabledKey)
    let saved = defaults.integer(forKey: AppLock.minutesKey)
    minutes = AppLock.choices.contains(saved) ? saved : 0
    locked = enabled
    lockedNow = enabled
  }

  /// Whether the app is locked, from any thread.
  var isLocked: Bool { mirror.sync { lockedNow } }

  private static var secure: Bool {
    LAContext().canEvaluatePolicy(.deviceOwnerAuthentication, error: nil)
  }

  func state() -> JsonObject {
    let available = AppLock.secure
    return [
      "available": available,
      "reason": available ? NSNull() as Any : "Set a passcode in iPhone Settings first.",
      "enabled": enabled,
      "locked": locked,
      "relockMinutes": minutes,
    ]
  }

  func attach(_ webView: UIView) {
    self.webView = webView
    paint()
  }

  /// The app left the screen.
  func pause() {
    guard enabled else { return }
    // Keep the departure time even while authenticating so returning can't bypass relock.
    if leftAt == nil { leftAt = monotonicNanos() }
    if !prompting && minutes == 0 { setLocked(true) }
    publish()
  }

  /// The app is coming back to the screen.
  func resume() {
    if enabled, let left = leftAt, monotonicNanos() - left >= UInt64(minutes) * 60_000_000_000 {
      setLocked(true)
    }
    leftAt = nil
    publish()
  }

  /// The app is about to stop being active: the app switcher, Control
  /// Center or a call. iOS takes its snapshot after this.
  func cover() {
    guard enabled, !prompting else { return }
    covered = true
    paint()
  }

  func uncover() {
    guard covered else { return }
    covered = false
    paint()
  }

  func lock(_ invoke: Invoke) {
    if enabled { setLocked(true) }
    publish()
    invoke.resolve(state())
  }

  func configure(_ invoke: Invoke, enabled nextEnabled: Bool, minutes nextMinutes: Int64) {
    guard let nextMinutes = AppLock.choices.first(where: { Int64($0) == nextMinutes }) else {
      return invoke.reject("Choose when the app should lock again.")
    }
    authenticate(invoke) {
      let defaults = UserDefaults.standard
      defaults.set(nextEnabled, forKey: AppLock.enabledKey)
      defaults.set(nextMinutes, forKey: AppLock.minutesKey)
      guard defaults.bool(forKey: AppLock.enabledKey) == nextEnabled,
        defaults.integer(forKey: AppLock.minutesKey) == nextMinutes
      else { return invoke.reject("The lock setting could not be saved.") }
      let was = self.enabled
      self.enabled = nextEnabled
      self.minutes = nextMinutes
      self.setLocked(false)
      self.leftAt = nil
      self.publish()
      if nextEnabled && !was { self.turnedOn() }
      invoke.resolve(self.state())
    }
  }

  func unlock(_ invoke: Invoke) {
    guard enabled, locked else { return invoke.resolve(state()) }
    authenticate(invoke) {
      self.setLocked(false)
      self.leftAt = nil
      self.publish()
      invoke.resolve(self.state())
    }
  }

  /// Asks for the phone's own unlock before one account action, every time.
  func confirm(_ invoke: Invoke, title: String) {
    let trimmed = String(title.trimmingCharacters(in: .whitespacesAndNewlines).prefix(80))
    authenticate(
      invoke, title: trimmed.isEmpty ? "Confirm" : trimmed,
      description: "Confirms this one action on your computer."
    ) { invoke.resolve(self.state()) }
  }

  /// Called only after the app has removed all private screens and painted its unlock view.
  func screenReady(_ invoke: Invoke) {
    if locked { screenShowsLock = true }
    paint()
    invoke.resolve(state())
  }

  private func authenticate(
    _ invoke: Invoke, title: String = AppLock.unlockTitle,
    description: String = "Unlocking the app doesn’t approve or start work.",
    success: @escaping () -> Void
  ) {
    let context = LAContext()
    guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: nil) else {
      return invoke.reject("Set a secure screen lock in iPhone Settings first.")
    }
    guard !prompting else { return invoke.reject("An unlock request is already open.") }
    prompting = true
    // Face ID shows the app's name over this; there's no separate title.
    let reason = title == AppLock.unlockTitle ? description : "\(title). \(description)"
    context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason) { passed, _ in
      onMain {
        self.prompting = false
        if passed {
          success()
        } else {
          invoke.reject(
            title == AppLock.unlockTitle
              ? "Not unlocked. Use your phone’s biometrics or screen lock to try again."
              : "Not confirmed. Nothing was sent to the computer.")
          self.publish()
        }
      }
    }
  }

  private func setLocked(_ next: Bool) {
    if next && !locked { screenShowsLock = false }
    locked = next
    mirror.sync { lockedNow = next }
  }

  private func publish() {
    paint()
    emit(["type": "lockChanged", "state": state()])
  }

  private func paint() {
    guard let window = webView?.window else { return }
    let curtain = self.curtain ?? makeCurtain()
    if curtain.superview !== window {
      curtain.frame = window.bounds
      window.addSubview(curtain)
    }
    curtain.backgroundColor = Palette.current.surface
    words?.arrangedSubviews.forEach { ($0 as? UILabel)?.textColor = Palette.current.ink }
    let shut = locked && !screenShowsLock
    // Covering for the app switcher says nothing; the locked app says why.
    words?.isHidden = !shut
    curtain.isHidden = !(shut || covered)
    if !curtain.isHidden { window.bringSubviewToFront(curtain) }
  }

  private func makeCurtain() -> UIView {
    let view = UIView()
    view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
    view.accessibilityViewIsModal = true
    let title = UILabel()
    title.text = "App locked"
    title.font = .preferredFont(forTextStyle: .title2)
    let note = UILabel()
    note.text = "Unlock with your phone’s biometrics or screen lock."
    note.font = .preferredFont(forTextStyle: .body)
    note.numberOfLines = 0
    for label in [title, note] {
      label.textAlignment = .center
      label.adjustsFontForContentSizeCategory = true
    }
    let stack = UIStackView(arrangedSubviews: [title, note])
    stack.axis = .vertical
    stack.spacing = 24
    stack.translatesAutoresizingMaskIntoConstraints = false
    view.addSubview(stack)
    NSLayoutConstraint.activate([
      stack.centerYAnchor.constraint(equalTo: view.centerYAnchor),
      stack.leadingAnchor.constraint(equalTo: view.layoutMarginsGuide.leadingAnchor, constant: 8),
      stack.trailingAnchor.constraint(equalTo: view.layoutMarginsGuide.trailingAnchor, constant: -8),
    ])
    curtain = view
    words = stack
    return view
  }
}
