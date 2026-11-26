// The design's frames as fixed views, to check the screens without a device:
// `--mock I1.2` (or ORCHARD_MOCK=I1.2). The helper is not contacted.
import UIKit

enum Mock {
    /// The frame to show, when the app was started to show one.
    static var frame: String? {
        let arguments = ProcessInfo.processInfo.arguments
        if let index = arguments.firstIndex(of: "--mock"), index + 1 < arguments.count { return arguments[index + 1] }
        return ProcessInfo.processInfo.environment["ORCHARD_MOCK"]
    }

    /// `--mock-size 320x568`: a window of the design's size on a larger screen.
    static var size: CGSize? {
        let arguments = ProcessInfo.processInfo.arguments
        guard let index = arguments.firstIndex(of: "--mock-size"), index + 1 < arguments.count else { return nil }
        let parts = arguments[index + 1].split(separator: "x").compactMap { Double($0) }
        return parts.count == 2 ? CGSize(width: parts[0], height: parts[1]) : nil
    }

    /// `--mock-kind "iPod touch"`: the device the frames are worded for.
    private static var kind: String {
        let arguments = ProcessInfo.processInfo.arguments
        if let index = arguments.firstIndex(of: "--mock-kind"), index + 1 < arguments.count { return arguments[index + 1] }
        return UIDevice.current.userInterfaceIdiom == .pad ? "iPad" : "iPhone"
    }

    /// The frames that show a ticked checkbox, a filled form or an open dialog.
    static func approved(_ frame: String) -> Bool { ["I1.2", "I1.4", "I1.6", "I1.7", "I1.8"].contains(frame) }
    static func dialog(_ frame: String) -> Bool { frame == "I1.7" || frame == "I3.4" }

    static func view(_ frame: String) -> InstallerView? {
        let gb: Int64 = 1_000_000_000
        let kind = Mock.kind
        let name = ["iPad": "iPad (8th generation)", "iPod touch": "iPod touch (5th generation)"][kind] ?? "iPhone XR"
        let device = InstallerView.Device(name: name, kind: kind, os: kind == "iPad" ? "iPadOS" : "iOS")
        let linux = [InstallerView.Partition(name: "BOOT", filesystem: "FAT32", bytes: 1_073_741_824),
                     InstallerView.Partition(name: "omarchy-a", filesystem: "ext4", bytes: 63 * gb)]
        func storage(ios: Int64, used: Int64 = 41, outside: [InstallerView.Partition] = []) -> InstallerView.Storage {
            InstallerView.Storage(totalBytes: 128 * gb, iosBytes: ios * gb, iosUsedBytes: used * gb, outsideBytes: (128 - ios) * gb, outside: outside)
        }
        func split(suggested: Int64, min: Int64 = 45, max: Int64 = 116, fits: Bool = false, refused: String? = nil) -> InstallerView.Split {
            InstallerView.Split(minIosBytes: min * gb, maxIosBytes: max * gb, suggestedIosBytes: suggested * gb, neededBytes: 12 * gb,
                                fits: fits, refused: refused)
        }
        var view = InstallerView(screen: "welcome", system: "Omarchy A", device: device, storage: storage(ios: 96))
        func splitting(reconnecting: Bool) -> InstallerView.Splitting {
            InstallerView.Splitting(targetIosBytes: 96 * gb,
                                    steps: [InstallerView.Step(title: "Check storage", state: "done"),
                                            InstallerView.Step(title: "Shrink \(device.os)", state: "active"),
                                            InstallerView.Step(title: "Check the result", state: "pending")],
                                    confirmedSecondsAgo: reconnecting ? 40 : 4, elapsedSeconds: 720, reconnecting: reconnecting)
        }
        switch frame {
        case "I0.1": view.screen = "welcome"; view.supported = true; view.storage = nil; view.welcome = InstallerView.Welcome(checked: false)
        case "I0.2": view.screen = "welcome"; view.supported = true; view.welcome = InstallerView.Welcome(checked: true)
        case "I1.1": view.screen = "unsupported"
        case "I1.2": view.screen = "split"; view.storage = storage(ios: 128); view.split = split(suggested: 96)
        case "I1.3": view.screen = "split"; view.split = split(suggested: 96, fits: true)
        case "I1.4":
            view.screen = "split"; view.storage = storage(ios: 128, used: 110); view.split = split(suggested: 114, min: 114)
        case "I1.5":
            view.screen = "split"; view.storage = storage(ios: 128, used: 124); view.split = split(suggested: 128, min: 126)
        case "I1.6", "I1.7":
            // I1.7 is the dialog of a split that changes something.
            view.screen = "split"; view.storage = storage(ios: 64, outside: linux); view.split = split(suggested: 64, fits: frame == "I1.6")
        case "I1.8":
            view.screen = "split"; view.storage = storage(ios: 128)
            view.split = split(suggested: 96, refused: "The requested size exceeds the contiguous space available after APFS.")
        case "I2.1": view.screen = "splitting"; view.storage = storage(ios: 128); view.splitting = splitting(reconnecting: false)
        case "I2.2": view.screen = "splitting"; view.storage = storage(ios: 128); view.splitting = splitting(reconnecting: true)
        case "I2.3": view.screen = "split_done"
        case "I3.1": view.screen = "account"; view.account = InstallerView.Account(user: "tablet", download: "downloading", erases: [])
        case "I3.2": view.screen = "account"; view.account = InstallerView.Account(user: "tablet", download: "ready", erases: [])
        case "I3.3": view.screen = "account"; view.account = InstallerView.Account(user: "tablet", download: "offline", erases: [])
        case "I3.4":
            view.screen = "account"; view.storage = storage(ios: 64, outside: linux)
            view.account = InstallerView.Account(user: "tablet", download: "ready", erases: linux)
        case "I3.5": view.screen = "installing"; view.installing = InstallerView.Installing(phase: "writing", percent: 64)
        case "I3.6": view.screen = "installing"; view.installing = InstallerView.Installing(phase: "finishing", percent: nil)
        case "I3.7": view.screen = "installed"; view.installed = InstallerView.Installed(seconds: 60.4)
        case "I3.8":
            view.screen = "failed"
            view.failed = InstallerView.Failed(message: "Something went wrong while writing it. \(device.os) and your data weren't touched.",
                                               detail: "writing the system: no space left on device")
        default: return nil
        }
        return view
    }
}
