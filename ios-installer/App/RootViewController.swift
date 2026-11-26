// Parts from OrchardLinux/resize-apfs (MIT)
import UIKit

/// Shows the helper's view as the frames of installer-app.pen (I0.1 to I3.8).
/// Everything on screen comes from the view, asked for once a second; the app
/// keeps only where the divider was dragged to, the ticked box and what is
/// typed into the account form.
final class RootViewController: UIViewController {
    /// "iOS can only spare N GB" (I1.4) shows while the most the system could
    /// get is under this many times what it needs.
    static let littleRoomFactor = 1.5
    static let supportedDevices = URL(string: "https://github.com/OrchardLinux/orchard/blob/main/docs/compatibility.md")!

    private let client: HelperClient?
    private let mock: String?
    private var current: InstallerView?
    private var metrics = Metrics(width: UIScreen.main.bounds.width)

    private let scroll = UIScrollView()
    private let content = UIView()
    private let footer = text("Orchard Installer", Theme.mono(12), Theme.comment, lines: 1)
    private var footerHeight: NSLayoutConstraint!
    private var footerCenter: NSLayoutConstraint!
    private var columnWidth: NSLayoutConstraint!
    private var column: UIView?
    private var dialog: DialogView?

    // What the user is doing.
    private var selectedIos: Int64?
    private var approved = false
    private var slider: SplitSlider?
    private var form: AccountForm?
    private var installButton: ActionButton?
    private var accountHint: UILabel?
    private weak var shareButton: UIView?

    private var error: String?
    private var busy = false
    private var polling = false
    /// Polls in a row without an answer, and how many of them it takes to show the welcome as still checking again.
    private var missed = 0
    private static let missedLimit = 10
    private var signature: String?
    private var timer: Timer?

    init(mock: String?) {
        self.mock = mock
        client = mock == nil ? HelperClient() : nil
        super.init(nibName: nil, bundle: nil)
    }
    required init?(coder: NSCoder) { fatalError() }

    override var preferredStatusBarStyle: UIStatusBarStyle { .lightContent }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Theme.background
        scroll.alwaysBounceVertical = false
        scroll.keyboardDismissMode = .interactive
        scroll.indicatorStyle = .white
        scroll.contentInsetAdjustmentBehavior = .never
        for child in [scroll, footer] { child.translatesAutoresizingMaskIntoConstraints = false; view.addSubview(child) }
        content.translatesAutoresizingMaskIntoConstraints = false
        scroll.addSubview(content)
        footerHeight = scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -72)
        footerCenter = footer.centerYAnchor.constraint(equalTo: view.bottomAnchor, constant: -36)
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor), scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            footerHeight, footerCenter, footer.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            content.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor),
            content.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor),
            content.leadingAnchor.constraint(equalTo: scroll.contentLayoutGuide.leadingAnchor),
            content.trailingAnchor.constraint(equalTo: scroll.contentLayoutGuide.trailingAnchor),
            content.widthAnchor.constraint(equalTo: scroll.frameLayoutGuide.widthAnchor),
            content.heightAnchor.constraint(greaterThanOrEqualTo: scroll.frameLayoutGuide.heightAnchor),
        ])
        let tap = UITapGestureRecognizer(target: self, action: #selector(backgroundTapped))
        tap.cancelsTouchesInView = false
        scroll.addGestureRecognizer(tap)
        NotificationCenter.default.addObserver(self, selector: #selector(keyboardChanged(_:)), name: UIResponder.keyboardWillChangeFrameNotification, object: nil)
        NotificationCenter.default.addObserver(self, selector: #selector(keyboardChanged(_:)), name: UIResponder.keyboardWillHideNotification, object: nil)

        if let mock = mock {
            Spinner.still = true
            current = Mock.view(mock)
            approved = Mock.approved(mock)
        }
        render()
        if let mock = mock {
            if current?.screen == "account" {
                for field in [form?.password, form?.repeatPassword] { field?.text = "0123456789" }
                for field in [form?.pin, form?.repeatPin] { field?.text = "123456" }
                refreshAccount()
            }
            if Mock.dialog(mock) { primaryTapped() }
        } else {
            poll()
            let timer = Timer(timeInterval: 1, repeats: true) { [weak self] _ in self?.poll() }
            RunLoop.main.add(timer, forMode: .common)
            self.timer = timer
        }
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        // The design's footer: 72 points with the label above the home
        // indicator, 72 with it centred on iPad, 44 on phones with a Home button.
        let indicator = view.safeAreaInsets.bottom > 0 && !metrics.isPad
        let height: CGFloat = metrics.isPad || indicator ? 72 : 44
        footerHeight.constant = -height
        footerCenter.constant = indicator ? -37 : -height / 2
        let now = Metrics(width: view.bounds.width)
        if now != metrics {
            metrics = now
            // Components are made for one size; what was typed moves to the new form.
            let typed = [form?.password.text, form?.repeatPassword.text, form?.pin.text, form?.repeatPin.text]
            slider = nil
            form = nil
            signature = nil
            let open = dialog != nil
            closeDialog()
            render()
            if let form = form {
                for (field, value) in zip([form.password, form.repeatPassword, form.pin, form.repeatPin], typed) { field.text = value }
                refreshAccount()
            }
            if open { primaryTapped() }
        }
    }

    // MARK: The helper

    private func poll() {
        guard let client = client, !polling, !busy else { return }
        polling = true
        client.request("view") { [weak self] answer in
            guard let self = self else { return }
            self.polling = false
            // An answer that was on its way while the user asked for something is older than that request's.
            guard !self.busy else { return }
            if case .ok(let object) = answer, let view = InstallerView.decode(object) {
                self.missed = 0
                self.apply(view)
            } else {
                // One answer that didn't come is not the helper gone: the screen, and what
                // is typed on it, stays until several in a row are missing.
                self.missed += 1
                if self.current == nil || self.missed >= RootViewController.missedLimit { self.apply(nil) }
            }
        }
    }

    /// Sends a request that changes something, once. The helper's answer is the new view or why not.
    private func send(_ name: String, _ arguments: [String: Any] = [:]) {
        guard let client = client, !busy else { return }
        busy = true
        error = nil
        render()
        client.request(name, arguments) { [weak self] answer in
            guard let self = self else { return }
            self.busy = false
            switch answer {
            case .ok(let object):
                if let view = InstallerView.decode(object) { self.apply(view) } else { self.render() }
            case .refused(let message):
                self.error = message
                self.render()
            case .unreachable:
                self.error = "The installer didn't answer. Try again."
                self.render()
            }
        }
    }

    private func apply(_ view: InstallerView?) {
        if view?.screen != current?.screen {
            approved = false
            error = nil
            closeDialog()
            self.view.endEditing(true)
            if view?.screen != "split" { selectedIos = nil; slider = nil }
            if view?.screen != "account" { form = nil }
        }
        current = view
        // The split and the install go on for a long time without a touch.
        UIApplication.shared.isIdleTimerDisabled = view?.screen == "splitting" || view?.screen == "installing"
        render()
    }

    // MARK: Rendering

    /// The system's name: the helper's, and before it answers the one the app was built for.
    private var system: String {
        current?.system ?? Bundle.main.object(forInfoDictionaryKey: "OrchardSystem") as? String ?? "the system"
    }
    private var kind: String { current?.device?.kind ?? UIDevice.current.model }
    private var os: String { current?.device?.os ?? UIDevice.current.systemName }

    private func render() {
        // A drag in progress keeps its views; the screen follows when the finger lifts.
        if slider?.dragging == true { return }
        let encoder = JSONEncoder()
        if #available(iOS 11.0, *) { encoder.outputFormatting = .sortedKeys }
        // Only what the screen shows decides whether it is built anew: the used space grows
        // with every downloaded megabyte, and a rebuilt form loses the keyboard.
        var shown = current
        if let screen = shown?.screen, !["split", "splitting", "split_done"].contains(screen) { shown?.storage = nil }
        if let used = shown?.storage?.iosUsedBytes { shown?.storage?.iosUsedBytes = used / 1_000_000_000 }
        // The download's state only changes the Install button and the line under it.
        shown?.account?.download = ""
        let state = shown.flatMap { try? encoder.encode($0) }.flatMap { String(data: $0, encoding: .utf8) } ?? "-"
        let next = "\(state)|\(approved)|\(error ?? "")|\(busy)|\(selectedIos ?? -1)|\(metrics.screenWidth)"
        guard next != signature else {
            if current?.screen == "account" { refreshAccount() }
            return
        }
        signature = next

        let focused = form?.editing
        installButton = nil
        accountHint = nil
        let blocks: [UIView]
        switch current?.screen {
        case "welcome"?: blocks = welcome()
        case "unsupported"?: blocks = unsupported()
        case "split"?: blocks = split()
        case "splitting"?: blocks = splitting()
        case "split_done"?: blocks = splitDone()
        case "account"?: blocks = account()
        case "installing"?: blocks = installing()
        case "installed"?: blocks = installed()
        case "failed"?: blocks = failed()
        // Until the helper answers, and while it can't be reached: the welcome, still checking.
        default: blocks = welcome()
        }
        column?.removeFromSuperview()
        let column = stack(.vertical, metrics.gap, blocks)
        column.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(column)
        // The group is centred between the status bar and the footer and never stretched; a taller one scrolls.
        NSLayoutConstraint.activate([
            column.centerXAnchor.constraint(equalTo: content.centerXAnchor),
            column.centerYAnchor.constraint(equalTo: content.centerYAnchor),
            column.widthAnchor.constraint(equalToConstant: metrics.column),
            column.topAnchor.constraint(greaterThanOrEqualTo: content.topAnchor, constant: 24),
            column.bottomAnchor.constraint(lessThanOrEqualTo: content.bottomAnchor, constant: -24),
        ])
        self.column = column
        if current?.screen == "account" { refreshAccount() }
        if let focused = focused, focused.window != nil { focused.becomeFirstResponder() }
    }

    private func button(_ title: String, style: ActionButton.Style = .primary, dimmed: Bool = false, action: @escaping () -> Void) -> ActionButton {
        let button = ActionButton(title, style: style, height: metrics.buttonHeight, hug: metrics.isPad, action: action)
        button.dimmed = dimmed || busy
        return button
    }


    /// I0.1, I0.2
    private func welcome() -> [UIView] {
        let m = metrics
        var blocks: [UIView] = []
        if let mark = Wordmark.view(height: m.pick(33, 28, 24), Theme.foreground) { blocks.append(placed(mark, centered: false)) }
        let head = header(m, title: "Let's install \(system)")
        // The model and that it is supported come with the board data.
        if let name = current?.device?.name {
            var row: [UIView] = [text(name, Theme.sans(m.bodySize), Theme.text, lines: 1)]
            if current?.supported == true {
                let tag = UIView()
                tag.layer.borderWidth = 1
                tag.layer.borderColor = Theme.blue.withAlphaComponent(0.4).cgColor
                embed(text("supported", Theme.mono(m.monoSize), Theme.blue, lines: 1), in: tag, UIEdgeInsets(top: 3, left: 8, bottom: 3, right: 8))
                row.append(tag)
            }
            head.addArrangedSubview(placed(stack(.horizontal, 10, row, align: .center), centered: false))
        }
        blocks.append(head)
        blocks.append(stack(.vertical, 8, [
            text("First, make room", Theme.sans(m.subtitleSize, semibold: true), Theme.title, lines: 1),
            text("The installer splits the storage between \(os) and \(system). Your apps and data stay where they are.",
                 Theme.sans(m.bodySize), Theme.text, line: 1.5),
        ]))
        let checked = current?.welcome?.checked == true
        let go = button("Continue", dimmed: !checked) { [weak self] in
            if checked { self?.send("continue") }
        }
        var row: [UIView] = [go]
        if let error = error {
            row.append(hint(m, error, Theme.red))
        } else if !checked {
            row.append(hint(m, "Checking your storage…", Theme.muted))
        }
        blocks.append(m.isPad ? placed(stack(.horizontal, 16, row, align: .center), centered: false) : stack(.vertical, 12, row))
        return blocks
    }

    /// I1.1
    private func unsupported() -> [UIView] {
        let m = metrics
        var blocks = [badge(m, tint: Theme.red, fill: 0.12, Icon.circleSlash.view(m.badgeIcon, Theme.red)),
                      header(m, title: "This \(kind) isn't supported yet", centered: true)]
        if let name = current?.device?.name {
            let tag = UIView()
            tag.backgroundColor = Theme.red.withAlphaComponent(0.08)
            tag.layer.borderWidth = 1
            tag.layer.borderColor = Theme.red.withAlphaComponent(0.4).cgColor
            embed(text("not supported", Theme.mono(m.monoSize), Theme.red, lines: 1), in: tag, UIEdgeInsets(top: 3, left: 8, bottom: 3, right: 8))
            blocks.append(placed(stack(.horizontal, 10, [text(name, Theme.sans(m.bodySize), Theme.text, lines: 1), tag], align: .center), centered: true))
        }
        let link = LinkButton(stack(.horizontal, 6, [text("See supported devices on GitHub", Theme.sans(m.bodySize), Theme.blue, lines: 1),
                                                    Icon.arrowUpRight.view(14, Theme.blue)], align: .center)) {
            UIApplication.shared.open(RootViewController.supportedDevices)
        }
        blocks.append(placed(link, centered: true))
        return blocks
    }

    // MARK: I1.2 to I1.8

    private struct SplitState {
        var total: Int64, lower: Int64, upper: Int64, selected: Int64
        /// Not enough free space for the system (I1.5).
        var short: Bool
        /// Partitions after iOS are replaced (I1.6).
        var replaces: Bool
        /// The storage can stay as it is and the divider is where it is now (I1.3).
        var keeps: Bool
        /// The system fits, but with little room (I1.4).
        var little: Bool
    }

    private func splitState() -> SplitState? {
        guard let storage = current?.storage, let split = current?.split else { return nil }
        let lower = split.minIosBytes ?? storage.iosUsedBytes
        let selected = selectedIos ?? split.suggestedIosBytes
        let short = split.maxIosBytes < lower && !split.fits
        let keeps = split.fits && selected == storage.iosBytes
        let room = Double(storage.totalBytes - lower)
        return SplitState(total: storage.totalBytes, lower: lower, upper: split.maxIosBytes, selected: selected, short: short,
                          replaces: !storage.outside.isEmpty, keeps: keeps,
                          little: !short && room < Double(split.neededBytes) * RootViewController.littleRoomFactor)
    }

    private func split() -> [UIView] {
        let m = metrics
        guard let state = splitState(), let storage = current?.storage, let split = current?.split else { return [] }
        let slider = self.slider ?? SplitSlider(m)
        self.slider = slider
        slider.changed = { [weak self] bytes, finished in
            self?.selectedIos = bytes
            if finished { self?.render() }
        }
        if state.short {
            let needed = Capacity.text(split.neededBytes)
            slider.configure(os: os, system: system, total: state.total, used: storage.iosUsedBytes, ios: storage.iosBytes,
                             mode: .short(needed: m.isPad ? "needs \(needed)" : needed))
            let free = Capacity.text(max(0, storage.iosBytes - storage.iosUsedBytes))
            var blocks: [UIView] = [
                header(m, title: "Not enough free space on your \(kind)",
                       subtitle: "\(system) needs at least \(needed), and \(os) only has \(free) free right now. Delete some apps or files, then check again."),
                slider,
                actions(m, [button("Check again") { [weak self] in self?.send("check") }]),
            ]
            if let error = error { blocks.append(hint(m, error, Theme.red)) }
            return blocks
        }

        slider.configure(os: os, system: system, total: state.total, used: storage.iosUsedBytes, ios: state.selected,
                         mode: .drag(lower: state.lower, upper: state.upper, home: split.suggestedIosBytes),
                         note: state.replaces ? (m.isPad ? "replaces existing Linux" : "replaces Linux") : nil)
        let subtitle = state.keeps && !state.replaces
            ? "Your storage is already split, and \(system) fits. Keep the split, or drag the divider to change it."
            : "Drag the divider to choose how much space \(system) gets. Your apps and data stay where they are."
        var blocks: [UIView] = [header(m, title: "Split the storage", subtitle: subtitle), slider]
        let tight = state.little && !state.replaces && !state.keeps
        if state.replaces {
            blocks.append(eraseAlert(m, storage.outside))
        } else if tight {
            blocks.append(notice(m, head: "\(os) can only spare \(Capacity.text(state.total - state.lower))",
                                 body: "That's enough for \(system), but tight. For more room, delete some apps or files and check again.", fill: 0.06))
        }
        let asks = state.replaces || !state.keeps
        if asks {
            blocks.append(ApproveRow(m, checked: approved) { [weak self] checked in
                self?.approved = checked
                self?.render()
            })
        }
        let title = state.replaces ? "Continue" : state.keeps ? "Keep the split" : "Split storage"
        var buttons: [UIView] = [button(title, dimmed: asks && !approved) { [weak self] in self?.primaryTapped() }]
        if tight { buttons.append(button("Check again", style: .secondary) { [weak self] in self?.send("check") }) }
        blocks.append(actions(m, buttons))
        if let error = error {
            blocks.append(hint(m, error, Theme.red))
        } else if let refused = split.refused {
            blocks.append(hint(m, "Your \(kind) didn't split the storage: \(refused)", Theme.red))
        }
        return blocks
    }

    /// The main button of the split and account screens.
    private func primaryTapped() {
        if current?.screen == "account" { installTapped(); return }
        guard let state = splitState(), let storage = current?.storage, !state.short else { return }
        if state.keeps { send("keep"); return }
        let split = { [weak self] in self?.send("split", ["ios_bytes": state.selected]); return }
        guard state.replaces else { split(); return }
        // I1.7
        openDialog(title: "Before splitting", body: "The partitions after \(os) are erased, and \(system) gets their space.",
                   partitions: storage.outside, action: "Split storage", confirm: split)
    }

    /// I2.1 and I2.2
    private func splitting() -> [UIView] {
        let m = metrics
        guard let splitting = current?.splitting else { return [] }
        let reconnecting = splitting.reconnecting
        let minutes = "\(Int(splitting.elapsedSeconds) / 60) min so far"
        let confirmed = splitting.confirmedSecondsAgo.map { "\(Int($0)) s ago" }
        // The design's short wording (its iPod touch frames) wherever the long line doesn't fit.
        func status(short: Bool) -> String {
            var parts: [String]
            if reconnecting {
                parts = ["reconnecting"]
                if let confirmed = confirmed { parts.append(short ? confirmed : "last confirmed " + confirmed) }
            } else {
                parts = ["still working"]
                if let confirmed = confirmed, !short { parts.append("confirmed " + confirmed) }
                parts.append(minutes)
            }
            return parts.joined(separator: " · ")
        }
        var line = status(short: m.size == .pod)
        let width = (line as NSString).size(withAttributes: [.font: Theme.mono(m.monoSize)]).width
        if width + 14 > m.column { line = status(short: true) }
        // The design's "This can take around 30 minutes." is left out: the app doesn't know how long.
        let subtitle = reconnecting ? "This app lost touch with the split for a moment. The split keeps going on its own."
                                    : "\(os) is shrinking to \(Capacity.text(splitting.targetIosBytes))."
        let tint = reconnecting ? Theme.yellow : Theme.blue
        return [
            badge(m, tint: tint, fill: 0.08, Spinner()),
            header(m, title: "Splitting the storage", subtitle: subtitle, centered: true),
            stepList(m, splitting.steps, reconnecting: reconnecting),
            liveness(m, line, dot: reconnecting ? Theme.yellow : Theme.green),
            notice(m, icon: .plugZap, head: "Keep your \(kind) charging and leave it alone",
                   body: "It may freeze or look idle for a long time. That doesn't mean it's finished or failed. Don't restart it or close this app.",
                   fill: 0.05),
        ]
    }

    /// I2.3
    private func splitDone() -> [UIView] {
        let m = metrics
        var blocks: [UIView] = [header(m, title: "Your \(kind) is ready",
                                       subtitle: "\(os) and \(system) now each have their own space. Your apps and data are where they were.")]
        if let storage = current?.storage {
            let slider = SplitSlider(m)
            slider.configure(os: os, system: system, total: storage.totalBytes, used: storage.iosUsedBytes, ios: storage.iosBytes, mode: .still)
            blocks.append(slider)
        }
        blocks.append(actions(m, [button("Continue") { [weak self] in self?.send("continue") }]))
        if let error = error { blocks.append(hint(m, error, Theme.red)) }
        return blocks
    }

    // MARK: I3.1 to I3.4

    private func account() -> [UIView] {
        let m = metrics
        guard let account = current?.account else { return [] }
        let form = self.form ?? AccountForm(m)
        self.form = form
        form.user = account.user
        form.changed = { [weak self] in self?.refreshAccount() }
        let install = button("Install") { [weak self] in self?.primaryTapped() }
        installButton = install
        // The line beside (iPad) or under the button: yellow while offline (I3.3), red for what can't go on.
        let line = UILabel()
        line.numberOfLines = 0
        accountHint = line
        let buttons = m.isPad ? stack(.horizontal, 16, [install, line], align: .center) : stack(.vertical, 12, [install, line])
        return [header(m, title: "Create your account", subtitle: "\(system) downloads in the meantime."), form, buttons]
    }

    /// The Install button and the line under it follow what is typed, without rebuilding the form under the keyboard.
    private func refreshAccount() {
        guard let account = current?.account, let form = form else { return }
        installButton?.dimmed = busy || account.download != "ready" || !form.valid
        let font = Theme.sans(metrics.noteSize)
        if let error = error {
            accountHint?.attributedText = styled(error, font, Theme.red, line: 1.45)
        } else if account.download == "offline" {
            accountHint?.attributedText = styled("Your \(kind) is offline. The download goes on once it's back online.", font, Theme.yellow, line: 1.45)
        } else if let problem = form.problem {
            accountHint?.attributedText = styled(problem, font, Theme.red, line: 1.45)
        } else {
            accountHint?.attributedText = nil
        }
        // Beside the button (iPad) the empty line keeps the row's width; under it (phones) it leaves no gap.
        accountHint?.isHidden = !metrics.isPad && accountHint?.attributedText == nil
    }

    private func installTapped() {
        guard let account = current?.account, let form = form, form.valid || mock != nil else { return }
        let password = form.password.text ?? "", pin = form.pin.text ?? ""
        view.endEditing(true)
        let install = { [weak self] in self?.send("install", ["password": password, "pin": pin]); return }
        guard !account.erases.isEmpty else { install(); return }
        // I3.4
        openDialog(title: "Before installing", body: "Installing erases these partitions. \(os) and its data stay as they are.",
                   partitions: account.erases, action: "Install", confirm: install)
    }

    // MARK: I3.5 to I3.8

    private func installing() -> [UIView] {
        let m = metrics
        let finishing = current?.installing?.phase == "finishing"
        let percent = finishing ? 100 : Int((current?.installing?.percent ?? 0).rounded())
        // The design's "About a minute left." is left out: the view has no time left.
        return [badge(m, tint: Theme.blue, fill: 0.08, Spinner()),
                header(m, title: "Installing \(system)", subtitle: "Keep this app open.", centered: true),
                progress(m, fraction: CGFloat(percent) / 100, label: finishing ? "Finishing up" : "Writing the system", percent: "\(percent)%")]
    }

    private func installed() -> [UIView] {
        let m = metrics
        let top = stack(.vertical, 10, [text("\(system) is installed", Theme.sans(m.titleSize, semibold: true), Theme.title, line: 1.2, kern: -0.2, align: .center)])
        if let seconds = current?.installed?.seconds {
            top.addArrangedSubview(placed(GradientLabel("Installed \(system) in \(String(format: "%.1f", seconds))s", Theme.mono(m.pick(13, 12, 12))), centered: true))
        }
        let later = stack(.vertical, 8, [
            text("Starting it", Theme.sans(m.pick(17, 16, 15), semibold: true), Theme.title, align: .center),
            text("After a restart your \(kind) starts \(os) as usual. To start \(system), open Orchard on a computer and put your \(kind) in DFU mode.",
                 Theme.sans(m.bodySize), Theme.text, line: 1.5, align: .center),
        ])
        let done = button("Done") {
            // Back to the Home screen; the app stays as it is.
            UIControl().sendAction(#selector(URLSessionTask.suspend), to: UIApplication.shared, for: nil)
        }
        return [badge(m, tint: Theme.green, fill: 0.12, Icon.check.view(m.badgeIcon, Theme.green)), top, later, actions(m, [done], centered: true)]
    }

    private func failed() -> [UIView] {
        let m = metrics
        var blocks: [UIView] = [badge(m, tint: Theme.red, fill: 0.12, Icon.circleAlert.view(m.badgeIcon, Theme.red)),
                                header(m, title: "\(system) couldn't be installed", subtitle: current?.failed?.message, centered: true, titleColor: Theme.red)]
        if let detail = current?.failed?.detail, !detail.isEmpty {
            blocks.append(text(detail, Theme.mono(m.pick(13, 12, 12)), Theme.text, line: 1.5, align: .center))
        }
        let share = button("Share log", style: .secondary) { [weak self] in self?.shareLog() }
        shareButton = share
        blocks.append(actions(m, [button("Try again") { [weak self] in self?.send("retry") }, share], centered: true))
        if let error = error { blocks.append(text(error, Theme.sans(m.noteSize), Theme.red, line: 1.45, align: .center)) }
        return blocks
    }

    private func shareLog() {
        guard let client = client, !busy else { return }
        client.request("log") { [weak self] answer in
            guard let self = self else { return }
            switch answer {
            case .ok(let object):
                let log = (object as? [String: Any])?["text"] as? String ?? ""
                let sheet = UIActivityViewController(activityItems: [log], applicationActivities: nil)
                sheet.popoverPresentationController?.sourceView = self.shareButton ?? self.view
                sheet.popoverPresentationController?.sourceRect = (self.shareButton ?? self.view).bounds
                self.present(sheet, animated: true)
            case .refused(let message):
                self.error = message
                self.render()
            case .unreachable:
                self.error = "The installer didn't answer. Try again."
                self.render()
            }
        }
    }

    // MARK: Dialog

    private func openDialog(title: String, body: String, partitions: [InstallerView.Partition], action: String, confirm: @escaping () -> Void) {
        closeDialog()
        // The design shows the box ticked; it starts empty so that erasing takes a decision of its own.
        let dialog = DialogView(metrics, title: title, body: body, partitions: partitions, action: action, checked: mock != nil,
                                onCancel: { [weak self] in self?.closeDialog() },
                                onConfirm: { [weak self] in self?.closeDialog(); confirm() })
        embed(dialog, in: view)
        self.dialog = dialog
        if mock == nil {
            dialog.alpha = 0
            UIView.animate(withDuration: 0.15) { dialog.alpha = 1 }
        }
        UIAccessibility.post(notification: .screenChanged, argument: dialog)
    }

    private func closeDialog() {
        dialog?.removeFromSuperview()
        dialog = nil
    }

    // MARK: Keyboard

    @objc private func backgroundTapped() { view.endEditing(true) }

    /// The keyboard takes the bottom of the scroll area; the field being typed in stays above it.
    @objc private func keyboardChanged(_ note: Notification) {
        guard let end = (note.userInfo?[UIResponder.keyboardFrameEndUserInfoKey] as? NSValue)?.cgRectValue else { return }
        let hidden = note.name == UIResponder.keyboardWillHideNotification
        let overlap = hidden ? 0 : max(0, scroll.frame.maxY - view.convert(end, from: nil).minY)
        scroll.contentInset.bottom = overlap
        scroll.scrollIndicatorInsets = UIEdgeInsets(top: 0, left: 0, bottom: overlap, right: 0)
        guard overlap > 0, let field = form?.editing, let row = field.superview?.superview else { return }
        view.layoutIfNeeded()
        scroll.scrollRectToVisible(row.convert(row.bounds, to: scroll).insetBy(dx: 0, dy: -16), animated: true)
    }
}

/// Plain content that acts as a link: it dims while pressed, and its target is at least 44 points high.
final class LinkButton: UIControl {
    private let action: () -> Void
    init(_ content: UIView, action: @escaping () -> Void) {
        self.action = action
        super.init(frame: .zero)
        content.isUserInteractionEnabled = false
        embed(content, in: self)
        addTarget(self, action: #selector(tapped), for: .touchUpInside)
        isAccessibilityElement = true
        accessibilityTraits = .link
        accessibilityLabel = "See supported devices on GitHub"
    }
    required init?(coder: NSCoder) { fatalError() }
    override var isHighlighted: Bool { didSet { alpha = isHighlighted ? 0.5 : 1 } }
    override func point(inside point: CGPoint, with event: UIEvent?) -> Bool {
        bounds.insetBy(dx: -8, dy: -max(0, 44 - bounds.height) / 2).contains(point)
    }
    @objc private func tapped() { action() }
}
