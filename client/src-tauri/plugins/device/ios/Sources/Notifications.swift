// SPDX-License-Identifier: Apache-2.0
//
// Task alerts, raised by the app itself from what the computer sends while
// it's open. There's no push service, so iOS shows them only while the app
// runs; tapping one opens its route. With app lock on, an alert says only
// "Open the app to review." and a project's alert doesn't name it.

import Foundation
import Tauri
import UserNotifications

final class Notifications: NSObject, UNUserNotificationCenterDelegate {
  static let locked = "Open the app to review."
  /// A project's alert carries this in its id, and names the project in its title.
  private static let folded = "/project:"

  private let center = UNUserNotificationCenter.current()
  private let lock = NSLock()
  private var opened: JsonObject?
  /// A tapped alert is waiting for the app; called on the main thread.
  var onOpen: (() -> Void)?

  override init() {
    super.init()
    center.delegate = self
  }

  func state(_ done: @escaping (String) -> Void) {
    center.getNotificationSettings { settings in
      switch settings.authorizationStatus {
      case .authorized, .provisional, .ephemeral: done("granted")
      case .notDetermined: done("promptable")
      default: done("blocked")
      }
    }
  }

  func request(_ done: @escaping () -> Void) {
    center.requestAuthorization(options: [.alert, .sound, .badge]) { _, _ in done() }
  }

  func show(
    id: String, title: String, body: String, category: String?, group: String?,
    groupTitle: String?, route: Any?, locked: Bool, silent: Bool = false,
    done: @escaping (Bool) -> Void
  ) {
    guard !id.isEmpty, id.count <= 256, title.count <= 256, body.count <= 2048,
      (group?.count ?? 0) <= 256, (groupTitle?.count ?? 0) <= 256
    else { return done(false) }
    let content = UNMutableNotificationContent()
    let named = id.contains(Notifications.folded)
    content.title = named && locked ? "Øphio" : title
    content.body = locked ? Notifications.locked : body
    // Silent: in Notification Center only, with no sound or banner.
    content.sound = silent ? nil : .default
    content.interruptionLevel = silent ? .passive : .active
    content.categoryIdentifier = category == "finished" ? "finished" : "needs-you"
    if let group = group { content.threadIdentifier = group }
    var info: [String: Any] = ["id": id, "named": named]
    if let route = route,
      let data = try? JSONSerialization.data(withJSONObject: route, options: [.fragmentsAllowed]),
      let text = String(data: data, encoding: .utf8)
    {
      info["route"] = text
    }
    content.userInfo = info
    // The same id replaces the alert it was raised with before.
    center.add(UNNotificationRequest(identifier: id, content: content, trigger: nil)) { error in
      done(error == nil)
    }
  }

  func cancel(_ ids: [String]) {
    guard !ids.isEmpty else { return }
    center.removeDeliveredNotifications(withIdentifiers: ids)
    center.removePendingNotificationRequests(withIdentifiers: ids)
  }

  /// App lock was turned on: alerts already shown lose their words.
  func redact() {
    center.getDeliveredNotifications { delivered in
      for note in delivered {
        let old = note.request.content
        let content = UNMutableNotificationContent()
        let named = old.userInfo["named"] as? Bool ?? false
        content.title = named ? "Øphio" : old.title
        content.body = Notifications.locked
        content.categoryIdentifier = old.categoryIdentifier
        content.threadIdentifier = old.threadIdentifier
        content.userInfo = old.userInfo
        content.interruptionLevel = .passive
        self.center.add(
          UNNotificationRequest(identifier: note.request.identifier, content: content, trigger: nil))
      }
    }
  }

  /// The alert tapped last, `{id, route}`, once.
  func takeOpen() -> JsonObject? {
    lock.sync {
      defer { opened = nil }
      return opened
    }
  }

  func userNotificationCenter(
    _ center: UNUserNotificationCenter, willPresent notification: UNNotification,
    withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
  ) {
    completionHandler([.banner, .list, .sound])
  }

  func userNotificationCenter(
    _ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
    withCompletionHandler completionHandler: @escaping () -> Void
  ) {
    let info = response.notification.request.content.userInfo
    if response.actionIdentifier == UNNotificationDefaultActionIdentifier,
      let id = info["id"] as? String
    {
      var route: Any = NSNull()
      if let text = info["route"] as? String, let data = text.data(using: .utf8),
        let value = try? JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
      {
        route = value
      }
      lock.sync { opened = ["id": id, "route": route] }
      onMain { self.onOpen?() }
    }
    completionHandler()
  }
}
