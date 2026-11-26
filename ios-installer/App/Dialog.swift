// Parts from OrchardLinux/resize-apfs (MIT)
import UIKit

/// The confirmation before partitions are erased (I1.7, I3.4): the design's
/// dialog over a scrim. UIAlertController is always light on iOS 12 and can't
/// hold the list or the checkbox.
final class DialogView: UIView {
    private let onCancel: () -> Void
    private let onConfirm: () -> Void
    private var confirm: ActionButton!

    init(_ m: Metrics, title: String, body: String, partitions: [InstallerView.Partition], action: String, checked: Bool,
         onCancel: @escaping () -> Void, onConfirm: @escaping () -> Void) {
        self.onCancel = onCancel
        self.onConfirm = onConfirm
        super.init(frame: .zero)
        backgroundColor = Theme.scrim

        let box = UIView()
        box.backgroundColor = Theme.background
        box.layer.borderWidth = 1
        box.layer.borderColor = Theme.dialogBorder.cgColor
        box.layer.shadowColor = UIColor.black.cgColor
        box.layer.shadowOpacity = 0.5
        box.layer.shadowRadius = 25
        box.layer.shadowOffset = CGSize(width: 0, height: 20)
        box.accessibilityViewIsModal = true

        let heading = text(title, Theme.sans(m.pick(20, 19, 18), semibold: true), Theme.title)
        heading.accessibilityTraits = .header
        let approve = ApproveRow(m, checked: checked) { [weak self] checked in self?.confirm.dimmed = !checked }
        let cancel = ActionButton("Cancel", style: .secondary, height: 44, fontSize: 16, padding: 18, hug: m.isPad) { [weak self] in self?.onCancel() }
        confirm = ActionButton(action, height: 44, fontSize: 16, padding: 18, hug: m.isPad) { [weak self] in self?.onConfirm() }
        confirm.dimmed = !checked
        let buttons: UIView
        if m.isPad {
            let spacer = UIView()
            spacer.setContentHuggingPriority(.defaultLow, for: .horizontal)
            buttons = stack(.horizontal, 10, [spacer, cancel, confirm])
        } else {
            let row = stack(.horizontal, 10, [cancel, confirm])
            row.distribution = .fillEqually
            buttons = row
        }
        let column = stack(.vertical, 20, [heading, text(body, Theme.sans(m.bodySize), Theme.text, line: 1.5),
                                          eraseAlert(m, partitions), approve, buttons])
        column.setCustomSpacing(24, after: approve)
        let padding: CGFloat = m.pick(28, 20, 20)
        embed(column, in: box, UIEdgeInsets(top: padding, left: padding, bottom: padding, right: padding))
        box.translatesAutoresizingMaskIntoConstraints = false
        addSubview(box)
        NSLayoutConstraint.activate([
            box.centerXAnchor.constraint(equalTo: centerXAnchor), box.centerYAnchor.constraint(equalTo: centerYAnchor),
            box.widthAnchor.constraint(equalToConstant: m.pick(480, m.screenWidth - 32, m.screenWidth - 16)),
        ])
    }
    required init?(coder: NSCoder) { fatalError() }

    // Escape (VoiceOver's two-finger scrub) cancels, as an alert's Cancel would.
    override func accessibilityPerformEscape() -> Bool { onCancel(); return true }
}
