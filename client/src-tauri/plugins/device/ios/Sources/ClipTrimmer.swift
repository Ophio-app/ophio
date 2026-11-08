// SPDX-License-Identifier: Apache-2.0
//
// Cuts up to a minute from a clip the person chose to send. Only app-private
// copies of what they picked pass through here; there's no capture and no
// photo library access. The cut keeps the clip's single video track as it
// was encoded, without re-encoding, as on Android.

import AVFoundation
import CryptoKit
import Foundation
import Tauri

final class ClipTrimmer {
  private static let maxBytes: Int64 = 128 * 1024 * 1024
  private static let chunk = 256 * 1024
  private static let codecs: Set<FourCharCode> = [
    kCMVideoCodecType_H264, kCMVideoCodecType_HEVC, kCMVideoCodecType_MPEG4Video,
  ]

  private final class Job {
    let input: URL
    let output: URL
    let size: Int64
    let hash: String
    var written: Int64 = 0
    var trimming = false
    var ready = false
    init(input: URL, output: URL, size: Int64, hash: String) {
      self.input = input
      self.output = output
      self.size = size
      self.hash = hash
    }
  }

  private let directory: URL
  private let lock = NSLock()
  private var jobs: [String: Job] = [:]

  init() {
    let caches = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
    directory = caches.appendingPathComponent("clip-exports", isDirectory: true)
    try? FileManager.default.removeItem(at: directory)
    try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
  }

  func begin(size: Int64, hash: String) throws -> String {
    try lock.sync {
      guard jobs.isEmpty else {
        throw Failure("Another clip is being prepared. Finish or cancel it first.")
      }
      guard (1...ClipTrimmer.maxBytes).contains(size), hash.count == 64,
        hash.allSatisfy({ ("0"..."9").contains($0) || ("a"..."f").contains($0) })
      else { throw Failure("Choose a clip smaller than 128 MB.") }
      let id = newId()
      let input = directory.appendingPathComponent("\(id).input")
      guard FileManager.default.createFile(atPath: input.path, contents: nil) else {
        throw Failure("The clip could not be staged on this device.")
      }
      jobs[id] = Job(
        input: input, output: directory.appendingPathComponent("\(id).mp4"), size: size, hash: hash)
      return id
    }
  }

  func write(_ id: String, _ bytes: Data) throws {
    try lock.sync {
      let job = try find(id)
      guard !job.ready, !job.trimming, !bytes.isEmpty, bytes.count <= ClipTrimmer.chunk,
        job.written + Int64(bytes.count) <= job.size
      else { throw Failure("The imported clip is incomplete or changed.") }
      let handle = try FileHandle(forWritingTo: job.input)
      defer { try? handle.close() }
      try handle.seekToEnd()
      try handle.write(contentsOf: bytes)
      job.written += Int64(bytes.count)
    }
  }

  /// Cuts `startMs` to `endMs` and calls `done` off the main thread.
  func trim(
    _ id: String, startMs: Int64, endMs: Int64, done: @escaping (Result<JsonObject, Failure>) -> Void
  ) {
    let job: Job
    do {
      job = try lock.sync { () -> Job in
        let job = try find(id)
        guard !job.ready, !job.trimming, job.written == job.size else {
          throw Failure("The imported clip changed. Import it again.")
        }
        job.trimming = true
        return job
      }
      guard try ClipTrimmer.digest(job.input) == job.hash else {
        throw Failure("The imported clip changed. Import it again.")
      }
      guard startMs >= 0, endMs > startMs, endMs - startMs <= 60_000 else {
        throw Failure("Choose a range of up to 60 seconds.")
      }
    } catch {
      lock.sync {
        if let staged = jobs[id] { staged.trimming = false }
      }
      return done(.failure(error as? Failure ?? Failure("The imported clip changed. Import it again.")))
    }
    Task {
      let result: Result<JsonObject, Failure>
      do {
        result = .success(try await cut(job, startMs: startMs, endMs: endMs))
      } catch {
        try? FileManager.default.removeItem(at: job.output)
        result = .failure(
          error as? Failure ?? Failure("iPhone can’t trim this clip. Use screenshots instead."))
      }
      let kept = lock.sync { () -> Bool in
        job.trimming = false
        if case .success = result { job.ready = true }
        return jobs[id] === job
      }
      // Discarded while it was cut: nothing is left behind.
      if !kept { try? FileManager.default.removeItem(at: job.output) }
      done(kept ? result : .failure(Failure("This clip is no longer staged. Try again.")))
    }
  }

  private func cut(_ job: Job, startMs: Int64, endMs: Int64) async throws -> JsonObject {
    let asset = AVURLAsset(url: job.input)
    let videos = try await asset.loadTracks(withMediaType: .video)
    guard videos.count == 1, let video = videos.first else {
      throw Failure("This clip needs a single video track. Use screenshots instead.")
    }
    let duration = try await asset.load(.duration)
    let start = CMTime(value: startMs, timescale: 1000)
    let end = CMTime(value: endMs, timescale: 1000)
    guard duration.isNumeric, end <= duration else {
      throw Failure("Choose a range inside this clip.")
    }
    let formats = try await video.load(.formatDescriptions)
    guard formats.count == 1, let format = formats.first,
      ClipTrimmer.codecs.contains(CMFormatDescriptionGetMediaSubType(format))
    else { throw Failure("iPhone can’t trim this format here. Use screenshots instead.") }
    // Only the video track goes: no sound, and nothing else the file held.
    let composition = AVMutableComposition()
    guard
      let track = composition.addMutableTrack(
        withMediaType: .video, preferredTrackID: kCMPersistentTrackID_Invalid)
    else { throw Failure("iPhone can’t trim this clip. Use screenshots instead.") }
    let range = CMTimeRange(start: start, end: end)
    try track.insertTimeRange(range, of: video, at: .zero)
    track.preferredTransform = try await video.load(.preferredTransform)
    guard
      let export = AVAssetExportSession(
        asset: composition, presetName: AVAssetExportPresetPassthrough)
    else { throw Failure("iPhone can’t trim this clip. Use screenshots instead.") }
    export.outputURL = job.output
    export.outputFileType = .mp4
    export.shouldOptimizeForNetworkUse = true
    export.fileLengthLimit = ClipTrimmer.maxBytes
    await withCheckedContinuation { (finished: CheckedContinuation<Void, Never>) in
      export.exportAsynchronously { finished.resume() }
    }
    guard export.status == .completed else {
      throw Failure("iPhone can’t trim this clip. Use screenshots instead.")
    }
    let size =
      (try? FileManager.default.attributesOfItem(atPath: job.output.path)[.size] as? NSNumber)?
      .int64Value ?? 0
    guard (1...ClipTrimmer.maxBytes).contains(size) else {
      throw Failure("The trimmed clip is too large. Choose a shorter range.")
    }
    let made = AVURLAsset(url: job.output)
    let tracks = try await made.loadTracks(withMediaType: .video)
    let length = try await made.load(.duration)
    guard tracks.count == 1, abs(length.seconds - range.duration.seconds) <= 0.1 else {
      throw Failure("iPhone couldn’t verify the trimmed duration. Use screenshots instead.")
    }
    return [
      "startMs": Double(startMs),
      "endMs": Double(startMs) + length.seconds * 1000,
      "size": size,
      "sha256": try ClipTrimmer.digest(job.output),
    ]
  }

  func read(_ id: String, offset: Int64) throws -> Data {
    try lock.sync {
      let job = try find(id)
      let size =
        (try? FileManager.default.attributesOfItem(atPath: job.output.path)[.size] as? NSNumber)?
        .int64Value ?? 0
      guard job.ready, offset >= 0, offset < size else {
        throw Failure("The trimmed clip is no longer available.")
      }
      let handle = try FileHandle(forReadingFrom: job.output)
      defer { try? handle.close() }
      try handle.seek(toOffset: UInt64(offset))
      return try handle.read(upToCount: min(ClipTrimmer.chunk, Int(size - offset))) ?? Data()
    }
  }

  func discard(_ id: String) {
    lock.sync {
      guard let job = jobs.removeValue(forKey: id) else { return }
      try? FileManager.default.removeItem(at: job.input)
      if !job.trimming { try? FileManager.default.removeItem(at: job.output) }
    }
  }

  private func find(_ id: String) throws -> Job {
    guard let job = jobs[id] else { throw Failure("This clip is no longer staged. Try again.") }
    return job
  }

  private static func digest(_ file: URL) throws -> String {
    let handle = try FileHandle(forReadingFrom: file)
    defer { try? handle.close() }
    var hash = SHA256()
    while let bytes = try handle.read(upToCount: chunk), !bytes.isEmpty { hash.update(data: bytes) }
    return hash.finalize().map { String(format: "%02x", $0) }.joined()
  }
}
