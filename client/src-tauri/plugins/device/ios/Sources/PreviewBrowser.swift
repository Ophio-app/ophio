// SPDX-License-Identifier: Apache-2.0
//
// A private preview of what an agent is building, served by the computer and
// reached through the app's own loopback port. Its web view never receives
// Tauri scripts, script message handlers or device credentials, keeps nothing
// once closed, and loads nothing but that one port. Pictures the person takes
// wait in the app's own files until the app has sent them.

import Foundation
import Tauri
import UIKit
import WebKit

final class PreviewBrowser {
  private let root: URL
  private let files = NSLock()
  private weak var open: PreviewController?
  private var active: String?
  private var opening = false

  init() {
    let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
    var folder = support.appendingPathComponent("preview-pictures", isDirectory: true)
    try? FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    var values = URLResourceValues()
    values.isExcludedFromBackup = true
    try? folder.setResourceValues(values)
    root = folder
  }

  /// On the main thread.
  func show(_ invoke: Invoke, _ args: Args, from host: UIViewController?) {
    guard open == nil, !opening else { return invoke.reject("A preview is already open.") }
    let failed = "The private preview could not open."
    guard let raw = try? args.string("url"), let url = URL(string: raw), url.scheme == "http",
      url.host == "127.0.0.1", let port = url.port, port > 0, url.user == nil, url.password == nil,
      let id = try? args.string("id"), let profileId = try? args.string("profileId"),
      let previewId = try? args.string("previewId")
    else { return invoke.reject(failed) }
    let setup = PreviewController.Setup(
      url: url, port: port, profileId: profileId, previewId: previewId,
      taskId: args.optString("taskId") ?? "", asked: args.array("questions"),
      given: args.array("answers"))
    opening = true
    active = id
    // Everything but the preview's own port is refused before it's asked for.
    let rules = """
      [{"trigger":{"url-filter":".*"},"action":{"type":"block"}},
      {"trigger":{"url-filter":"^http://127\\\\.0\\\\.0\\\\.1:\(port)/"},"action":{"type":"ignore-previous-rules"}},
      {"trigger":{"url-filter":"^ws://127\\\\.0\\\\.0\\\\.1:\(port)/"},"action":{"type":"ignore-previous-rules"}},
      {"trigger":{"url-filter":"^data:"},"action":{"type":"ignore-previous-rules"}},
      {"trigger":{"url-filter":"^blob:"},"action":{"type":"ignore-previous-rules"}},
      {"trigger":{"url-filter":"^about:"},"action":{"type":"ignore-previous-rules"}}]
      """
    WKContentRuleListStore.default().compileContentRuleList(
      forIdentifier: "preview-\(port)", encodedContentRuleList: rules
    ) { list, _ in
      onMain {
        self.opening = false
        guard self.active == id, let list = list, let host = topController(host) else {
          if self.active == id { self.active = nil }
          return invoke.reject(failed)
        }
        let controller = PreviewController(setup, rules: list, root: self.root, files: self.files) {
          result in
          if self.active == id { self.active = nil }
          invoke.resolve(result)
        }
        self.open = controller
        host.present(controller, animated: true)
      }
    }
  }

  /// Closes the preview `id`, or whichever is open.
  func close(_ id: String? = nil) {
    onMain {
      guard id == nil || id == self.active else { return }
      if self.opening { self.active = nil }
      self.open?.close()
    }
  }

  private static func valid(_ id: String) -> Bool {
    id.count == 36 && id.allSatisfy { ("0"..."9").contains($0) || ("a"..."f").contains($0) || $0 == "-" }
  }

  func read(_ id: String, offset: Int64) throws -> Data {
    try files.sync {
      guard PreviewBrowser.valid(id), offset >= 0 else { throw Failure("Invalid picture.") }
      let handle = try FileHandle(forReadingFrom: root.appendingPathComponent("\(id).png"))
      defer { try? handle.close() }
      let size = Int64(try handle.seekToEnd())
      guard offset <= size else { throw Failure("Invalid offset.") }
      try handle.seek(toOffset: UInt64(offset))
      return try handle.read(upToCount: Int(min(256 * 1024, size - offset))) ?? Data()
    }
  }

  func pending(profileId: String, previewId: String) -> [[String: Any]] {
    files.sync {
      let names = (try? FileManager.default.contentsOfDirectory(atPath: root.path)) ?? []
      return names.filter { $0.hasSuffix(".json") }.compactMap { name -> [String: Any]? in
        guard let data = FileManager.default.contents(atPath: root.appendingPathComponent(name).path),
          let metadata = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
          let id = metadata["id"] as? String, PreviewBrowser.valid(id),
          metadata["profileId"] as? String == profileId,
          metadata["previewId"] as? String == previewId,
          let attributes = try? FileManager.default.attributesOfItem(
            atPath: root.appendingPathComponent("\(id).png").path),
          let size = attributes[.size] as? NSNumber
        else { return nil }
        return [
          "id": id,
          "size": size,
          "capturedAt": metadata["capturedAt"] ?? NSNull(),
          "taskId": metadata["taskId"] ?? NSNull(),
        ]
      }
    }
  }

  func acknowledge(_ id: String) throws {
    try files.sync {
      guard PreviewBrowser.valid(id) else { throw Failure("Invalid picture.") }
      let manager = FileManager.default
      let image = root.appendingPathComponent("\(id).png")
      if manager.fileExists(atPath: image.path) { try manager.removeItem(at: image) }
      try? manager.removeItem(at: root.appendingPathComponent("\(id).json"))
      try? manager.removeItem(at: root.appendingPathComponent("\(id).part"))
    }
  }
}

final class PreviewController: UIViewController, WKNavigationDelegate, WKUIDelegate,
  UITextFieldDelegate
{
  struct Setup {
    let url: URL
    let port: Int
    let profileId: String
    let previewId: String
    let taskId: String
    let asked: [Any]
    let given: [Any]
  }

  private struct Question {
    let header: String
    let text: String
    let multi: Bool
    let options: [(label: String, description: String)]
  }

  /// One question's answer in progress: chosen labels, then the person's own words.
  private final class Answer {
    var labels: [String]
    var own: String
    init(labels: [String], own: String) {
      self.labels = labels
      self.own = own
    }
  }

  private static let leaves = "This link leaves the private preview."
  private static let lost = "The preview connection was lost. Close and open it again."

  private let setup: Setup
  private let rules: WKContentRuleList
  private let root: URL
  private let files: NSLock
  private let done: (JsonObject) -> Void
  private let palette = Palette.current
  private let questions: [Question]
  private let answers: [Answer]
  private var pictures: [[String: Any]] = []
  private var annotate: String?
  private var submitted = false
  private var finished = false
  private var appeared = false
  private var waiting = false

  private var web: WKWebView!
  private let heading = UILabel()
  private let notice = UILabel()
  private let annotation = UIButton(type: .system)
  private let submit = UIButton(type: .system)
  private let asks = UILabel()
  private let answering = UIScrollView()
  private let panel = UIStackView()
  private var minimize: UIButton!
  private let bubble = UIButton(type: .custom)
  private let badge = UILabel()
  private var choices: [(question: Int, label: String, button: UIButton)] = []
  private var fields: [Int: UITextField] = [:]

  init(
    _ setup: Setup, rules: WKContentRuleList, root: URL, files: NSLock,
    done: @escaping (JsonObject) -> Void
  ) {
    self.setup = setup
    self.rules = rules
    self.root = root
    self.files = files
    self.done = done
    var questions: [Question] = []
    var answers: [Answer] = []
    for (i, item) in setup.asked.prefix(6).enumerated() {
      guard let asked = item as? [String: Any], let text = asked["question"] as? String else { break }
      let options = (asked["options"] as? [[String: Any]] ?? []).compactMap {
        option -> (label: String, description: String)? in
        guard let label = option["label"] as? String else { return nil }
        return (label, option["description"] as? String ?? "")
      }
      let question = Question(
        header: asked["header"] as? String ?? "", text: text,
        multi: asked["multi_select"] as? Bool ?? false, options: options)
      let given = i < setup.given.count ? setup.given[i] as? [String: Any] : nil
      let labels = (given?["labels"] as? [Any] ?? []).compactMap { $0 as? String }
        .filter { label in options.contains { $0.label == label } }
      questions.append(question)
      answers.append(Answer(labels: labels, own: String((given?["own"] as? String ?? "").prefix(4000))))
    }
    self.questions = questions
    self.answers = answers
    super.init(nibName: nil, bundle: nil)
    modalPresentationStyle = .fullScreen
    isModalInPresentation = true
    overrideUserInterfaceStyle = Shell.dark ? .dark : .light
  }

  @available(*, unavailable)
  required init?(coder: NSCoder) { fatalError("init(coder:) is not supported") }

  override var preferredStatusBarStyle: UIStatusBarStyle {
    Shell.dark ? .lightContent : .darkContent
  }

  override func viewDidLoad() {
    super.viewDidLoad()
    view.backgroundColor = palette.surface
    let config = WKWebViewConfiguration()
    config.websiteDataStore = .nonPersistent()
    config.preferences.javaScriptCanOpenWindowsAutomatically = false
    config.allowsInlineMediaPlayback = true
    config.userContentController.add(rules)
    web = WKWebView(frame: .zero, configuration: config)
    web.navigationDelegate = self
    web.uiDelegate = self
    web.allowsLinkPreview = false
    web.backgroundColor = palette.surface

    heading.text = "Opening preview…"
    heading.font = .preferredFont(forTextStyle: .body)
    heading.adjustsFontForContentSizeCategory = true
    heading.textColor = palette.ink
    heading.lineBreakMode = .byTruncatingTail
    heading.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
    notice.font = .preferredFont(forTextStyle: .subheadline)
    notice.adjustsFontForContentSizeCategory = true
    notice.textColor = palette.ink
    notice.backgroundColor = palette.line
    notice.numberOfLines = 0
    notice.isHidden = true
    let close = icon("xmark", "Close preview") { [weak self] in self?.close() }
    var annotating = UIButton.Configuration.plain()
    annotating.title = "Annotate"
    annotating.baseForegroundColor = palette.accent
    annotation.configuration = annotating
    annotation.isHidden = true
    annotation.addAction(
      UIAction { [weak self] _ in
        guard let self = self else { return }
        self.annotate = self.pictures.last?["id"] as? String
        self.close()
      }, for: .primaryActionTriggered)
    let picture = icon("camera", "Take picture") { [weak self] in self?.takePicture() }

    let bar = UIStackView(arrangedSubviews: [close, heading, annotation, picture])
    bar.axis = .horizontal
    bar.alignment = .center
    bar.spacing = 8
    bar.isLayoutMarginsRelativeArrangement = true
    bar.directionalLayoutMargins = NSDirectionalEdgeInsets(top: 4, leading: 4, bottom: 4, trailing: 4)
    let noticeBox = UIStackView(arrangedSubviews: [notice])
    noticeBox.isLayoutMarginsRelativeArrangement = true
    noticeBox.directionalLayoutMargins = NSDirectionalEdgeInsets(
      top: 10, leading: 16, bottom: 10, trailing: 16)
    noticeBox.backgroundColor = palette.line
    noticeBox.isHidden = true
    // The page, with the minimized questions' bubble over its lower corner.
    let stage = UIView()
    // The page takes whatever height the bars and questions leave.
    stage.setContentHuggingPriority(.init(1), for: .vertical)
    stage.setContentCompressionResistancePriority(.init(1), for: .vertical)
    web.translatesAutoresizingMaskIntoConstraints = false
    stage.addSubview(web)
    let content = UIStackView(arrangedSubviews: [bar, rule(), noticeBox, stage, asking()])
    content.axis = .vertical
    content.translatesAutoresizingMaskIntoConstraints = false
    view.addSubview(content)
    let safe = view.safeAreaLayoutGuide
    NSLayoutConstraint.activate([
      bar.heightAnchor.constraint(greaterThanOrEqualToConstant: 56),
      content.topAnchor.constraint(equalTo: safe.topAnchor),
      content.leadingAnchor.constraint(equalTo: safe.leadingAnchor),
      content.trailingAnchor.constraint(equalTo: safe.trailingAnchor),
      // Clear of the home indicator, and of the keyboard while it's up.
      content.bottomAnchor.constraint(equalTo: view.keyboardLayoutGuide.topAnchor),
      // The questions never take more than 40% of the screen; they scroll.
      answering.heightAnchor.constraint(lessThanOrEqualTo: view.heightAnchor, multiplier: 0.4),
      web.topAnchor.constraint(equalTo: stage.topAnchor),
      web.bottomAnchor.constraint(equalTo: stage.bottomAnchor),
      web.leadingAnchor.constraint(equalTo: stage.leadingAnchor),
      web.trailingAnchor.constraint(equalTo: stage.trailingAnchor),
    ])
    if !questions.isEmpty { place(on: stage) }
    refresh()
    web.load(URLRequest(url: setup.url))
  }

  override func viewDidLayoutSubviews() {
    super.viewDidLayoutSubviews()
    keep(settle: false)
  }

  override func viewDidAppear(_ animated: Bool) {
    super.viewDidAppear(animated)
    appeared = true
    if waiting { finish() }
  }

  /// Closes the preview and answers the app; only the first call counts.
  func close() {
    guard !finished else { return }
    finished = true
    web?.stopLoading()
    web?.navigationDelegate = nil
    view.endEditing(true)
    if appeared || presentingViewController == nil { finish() } else { waiting = true }
  }

  private func finish() {
    waiting = false
    let result: JsonObject = [
      "pictures": pictures,
      "annotate": annotate.map { $0 as Any } ?? NSNull(),
      "answers": answers.map { ["labels": $0.labels, "own": $0.own] as [String: Any] },
      "submitted": submitted,
    ]
    WKContentRuleListStore.default().removeContentRuleList(forIdentifier: "preview-\(setup.port)") {
      _ in
    }
    if presentingViewController != nil {
      dismiss(animated: true) { self.done(result) }
    } else {
      done(result)
    }
  }

  private func say(_ message: String?) {
    notice.text = message
    notice.superview?.isHidden = message == nil
    notice.isHidden = message == nil
    if let message = message { UIAccessibility.post(notification: .announcement, argument: message) }
  }

  private func saved() -> String {
    switch pictures.count {
    case 0: return "Private preview"
    case 1: return "Picture saved"
    default: return "\(pictures.count) pictures saved"
    }
  }

  private func allowed(_ url: URL) -> Bool {
    url.scheme == "http" && url.host == "127.0.0.1" && url.port == setup.port && url.user == nil
      && url.password == nil
  }

  private func leaving() {
    say(PreviewController.leaves)
    DispatchQueue.main.asyncAfter(deadline: .now() + 4) { [weak self] in
      if self?.notice.text == PreviewController.leaves { self?.say(nil) }
    }
  }

  // MARK: The page

  func webView(
    _ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
    decisionHandler: @escaping (WKNavigationActionPolicy) -> Void
  ) {
    guard let url = action.request.url else { return decisionHandler(.cancel) }
    let main = action.targetFrame?.isMainFrame ?? true
    if allowed(url) || url.absoluteString == "about:blank"
      || (!main && ["about", "data", "blob"].contains(url.scheme ?? ""))
    {
      return decisionHandler(.allow)
    }
    if main { leaving() }
    decisionHandler(.cancel)
  }

  func webView(
    _ webView: WKWebView, decidePolicyFor response: WKNavigationResponse,
    decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void
  ) {
    if response.isForMainFrame, let http = response.response as? HTTPURLResponse,
      http.statusCode >= 400
    {
      say("The computer could not open this preview.")
    }
    decisionHandler(.allow)
  }

  func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
    heading.text = "Opening preview…"
    say(nil)
  }

  func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
    heading.text = saved()
  }

  func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
    failed(error)
  }

  func webView(
    _ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!,
    withError error: Error
  ) {
    failed(error)
  }

  private func failed(_ error: Error) {
    heading.text = saved()
    let code = (error as NSError).code
    // A link the preview refused stops its load; that's said already.
    guard code != NSURLErrorCancelled, code != 102 else { return }
    say(PreviewController.lost)
  }

  /// Pages open their new windows here, as on Android.
  func webView(
    _ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
    for action: WKNavigationAction, windowFeatures: WKWindowFeatures
  ) -> WKWebView? {
    if let url = action.request.url, allowed(url) { webView.load(action.request) } else { leaving() }
    return nil
  }

  // MARK: Pictures

  private func takePicture() {
    let size = web.bounds.size
    let scale = traitCollection.displayScale
    let width = Int(size.width * scale)
    let height = Int(size.height * scale)
    let cannot = "The picture could not be saved. Free some space and try again."
    guard width > 0, height > 0, width * height <= 16 * 1024 * 1024, used() < 32 * 1024 * 1024
    else { return say(cannot) }
    web.takeSnapshot(with: nil) { [weak self] image, _ in
      guard let self = self else { return }
      guard let png = image?.pngData(), let picture = try? self.store(png, width, height) else {
        return self.say(cannot)
      }
      self.pictures.append(picture)
      self.heading.text = self.saved()
      self.say(nil)
      self.annotation.isHidden = false
    }
  }

  private func used() -> Int64 {
    files.sync {
      let names = (try? FileManager.default.contentsOfDirectory(atPath: root.path)) ?? []
      return names.reduce(Int64(0)) { total, name in
        let attributes = try? FileManager.default.attributesOfItem(
          atPath: root.appendingPathComponent(name).path)
        return total + ((attributes?[.size] as? NSNumber)?.int64Value ?? 0)
      }
    }
  }

  private func store(_ png: Data, _ width: Int, _ height: Int) throws -> [String: Any] {
    try files.sync {
      let id = newId()
      let capturedAt = Int64(Date().timeIntervalSince1970 * 1000)
      let metadata: [String: Any] = [
        "id": id, "profileId": setup.profileId, "previewId": setup.previewId,
        "capturedAt": capturedAt, "taskId": setup.taskId.isEmpty ? NSNull() as Any : setup.taskId,
      ]
      let json = root.appendingPathComponent("\(id).json")
      let part = root.appendingPathComponent("\(id).part")
      let image = root.appendingPathComponent("\(id).png")
      defer {
        try? FileManager.default.removeItem(at: part)
        if !FileManager.default.fileExists(atPath: image.path) {
          try? FileManager.default.removeItem(at: json)
        }
      }
      try JSONSerialization.data(withJSONObject: metadata).write(to: json, options: .atomic)
      try png.write(to: part)
      try FileManager.default.moveItem(at: part, to: image)
      return [
        "id": id, "size": png.count, "width": width, "height": height, "capturedAt": capturedAt,
      ]
    }
  }

  // MARK: Questions

  /// What the agent asked about the page, answered under it as the person
  /// tries it. The answers go back to the app as the preview closes.
  private func asking() -> UIView {
    let section = panel
    section.axis = .vertical
    section.isHidden = questions.isEmpty
    guard !questions.isEmpty else { return section }
    asks.text = questions.count == 1 ? "The agent asks" : "The agent asks \(questions.count) questions"
    asks.font = UIFont.preferredFont(forTextStyle: .subheadline).bold()
    asks.adjustsFontForContentSizeCategory = true
    asks.textColor = palette.ink
    asks.numberOfLines = 2
    asks.lineBreakMode = .byTruncatingTail
    minimize = icon("chevron.down", "Minimize questions") { [weak self] in self?.fold(true) }
    var sending = UIButton.Configuration.filled()
    sending.title = "Submit answers"
    sending.baseBackgroundColor = palette.accent
    sending.baseForegroundColor = palette.surface
    submit.configuration = sending
    submit.addAction(
      UIAction { [weak self] _ in
        self?.submitted = true
        self?.close()
      }, for: .primaryActionTriggered)
    let top = UIStackView(arrangedSubviews: [minimize, asks, submit])
    top.alignment = .center
    top.spacing = 4
    top.setCustomSpacing(8, after: asks)
    top.isLayoutMarginsRelativeArrangement = true
    top.directionalLayoutMargins = NSDirectionalEdgeInsets(top: 4, leading: 4, bottom: 4, trailing: 12)
    asks.setContentHuggingPriority(.defaultLow, for: .horizontal)

    let list = UIStackView()
    list.axis = .vertical
    list.spacing = 4
    list.isLayoutMarginsRelativeArrangement = true
    list.directionalLayoutMargins = NSDirectionalEdgeInsets(top: 0, leading: 16, bottom: 8, trailing: 16)
    for (i, question) in questions.enumerated() {
      if !question.header.trimmingCharacters(in: .whitespaces).isEmpty {
        list.addArrangedSubview(text(question.header, .footnote, muted: true))
      }
      list.addArrangedSubview(text(question.text, .body, muted: false))
      if question.multi && !question.options.isEmpty {
        list.addArrangedSubview(text("Choose one or more", .footnote, muted: true))
      }
      for option in question.options {
        let button = UIButton(type: .system)
        button.contentHorizontalAlignment = .leading
        button.addAction(
          UIAction { [weak self] _ in self?.choose(i, option.label) }, for: .primaryActionTriggered)
        button.heightAnchor.constraint(greaterThanOrEqualToConstant: 48).isActive = true
        choices.append((i, option.label, button))
        list.addArrangedSubview(button)
      }
      // The person's own words, as the app's own question cards take them.
      let field = UITextField()
      field.placeholder =
        question.options.isEmpty
        ? "Your answer" : question.multi ? "Add your own answer" : "Or answer in your own words"
      field.text = answers[i].own
      field.font = .preferredFont(forTextStyle: .body)
      field.adjustsFontForContentSizeCategory = true
      field.textColor = palette.ink
      field.tintColor = palette.accent
      field.borderStyle = .roundedRect
      field.autocapitalizationType = .sentences
      field.returnKeyType = .done
      field.delegate = self
      field.tag = i
      field.addAction(
        UIAction { [weak self, weak field] _ in
          guard let self = self, let field = field else { return }
          self.typed(i, field)
        }, for: .editingChanged)
      field.heightAnchor.constraint(greaterThanOrEqualToConstant: 48).isActive = true
      fields[i] = field
      list.addArrangedSubview(field)
      list.setCustomSpacing(12, after: field)
    }
    list.translatesAutoresizingMaskIntoConstraints = false
    answering.addSubview(list)
    answering.keyboardDismissMode = .interactive
    let fit = answering.heightAnchor.constraint(equalTo: list.heightAnchor)
    fit.priority = .defaultHigh
    NSLayoutConstraint.activate([
      list.topAnchor.constraint(equalTo: answering.contentLayoutGuide.topAnchor),
      list.bottomAnchor.constraint(equalTo: answering.contentLayoutGuide.bottomAnchor),
      list.leadingAnchor.constraint(equalTo: answering.contentLayoutGuide.leadingAnchor),
      list.trailingAnchor.constraint(equalTo: answering.contentLayoutGuide.trailingAnchor),
      list.widthAnchor.constraint(equalTo: answering.frameLayoutGuide.widthAnchor),
      fit,
    ])
    section.addArrangedSubview(rule())
    section.addArrangedSubview(top)
    section.addArrangedSubview(answering)
    return section
  }

  // MARK: The minimized questions

  /// Minimized, the questions wait in a small bubble over the page, which the
  /// person can drag out of the way; a tap brings them back.
  private func place(on stage: UIView) {
    let disc = UIView()
    disc.backgroundColor = palette.accent
    disc.layer.cornerRadius = 26
    disc.layer.shadowColor = UIColor.black.cgColor
    disc.layer.shadowOpacity = 0.3
    disc.layer.shadowRadius = 6
    disc.layer.shadowOffset = CGSize(width: 0, height: 3)
    disc.isUserInteractionEnabled = false
    let mark = UIImageView(image: UIImage(systemName: "questionmark.bubble"))
    mark.tintColor = palette.surface
    mark.contentMode = .scaleAspectFit
    badge.font = .systemFont(ofSize: 12, weight: .bold)
    badge.textColor = palette.surface
    badge.textAlignment = .center
    // The badge's ring, in the page's colour, keeps it apart from the disc.
    let pill = UIView()
    pill.backgroundColor = palette.ink
    pill.layer.cornerRadius = 10
    pill.layer.borderWidth = 2
    pill.layer.borderColor = palette.surface.cgColor
    pill.layer.shadowColor = UIColor.black.cgColor
    pill.layer.shadowOpacity = 0.3
    pill.layer.shadowRadius = 7
    pill.layer.shadowOffset = CGSize(width: 0, height: 3)
    pill.isUserInteractionEnabled = false
    for piece in [disc, mark, pill, badge] as [UIView] {
      piece.translatesAutoresizingMaskIntoConstraints = false
    }
    disc.addSubview(mark)
    pill.addSubview(badge)
    bubble.addSubview(disc)
    bubble.addSubview(pill)
    bubble.isHidden = true
    bubble.isAccessibilityElement = true
    bubble.accessibilityTraits = .button
    // A ring on the circle shows keyboard focus, not the square frame.
    bubble.focusEffect = UIFocusHaloEffect(
      roundedRect: CGRect(x: 0, y: 8, width: 52, height: 52), cornerRadius: 26, curve: .circular)
    bubble.addAction(UIAction { [weak self] _ in self?.fold(false) }, for: .primaryActionTriggered)
    // Dragged, the bubble follows the finger inside the page and then settles
    // against the nearer side; a tap without a drag opens it.
    bubble.addGestureRecognizer(UIPanGestureRecognizer(target: self, action: #selector(drag(_:))))
    bubble.translatesAutoresizingMaskIntoConstraints = false
    stage.addSubview(bubble)
    NSLayoutConstraint.activate([
      bubble.widthAnchor.constraint(equalToConstant: 60),
      bubble.heightAnchor.constraint(equalToConstant: 60),
      bubble.trailingAnchor.constraint(equalTo: stage.trailingAnchor, constant: -Self.inset),
      bubble.bottomAnchor.constraint(equalTo: stage.bottomAnchor, constant: -Self.inset),
      disc.widthAnchor.constraint(equalToConstant: 52),
      disc.heightAnchor.constraint(equalToConstant: 52),
      disc.leadingAnchor.constraint(equalTo: bubble.leadingAnchor),
      disc.bottomAnchor.constraint(equalTo: bubble.bottomAnchor),
      mark.centerXAnchor.constraint(equalTo: disc.centerXAnchor),
      mark.centerYAnchor.constraint(equalTo: disc.centerYAnchor),
      mark.widthAnchor.constraint(equalToConstant: 24),
      mark.heightAnchor.constraint(equalToConstant: 24),
      pill.topAnchor.constraint(equalTo: bubble.topAnchor),
      pill.trailingAnchor.constraint(equalTo: bubble.trailingAnchor),
      pill.heightAnchor.constraint(equalToConstant: 20),
      pill.widthAnchor.constraint(greaterThanOrEqualToConstant: 20),
      badge.leadingAnchor.constraint(equalTo: pill.leadingAnchor, constant: 5),
      badge.trailingAnchor.constraint(equalTo: pill.trailingAnchor, constant: -5),
      badge.centerYAnchor.constraint(equalTo: pill.centerYAnchor),
    ])
  }

  private static let inset: CGFloat = 12
  private var dragFrom = CGPoint.zero

  private func counted() {
    let open = answers.filter {
      $0.labels.isEmpty && $0.own.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }.count
    badge.text = open == 0 ? "✓" : "\(open)"
    let asked =
      questions.count == 1 ? "Show the agent’s question" : "Show the agent’s \(questions.count) questions"
    bubble.accessibilityLabel =
      asked
      + (open == 0 ? ", all answered" : open == questions.count ? "" : ", \(open) not answered")
  }

  private func fold(_ away: Bool) {
    if away {
      // A field being typed in gives the keyboard back as it folds.
      view.endEditing(true)
      counted()
    }
    panel.isHidden = away
    bubble.isHidden = !away
    UIAccessibility.post(notification: .layoutChanged, argument: away ? bubble : minimize)
  }

  @objc private func drag(_ pan: UIPanGestureRecognizer) {
    switch pan.state {
    case .began:
      dragFrom = CGPoint(x: bubble.transform.tx, y: bubble.transform.ty)
    case .changed:
      let moved = pan.translation(in: bubble.superview)
      bubble.transform = CGAffineTransform(
        translationX: dragFrom.x + moved.x, y: dragFrom.y + moved.y)
      keep(settle: false)
    case .ended, .cancelled, .failed:
      keep(settle: true)
    default:
      break
    }
  }

  /// Keeps the bubble inside the page; settling, it moves to the nearer side.
  private func keep(settle: Bool) {
    guard let stage = bubble.superview else { return }
    let left = -max(stage.bounds.width - bubble.bounds.width - 2 * Self.inset, 0)
    let top = -max(stage.bounds.height - bubble.bounds.height - 2 * Self.inset, 0)
    let x = min(max(bubble.transform.tx, left), 0)
    let y = min(max(bubble.transform.ty, top), 0)
    if settle {
      UIView.animate(withDuration: 0.16) {
        self.bubble.transform = CGAffineTransform(translationX: x < left / 2 ? left : 0, y: y)
      }
    } else {
      bubble.transform = CGAffineTransform(translationX: x, y: y)
    }
  }

  private func choose(_ i: Int, _ label: String) {
    let answer = answers[i]
    if questions[i].multi {
      if let at = answer.labels.firstIndex(of: label) {
        answer.labels.remove(at: at)
      } else {
        answer.labels.append(label)
      }
    } else {
      // One choice only: an option, or the person's own words instead.
      answer.labels = [label]
      if !answer.own.isEmpty {
        answer.own = ""
        fields[i]?.text = ""
      }
    }
    refresh()
  }

  private func typed(_ i: Int, _ field: UITextField) {
    let own = String((field.text ?? "").prefix(4000))
    if field.text != own { field.text = own }
    let answer = answers[i]
    answer.own = own
    if !questions[i].multi && !own.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
      && !answer.labels.isEmpty
    {
      answer.labels.removeAll()
    }
    refresh()
  }

  func textFieldShouldReturn(_ textField: UITextField) -> Bool {
    textField.resignFirstResponder()
    return true
  }

  private func refresh() {
    for (i, label, button) in choices {
      let question = questions[i]
      let chosen = answers[i].labels.contains(label)
      let description = question.options.first { $0.label == label }?.description ?? ""
      var style = UIButton.Configuration.plain()
      style.title = label
      style.subtitle = description.isEmpty ? nil : description
      style.image = UIImage(
        systemName: question.multi
          ? (chosen ? "checkmark.square.fill" : "square")
          : (chosen ? "largecircle.fill.circle" : "circle"))
      style.imagePadding = 12
      style.baseForegroundColor = palette.ink
      style.titleAlignment = .leading
      button.configuration = style
      button.tintColor = palette.accent
      button.accessibilityTraits = chosen ? [.button, .selected] : .button
    }
    let answered = answers.contains {
      !$0.labels.isEmpty || !$0.own.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
    submit.isEnabled = answered
    submit.alpha = answered ? 1 : 0.5
  }

  // MARK: Pieces

  private func icon(_ symbol: String, _ label: String, _ action: @escaping () -> Void) -> UIButton {
    let button = UIButton(type: .system)
    button.setImage(UIImage(systemName: symbol), for: .normal)
    button.tintColor = palette.ink
    button.accessibilityLabel = label
    button.addAction(UIAction { _ in action() }, for: .primaryActionTriggered)
    NSLayoutConstraint.activate([
      button.widthAnchor.constraint(equalToConstant: 48),
      button.heightAnchor.constraint(equalToConstant: 48),
    ])
    return button
  }

  private func rule() -> UIView {
    let line = UIView()
    line.backgroundColor = palette.line
    line.heightAnchor.constraint(equalToConstant: 1 / max(traitCollection.displayScale, 1)).isActive =
      true
    return line
  }

  private func text(_ words: String, _ style: UIFont.TextStyle, muted: Bool) -> UILabel {
    let label = UILabel()
    label.text = words
    label.font = .preferredFont(forTextStyle: style)
    label.adjustsFontForContentSizeCategory = true
    label.textColor = muted ? palette.ink.withAlphaComponent(0.7) : palette.ink
    label.numberOfLines = 0
    return label
  }
}

extension UIFont {
  fileprivate func bold() -> UIFont {
    guard let bold = fontDescriptor.withSymbolicTraits(.traitBold) else { return self }
    return UIFont(descriptor: bold, size: 0)
  }
}
