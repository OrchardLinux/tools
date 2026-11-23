// The companion app: read-only status on the iPad while the installer on
// the computer splits the storage (designs 2.7, 2.7b, 2.8 and 2.9). It reads the
// helper's status file and has no controls. UIKit, for iOS 12 and later.
import UIKit

private let statusPath = "/var/run/orchard-companion/status.json"
#if targetEnvironment(simulator)
// The simulator reads a status file the developer writes, to check the screens.
private let readPath = ProcessInfo.processInfo.environment["ORCHARD_COMPANION_STATUS"] ?? statusPath
#else
private let readPath = statusPath
#endif

struct Status: Decodable, Equatable {
    var host: Bool
    var phase: String
    var started: Double?
    var message: String?
    /// The system the installer installs (from the overlay).
    var system: String?
}

private extension UIColor {
    convenience init(_ hex: UInt32, _ alpha: CGFloat = 1) {
        self.init(red: CGFloat((hex >> 16) & 0xff) / 255, green: CGFloat((hex >> 8) & 0xff) / 255,
                  blue: CGFloat(hex & 0xff) / 255, alpha: alpha)
    }
    static let background = UIColor(0x1a1b26)
    static let title = UIColor(0xe6e9fa)
    static let body = UIColor(0x737aa2)
    static let faint = UIColor(0x565f89)
    static let rule = UIColor(0xc0caf5)
    static let accent = UIColor(0x7aa2f7)
    static let ok = UIColor(0x9ece6a)
    static let amber = UIColor(0xe0af68)
}

private func sans(_ size: CGFloat, semibold: Bool = false) -> UIFont {
    UIFont(name: semibold ? "SourceSans3-Semibold" : "SourceSans3-Regular", size: size) ?? .systemFont(ofSize: size, weight: semibold ? .semibold : .regular)
}
private func mono(_ size: CGFloat) -> UIFont {
    UIFont(name: "JetBrainsMono-Regular", size: size) ?? .monospacedDigitSystemFont(ofSize: size, weight: .regular)
}

private func label(_ text: String, _ font: UIFont, _ color: UIColor, lineSpacing: CGFloat = 0, kern: CGFloat = 0, center: Bool = false) -> UILabel {
    let l = UILabel()
    let style = NSMutableParagraphStyle()
    style.lineSpacing = lineSpacing
    style.alignment = center ? .center : .natural
    l.attributedText = NSAttributedString(string: text, attributes: [.font: font, .foregroundColor: color, .paragraphStyle: style, .kern: kern])
    l.numberOfLines = 0
    return l
}

private func fixed(_ view: UIView, _ width: CGFloat, _ height: CGFloat) -> UIView {
    view.translatesAutoresizingMaskIntoConstraints = false
    view.widthAnchor.constraint(equalToConstant: width).isActive = true
    view.heightAnchor.constraint(equalToConstant: height).isActive = true
    return view
}

/// A Lucide icon exported from the design at 3x, centred in its box.
private func icon(_ name: String, _ box: CGFloat) -> UIView {
    let view = UIImageView()
    view.contentMode = .center
    if let path = Bundle.main.path(forResource: name + "@3x", ofType: "png", inDirectory: "Icons"),
       let image = UIImage(contentsOfFile: path), let cg = image.cgImage {
        view.image = UIImage(cgImage: cg, scale: 3, orientation: .up)
    }
    return fixed(view, box, box)
}

/// The square badge above each screen's title, edged in its own colour.
private func badge(_ tint: UIColor, _ alpha: CGFloat, _ content: UIView) -> UIView {
    let box = UIView()
    box.backgroundColor = tint.withAlphaComponent(alpha)
    box.layer.borderColor = tint.withAlphaComponent(0.35).cgColor
    box.layer.borderWidth = 1
    content.translatesAutoresizingMaskIntoConstraints = false
    box.addSubview(content)
    content.centerXAnchor.constraint(equalTo: box.centerXAnchor).isActive = true
    content.centerYAnchor.constraint(equalTo: box.centerYAnchor).isActive = true
    return fixed(box, 88, 88)
}

/// Eight fading spokes, turning one step at a time (2.8's activity indicator).
private final class Spinner: UIView {
    private var spokes: [CALayer] = []
    private var timer: Timer?
    private var step = 0

    init() {
        super.init(frame: CGRect(x: 0, y: 0, width: 40, height: 40))
        for i in 0..<8 {
            let spoke = CALayer()
            spoke.bounds = CGRect(x: 0, y: 0, width: 3.5, height: 11)
            spoke.cornerRadius = 1.75
            spoke.backgroundColor = UIColor(0xa9b1d6).cgColor
            spoke.position = CGPoint(x: 20, y: 20)
            spoke.anchorPoint = CGPoint(x: 0.5, y: 0.5 + 13 / 11)
            spoke.setAffineTransform(CGAffineTransform(rotationAngle: CGFloat(i) * .pi / 4))
            layer.addSublayer(spoke)
            spokes.append(spoke)
        }
        tick()
        timer = Timer.scheduledTimer(withTimeInterval: 0.1, repeats: true) { [weak self] _ in self?.tick() }
    }
    required init?(coder: NSCoder) { nil }
    deinit { timer?.invalidate() }

    private func tick() {
        step = (step + 1) % 8
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        for (i, spoke) in spokes.enumerated() { spoke.opacity = Float(1 - Double((i - step + 8) % 8) * 0.11) }
        CATransaction.commit()
    }
}

private func heading(_ title: String, _ text: String) -> UIView {
    let stack = UIStackView(arrangedSubviews: [
        label(title, sans(30, semibold: true), .title, kern: -0.3, center: true),
        label(text, sans(17), .body, lineSpacing: 8.5, center: true),
    ])
    stack.axis = .vertical
    stack.alignment = .center
    stack.spacing = 12
    stack.translatesAutoresizingMaskIntoConstraints = false
    stack.widthAnchor.constraint(lessThanOrEqualToConstant: 500).isActive = true
    return stack
}

private func framed(_ content: UIView, _ background: UIColor, _ border: UIColor, vertical: CGFloat, horizontal: CGFloat) -> UIView {
    let box = UIView()
    box.backgroundColor = background
    box.layer.borderColor = border.cgColor
    box.layer.borderWidth = 1
    content.translatesAutoresizingMaskIntoConstraints = false
    box.addSubview(content)
    NSLayoutConstraint.activate([
        content.topAnchor.constraint(equalTo: box.topAnchor, constant: vertical),
        content.bottomAnchor.constraint(equalTo: box.bottomAnchor, constant: -vertical),
        content.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: horizontal),
        content.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -horizontal),
    ])
    return box
}

private func row(_ views: [UIView], spacing: CGFloat) -> UIStackView {
    let stack = UIStackView(arrangedSubviews: views)
    stack.axis = .horizontal
    stack.alignment = .center
    stack.spacing = spacing
    return stack
}

/// The note under 2.7 and 2.7b.
private func keepNote() -> UIView {
    framed(row([icon("plug", 16), label("Keep this app open and the cable connected.", sans(15), .body)], spacing: 10),
           UIColor(0x1d1f2b), UIColor(0x292e42), vertical: 10, horizontal: 16)
}

private func column(_ views: [UIView]) -> UIStackView {
    let stack = UIStackView(arrangedSubviews: views)
    stack.axis = .vertical
    stack.alignment = .center
    stack.spacing = 28
    return stack
}

/// 2.7b: the computer hasn't connected yet.
private func waiting() -> UIView {
    column([badge(.accent, 0.08, fixed(Spinner(), 40, 40)),
            heading("Waiting for your computer", "Orchard Installer continues once it reaches this iPad."),
            keepNote()])
}

/// 2.7: the computer is connected and the split hasn't started.
private func before(system: String) -> UIView {
    column([badge(.accent, 0.12, icon("laptop", 38)),
            heading("Continue on your computer", "Your iPad is connected to Orchard Installer. Choose how much space \(system) gets there."),
            keepNote()])
}

/// 2.8: splitting; `minutes` is the label the controller keeps current.
private func splitting(minutes: UILabel) -> UIView {
    let rule = { (name: String, text: String) in row([icon(name, 18), label(text, sans(16, semibold: true), .rule)], spacing: 14) }
    let rules = UIStackView(arrangedSubviews: [
        rule("plug-zap", "Keep the cable connected"),
        rule("power-off", "Don't restart or turn off your iPad"),
        rule("app-window", "Keep this app open"),
        label("Your iPad may freeze or look idle for a while. That's normal, and it doesn't mean anything went wrong.", sans(14), .body, lineSpacing: 7),
    ])
    rules.axis = .vertical
    rules.alignment = .leading
    rules.spacing = 16
    let box = framed(rules, UIColor.amber.withAlphaComponent(0.05), UIColor.amber.withAlphaComponent(0.32), vertical: 24, horizontal: 24)
    box.translatesAutoresizingMaskIntoConstraints = false
    box.widthAnchor.constraint(equalToConstant: 500).isActive = true
    return column([badge(.accent, 0.08, fixed(Spinner(), 40, 40)),
                   heading("Splitting the storage", "This can take around 30 minutes. Your computer shows how it's going."),
                   minutes, box])
}

/// 2.9: done.
private func done() -> UIView {
    column([badge(.ok, 0.12, icon("check", 38)),
            heading("Done on your iPad", "The storage is split. Continue on your computer.")])
}

/// Shows the screen for the helper's status, read every second.
final class StatusController: UIViewController {
    private var status: Status?
    private var shown: String?
    private let content = UIView()
    private let minutes = label("", mono(14), .faint)
    private var timer: Timer?

    override var preferredStatusBarStyle: UIStatusBarStyle { .lightContent }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .background
        let footer = label("Orchard Installer", mono(12), .faint, center: true)
        for v in [content, footer] { v.translatesAutoresizingMaskIntoConstraints = false; view.addSubview(v) }
        NSLayoutConstraint.activate([
            content.topAnchor.constraint(equalTo: view.topAnchor),
            content.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 72),
            content.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -72),
            content.bottomAnchor.constraint(equalTo: footer.topAnchor),
            footer.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            footer.heightAnchor.constraint(equalToConstant: 72),
            footer.bottomAnchor.constraint(equalTo: view.bottomAnchor),
        ])
        read()
        render()
        timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            guard let self = self else { return }
            if self.read() { self.render() }
            self.updateMinutes()
        }
    }

    /// Whether the status changed.
    @discardableResult private func read() -> Bool {
        guard let data = FileManager.default.contents(atPath: readPath),
              let value = try? JSONDecoder().decode(Status.self, from: data), value != status else { return false }
        status = value
        return true
    }

    private func updateMinutes() {
        let elapsed = status?.started.map { max(0, Int(Date().timeIntervalSince1970 - $0) / 60) } ?? 0
        minutes.attributedText = NSAttributedString(string: "\(elapsed) min so far", attributes: [.font: mono(14), .foregroundColor: UIColor.faint])
    }

    private func render() {
        let screen: String
        switch (status?.phase, status?.host) {
        case ("splitting"?, _): screen = "splitting"
        case ("done"?, _): screen = "done"
        // A stopped split is not a dead end here: the computer shows why and
        // lets the user choose again, so this shows 2.7/2.7b.
        case ("idle"?, true?), ("failed"?, true?): screen = "before"
        default: screen = "waiting"
        }
        guard screen != shown else { return }
        shown = screen
        content.subviews.forEach { $0.removeFromSuperview() }
        let view: UIView
        switch screen {
        case "splitting": updateMinutes(); view = splitting(minutes: minutes)
        case "done": view = done()
        case "before": view = before(system: status?.system ?? "the system")
        default: view = waiting()
        }
        view.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(view)
        NSLayoutConstraint.activate([
            view.centerXAnchor.constraint(equalTo: content.centerXAnchor),
            view.centerYAnchor.constraint(equalTo: content.centerYAnchor),
            view.leadingAnchor.constraint(greaterThanOrEqualTo: content.leadingAnchor),
            view.trailingAnchor.constraint(lessThanOrEqualTo: content.trailingAnchor),
        ])
    }
}

private func makeWindow(_ window: UIWindow) -> UIWindow {
    if #available(iOS 13, *) { window.overrideUserInterfaceStyle = .dark }
    window.backgroundColor = .background
    window.rootViewController = StatusController()
    window.makeKeyAndVisible()
    return window
}

/// iOS 13 and later: the window comes from the scene (current iOS requires it).
@available(iOS 13, *)
@objc(SceneDelegate)
final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    func scene(_ scene: UIScene, willConnectTo session: UISceneSession, options: UIScene.ConnectionOptions) {
        guard let scene = scene as? UIWindowScene else { return }
        window = makeWindow(UIWindow(windowScene: scene))
    }
}

@UIApplicationMain
final class AppDelegate: UIResponder, UIApplicationDelegate {
    var window: UIWindow?

    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?) -> Bool {
        application.isIdleTimerDisabled = true
        // iOS 12 has no scenes: the window is the app's own.
        if #available(iOS 13, *) {} else { window = makeWindow(UIWindow(frame: UIScreen.main.bounds)) }
        return true
    }
}
