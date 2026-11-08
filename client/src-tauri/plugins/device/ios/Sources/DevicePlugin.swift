// SPDX-License-Identifier: Apache-2.0
//
// The iPhone side of the device plugin: the same commands, payloads and
// messages as Android's DevicePlugin. Tauri calls every command on one serial
// queue, so screen work moves to the main thread and slow work (keys, files,
// the microphone, clips) to this plugin's own queue, as Android's `work` does.

import AVFoundation
import Foundation
import Tauri
import UIKit
import WebKit

private struct EventArgs: Decodable {
  let events: Channel
}

final class DevicePlugin: Plugin {
  private static let unsupported = "This isn’t available on iPhone."

  private let io = DispatchQueue(
    label: "app.ophio.device.io", qos: .userInitiated, attributes: .concurrent)
  private let shell = Shell()
  private let links = AppLinks()
  private let notifications = Notifications()
  private let microphone = Microphone()
  private let downloads = Downloads()
  private let clips = ClipTrimmer()
  private let previews = PreviewBrowser()
  private let reports = Reports()
  private var appLock: AppLock!

  /// Guards what other threads read: the app's event channel and whether it's on screen.
  private let state = NSLock()
  private var channel: Channel?
  private var shown = true

  // On the main thread only.
  private var loaded = false
  private var observers: [NSObjectProtocol] = []
  private var microphoneWaiting: [(invoke: Invoke, expected: Int64)] = []
  private var notificationPrompt = false
  private var scannerOpen = false

  override init() {
    super.init()
    appLock = AppLock(
      emit: { [weak self] event in self?.lockChanged(event) },
      turnedOn: { [weak self] in self?.notifications.redact() })
  }

  deinit {
    observers.forEach { NotificationCenter.default.removeObserver($0) }
  }

  private var foreground: Bool { state.sync { shown } }

  override func load(webview: WKWebView) {
    onMain {
      guard !self.loaded else { return }
      self.loaded = true
      self.state.sync { self.shown = UIApplication.shared.applicationState != .background }
      self.shell.attach(webview, self.manager.viewController)
      self.appLock.attach(webview)
      self.reports.attach(webview) { [weak self] in
        self?.foreground == true && self?.appLock.isLocked == false
      }
      self.notifications.onOpen = { [weak self] in self?.flushEvents() }
      let center = NotificationCenter.default
      let watch = { (name: Notification.Name, body: @escaping (Notification) -> Void) in
        self.observers.append(center.addObserver(forName: name, object: nil, queue: .main, using: body))
      }
      watch(UIApplication.didEnterBackgroundNotification) { [weak self] _ in self?.paused() }
      watch(UIApplication.willEnterForegroundNotification) { [weak self] _ in self?.resumed() }
      // Hidden from the app switcher whenever the app isn't active.
      watch(UIApplication.willResignActiveNotification) { [weak self] _ in self?.appLock.cover() }
      watch(UIApplication.didBecomeActiveNotification) { [weak self] _ in self?.appLock.uncover() }
      watch(AVAudioSession.interruptionNotification) { [weak self] note in
        let type = (note.userInfo?[AVAudioSessionInterruptionTypeKey] as? NSNumber)?.uintValue
        if type == AVAudioSession.InterruptionType.began.rawValue {
          self?.microphone.interrupt("The microphone was interrupted.")
        }
      }
    }
  }

  private func paused() {
    appLock.pause()
    state.sync { shown = false }
    microphone.interrupt("Recording stopped when the app left the foreground.")
    microphone.cancelPending()
    shell.apply(foreground: false)
    send(["type": "background"])
  }

  private func resumed() {
    appLock.resume()
    state.sync { shown = true }
    shell.apply(foreground: true)
    send(["type": "foreground"])
    let waiting = microphoneWaiting
    microphoneWaiting.removeAll()
    waiting.forEach { beginMic($0.invoke, $0.expected) }
  }

  private func send(_ event: JsonObject) {
    state.sync { channel }?.send(event)
  }

  /// On the main thread: links and a tapped alert the app hasn't had yet.
  private func flushEvents() {
    guard let target = state.sync({ channel }) else { return }
    for url in links.drain() {
      let event: JsonObject = ["type": "appLink", "url": url]
      target.send(event)
    }
    if let opened = notifications.takeOpen() {
      let event: JsonObject = ["type": "notificationOpen", "notification": opened]
      target.send(event)
    }
  }

  private func lockChanged(_ event: JsonObject) {
    if appLock.isLocked { previews.close() }
    send(event)
  }

  /// Runs `action` on the plugin's own queue; any failure answers `message`.
  private func work(_ invoke: Invoke, _ message: String, _ action: @escaping (Args) throws -> Reply) {
    io.async {
      do {
        try action(Args(invoke)).send(invoke)
      } catch {
        invoke.reject(message)
      }
    }
  }

  /// Runs `action` on the main thread with the command's arguments.
  private func screen(_ invoke: Invoke, _ action: @escaping (Args) -> Void) {
    guard let args = try? Args(invoke) else { return invoke.reject("Invalid arguments.") }
    onMain { action(args) }
  }

  // MARK: Events

  @objc func events(_ invoke: Invoke) {
    guard let args = try? invoke.parseArgs(EventArgs.self) else {
      return invoke.reject("Invalid arguments.")
    }
    onMain {
      self.state.sync { self.channel = args.events }
      self.shell.apply(foreground: self.foreground)
      self.flushEvents()
      invoke.resolve()
    }
  }

  /// Links the phone opened the app with, from the Rust shell.
  @objc func appLinks(_ invoke: Invoke) {
    screen(invoke) { args in
      for case let url as String in args.array("urls") { self.links.accept(url) }
      self.flushEvents()
      invoke.resolve()
    }
  }

  // MARK: Reports

  @objc func screenshotSupported(_ invoke: Invoke) {
    onMain { self.reports.supported(invoke) }
  }

  @objc func startScreenshotWatch(_ invoke: Invoke) {
    guard let args = try? invoke.parseArgs(EventArgs.self) else {
      return invoke.reject("Invalid arguments.")
    }
    onMain { self.reports.start(invoke, events: args.events) }
  }

  @objc func stopScreenshotWatch(_ invoke: Invoke) {
    onMain { self.reports.stop(invoke) }
  }

  @objc func takeNativeCrashes(_ invoke: Invoke) {
    work(invoke, "The phone could not read its crash diagnostics.") { _ in
      .object(try self.reports.takeCrashes())
    }
  }

  @objc func captureOwnView(_ invoke: Invoke) {
    onMain { self.reports.capture(invoke) }
  }

  // MARK: Keys

  @objc func keyCreate(_ invoke: Invoke) {
    work(invoke, "The phone could not create a protected device key.") { a in
      .object(["publicKey": try Keys.create(a.string("alias")).map { $0 as Any } ?? NSNull()])
    }
  }

  /// The iPhone has no Ed25519 key of its own to sign with; `keyCreate` says so.
  @objc func keySign(_ invoke: Invoke) {
    invoke.reject("The phone could not sign with its protected device key.")
  }

  @objc func keyWrap(_ invoke: Invoke) {
    work(invoke, "The phone could not protect the device key.") { a in
      .object(["sealed": try Keys.wrap(a.string("alias"), Base64URL.decode(a.string("seed")))])
    }
  }

  @objc func keyUnwrap(_ invoke: Invoke) {
    work(invoke, "The protected device key is unavailable. Pair again on the computer.") { a in
      var seed = try Keys.unwrap(a.string("alias"), a.string("sealed"))
      defer { seed.resetBytes(in: 0..<seed.count) }
      return .object(["seed": Base64URL.encode(seed)])
    }
  }

  @objc func keyDelete(_ invoke: Invoke) {
    work(invoke, "The phone could not remove its device key.") { a in
      try Keys.delete(a.string("alias"))
      return .none
    }
  }

  // MARK: Microphone

  @objc func micStart(_ invoke: Invoke) {
    onMain {
      guard self.foreground else { return invoke.reject("Open the app before recording.") }
      let expected = self.microphone.prepare()
      let denied = "Microphone permission was denied. Typing is still available."
      switch Microphone.permission() {
      case .granted: self.beginMic(invoke, expected)
      case .denied: invoke.reject(denied)
      case .undetermined:
        Microphone.request { granted in
          onMain {
            guard granted else { return invoke.reject(denied) }
            if self.foreground {
              self.beginMic(invoke, expected)
            } else {
              self.microphoneWaiting.append((invoke, expected))
            }
          }
        }
      }
    }
  }

  private func beginMic(_ invoke: Invoke, _ expected: Int64) {
    work(
      invoke,
      "The microphone could not start. Check its permission and whether another app is recording."
    ) { a in
      guard self.foreground else { throw Failure("Recording cancelled.") }
      try self.microphone.start(a.int64("linkId"), expected: expected)
      return .none
    }
  }

  @objc func micRead(_ invoke: Invoke) {
    work(invoke, "The microphone was interrupted.") { a in
      .object(self.microphone.read(try a.int64("linkId")))
    }
  }

  @objc func micStop(_ invoke: Invoke) {
    work(invoke, "The microphone could not finish recording.") { a in
      let pcm = self.microphone.stop(try a.int64("linkId"), drain: a.bool("drain"))
      return .object(["pcm": Base64URL.encode(pcm)])
    }
  }

  // MARK: Sharing and downloads

  @objc func shareText(_ invoke: Invoke) {
    screen(invoke) { args in
      guard let text = try? args.string("text"),
        let host = topController(self.manager.viewController)
      else { return invoke.reject("The phone could not open the share sheet.") }
      let sheet = UIActivityViewController(activityItems: [text], applicationActivities: nil)
      if let popover = sheet.popoverPresentationController {
        popover.sourceView = host.view
        popover.sourceRect = CGRect(x: host.view.bounds.midX, y: host.view.bounds.maxY, width: 0, height: 0)
        popover.permittedArrowDirections = []
      }
      host.present(sheet, animated: true)
      invoke.resolve()
    }
  }

  @objc func saveBegin(_ invoke: Invoke) {
    work(invoke, "The phone could not create this download. Check its name and available storage.") {
      a in
      .object([
        "id": try self.downloads.begin(
          name: a.string("name"), mime: a.string("mime"), size: a.int64("size"))
      ])
    }
  }

  @objc func saveChunk(_ invoke: Invoke) {
    work(invoke, "The phone could not write this download.") { a in
      try self.downloads.write(a.string("id"), Base64URL.decode(a.string("bytes")))
      return .none
    }
  }

  @objc func saveFinish(_ invoke: Invoke) {
    work(invoke, "The download is incomplete or could not be saved.") { a in
      .object(["savedAs": try self.downloads.finish(a.string("id"))])
    }
  }

  @objc func saveCancel(_ invoke: Invoke) {
    work(invoke, "The phone could not remove the incomplete download.") { a in
      try self.downloads.cancel(a.string("id"))
      return .none
    }
  }

  // Shared files, app updates and the alert service are Android's alone: the
  // App Store updates the app, and iOS has no background service for alerts.
  @objc func sharedRead(_ invoke: Invoke) { invoke.reject(DevicePlugin.unsupported) }
  @objc func sharedAck(_ invoke: Invoke) { invoke.reject(DevicePlugin.unsupported) }
  @objc func appUpdateCheck(_ invoke: Invoke) { invoke.reject(DevicePlugin.unsupported) }
  @objc func appUpdateBegin(_ invoke: Invoke) { invoke.reject(DevicePlugin.unsupported) }
  @objc func appUpdateChunk(_ invoke: Invoke) { invoke.reject(DevicePlugin.unsupported) }
  @objc func appUpdateCommit(_ invoke: Invoke) { invoke.reject(DevicePlugin.unsupported) }
  @objc func appUpdateCancel(_ invoke: Invoke) { invoke.reject(DevicePlugin.unsupported) }
  @objc func alertsService(_ invoke: Invoke) { invoke.reject(DevicePlugin.unsupported) }
  @objc func alertsInfo(_ invoke: Invoke) { invoke.reject(DevicePlugin.unsupported) }
  @objc func alertsBattery(_ invoke: Invoke) { invoke.reject(DevicePlugin.unsupported) }
  @objc func alertsChannel(_ invoke: Invoke) { invoke.reject(DevicePlugin.unsupported) }

  // MARK: Task alerts

  @objc func notify(_ invoke: Invoke) {
    let cannot = "The phone could not show this notification."
    guard let a = try? Args(invoke), let id = try? a.string("id"), let title = try? a.string("title"),
      let body = try? a.string("body")
    else { return invoke.reject(cannot) }
    notifications.state { state in
      guard state == "granted" else { return invoke.reject("Task alerts are off.") }
      self.notifications.show(
        id: id, title: title, body: body, category: a.optString("category"),
        group: a.optString("group"), groupTitle: a.optString("groupTitle"), route: a.raw("route"),
        locked: AppLock.enabledNow, silent: a.bool("silent")
      ) { shown in shown ? invoke.resolve() : invoke.reject(cannot) }
    }
  }

  @objc func cancelNotifications(_ invoke: Invoke) {
    let ids = ((try? Args(invoke))?.array("ids") ?? []).compactMap { $0 as? String }
      .filter { !$0.isEmpty }
    notifications.cancel(ids)
    invoke.resolve()
  }

  @objc func notificationState(_ invoke: Invoke) {
    notifications.state { invoke.resolve($0) }
  }

  @objc func notificationSettings(_ invoke: Invoke) {
    onMain {
      guard self.foreground else {
        return invoke.reject("Open the app before changing task alerts.")
      }
      guard let url = URL(string: UIApplication.openNotificationSettingsURLString) else {
        return invoke.reject("iPhone Settings could not be opened.")
      }
      UIApplication.shared.open(url) { opened in
        opened ? invoke.resolve() : invoke.reject("iPhone Settings could not be opened.")
      }
    }
  }

  @objc func requestNotifications(_ invoke: Invoke) {
    onMain {
      guard self.foreground, !self.notificationPrompt else {
        return invoke.reject("Open the app and finish the current permission request first.")
      }
      self.notificationPrompt = true
      self.notifications.state { state in
        onMain {
          switch state {
          case "granted":
            self.notificationPrompt = false
            invoke.resolve()
          case "blocked":
            self.notificationPrompt = false
            invoke.reject("Task alerts are blocked in iPhone Settings.")
          default:
            self.askForAlerts(invoke)
          }
        }
      }
    }
  }

  /// Says why before iOS asks, as on Android; iOS asks only once.
  private func askForAlerts(_ invoke: Invoke) {
    let declined = {
      self.notificationPrompt = false
      invoke.reject("Notifications were not enabled.")
    }
    guard let host = topController(manager.viewController) else { return declined() }
    let alert = UIAlertController(
      title: "Allow task alerts?",
      message:
        "The app alerts you when a task needs you or finishes. Alerts come straight from your computer; no push service is used.",
      preferredStyle: .alert)
    alert.addAction(UIAlertAction(title: "Later", style: .cancel) { _ in declined() })
    alert.addAction(
      UIAlertAction(title: "Continue", style: .default) { _ in
        // A declined prompt is a permission state, not a delivery error.
        self.notifications.request {
          onMain {
            self.notificationPrompt = false
            invoke.resolve()
          }
        }
      })
    alert.preferredAction = alert.actions.last
    host.present(alert, animated: true)
  }

  // MARK: The app's frame

  @objc func keepAwake(_ invoke: Invoke) {
    screen(invoke) { args in
      self.shell.keepAwake(args.bool("on"), foreground: self.foreground)
      invoke.resolve()
    }
  }

  @objc func immersive(_ invoke: Invoke) {
    screen(invoke) { args in
      self.shell.setImmersive(args.bool("on"))
      invoke.resolve()
    }
  }

  @objc func theme(_ invoke: Invoke) {
    screen(invoke) { args in
      self.shell.theme(dark: args.bool("dark"))
      invoke.resolve()
    }
  }

  @objc func haptic(_ invoke: Invoke) {
    onMain {
      Shell.haptic()
      invoke.resolve()
    }
  }

  @objc func info(_ invoke: Invoke) {
    onMain { invoke.resolve(Shell.info()) }
  }

  // MARK: Pairing scanner

  @objc func scan(_ invoke: Invoke) {
    onMain {
      guard !self.scannerOpen else { return invoke.reject("The camera scanner is already open.") }
      self.scannerOpen = true
      switch AVCaptureDevice.authorizationStatus(for: .video) {
      case .authorized: self.startScanner(invoke)
      case .notDetermined:
        AVCaptureDevice.requestAccess(for: .video) { granted in
          onMain { granted ? self.startScanner(invoke) : self.cameraDenied(invoke) }
        }
      default: self.cameraDenied(invoke)
      }
    }
  }

  private func cameraDenied(_ invoke: Invoke) {
    scannerOpen = false
    invoke.reject("Camera permission was denied. Paste the pairing link instead.")
  }

  private func startScanner(_ invoke: Invoke) {
    guard let host = topController(manager.viewController) else {
      scannerOpen = false
      return invoke.reject(Scanner.failed)
    }
    let scanner = Scanner { result in
      self.scannerOpen = false
      switch result {
      case .success(let content?): invoke.resolve(content)
      case .success(nil): invoke.resolve()
      case .failure(let failure): invoke.reject(failure.message)
      }
    }
    host.present(scanner, animated: true)
  }

  // MARK: App lock

  @objc func lockState(_ invoke: Invoke) {
    onMain { invoke.resolve(self.appLock.state()) }
  }

  @objc func lockConfigure(_ invoke: Invoke) {
    screen(invoke) { args in
      self.appLock.configure(
        invoke, enabled: args.bool("enabled"), minutes: (try? args.int64("relockMinutes")) ?? -1)
    }
  }

  @objc func unlock(_ invoke: Invoke) {
    onMain { self.appLock.unlock(invoke) }
  }

  @objc func confirm(_ invoke: Invoke) {
    screen(invoke) { args in
      self.appLock.confirm(invoke, title: args.optString("title") ?? "")
    }
  }

  @objc func lock(_ invoke: Invoke) {
    onMain { self.appLock.lock(invoke) }
  }

  @objc func lockScreenReady(_ invoke: Invoke) {
    onMain { self.appLock.screenReady(invoke) }
  }

  // MARK: Private previews

  @objc func previewShow(_ invoke: Invoke) {
    screen(invoke) { args in
      guard self.foreground, !self.appLock.isLocked else {
        return invoke.reject("Unlock the app before opening a preview.")
      }
      self.previews.show(invoke, args, from: self.manager.viewController)
    }
  }

  @objc func previewClose(_ invoke: Invoke) {
    previews.close((try? Args(invoke))?.optString("id"))
    invoke.resolve()
  }

  @objc func previewPending(_ invoke: Invoke) {
    work(invoke, "Saved preview pictures could not be read.") { a in
      .object([
        "pictures": self.previews.pending(
          profileId: try a.string("profileId"), previewId: try a.string("previewId"))
      ])
    }
  }

  @objc func previewPictureRead(_ invoke: Invoke) {
    work(invoke, "The preview picture is unavailable.") { a in
      .object([
        "bytes": Base64URL.encode(try self.previews.read(a.string("id"), offset: a.int64("offset")))
      ])
    }
  }

  @objc func previewPictureAck(_ invoke: Invoke) {
    work(invoke, "The preview picture could not be removed.") { a in
      try self.previews.acknowledge(a.string("id"))
      return .none
    }
  }

  // MARK: Clips

  /// Clips are prepared only while the app is open and unlocked. The clip's
  /// own refusals reach the app as written.
  private func clipWork(_ invoke: Invoke, _ action: @escaping (Args) throws -> Reply) {
    guard foreground, !appLock.isLocked else {
      return invoke.reject("Unlock the app before preparing a clip.")
    }
    io.async {
      do {
        try action(Args(invoke)).send(invoke)
      } catch let failure as Failure {
        invoke.reject(failure.message)
      } catch {
        invoke.reject("The clip could not be prepared. Check available device storage.")
      }
    }
  }

  @objc func clipBegin(_ invoke: Invoke) {
    clipWork(invoke) { a in .text(try self.clips.begin(size: a.int64("size"), hash: a.string("sha256"))) }
  }

  @objc func clipWrite(_ invoke: Invoke) {
    clipWork(invoke) { a in
      try self.clips.write(a.string("id"), Base64URL.decode(a.string("bytes")))
      return .none
    }
  }

  @objc func clipTrim(_ invoke: Invoke) {
    guard foreground, !appLock.isLocked else {
      return invoke.reject("Unlock the app before preparing a clip.")
    }
    guard let a = try? Args(invoke), let id = try? a.string("id"),
      let startMs = try? a.int64("startMs"), let endMs = try? a.int64("endMs")
    else { return invoke.reject("The clip could not be prepared. Check available device storage.") }
    io.async {
      self.clips.trim(id, startMs: startMs, endMs: endMs) { result in
        switch result {
        case .success(let trimmed): invoke.resolve(trimmed)
        case .failure(let failure): invoke.reject(failure.message)
        }
      }
    }
  }

  @objc func clipRead(_ invoke: Invoke) {
    clipWork(invoke) { a in
      .object(["bytes": Base64URL.encode(try self.clips.read(a.string("id"), offset: a.int64("offset")))])
    }
  }

  @objc func clipDiscard(_ invoke: Invoke) {
    work(invoke, "The temporary clip could not be removed.") { a in
      self.clips.discard(try a.string("id"))
      return .none
    }
  }
}

@_cdecl("init_plugin_device")
func initPlugin() -> Plugin {
  DevicePlugin()
}
