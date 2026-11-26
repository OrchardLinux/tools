// Parts from OrchardLinux/resize-apfs (MIT)
import UIKit

/// The colours of installer-app.pen (Tokyo Night). iOS 12 has no dynamic
/// colours: they are fixed.
enum Theme {
    static let background = UIColor(hex: 0x1A1B26)
    static let title = UIColor(hex: 0xE6E9FA)
    static let text = UIColor(hex: 0xA9B1D6)
    static let foreground = UIColor(hex: 0xC0CAF5)
    static let muted = UIColor(hex: 0x737AA2)
    static let comment = UIColor(hex: 0x565F89)
    static let blue = UIColor(hex: 0x7AA2F7)
    static let purple = UIColor(hex: 0xBB9AF7)
    static let green = UIColor(hex: 0x9ECE6A)
    static let yellow = UIColor(hex: 0xE0AF68)
    static let red = UIColor(hex: 0xF7768E)
    static let surface = UIColor(hex: 0x1F2335)
    static let border = UIColor(hex: 0x292E42)
    static let panel = UIColor(hex: 0x1D1F2B)
    static let used = UIColor(hex: 0x2F334D)
    static let onAccent = UIColor(hex: 0x16161E)
    static let quietBorder = UIColor(hex: 0x3B4261)
    static let pending = UIColor(hex: 0x414868)
    static let scrim = UIColor(hex: 0x0D0E14, alpha: 0.7)
    static let dialogBorder = UIColor(hex: 0x2F334D)

    static func sans(_ size: CGFloat, semibold: Bool = false) -> UIFont {
        UIFont(name: semibold ? "SourceSans3-Semibold" : "SourceSans3-Regular", size: size)
            ?? .systemFont(ofSize: size, weight: semibold ? .semibold : .regular)
    }
    static func mono(_ size: CGFloat) -> UIFont {
        UIFont(name: "JetBrainsMono-Regular", size: size) ?? .monospacedDigitSystemFont(ofSize: size, weight: .regular)
    }
}

extension UIColor {
    convenience init(hex: UInt32, alpha: CGFloat = 1) {
        self.init(red: CGFloat((hex >> 16) & 0xFF) / 255, green: CGFloat((hex >> 8) & 0xFF) / 255,
                  blue: CGFloat(hex & 0xFF) / 255, alpha: alpha)
    }
}

/// The design's three sizes: iPad (768 wide), iPhone XR (414) and iPod touch (320).
/// Other screens take the nearest by width.
struct Metrics: Equatable {
    enum Size { case pad, phone, pod }
    let size: Size
    /// The width of the screen's content column.
    let column: CGFloat
    let screenWidth: CGFloat

    init(width: CGFloat) {
        screenWidth = width
        if width >= 600 { size = .pad; column = min(624, width - 144) }
        else if width >= 350 { size = .phone; column = width - 48 }
        else { size = .pod; column = width - 32 }
    }

    func pick<T>(_ pad: T, _ phone: T, _ pod: T) -> T {
        switch size {
        case .pad: return pad
        case .phone: return phone
        case .pod: return pod
        }
    }
    var isPad: Bool { size == .pad }
    /// Between the blocks of a screen.
    var gap: CGFloat { pick(44, 36, 28) }
    var titleSize: CGFloat { pick(30, 26, 22) }
    var subtitleSize: CGFloat { pick(17, 16, 15) }
    /// Body text: checkbox label, step, form label, model.
    var bodySize: CGFloat { pick(16, 15, 14) }
    /// Notice bodies and the hint under a button.
    var noteSize: CGFloat { pick(15, 14, 13) }
    /// Small monospaced text.
    var monoSize: CGFloat { pick(12, 11, 11) }
    var buttonHeight: CGFloat { pick(48, 50, 44) }
    var badge: CGFloat { pick(88, 72, 60) }
    var badgeIcon: CGFloat { pick(38, 32, 26) }
}

/// Text in the design's line height: a multiple of the font size where the
/// design gives one, otherwise the fonts' own (1.424 for Source Sans 3, 1.32
/// for JetBrains Mono, which UIKit measures differently). The glyphs sit in
/// the line as the design's renderer puts them: ascent and descent centred.
func styled(_ string: String, _ font: UIFont, _ color: UIColor, line: CGFloat? = nil, kern: CGFloat = 0,
            align: NSTextAlignment = .left) -> NSAttributedString {
    let mono = font.fontName.hasPrefix("JetBrains")
    let ascent: CGFloat = mono ? 1.02 : 1.024, descent: CGFloat = mono ? 0.3 : 0.4
    let paragraph = NSMutableParagraphStyle()
    paragraph.alignment = align
    paragraph.lineBreakMode = .byWordWrapping
    var attributes: [NSAttributedString.Key: Any] = [.font: font, .foregroundColor: color]
    if kern != 0 { attributes[.kern] = kern }
    let height = (font.pointSize * (line ?? ascent + descent)).rounded(.toNearestOrAwayFromZero)
    paragraph.minimumLineHeight = height
    paragraph.maximumLineHeight = height
    // TextKit keeps the font's descent at the bottom of the line; this moves
    // the baseline to where centring puts it. Before iOS 16.4 UILabel applied
    // the offset twice.
    var offset = height / 2 + font.descender - (ascent - descent) * font.pointSize / 2
    if #available(iOS 16.4, *) {} else { offset /= 2 }
    attributes[.baselineOffset] = offset
    attributes[.paragraphStyle] = paragraph
    return NSAttributedString(string: string, attributes: attributes)
}

func text(_ string: String, _ font: UIFont, _ color: UIColor, line: CGFloat? = nil, kern: CGFloat = 0,
          align: NSTextAlignment = .left, lines: Int = 0) -> UILabel {
    let label = UILabel()
    label.numberOfLines = lines
    label.attributedText = styled(string, font, color, line: line, kern: kern, align: align)
    return label
}

func stack(_ axis: NSLayoutConstraint.Axis, _ spacing: CGFloat, _ views: [UIView] = [],
           align: UIStackView.Alignment = .fill) -> UIStackView {
    let stack = UIStackView(arrangedSubviews: views)
    stack.axis = axis; stack.spacing = spacing; stack.alignment = align
    return stack
}

/// Pins `content` inside `box` with `insets`.
func embed(_ content: UIView, in box: UIView, _ insets: UIEdgeInsets = .zero) {
    content.translatesAutoresizingMaskIntoConstraints = false
    box.addSubview(content)
    NSLayoutConstraint.activate([
        content.topAnchor.constraint(equalTo: box.topAnchor, constant: insets.top),
        content.bottomAnchor.constraint(equalTo: box.bottomAnchor, constant: -insets.bottom),
        content.leadingAnchor.constraint(equalTo: box.leadingAnchor, constant: insets.left),
        content.trailingAnchor.constraint(equalTo: box.trailingAnchor, constant: -insets.right),
    ])
}

func fixed(_ view: UIView, _ width: CGFloat, _ height: CGFloat) -> UIView {
    view.translatesAutoresizingMaskIntoConstraints = false
    NSLayoutConstraint.activate([view.widthAnchor.constraint(equalToConstant: width),
                                 view.heightAnchor.constraint(equalToConstant: height)])
    view.setContentHuggingPriority(.required, for: .horizontal)
    view.setContentCompressionResistancePriority(.required, for: .horizontal)
    return view
}

/// `view` at its own width, centred or at the leading edge of a full-width row.
func placed(_ view: UIView, centered: Bool) -> UIView {
    let row = UIView()
    view.translatesAutoresizingMaskIntoConstraints = false
    row.addSubview(view)
    NSLayoutConstraint.activate([
        view.topAnchor.constraint(equalTo: row.topAnchor), view.bottomAnchor.constraint(equalTo: row.bottomAnchor),
        centered ? view.centerXAnchor.constraint(equalTo: row.centerXAnchor) : view.leadingAnchor.constraint(equalTo: row.leadingAnchor),
        view.leadingAnchor.constraint(greaterThanOrEqualTo: row.leadingAnchor),
        view.trailingAnchor.constraint(lessThanOrEqualTo: row.trailingAnchor),
    ])
    return row
}

/// Sizes as the design shows them: whole GB, 1 GB = 1e9 bytes.
enum Capacity {
    static let gb: Int64 = 1_000_000_000
    static func whole(_ bytes: Int64) -> Int64 { Int64((Double(bytes) / Double(gb)).rounded()) }
    static func text(_ bytes: Int64) -> String { "\(whole(bytes)) GB" }
}

/// The design's Lucide icons (24-point grid, 2-point round strokes), drawn from
/// their paths: iOS 12 has no SF Symbols and the bundle has no asset catalog.
enum Icon {
    case check, circle, circleSlash, circleAlert, arrowUpRight, triangleAlert, loaderCircle, eye, plugZap

    private var paths: [String] {
        switch self {
        case .check: return ["M20 6 9 17l-5-5"]
        case .circle: return [Icon.ring(10)]
        case .circleSlash: return [Icon.ring(10), "M9 15l6-6"]
        case .circleAlert: return [Icon.ring(10), "M12 8v4", "M12 16h.01"]
        case .arrowUpRight: return ["M7 7h10v10", "M7 17 17 7"]
        case .triangleAlert:
            return ["m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3", "M12 9v4", "M12 17h.01"]
        case .loaderCircle: return ["M21 12a9 9 0 1 1-6.219-8.56"]
        case .eye:
            return ["M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0", Icon.ring(3)]
        case .plugZap:
            return ["M6.3 20.3a2.4 2.4 0 0 0 3.4 0L12 18l-6-6-2.3 2.3a2.4 2.4 0 0 0 0 3.4Z", "m2 22 3-3", "M7.5 13.5 10 11",
                    "M10.5 16.5 13 14", "m18 3-4 4h6l-4 4"]
        }
    }
    private static func ring(_ r: CGFloat) -> String { "M\(12 - r) 12a\(r) \(r) 0 1 0 \(2 * r) 0a\(r) \(r) 0 1 0 -\(2 * r) 0" }

    func image(_ size: CGFloat, _ color: UIColor) -> UIImage {
        let format = UIGraphicsImageRendererFormat.default()
        format.opaque = false
        return UIGraphicsImageRenderer(size: CGSize(width: size, height: size), format: format).image { context in
            context.cgContext.scaleBy(x: size / 24, y: size / 24)
            color.setStroke()
            for d in paths {
                let path = SVGPath.parse(d)
                path.lineWidth = 2; path.lineCapStyle = .round; path.lineJoinStyle = .round
                path.stroke()
            }
        }
    }

    func view(_ size: CGFloat, _ color: UIColor) -> UIImageView {
        let view = UIImageView(image: image(size, color))
        _ = fixed(view, size, size)
        return view
    }
}

/// The system's wordmark, from the bundle's `wordmark.svg` (one filled path),
/// drawn at a given height.
enum Wordmark {
    static func view(height: CGFloat, _ color: UIColor) -> UIView? {
        guard let url = Bundle.main.url(forResource: "wordmark", withExtension: "svg"),
              let svg = try? String(contentsOf: url, encoding: .utf8),
              let box = attribute("viewBox", in: svg)?.split(separator: " ").compactMap({ Double($0) }), box.count == 4, box[3] > 0,
              let d = attribute("d", in: svg) else { return nil }
        let scale = height / CGFloat(box[3])
        let size = CGSize(width: (CGFloat(box[2]) * scale).rounded(), height: height)
        let format = UIGraphicsImageRendererFormat.default()
        format.opaque = false
        let image = UIGraphicsImageRenderer(size: size, format: format).image { context in
            context.cgContext.scaleBy(x: scale, y: scale)
            context.cgContext.translateBy(x: -CGFloat(box[0]), y: -CGFloat(box[1]))
            color.setFill()
            SVGPath.parse(d).fill()
        }
        let view = UIImageView(image: image)
        _ = fixed(view, size.width, size.height)
        return view
    }

    private static func attribute(_ name: String, in svg: String) -> String? {
        guard let start = svg.range(of: " \(name)=\"") else { return nil }
        guard let end = svg[start.upperBound...].firstIndex(of: "\"") else { return nil }
        return String(svg[start.upperBound..<end])
    }
}

/// The part of SVG path data Lucide's icons and the wordmark use: M L H V Z and circular arcs.
enum SVGPath {
    static func parse(_ d: String) -> UIBezierPath {
        let path = UIBezierPath()
        let chars = Array(d)
        var i = 0
        var command: Character = " "
        var current = CGPoint.zero, start = CGPoint.zero

        func skip() { while i < chars.count, chars[i] == " " || chars[i] == "," { i += 1 } }
        func number() -> CGFloat {
            skip()
            var s = ""
            if i < chars.count, chars[i] == "-" || chars[i] == "+" { s.append(chars[i]); i += 1 }
            var dot = false
            while i < chars.count, chars[i].isNumber || (chars[i] == "." && !dot) {
                if chars[i] == "." { dot = true }
                s.append(chars[i]); i += 1
            }
            return CGFloat(Double(s) ?? 0)
        }
        func point(_ relative: Bool) -> CGPoint {
            let x = number(), y = number()
            return relative ? CGPoint(x: current.x + x, y: current.y + y) : CGPoint(x: x, y: y)
        }

        while true {
            skip()
            guard i < chars.count else { break }
            if chars[i].isLetter {
                command = chars[i]; i += 1
                if command == "Z" || command == "z" { path.close(); current = start; continue }
            }
            let relative = command.isLowercase
            switch command {
            case "M", "m":
                current = point(relative); start = current
                path.move(to: current)
                command = relative ? "l" : "L"
            case "L", "l":
                current = point(relative); path.addLine(to: current)
            case "H", "h":
                let x = number(); current.x = relative ? current.x + x : x; path.addLine(to: current)
            case "V", "v":
                let y = number(); current.y = relative ? current.y + y : y; path.addLine(to: current)
            case "A", "a":
                var r = number(); _ = number(); _ = number()
                let large = number() != 0, sweep = number() != 0
                let end = point(relative)
                let dx = end.x - current.x, dy = end.y - current.y
                let distance = (dx * dx + dy * dy).squareRoot()
                guard distance > 0 else { break }
                r = max(r, distance / 2)
                let h = max(0, r * r - distance * distance / 4).squareRoot() * (large != sweep ? 1 : -1)
                let center = CGPoint(x: (current.x + end.x) / 2 - dy / distance * h, y: (current.y + end.y) / 2 + dx / distance * h)
                path.addArc(withCenter: center, radius: r, startAngle: atan2(current.y - center.y, current.x - center.x),
                            endAngle: atan2(end.y - center.y, end.x - center.x), clockwise: sweep)
                current = end
            default:
                i += 1
            }
        }
        return path
    }
}
