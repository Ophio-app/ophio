// SPDX-License-Identifier: Apache-2.0
//
// The microphone, as on Android: 16 kHz mono 16-bit PCM in 2048-byte chunks
// the app reads one at a time. Recording is only ever for one link at once,
// only in the foreground, and stops for good when the app leaves the screen
// or another app takes the audio session.

import AVFoundation
import Foundation
import Tauri

final class Microphone {
  enum Permission { case granted, denied, undetermined }

  private static let chunkBytes = 2048
  /// Chunks waiting for the app; more means the connection fell behind.
  private static let queueLimit = 16

  private final class Recording {
    let id: Int64
    let engine = AVAudioEngine()
    var chunks: [Data] = []
    var pending = Data()
    var error: String?
    var running = true
    var closed = false
    init(id: Int64) { self.id = id }
  }

  private let condition = NSCondition()
  private var generation: Int64 = 0
  private var recording: Recording?

  static func permission() -> Permission {
    if #available(iOS 17, *) {
      switch AVAudioApplication.shared.recordPermission {
      case .granted: return .granted
      case .denied: return .denied
      default: return .undetermined
      }
    }
    switch AVAudioSession.sharedInstance().recordPermission {
    case .granted: return .granted
    case .denied: return .denied
    default: return .undetermined
    }
  }

  static func request(_ done: @escaping (Bool) -> Void) {
    if #available(iOS 17, *) {
      AVAudioApplication.requestRecordPermission(completionHandler: done)
    } else {
      AVAudioSession.sharedInstance().requestRecordPermission(done)
    }
  }

  /// A new request to record; any earlier one still waiting is cancelled.
  func prepare() -> Int64 {
    condition.lock()
    defer { condition.unlock() }
    generation += 1
    return generation
  }

  func cancelPending() {
    condition.lock()
    generation += 1
    condition.unlock()
  }

  func start(_ id: Int64, expected: Int64) throws {
    condition.lock()
    defer { condition.unlock() }
    guard generation == expected else { throw Failure("This recording request was cancelled.") }
    guard recording == nil else { throw Failure("The microphone is already in use.") }
    let rec = Recording(id: id)
    try open(rec)
    recording = rec
  }

  private func open(_ rec: Recording) throws {
    let audio = AVAudioSession.sharedInstance()
    try audio.setCategory(.record, mode: .measurement, options: [])
    try audio.setActive(true)
    let input = rec.engine.inputNode
    let format = input.outputFormat(forBus: 0)
    guard format.sampleRate > 0, format.channelCount > 0,
      let target = AVAudioFormat(
        commonFormat: .pcmFormatInt16, sampleRate: 16_000, channels: 1, interleaved: true),
      let converter = AVAudioConverter(from: format, to: target)
    else {
      try? audio.setActive(false, options: .notifyOthersOnDeactivation)
      throw Failure("The microphone has no input.")
    }
    input.installTap(onBus: 0, bufferSize: 4096, format: format) { [weak self, weak rec] buffer, _ in
      guard let self = self, let rec = rec else { return }
      self.receive(buffer, converter, target, rec)
    }
    rec.engine.prepare()
    do {
      try rec.engine.start()
    } catch {
      input.removeTap(onBus: 0)
      try? audio.setActive(false, options: .notifyOthersOnDeactivation)
      throw error
    }
  }

  /// On the audio thread: converts one buffer and queues whole chunks.
  private func receive(
    _ buffer: AVAudioPCMBuffer, _ converter: AVAudioConverter, _ target: AVAudioFormat,
    _ rec: Recording
  ) {
    let ratio = target.sampleRate / buffer.format.sampleRate
    let capacity = AVAudioFrameCount(Double(buffer.frameLength) * ratio) + 64
    guard let out = AVAudioPCMBuffer(pcmFormat: target, frameCapacity: capacity) else { return }
    var fed = false
    var error: NSError?
    let status = converter.convert(to: out, error: &error) { _, state in
      if fed {
        state.pointee = .noDataNow
        return nil
      }
      fed = true
      state.pointee = .haveData
      return buffer
    }
    guard status != .error, let samples = out.int16ChannelData, out.frameLength > 0 else { return }
    let bytes = Data(bytes: samples[0], count: Int(out.frameLength) * 2)
    var overflow = false
    condition.lock()
    if recording === rec, rec.running, rec.error == nil {
      rec.pending.append(bytes)
      while rec.pending.count >= Microphone.chunkBytes {
        rec.chunks.append(Data(rec.pending.prefix(Microphone.chunkBytes)))
        rec.pending = Data(rec.pending.dropFirst(Microphone.chunkBytes))
      }
      if rec.chunks.count > Microphone.queueLimit {
        rec.error = "Recording stopped because the connection could not keep up."
        rec.running = false
        rec.chunks.removeAll()
        rec.pending.removeAll()
        overflow = true
      }
      condition.broadcast()
    }
    condition.unlock()
    // Never stop the engine from its own thread.
    if overflow { DispatchQueue.global(qos: .userInitiated).async { self.close(rec) } }
  }

  func read(_ id: Int64) -> JsonObject {
    condition.lock()
    defer { condition.unlock() }
    guard let rec = recording else { return ["error": "Recording ended."] }
    guard rec.id == id else { return ["error": "This recording has ended."] }
    if let error = rec.error { return ["error": error] }
    if rec.chunks.isEmpty { _ = condition.wait(until: Date().addingTimeInterval(0.1)) }
    guard recording === rec else { return ["error": "Recording ended."] }
    if let error = rec.error { return ["error": error] }
    guard !rec.chunks.isEmpty else { return ["rms": 0.0] }
    let chunk = rec.chunks.removeFirst()
    return ["pcm": Base64URL.encode(chunk), "rms": Microphone.rms(chunk)]
  }

  /// Ends the recording; with `drain`, what was still queued comes back.
  func stop(_ id: Int64, drain: Bool) -> Data {
    condition.lock()
    generation += 1
    guard let rec = recording, rec.id == id else {
      condition.unlock()
      return Data()
    }
    condition.unlock()
    close(rec)
    condition.lock()
    defer { condition.unlock() }
    rec.running = false
    if recording === rec { recording = nil }
    var out = Data()
    if drain && rec.error == nil {
      for chunk in rec.chunks { out.append(chunk) }
      out.append(rec.pending)
    }
    rec.chunks.removeAll()
    rec.pending.removeAll()
    condition.broadcast()
    return out
  }

  /// Stops recording and says why on the next read; the app then stops it.
  func interrupt(_ reason: String) {
    condition.lock()
    generation += 1
    guard let rec = recording else {
      condition.unlock()
      return
    }
    if rec.error == nil { rec.error = reason }
    rec.running = false
    rec.chunks.removeAll()
    rec.pending.removeAll()
    condition.broadcast()
    condition.unlock()
    close(rec)
  }

  private func close(_ rec: Recording) {
    condition.lock()
    let already = rec.closed
    rec.closed = true
    condition.unlock()
    if already { return }
    rec.engine.inputNode.removeTap(onBus: 0)
    rec.engine.stop()
    try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
  }

  private static func rms(_ chunk: Data) -> Double {
    let count = chunk.count / 2
    var sum = 0.0
    var index = chunk.startIndex
    for _ in 0..<count {
      let low = UInt16(chunk[index])
      let high = UInt16(chunk[chunk.index(after: index)])
      let sample = Double(Int16(bitPattern: low | high << 8)) / 32768
      sum += sample * sample
      index = chunk.index(index, offsetBy: 2)
    }
    return (sum / Double(max(count, 1))).squareRoot()
  }
}
