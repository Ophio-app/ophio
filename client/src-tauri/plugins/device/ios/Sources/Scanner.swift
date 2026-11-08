// SPDX-License-Identifier: Apache-2.0
//
// Scans a computer's pairing code. Camera frames are decoded in memory and
// never kept or uploaded. A terminal draws the code in its text colour, so on
// a dark theme it arrives light on dark; a frame that doesn't read as it is
// is tried inverted, as on Android.

import AVFoundation
import CoreImage
import UIKit
import Vision

final class Scanner: UIViewController, AVCaptureVideoDataOutputSampleBufferDelegate {
  static let failed = "The camera could not scan. Paste the pairing link instead."

  private let session = AVCaptureSession()
  private let frames = DispatchQueue(label: "app.ophio.pairing-scanner")
  private let done: (Result<String?, Failure>) -> Void
  private let lock = NSLock()
  private var ended = false
  private var preview: AVCaptureVideoPreviewLayer?
  private var errors: NSObjectProtocol?
  private var appeared = false
  /// A result that came before the scanner finished opening.
  private var waiting: Result<String?, Failure>?

  /// `done` runs once on the main thread after the scanner closes: the code's
  /// text, nil when the person cancelled, or why it couldn't scan.
  init(done: @escaping (Result<String?, Failure>) -> Void) {
    self.done = done
    super.init(nibName: nil, bundle: nil)
    modalPresentationStyle = .fullScreen
    isModalInPresentation = true
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

  deinit {
    if let errors = errors { NotificationCenter.default.removeObserver(errors) }
  }

  override var preferredStatusBarStyle: UIStatusBarStyle {
    Shell.dark ? .lightContent : .darkContent
  }

  override func viewDidLoad() {
    super.viewDidLoad()
    let palette = Palette.current
    view.backgroundColor = palette.surface
    let title = UILabel()
    title.text = "Scan the pairing code on your computer"
    title.font = .preferredFont(forTextStyle: .title2)
    title.adjustsFontForContentSizeCategory = true
    title.textColor = palette.ink
    title.numberOfLines = 0
    let camera = UIView()
    camera.backgroundColor = .black
    camera.clipsToBounds = true
    camera.layer.cornerRadius = 12
    let layer = AVCaptureVideoPreviewLayer(session: session)
    layer.videoGravity = .resizeAspectFill
    camera.layer.addSublayer(layer)
    preview = layer
    var style = UIButton.Configuration.bordered()
    style.title = "Cancel scan"
    style.baseForegroundColor = palette.ink
    style.baseBackgroundColor = palette.line
    let cancel = UIButton(configuration: style, primaryAction: UIAction { [weak self] _ in
      self?.finish(.success(nil))
    })
    let stack = UIStackView(arrangedSubviews: [title, camera, cancel])
    stack.axis = .vertical
    stack.spacing = 16
    stack.translatesAutoresizingMaskIntoConstraints = false
    view.addSubview(stack)
    let margins = view.safeAreaLayoutGuide
    NSLayoutConstraint.activate([
      stack.topAnchor.constraint(equalTo: margins.topAnchor, constant: 16),
      stack.bottomAnchor.constraint(equalTo: margins.bottomAnchor, constant: -16),
      stack.leadingAnchor.constraint(equalTo: margins.leadingAnchor, constant: 16),
      stack.trailingAnchor.constraint(equalTo: margins.trailingAnchor, constant: -16),
      cancel.heightAnchor.constraint(greaterThanOrEqualToConstant: 56),
    ])
    errors = NotificationCenter.default.addObserver(
      forName: .AVCaptureSessionRuntimeError, object: session, queue: .main
    ) { [weak self] _ in self?.finish(.failure(Failure(Scanner.failed))) }
    frames.async { [weak self] in self?.configure() }
  }

  override func viewDidLayoutSubviews() {
    super.viewDidLayoutSubviews()
    if let layer = preview, let host = layer.superlayer { layer.frame = host.bounds }
  }

  override func viewDidAppear(_ animated: Bool) {
    super.viewDidAppear(animated)
    appeared = true
    if let result = waiting {
      waiting = nil
      close(result)
    }
  }

  override func viewDidDisappear(_ animated: Bool) {
    super.viewDidDisappear(animated)
    let session = self.session
    frames.async { if session.isRunning { session.stopRunning() } }
  }

  private func configure() {
    do {
      guard
        let device = AVCaptureDevice.default(.builtInWideAngleCamera, for: .video, position: .back)
          ?? AVCaptureDevice.default(for: .video)
      else { throw Failure(Scanner.failed) }
      if (try? device.lockForConfiguration()) != nil {
        if device.isFocusModeSupported(.continuousAutoFocus) { device.focusMode = .continuousAutoFocus }
        device.unlockForConfiguration()
      }
      let input = try AVCaptureDeviceInput(device: device)
      let output = AVCaptureVideoDataOutput()
      output.alwaysDiscardsLateVideoFrames = true
      output.videoSettings = [
        kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
      ]
      output.setSampleBufferDelegate(self, queue: frames)
      session.beginConfiguration()
      if session.canSetSessionPreset(.hd1280x720) { session.sessionPreset = .hd1280x720 }
      guard session.canAddInput(input), session.canAddOutput(output) else {
        session.commitConfiguration()
        throw Failure(Scanner.failed)
      }
      session.addInput(input)
      session.addOutput(output)
      session.commitConfiguration()
      if !isEnded { session.startRunning() }
    } catch {
      DispatchQueue.main.async { self.finish(.failure(Failure(Scanner.failed))) }
    }
  }

  private var isEnded: Bool { lock.sync { ended } }

  func captureOutput(
    _ output: AVCaptureOutput, didOutput sample: CMSampleBuffer, from connection: AVCaptureConnection
  ) {
    guard !isEnded, let pixels = CMSampleBufferGetImageBuffer(sample) else { return }
    let frame = CIImage(cvPixelBuffer: pixels)
    for image in [frame, frame.applyingFilter("CIColorInvert")] {
      if let text = Scanner.read(image), text.utf16.count <= 16384 {
        DispatchQueue.main.async { self.finish(.success(text)) }
        return
      }
    }
  }

  private static func read(_ image: CIImage) -> String? {
    let request = VNDetectBarcodesRequest()
    request.symbologies = [.qr]
    do {
      try VNImageRequestHandler(ciImage: image, options: [:]).perform([request])
    } catch {
      return nil
    }
    return request.results?.lazy.compactMap { $0.payloadStringValue }.first
  }

  /// On the main thread; only the first call counts.
  private func finish(_ result: Result<String?, Failure>) {
    let first = lock.sync { () -> Bool in
      defer { ended = true }
      return !ended
    }
    guard first else { return }
    let session = self.session
    frames.async { if session.isRunning { session.stopRunning() } }
    // A scanner still opening can't close yet; it closes once it has opened.
    if appeared || presentingViewController == nil { close(result) } else { waiting = result }
  }

  private func close(_ result: Result<String?, Failure>) {
    if presentingViewController != nil {
      dismiss(animated: true) { self.done(result) }
    } else {
      done(result)
    }
  }
}
