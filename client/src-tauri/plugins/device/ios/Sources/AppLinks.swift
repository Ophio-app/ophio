// SPDX-License-Identifier: Apache-2.0
//
// Links that open an object in the app. Identifiers carry no authorization.
// Pairing links aren't taken here: on the iPhone a computer is paired by
// scanning its code or pasting its link.

import Foundation

final class AppLinks {
  private static let prefixes = [
    "ophio-object://open/v1/",
    // Links made before the product was named, which may sit in notes and chats.
    "companion-object://open/v1/",
  ]
  /// A contribution handoff carries its whole package in the link, so it may run longer.
  private static let contributionPrefix = "ophio-contribution://open/v1?"
  private var pending: [String] = []

  /// On the main thread.
  func accept(_ url: String) {
    let contribution = url.hasPrefix(AppLinks.contributionPrefix)
    guard url.utf8.count <= (contribution ? 96 * 1024 : 4096),
      contribution || AppLinks.prefixes.contains(where: { url.hasPrefix($0) })
    else { return }
    if pending.count >= 8 { pending.removeFirst() }
    pending.append(url)
  }

  func drain() -> [String] {
    defer { pending.removeAll() }
    return pending
  }
}
