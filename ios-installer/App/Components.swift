// Parts from OrchardLinux/resize-apfs (MIT)
// The components of installer-app.pen, frames I0.1 to I3.8.
import UIKit

/// The title and the subtitle under it.
func header(_ m: Metrics, title: String, subtitle: String? = nil, centered: Bool = false, titleColor: UIColor = Theme.title) -> UIStackView {
    let align: NSTextAlignment = centered ? .center : .left
    let column = stack(.vertical, 16, [text(title, Theme.sans(m.titleSize, semibold: true), titleColor, line: 1.2, kern: -0.2, align: align)])
    if let subtitle = subtitle {
        column.addArrangedSubview(text(subtitle, Theme.sans(m.subtitleSize), Theme.text, line: 1.5, align: align))
    }
    return column
}

/// The line under a button: red for a refused request, yellow while waiting.
func hint(_ m: Metrics, _ string: String, _ color: UIColor) -> UILabel {
    text(string, Theme.sans(m.noteSize), color, line: 1.45)
}

/// A square button: blue with dark text, or outlined. Dimmed (40 %) while it can't be used.
final class ActionButton: UIControl {
    enum Style { case primary, secondary }
    private let action: () -> Void

    /// `hug`: as wide as its label and padding (iPad, dialogs); otherwise it fills its row.
    init(_ title: String, style: Style = .primary, height: CGFloat, fontSize: CGFloat = 17, padding: CGFloat = 24, hug: Bool,
         action: @escaping () -> Void) {
        self.action = action
        super.init(frame: .zero)
        let primary = style == .primary
        backgroundColor = primary ? Theme.blue : .clear
        if !primary { layer.borderWidth = 1; layer.borderColor = Theme.quietBorder.cgColor }
        let label = text(title, Theme.sans(fontSize, semibold: primary), primary ? Theme.onAccent : Theme.foreground, lines: 1)
        label.isUserInteractionEnabled = false
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        NSLayoutConstraint.activate([
            heightAnchor.constraint(equalToConstant: height),
            label.centerXAnchor.constraint(equalTo: centerXAnchor), label.centerYAnchor.constraint(equalTo: centerYAnchor),
            label.leadingAnchor.constraint(greaterThanOrEqualTo: leadingAnchor, constant: padding),
        ])
        if hug {
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: padding).isActive = true
            setContentHuggingPriority(.required, for: .horizontal)
        }
        addTarget(self, action: #selector(tapped), for: .touchUpInside)
        isAccessibilityElement = true
        accessibilityLabel = title
        accessibilityTraits = .button
    }
    required init?(coder: NSCoder) { fatalError() }

    var dimmed = false {
        didSet {
            isEnabled = !dimmed
            alpha = dimmed ? 0.4 : 1
            accessibilityTraits = dimmed ? [.button, .notEnabled] : .button
        }
    }
    override var isHighlighted: Bool { didSet { if !dimmed { alpha = isHighlighted ? 0.8 : 1 } } }
    @objc private func tapped() { action() }
}

/// The buttons of a screen: side by side at their own width on iPad, stacked and full width on phones.
func actions(_ m: Metrics, _ buttons: [UIView], centered: Bool = false) -> UIView {
    guard m.isPad else { return stack(.vertical, 12, buttons) }
    return placed(stack(.horizontal, 12, buttons, align: .center), centered: centered)
}

/// "I have a backup and approve these changes."
final class ApproveRow: UIControl {
    private let box = UIView()
    private let tick = Icon.check.view(13, Theme.onAccent)
    private let changed: (Bool) -> Void

    init(_ m: Metrics, checked: Bool, changed: @escaping (Bool) -> Void) {
        self.changed = changed
        self.checked = checked
        super.init(frame: .zero)
        _ = fixed(box, 18, 18)
        box.addSubview(tick)
        NSLayoutConstraint.activate([tick.centerXAnchor.constraint(equalTo: box.centerXAnchor), tick.centerYAnchor.constraint(equalTo: box.centerYAnchor)])
        let label = text("I have a backup and approve these changes.", Theme.sans(m.bodySize), Theme.foreground)
        let row = stack(.horizontal, 10, [box, label], align: .center)
        row.isUserInteractionEnabled = false
        embed(row, in: self)
        addTarget(self, action: #selector(tapped), for: .touchUpInside)
        isAccessibilityElement = true
        accessibilityLabel = label.text
        show()
    }
    required init?(coder: NSCoder) { fatalError() }

    private(set) var checked: Bool
    private func show() {
        box.backgroundColor = checked ? Theme.blue : .clear
        box.layer.borderWidth = checked ? 0 : 1
        box.layer.borderColor = Theme.comment.cgColor
        tick.isHidden = !checked
        accessibilityTraits = checked ? [.button, .selected] : .button
    }
    @objc private func tapped() { checked.toggle(); show(); changed(checked) }
    /// The whole row and a little around it is the target.
    override func point(inside point: CGPoint, with event: UIEvent?) -> Bool {
        bounds.insetBy(dx: 0, dy: -max(0, 44 - bounds.height) / 2).contains(point)
    }
}

/// Eight fading spokes turning one step at a time. The step comes from the
/// clock, so a spinner made anew (screens are rebuilt when the view changes)
/// goes on where the last one was.
final class Spinner: UIView {
    private var spokes: [CAShapeLayer] = []
    private var timer: Timer?

    init() {
        super.init(frame: CGRect(x: 0, y: 0, width: 40, height: 40))
        _ = fixed(self, 40, 40)
        for index in 0..<8 {
            let spoke = CAShapeLayer()
            let angle = CGFloat(index) * .pi / 4
            let path = UIBezierPath()
            path.move(to: CGPoint(x: 20 + sin(angle) * 9, y: 20 - cos(angle) * 9))
            path.addLine(to: CGPoint(x: 20 + sin(angle) * 17, y: 20 - cos(angle) * 17))
            spoke.path = path.cgPath
            spoke.strokeColor = Theme.text.cgColor
            spoke.lineWidth = 3.5
            spoke.lineCap = .round
            layer.addSublayer(spoke)
            spokes.append(spoke)
        }
        tick()
    }
    required init?(coder: NSCoder) { fatalError() }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        timer?.invalidate()
        timer = nil
        guard window != nil else { return }
        let timer = Timer(timeInterval: 0.1, repeats: true) { [weak self] _ in self?.tick() }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    private func tick() {
        let step = Spinner.still ? 0 : Int(CACurrentMediaTime() / 0.1) % 8
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        for (index, spoke) in spokes.enumerated() { spoke.opacity = Float(1 - Double((index - step + 8) % 8) * 0.8 / 7) }
        CATransaction.commit()
    }
    /// Mock mode holds the spinner as the design draws it.
    static var still = false
}

/// The square badge above a centred screen's title.
func badge(_ m: Metrics, tint: UIColor, fill: CGFloat, _ content: UIView) -> UIView {
    let box = UIView()
    box.backgroundColor = tint.withAlphaComponent(fill)
    box.layer.borderWidth = 1
    box.layer.borderColor = tint.withAlphaComponent(0.35).cgColor
    content.translatesAutoresizingMaskIntoConstraints = false
    box.addSubview(content)
    NSLayoutConstraint.activate([content.centerXAnchor.constraint(equalTo: box.centerXAnchor), content.centerYAnchor.constraint(equalTo: box.centerYAnchor)])
    return placed(fixed(box, m.badge, m.badge), centered: true)
}

/// A tinted box with a heading (and its icon) over a paragraph: the yellow notices.
func notice(_ m: Metrics, icon: Icon? = nil, head: String, body: String, fill: CGFloat) -> UIView {
    let box = UIView()
    box.backgroundColor = Theme.yellow.withAlphaComponent(fill)
    box.layer.borderWidth = 1
    box.layer.borderColor = Theme.yellow.withAlphaComponent(0.32).cgColor
    var heading: UIView = text(head, Theme.sans(m.bodySize, semibold: true), Theme.yellow)
    if let icon = icon { heading = stack(.horizontal, 8, [icon.view(15, Theme.yellow), heading], align: .center) }
    if let icon = icon, body.isEmpty { heading = stack(.horizontal, 8, [icon.view(15, Theme.yellow), text(head, Theme.sans(m.bodySize, semibold: true), Theme.yellow)], align: .top) }
    let column = stack(.vertical, 6, body.isEmpty ? [heading] : [heading, text(body, Theme.sans(m.noteSize), Theme.text, line: 1.45)])
    embed(column, in: box, UIEdgeInsets(top: 14, left: 16, bottom: 14, right: 16))
    return box
}

/// The red box listing the partitions that are erased.
func eraseAlert(_ m: Metrics, _ partitions: [InstallerView.Partition]) -> UIView {
    let box = UIView()
    box.backgroundColor = Theme.red.withAlphaComponent(0.06)
    box.layer.borderWidth = 1
    box.layer.borderColor = Theme.red.withAlphaComponent(0.35).cgColor
    let total = partitions.reduce(Int64(0)) { $0 + $1.bytes }
    let count = partitions.count == 1 ? "1 partition" : "\(partitions.count) partitions"
    let title = text("\(count) (\(Capacity.text(total))) will be erased", Theme.sans(m.bodySize, semibold: true), Theme.red)
    let head = stack(.horizontal, 8, [Icon.triangleAlert.view(15, Theme.red), title], align: .center)
    let font = Theme.mono(m.monoSize)
    let rows = partitions.map { partition -> UIView in
        let name = text(partition.name, font, Theme.foreground, lines: 1)
        let filesystem = text(partition.filesystem, font, Theme.text, lines: 1)
        let size = text(Capacity.text(partition.bytes), font, Theme.text, lines: 1)
        name.widthAnchor.constraint(equalToConstant: m.pick(132, 86, 86)).isActive = true
        filesystem.widthAnchor.constraint(equalToConstant: m.pick(82, 52, 52)).isActive = true
        return placed(stack(.horizontal, 12, [name, filesystem, size]), centered: false)
    }
    let list = stack(.vertical, 3, rows)
    list.isLayoutMarginsRelativeArrangement = true
    list.layoutMargins = UIEdgeInsets(top: 0, left: 23, bottom: 0, right: 0)
    embed(stack(.vertical, 8, [head, list]), in: box, UIEdgeInsets(top: 14, left: 16, bottom: 14, right: 16))
    return box
}

/// A box of rows with a line between them (the steps, the account form).
func rowsBox(_ rows: [UIView]) -> UIView {
    let box = UIView()
    box.backgroundColor = Theme.panel
    box.layer.borderWidth = 1
    box.layer.borderColor = Theme.border.cgColor
    let column = stack(.vertical, 0)
    for (index, row) in rows.enumerated() {
        if index > 0 {
            let line = UIView()
            line.backgroundColor = Theme.border
            line.translatesAutoresizingMaskIntoConstraints = false
            row.addSubview(line)
            NSLayoutConstraint.activate([line.topAnchor.constraint(equalTo: row.topAnchor), line.heightAnchor.constraint(equalToConstant: 1),
                                         line.leadingAnchor.constraint(equalTo: row.leadingAnchor), line.trailingAnchor.constraint(equalTo: row.trailingAnchor)])
        }
        column.addArrangedSubview(row)
    }
    embed(column, in: box)
    return box
}

/// The steps of the split: done, running (yellow while reconnecting) and to come.
func stepList(_ m: Metrics, _ steps: [InstallerView.Step], reconnecting: Bool) -> UIView {
    rowsBox(steps.map { step in
        let icon: UIView, label: UILabel
        let size = m.bodySize
        switch step.state {
        case "done":
            icon = Icon.check.view(15, Theme.green)
            label = text(step.title, Theme.sans(size), Theme.text)
        case "active":
            icon = Icon.loaderCircle.view(15, reconnecting ? Theme.yellow : Theme.blue)
            label = text(step.title, Theme.sans(size, semibold: true), reconnecting ? Theme.yellow : Theme.foreground)
        default:
            icon = Icon.circle.view(15, Theme.pending)
            label = text(step.title, Theme.sans(size), Theme.comment)
        }
        let row = UIView()
        embed(stack(.horizontal, 12, [icon, label], align: .center), in: row, UIEdgeInsets(top: 13, left: 16, bottom: 13, right: 16))
        return row
    })
}

/// The dot and the line that say the split is still heard from.
func liveness(_ m: Metrics, _ string: String, dot: UIColor) -> UIView {
    let circle = UIView()
    circle.backgroundColor = dot
    circle.layer.cornerRadius = 3
    let label = text(string, Theme.mono(m.monoSize), Theme.comment, lines: 1)
    return placed(stack(.horizontal, 8, [fixed(circle, 6, 6), label], align: .center), centered: true)
}

/// A horizontal gradient from the theme's purple to its blue, as the design's
/// system space, progress bar and install time.
final class GradientView: UIView {
    override class var layerClass: AnyClass { CAGradientLayer.self }
    override init(frame: CGRect) {
        super.init(frame: frame)
        guard let gradient = layer as? CAGradientLayer else { return }
        gradient.colors = [Theme.purple.cgColor, Theme.blue.cgColor]
        gradient.startPoint = CGPoint(x: 0, y: 0.5)
        gradient.endPoint = CGPoint(x: 1, y: 0.5)
    }
    required init?(coder: NSCoder) { fatalError() }
}

/// One line of text filled with the gradient.
final class GradientLabel: UIView {
    private let label: UILabel
    private let gradient = GradientView()
    init(_ string: String, _ font: UIFont) {
        label = text(string, font, .black, lines: 1)
        super.init(frame: .zero)
        embed(gradient, in: self)
        gradient.mask = label
        setContentHuggingPriority(.required, for: .horizontal)
        isAccessibilityElement = true
        accessibilityLabel = string
    }
    required init?(coder: NSCoder) { fatalError() }
    override var intrinsicContentSize: CGSize { label.intrinsicContentSize }
    override func layoutSubviews() { super.layoutSubviews(); label.frame = bounds }
}

/// The install's progress: a thin gradient bar over its label and percent.
func progress(_ m: Metrics, fraction: CGFloat, label: String, percent: String) -> UIView {
    let track = UIView()
    track.backgroundColor = Theme.panel
    track.layer.borderWidth = 1
    track.layer.borderColor = Theme.border.cgColor
    track.heightAnchor.constraint(equalToConstant: 6).isActive = true
    let fill = GradientView()
    fill.translatesAutoresizingMaskIntoConstraints = false
    track.addSubview(fill)
    let part = min(max(fraction, 0), 1)
    NSLayoutConstraint.activate([fill.leadingAnchor.constraint(equalTo: track.leadingAnchor), fill.topAnchor.constraint(equalTo: track.topAnchor),
                                 fill.bottomAnchor.constraint(equalTo: track.bottomAnchor),
                                 part > 0 ? fill.widthAnchor.constraint(equalTo: track.widthAnchor, multiplier: part)
                                          : fill.widthAnchor.constraint(equalToConstant: 0)])
    let name = text(label, Theme.sans(m.pick(15, 14, 13)), Theme.comment, lines: 1)
    let value = text(percent, Theme.mono(m.pick(13, 12, 12)), Theme.comment, lines: 1)
    value.setContentHuggingPriority(.required, for: .horizontal)
    let column = stack(.vertical, 10, [track, stack(.horizontal, 12, [name, value], align: .top)])
    column.isAccessibilityElement = true
    column.accessibilityLabel = label
    column.accessibilityValue = percent
    return column
}

/// The split of the storage: both sizes over a bar whose divider can be dragged.
final class SplitSlider: UIView {
    enum Mode: Equatable {
        /// The divider moves between `lower` and `upper` (bytes for iOS); `home` is where it started.
        case drag(lower: Int64, upper: Int64, home: Int64)
        /// The finished split: no handle to hold.
        case still
        /// Not enough space: the red part is what the system needs.
        case short(needed: String)
    }

    /// The divider moved: the new size of iOS, and whether the finger lifted.
    var changed: ((Int64, Bool) -> Void)?
    private(set) var dragging = false

    private let m: Metrics
    private let iosName = UILabel(), systemName = UILabel(), iosValue = UILabel(), systemValue = UILabel()
    private let bar = UIView()
    private let iosBox = UIView(), usedBox = UIView(), usedLabel = UILabel()
    private let systemBox = GradientView(), note = UILabel()
    private let neededBox = UIView(), neededLabel = UILabel()
    private let line = UIView()
    private let grip = GripView()
    private var barHeight: NSLayoutConstraint!
    private var nameFont = Theme.sans(14), valueFont = Theme.sans(24, semibold: true)

    private var total: Int64 = 1, used: Int64 = 0, ios: Int64 = 0
    private var mode = Mode.still
    private var dragStart: Int64 = 0

    init(_ m: Metrics) {
        self.m = m
        super.init(frame: .zero)
        let nameFont = Theme.sans(m.pick(15, 14, 13)), valueFont = Theme.sans(m.pick(26, 24, 20), semibold: true)
        self.nameFont = nameFont; self.valueFont = valueFont
        func key(_ color: UIColor, _ name: UILabel) -> UIView {
            let swatch = UIView()
            swatch.backgroundColor = color
            return stack(.horizontal, 8, [fixed(swatch, 8, 8), name], align: .center)
        }
        let left = stack(.vertical, 4, [key(Theme.comment, iosName), iosValue], align: .leading)
        let right = stack(.vertical, 4, [key(Theme.blue, systemName), systemValue], align: .trailing)
        let spacer = UIView()
        spacer.setContentHuggingPriority(.defaultLow, for: .horizontal)
        let labels = stack(.horizontal, 8, [left, spacer, right], align: .bottom)
        barHeight = bar.heightAnchor.constraint(equalToConstant: 72)
        barHeight.isActive = true
        embed(stack(.vertical, 16, [labels, bar]), in: self)

        iosBox.backgroundColor = Theme.surface
        iosBox.layer.borderWidth = 1
        iosBox.layer.borderColor = Theme.border.cgColor
        usedBox.backgroundColor = Theme.used
        usedLabel.font = Theme.mono(m.monoSize)
        note.font = Theme.mono(m.monoSize)
        note.textColor = Theme.background.withAlphaComponent(0.8)
        neededBox.backgroundColor = Theme.red.withAlphaComponent(0.08)
        neededBox.layer.borderWidth = 1
        neededBox.layer.borderColor = Theme.red.withAlphaComponent(0.5).cgColor
        neededLabel.font = Theme.mono(m.monoSize)
        neededLabel.textColor = Theme.red
        for view in [iosBox, systemBox, neededBox, line, grip] { bar.addSubview(view) }
        iosBox.addSubview(usedBox); iosBox.addSubview(usedLabel)
        systemBox.addSubview(note)
        neededBox.addSubview(neededLabel)
        iosBox.clipsToBounds = true

        let pan = UIPanGestureRecognizer(target: self, action: #selector(dragged(_:)))
        bar.addGestureRecognizer(pan)
        isAccessibilityElement = true
    }
    required init?(coder: NSCoder) { fatalError() }

    func configure(os: String, system: String, total: Int64, used: Int64, ios: Int64, mode: Mode, note: String? = nil) {
        iosName.attributedText = styled(os, nameFont, Theme.text)
        systemName.attributedText = styled(system, nameFont, Theme.text)
        self.total = max(1, total); self.used = used; self.mode = mode
        self.note.text = note
        if case .short(let needed) = mode { neededLabel.text = needed }
        if case .still = mode { barHeight.constant = 56 } else { barHeight.constant = 72 }
        usedLabel.text = "\(Capacity.whole(used)) GB used"
        accessibilityLabel = "\(os) and \(system)"
        if case .drag = mode { accessibilityTraits = .adjustable } else { accessibilityTraits = .none }
        set(ios)
    }

    private func set(_ bytes: Int64) {
        ios = bytes
        iosValue.attributedText = styled(Capacity.text(ios), valueFont, Theme.title)
        systemValue.attributedText = styled(Capacity.text(total - ios), valueFont, Theme.title)
        accessibilityValue = "\(iosValue.text ?? ""), \(systemValue.text ?? "")"
        setNeedsLayout()
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        bar.layoutIfNeeded()
        let width = bar.bounds.width, height = bar.bounds.height
        guard width > 0 else { return }
        var iosWidth: CGFloat
        let inset: CGFloat
        neededBox.isHidden = true; systemBox.isHidden = false; grip.isHidden = true
        switch mode {
        case .drag:
            inset = 8
            let systemWidth = ((width - 4) * CGFloat(total - ios) / CGFloat(total)).rounded()
            iosWidth = width - 4 - systemWidth
            systemBox.frame = CGRect(x: iosWidth + 4, y: inset, width: systemWidth, height: height - 2 * inset)
            // The line and its knob are one shape with one shadow.
            grip.isHidden = false
            grip.frame = CGRect(x: iosWidth - 8, y: 0, width: 20, height: height)
            line.isHidden = true
        case .still:
            inset = 0
            let systemWidth = ((width - 4) * CGFloat(total - ios) / CGFloat(total)).rounded()
            iosWidth = width - 2 - systemWidth
            systemBox.frame = CGRect(x: iosWidth + 2, y: 0, width: systemWidth, height: height)
            line.isHidden = false
            line.backgroundColor = Theme.foreground
            line.frame = CGRect(x: iosWidth, y: 0, width: 2, height: height)
        case .short:
            inset = 8
            let neededWidth = ceil(neededLabel.intrinsicContentSize.width) + 20
            iosWidth = width - 4 - neededWidth
            systemBox.isHidden = true
            neededBox.isHidden = false
            neededBox.frame = CGRect(x: iosWidth + 4, y: inset, width: neededWidth, height: height - 2 * inset)
            let size = neededLabel.intrinsicContentSize
            neededLabel.frame = CGRect(x: 10, y: (neededBox.bounds.height - size.height) / 2, width: size.width, height: size.height)
            line.isHidden = false
            line.backgroundColor = Theme.red
            line.frame = CGRect(x: iosWidth, y: 0, width: 4, height: height)
        }
        iosWidth = max(0, iosWidth)
        iosBox.frame = CGRect(x: 0, y: inset, width: iosWidth, height: height - 2 * inset)

        // What iOS uses, on the bar's scale; its label beside it, or inside when there is no room beside.
        let inner = max(0, iosWidth - 24), boxHeight = iosBox.bounds.height - 24
        let labelSize = usedLabel.intrinsicContentSize
        let usedWidth = min(inner, max(0, width * CGFloat(used) / CGFloat(total) - 12))
        usedLabel.isHidden = false
        if usedWidth + 10 + labelSize.width <= inner {
            usedBox.frame = CGRect(x: 12, y: 12, width: usedWidth, height: boxHeight)
            usedLabel.textColor = Theme.comment
            usedLabel.frame = CGRect(x: 12 + usedWidth + 10, y: (iosBox.bounds.height - labelSize.height) / 2, width: labelSize.width, height: labelSize.height)
        } else {
            usedBox.frame = CGRect(x: 12, y: 12, width: inner, height: boxHeight)
            usedLabel.textColor = Theme.muted
            usedLabel.frame = CGRect(x: 22, y: (iosBox.bounds.height - labelSize.height) / 2, width: labelSize.width, height: labelSize.height)
            usedLabel.isHidden = labelSize.width + 20 > inner
        }

        let noteSize = note.intrinsicContentSize
        note.isHidden = systemBox.isHidden || noteSize.width + 24 > systemBox.bounds.width
        note.frame = CGRect(x: systemBox.bounds.width - 12 - noteSize.width, y: (systemBox.bounds.height - noteSize.height) / 2,
                            width: noteSize.width, height: noteSize.height)
    }

    /// Whole GB between the limits; within half a GB of where the divider
    /// started it returns there exactly, so the split can be left as it was.
    private func snap(_ bytes: Int64) -> Int64 {
        guard case .drag(let lower, let upper, let home) = mode else { return ios }
        if abs(bytes - home) < Capacity.gb / 2 { return home }
        let low = (lower + Capacity.gb - 1) / Capacity.gb, high = upper / Capacity.gb
        guard low <= high else { return home }
        return min(max(Capacity.whole(bytes), low), high) * Capacity.gb
    }

    @objc private func dragged(_ pan: UIPanGestureRecognizer) {
        guard case .drag = mode, bar.bounds.width > 4 else { return }
        if pan.state == .began { dragStart = ios; dragging = true }
        let moved = Double(pan.translation(in: bar).x) / Double(bar.bounds.width - 4) * Double(total)
        let value = snap(dragStart + Int64(moved))
        let finished = pan.state == .ended || pan.state == .cancelled || pan.state == .failed
        if finished { dragging = false }
        guard finished || value != ios else { return }
        set(value)
        changed?(value, finished)
    }

    override func accessibilityIncrement() { step(1) }
    override func accessibilityDecrement() { step(-1) }
    private func step(_ direction: Int64) {
        guard case .drag = mode else { return }
        let value = snap(ios + direction * Capacity.gb)
        set(value)
        changed?(value, true)
    }
}

/// The divider's handle: the line and the knob with its six dots, drawn as one
/// shape in one colour, with one shadow around the whole of it.
private final class GripView: UIView {
    override init(frame: CGRect) {
        super.init(frame: frame)
        isOpaque = false
        backgroundColor = .clear
        isUserInteractionEnabled = false
        contentMode = .redraw
        layer.shadowColor = UIColor.black.cgColor
        layer.shadowOpacity = 0.45
        layer.shadowRadius = 5
        layer.shadowOffset = CGSize(width: 0, height: 2)
    }
    required init?(coder: NSCoder) { fatalError() }

    private var shape: UIBezierPath {
        let knob = CGRect(x: 0, y: (bounds.height - 40) / 2, width: 20, height: 40)
        let path = UIBezierPath()
        for (x, y) in [(8, 0), (12, 0), (12, knob.minY), (20, knob.minY), (20, knob.maxY), (12, knob.maxY), (12, bounds.height),
                       (8, bounds.height), (8, knob.maxY), (0, knob.maxY), (0, knob.minY), (8, knob.minY)] as [(CGFloat, CGFloat)] {
            let point = CGPoint(x: x, y: y)
            if path.isEmpty { path.move(to: point) } else { path.addLine(to: point) }
        }
        path.close()
        return path
    }
    override func layoutSubviews() { super.layoutSubviews(); layer.shadowPath = shape.cgPath }
    override func draw(_ rect: CGRect) {
        Theme.foreground.setFill()
        shape.fill()
        Theme.background.setFill()
        let top = (bounds.height - 17) / 2
        for column in 0..<2 { for row in 0..<3 {
            UIRectFill(CGRect(x: 5 + CGFloat(column) * 7, y: top + CGFloat(row) * 7, width: 3, height: 3))
        } }
    }
}

/// The account form: the user name the system gives, and the password and
/// lock screen PIN, each typed twice.
final class AccountForm: UIView, UITextFieldDelegate {
    let password = UITextField(), repeatPassword = UITextField(), pin = UITextField(), repeatPin = UITextField()
    /// Something was typed, or a field was entered or left.
    var changed: (() -> Void)?
    private let userValue = UILabel()
    private var eyes: [UIButton] = []
    private var valueFont = Theme.mono(13)
    private var fields: [UITextField] { [password, repeatPassword, pin, repeatPin] }

    init(_ m: Metrics) {
        super.init(frame: .zero)
        let labelFont = Theme.sans(m.bodySize), valueFont = Theme.mono(m.pick(14, 13, 13))
        self.valueFont = valueFont
        userValue.setContentHuggingPriority(.required, for: .horizontal)
        func row(_ title: String, _ views: [UIView]) -> UIView {
            let label = text(title, labelFont, Theme.foreground, lines: 1)
            label.setContentHuggingPriority(.required, for: .horizontal)
            label.setContentCompressionResistancePriority(.required, for: .horizontal)
            let row = UIView()
            embed(stack(.horizontal, 12, [label] + views, align: .center), in: row, UIEdgeInsets(top: 14, left: 16, bottom: 14, right: 16))
            return row
        }
        let spacer = UIView()
        var rows = [row("Username", [spacer, userValue])]
        let titles = ["Password", "Repeat password", "Lock screen PIN", "Repeat PIN"]
        for (index, field) in fields.enumerated() {
            field.font = valueFont
            field.textColor = Theme.text
            field.tintColor = Theme.blue
            field.textAlignment = .right
            field.isSecureTextEntry = true
            field.keyboardAppearance = .dark
            field.autocorrectionType = .no
            field.autocapitalizationType = .none
            field.spellCheckingType = .no
            // Keeps iOS from offering to make and save a password of its own.
            field.textContentType = .oneTimeCode
            field.keyboardType = index < 2 ? .asciiCapable : .numberPad
            field.returnKeyType = index == 0 ? .next : .done
            field.delegate = self
            field.accessibilityLabel = titles[index]
            field.addTarget(self, action: #selector(edited), for: [.editingChanged, .editingDidBegin, .editingDidEnd])
            field.heightAnchor.constraint(equalToConstant: (labelFont.pointSize * 1.424).rounded()).isActive = true
            let eye = UIButton(type: .custom)
            eye.tag = index
            eye.addTarget(self, action: #selector(toggle(_:)), for: .touchUpInside)
            eye.accessibilityLabel = "Show " + titles[index]
            _ = fixed(eye, 15, 15)
            eyes.append(eye)
            rows.append(row(titles[index], [field, eye]))
            show(index)
        }
        embed(rowsBox(rows), in: self)
    }
    required init?(coder: NSCoder) { fatalError() }

    var user: String {
        get { userValue.text ?? "" }
        set { userValue.attributedText = styled(newValue, valueFont, Theme.text) }
    }
    var editing: UITextField? { fields.first { $0.isFirstResponder } }
    func clear() { fields.forEach { $0.text = "" } }

    /// Why Install can't start yet, once the user has left the field in question.
    var problem: String? {
        let first = password.text ?? "", second = repeatPassword.text ?? "", code = pin.text ?? "", again = repeatPin.text ?? ""
        if !first.isEmpty, first.count < 8, !password.isFirstResponder { return "The password needs 8 characters or more." }
        if !second.isEmpty, second != first, !repeatPassword.isFirstResponder { return "The passwords don't match." }
        if !code.isEmpty, code.count != 6, !pin.isFirstResponder { return "The PIN needs 6 digits." }
        if !again.isEmpty, again != code, !repeatPin.isFirstResponder { return "The PINs don't match." }
        return nil
    }
    var valid: Bool {
        let first = password.text ?? "", code = pin.text ?? ""
        return first.count >= 8 && first == repeatPassword.text && code.count == 6 && code == repeatPin.text
    }

    private func show(_ index: Int) {
        eyes[index].setImage(Icon.eye.image(15, fields[index].isSecureTextEntry ? Theme.comment : Theme.blue), for: .normal)
    }
    @objc private func toggle(_ eye: UIButton) {
        fields[eye.tag].isSecureTextEntry.toggle()
        show(eye.tag)
    }
    @objc private func edited() { changed?() }

    func textFieldShouldReturn(_ textField: UITextField) -> Bool {
        if textField === password { repeatPassword.becomeFirstResponder() }
        else if textField === repeatPassword { pin.becomeFirstResponder() }
        else { textField.resignFirstResponder() }
        return true
    }
    /// The PIN is six digits.
    func textField(_ textField: UITextField, shouldChangeCharactersIn range: NSRange, replacementString string: String) -> Bool {
        guard textField === pin || textField === repeatPin else { return true }
        let current = textField.text ?? ""
        guard let swap = Range(range, in: current) else { return false }
        let next = current.replacingCharacters(in: swap, with: string)
        return next.count <= 6 && next.allSatisfy { $0.isASCII && $0.isNumber }
    }
    /// A tap anywhere in a row starts typing in it.
    override func hitTest(_ point: CGPoint, with event: UIEvent?) -> UIView? {
        let hit = super.hitTest(point, with: event)
        if hit is UIButton || hit is UITextField { return hit }
        for (field, eye) in zip(fields, eyes) {
            guard let row = field.superview?.superview else { continue }
            let inRow = row.convert(row.bounds, to: self)
            if inRow.contains(point) {
                // Around the eye, the eye; elsewhere, the field.
                let eyeFrame = eye.convert(eye.bounds, to: self).insetBy(dx: -14, dy: -17)
                return eyeFrame.contains(point) ? eye : field
            }
        }
        return hit
    }
}
