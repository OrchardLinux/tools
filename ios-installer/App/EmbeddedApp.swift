// The installer inside the Dopamine app (no DFU): its settings for resize-apfs's shared
// access code (Access/StorageAccess.swift there) and its start.
import UIKit

extension StorageAccess.Configuration {
    static var installer: StorageAccess.Configuration {
        let programs = "/var/jb/usr/libexec/orchard-installer"
        let folder = "/private/var/OrchardInstaller"
        // What launch.py was asked for (--index, --allow-unsupported), one argument a line.
        let asked = Bundle.main.url(forResource: "helper-arguments", withExtension: "txt")
            .flatMap { try? String(contentsOf: $0, encoding: .utf8) }?.split(separator: "\n").map(String.init) ?? []
        return StorageAccess.Configuration(
            name: "Orchard Installer", state: "OrchardInstaller", folder: folder, label: "org.orchardlinux.installer",
            // The helper, and next to it the e2fsprogs programs it runs.
            payloads: [("orchard-installer-helper", programs + "/orchard-installer-helper")]
                + ["tune2fs", "resize2fs", "debugfs", "e2fsck"].map { ($0, programs + "/e2fsprogs/" + $0) },
            arguments: asked,
            lock: "/var/root/Library/OrchardInstaller/helper.lock",
            // What the sessions over SSH left, and Resize APFS's helper: both apps are the
            // same app to iOS, so only one of them is on a device.
            leftovers: [folder + "/orchard-installer-helper", folder + "/helper.pid", folder + "/helper.log", folder + "/e2fsprogs",
                        "/var/jb/Library/LaunchDaemons/org.orchardlinux.resize-apfs.installed-helper.plist",
                        "/var/jb/Library/LaunchDaemons/dev.omarchy.resize-apfs.plist"])
    }
}

/// Started by resize-apfs's Access/AccessBridge.m once the Dopamine app has launched.
@objc(RAFEmbeddedApp) final class EmbeddedApp: NSObject {
    private static var window: UIWindow?

    @objc(startWithBridge:) static func start(bridge: AnyObject) {
        guard window == nil else { return }
        let access = StorageAccess(bridge: unsafeBitCast(bridge, to: AccessBridge.self), configuration: .installer)
        let window = UIWindow(frame: UIScreen.main.bounds)
        if #available(iOS 13, *) { window.overrideUserInterfaceStyle = .dark }
        window.backgroundColor = Theme.background
        window.rootViewController = RootViewController(mock: Mock.frame, access: access)
        AccessWindow.show(window)
        self.window = window
    }
}
