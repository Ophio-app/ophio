// SPDX-License-Identifier: Apache-2.0
//
// Files the app saves from the computer. Each is staged in the app's caches
// as it arrives and moves into the app's own folder in Files, which the
// person can open from On My iPhone › Øphio, only once all of it has.

import Foundation

final class Downloads {
  private static let maxSize: Int64 = 1 << 30
  private static let maxChunk = 256 * 1024
  private static let maxActive = 4

  private final class Job {
    let name: String
    let size: Int64
    let file: URL
    let handle: FileHandle
    var written: Int64 = 0
    init(name: String, size: Int64, file: URL, handle: FileHandle) {
      self.name = name
      self.size = size
      self.file = file
      self.handle = handle
    }
  }

  private let lock = NSLock()
  private var jobs: [String: Job] = [:]
  private let staging: URL
  private let files = FileManager.default

  init() {
    staging = files.urls(for: .cachesDirectory, in: .userDomainMask)[0]
      .appendingPathComponent("downloads-staging", isDirectory: true)
    // Whatever was staged before the app last ended is incomplete.
    try? files.removeItem(at: staging)
  }

  static func valid(_ name: String) -> Bool {
    !name.trimmingCharacters(in: .whitespaces).isEmpty && name.utf16.count <= 255
      && name != "." && name != ".."
      && !name.unicodeScalars.contains { $0 == "/" || $0 == "\\" || $0.value < 32 || $0.value == 127 }
  }

  func begin(name: String, mime: String, size: Int64) throws -> String {
    guard Downloads.valid(name), size >= 0, size <= Downloads.maxSize else {
      throw Failure("Invalid download.")
    }
    return try lock.sync {
      guard jobs.count < Downloads.maxActive else { throw Failure("Too many downloads.") }
      try files.createDirectory(at: staging, withIntermediateDirectories: true)
      let id = newId()
      let file = staging.appendingPathComponent(id)
      guard files.createFile(atPath: file.path, contents: nil) else {
        throw Failure("The download could not be staged.")
      }
      do {
        jobs[id] = Job(name: name, size: size, file: file, handle: try FileHandle(forWritingTo: file))
      } catch {
        try? files.removeItem(at: file)
        throw error
      }
      return id
    }
  }

  func write(_ id: String, _ bytes: Data) throws {
    try lock.sync {
      guard let job = jobs[id] else { throw Failure("No such download.") }
      guard bytes.count <= Downloads.maxChunk, job.written + Int64(bytes.count) <= job.size else {
        discard(id)
        throw Failure("The download is larger than it said.")
      }
      do {
        try job.handle.write(contentsOf: bytes)
      } catch {
        discard(id)
        throw error
      }
      job.written += Int64(bytes.count)
    }
  }

  /// Where the person finds it, as the app shows it.
  func finish(_ id: String) throws -> String {
    try lock.sync {
      guard let job = jobs.removeValue(forKey: id) else { throw Failure("No such download.") }
      do {
        guard job.written == job.size else { throw Failure("The download is incomplete.") }
        try job.handle.synchronize()
        try job.handle.close()
        let folder = files.urls(for: .documentDirectory, in: .userDomainMask)[0]
        try files.createDirectory(at: folder, withIntermediateDirectories: true)
        let target = unique(in: folder, job.name)
        try files.moveItem(at: job.file, to: target)
        return "On My iPhone/Øphio/\(target.lastPathComponent)"
      } catch {
        try? job.handle.close()
        try? files.removeItem(at: job.file)
        throw error
      }
    }
  }

  func cancel(_ id: String) throws {
    try lock.sync {
      guard let job = jobs.removeValue(forKey: id) else { return }
      try? job.handle.close()
      if files.fileExists(atPath: job.file.path) { try files.removeItem(at: job.file) }
    }
  }

  /// Called with the lock held.
  private func discard(_ id: String) {
    guard let job = jobs.removeValue(forKey: id) else { return }
    try? job.handle.close()
    try? files.removeItem(at: job.file)
  }

  /// `name`, or "stem (2).ext" and so on when that's taken.
  private func unique(in folder: URL, _ name: String) -> URL {
    let stem = (name as NSString).deletingPathExtension
    let ext = (name as NSString).pathExtension
    var candidate = folder.appendingPathComponent(name)
    var n = 2
    while files.fileExists(atPath: candidate.path) {
      candidate = folder.appendingPathComponent(ext.isEmpty ? "\(stem) (\(n))" : "\(stem) (\(n)).\(ext)")
      n += 1
    }
    return candidate
  }
}
