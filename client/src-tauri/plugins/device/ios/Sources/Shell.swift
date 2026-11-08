// SPDX-License-Identifier: Apache-2.0
//
// The app's frame on the phone: the page fills the screen behind the status
// bar and home indicator, as on Android, and shrinks to the space above the
// keyboard. The status bar follows the app's theme, not the phone's, and
// immersive mode hides it and the home indicator.

import Foundation
import ObjectiveC
import Tauri
import UIKit
import WebKit

final class Shell {
  /// The theme the app last chose; read on the main thread.
  static var dark = false
  private static var statusStyle: UIStatusBarStyle = .darkContent
  private static var installed = false

  private weak var webView: WKWebView?
  private weak var controller: UIViewController?
  private var awake = false
  private var immersive = false
  private var keyboard: CGFloat = 0
  private var observers: [NSObjectProtocol] = []

  /// On the main thread, once the page's web view exists.
  func attach(_ webView: WKWebView, _ controller: UIViewController?) {
    self.webView = webView
    self.controller = controller
    Shell.dark = webView.traitCollection.userInterfaceStyle == .dark
    Shell.statusStyle = Shell.dark ? .lightContent : .darkContent
    let scroll = webView.scrollView
    scroll.contentInsetAdjustmentBehavior = .never
    scroll.bounces = false
    // The web view would pan the page under the keyboard itself; instead it
    // gets the space above it, as Android's adjustResize gives the page.
    let center = NotificationCenter.default
    for name in [
      UIResponder.keyboardWillShowNotification, UIResponder.keyboardWillHideNotification,
      UIResponder.keyboardWillChangeFrameNotification, UIResponder.keyboardDidChangeFrameNotification,
    ] {
      center.removeObserver(webView, name: name, object: nil)
    }
    observers.append(
      center.addObserver(
        forName: UIResponder.keyboardWillChangeFrameNotification, object: nil, queue: .main
      ) { [weak self] note in self?.keyboardMoved(note) })
    observers.append(
      center.addObserver(
        forName: UIResponder.keyboardWillHideNotification, object: nil, queue: .main
      ) { [weak self] _ in self?.fit(0) })
    if let controller = controller { Shell.install(on: controller) }
    paint()
  }

  deinit {
    observers.forEach { NotificationCenter.default.removeObserver($0) }
  }

  /// Back in the foreground: everything the app asked for, again.
  func apply(foreground: Bool) {
    UIApplication.shared.isIdleTimerDisabled = awake && foreground
    setImmersive(immersive)
    paint()
  }

  func keepAwake(_ on: Bool, foreground: Bool) {
    awake = on
    UIApplication.shared.isIdleTimerDisabled = on && foreground
  }

  func setImmersive(_ on: Bool) {
    immersive = on
    // The window's controller is tao's, which keeps both as settable flags.
    call("setPrefersStatusBarHidden:", on)
    call("setPrefersHomeIndicatorAutoHidden:", on)
  }

  func theme(dark: Bool) {
    Shell.dark = dark
    Shell.statusStyle = dark ? .lightContent : .darkContent
    paint()
  }

  static func haptic() {
    let generator = UIImpactFeedbackGenerator(style: .light)
    generator.prepare()
    generator.impactOccurred()
  }

  static func info() -> JsonObject {
    let bundle = Bundle.main.infoDictionary ?? [:]
    return [
      "version": bundle["CFBundleShortVersionString"] as? String ?? "",
      "build": bundle["CFBundleVersion"] as? String ?? "",
      // Installed from the App Store or TestFlight; there's no other way in.
      "installerPackage": NSNull(),
      "deviceName": UIDevice.current.name,
    ]
  }

  private func paint() {
    let colour = Palette.current.surface
    if let webView = webView {
      webView.backgroundColor = colour
      webView.scrollView.backgroundColor = colour
      webView.superview?.backgroundColor = colour
      webView.window?.backgroundColor = colour
    }
    controller?.setNeedsStatusBarAppearanceUpdate()
  }

  private func call(_ name: String, _ on: Bool) {
    guard let controller = controller else { return }
    let selector = NSSelectorFromString(name)
    guard controller.responds(to: selector), let method = controller.method(for: selector) else {
      return
    }
    typealias Setter = @convention(c) (AnyObject, Selector, Bool) -> Void
    unsafeBitCast(method, to: Setter.self)(controller, selector, on)
  }

  /// tao's controller says nothing of the status bar's style, so it's given
  /// the app's: light text on the dark theme, dark on the light.
  private static func install(on controller: UIViewController) {
    guard !installed else { return }
    installed = true
    let selector = #selector(getter: UIViewController.preferredStatusBarStyle)
    guard let method = class_getInstanceMethod(UIViewController.self, selector) else { return }
    let style: @convention(block) (AnyObject) -> Int = { _ in Shell.statusStyle.rawValue }
    class_replaceMethod(
      type(of: controller), selector, imp_implementationWithBlock(style),
      method_getTypeEncoding(method))
  }

  private func keyboardMoved(_ note: Notification) {
    guard let webView = webView, let host = webView.superview, let screen = host.window?.screen,
      let end = (note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? NSValue)?.cgRectValue
    else { return }
    let frame = host.convert(end, from: screen.coordinateSpace)
    fit(max(0, min(host.bounds.maxY - frame.minY, host.bounds.height / 2 + 200)))
  }

  /// Gives the page the space above a keyboard `overlap` points tall.
  private func fit(_ overlap: CGFloat) {
    guard let webView = webView, let host = webView.superview else { return }
    guard abs(overlap - keyboard) >= 0.5 else { return }
    keyboard = overlap
    var bounds = host.bounds
    bounds.size.height -= overlap
    webView.frame = bounds
    if overlap > 0 {
      // The keyboard covers the home indicator, so the page keeps no room for it.
      webView.evaluateJavaScript(
        "document.documentElement.style.setProperty('--safe-bottom','0px');"
          + "requestAnimationFrame(()=>requestAnimationFrame(()=>{const e=document.activeElement;"
          + "if(e&&(e.tagName==='INPUT'||e.tagName==='TEXTAREA'||e.isContentEditable))"
          + "e.scrollIntoView({block:'center',inline:'nearest'});}));")
    } else {
      webView.evaluateJavaScript("document.documentElement.style.removeProperty('--safe-bottom');")
    }
  }
}
