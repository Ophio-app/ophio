// swift-tools-version:5.3
// SPDX-License-Identifier: Apache-2.0

import PackageDescription

let package = Package(
  name: "tauri-plugin-device",
  platforms: [
    .iOS("16.0")
  ],
  products: [
    .library(
      name: "tauri-plugin-device",
      type: .static,
      targets: ["tauri-plugin-device"])
  ],
  dependencies: [
    .package(name: "Tauri", path: "../.tauri/tauri-api")
  ],
  targets: [
    .target(
      name: "tauri-plugin-device",
      dependencies: [
        .byName(name: "Tauri")
      ],
      path: "Sources")
  ]
)
