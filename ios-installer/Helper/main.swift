// The installer's root helper. It serves the installer app over a local
// socket (PROTOCOL.md), hands every request to the engine (crates/inapp,
// linked in), and does the one thing the engine asks it for: resizing iOS,
// with resize-apfs's engine.
import Foundation
import Darwin
import ResizeCore
import ResizeDevice
import NativeSystem
import InstallerEngine

/// Where the socket and the token are. The app inside Dopamine names a folder of the helper's
/// own (--ipc-dir): its helper starts early in a screen restart, and iOS empties /var/run
/// after that.
let ipcDirectory: String = {
    let args = CommandLine.arguments
    return args.firstIndex(of: "--ipc-dir").flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil } ?? "/var/run/orchard-installer"
}()
let stateDirectory = "/var/root/Library/OrchardInstaller"
let cacheDirectory = "/var/root/Library/Caches/orchard"
// What an unstamped installer installs.
let defaultIndex = "https://github.com/OrchardLinux/omarchy-a/releases/latest/download/index.toml"
let defaultSystem = "Omarchy A"

func argument(_ name: String) -> String? {
    let args = CommandLine.arguments
    return args.firstIndex(of: name).flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil }
}

func machine() -> String {
    var length = 0
    guard sysctlbyname("hw.machine", nil, &length, nil, 0) == 0, length > 0, length < 256 else { return "unknown" }
    var buffer = [CChar](repeating: 0, count: length)
    guard sysctlbyname("hw.machine", &buffer, &length, nil, 0) == 0 else { return "unknown" }
    return String(cString: buffer)
}

func log(_ line: String) {
    line.withCString { oi_log($0) }
}

/// The resize engine, reached from the engine's effect callback, which is a
/// C function and carries no context.
final class Resizer: @unchecked Sendable {
    static var shared: Resizer?
    let engine: DeviceEngine

    init(executable: String) throws {
        try FileManager.default.createDirectory(atPath: stateDirectory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let source = DeviceSource(path: "/dev/disk0", fixtureSectorSize: nil, writesEnabled: true)
        engine = try DeviceEngine(source: source, directory: URL(fileURLWithPath: stateDirectory), executable: executable)
    }

    /// The storage and the split, as the resize engine has them, to the
    /// installer's engine.
    func report() {
        guard let snapshot = try? engine.snapshot(), let data = try? JSONEncoder().encode(snapshot),
              let text = String(data: data, encoding: .utf8) else { return }
        text.withCString { oi_storage($0) }
    }

    /// Inspects and reports once a second, on its own thread: the resize
    /// engine is synchronous and holds its lock while a resize step runs.
    func run() {
        Thread.detachNewThread { [self] in
            while true {
                engine.refresh()
                report()
                Thread.sleep(forTimeInterval: 1)
            }
        }
    }

    /// An effect of the installer's engine (crates/inapp, engine.rs).
    func carryOut(_ effect: [String: Any]) {
        if effect["refresh"] != nil {
            Thread.detachNewThread { [self] in
                engine.refresh()
                report()
            }
        }
        guard let split = effect["split"] as? [String: Any] else { return }
        Thread.detachNewThread { [self] in
            do {
                guard let bytes = (split["ios_bytes"] as? NSNumber)?.int64Value,
                      let revision = (split["revision"] as? String).flatMap(UUID.init(uuidString:)) else {
                    throw ResizeError("The split names no size or layout.")
                }
                let ids = split["replacing"] as? [String] ?? []
                let replacing = ids.compactMap(UUID.init(uuidString:))
                guard replacing.count == ids.count else { throw ResizeError("The split names an invalid partition.") }
                let plan = try engine.plan(PlanRequest(revision: revision, requestedBytes: bytes, createLinux: false, replacing: replacing))
                let started = try engine.start(StartRequest(planID: plan.id, requestID: UUID()))
                guard let operation = started.operation else { throw ResizeError("The split did not start.") }
                operation.id.uuidString.withCString { oi_split_started($0) }
                report()
            } catch {
                error.localizedDescription.withCString { oi_split_refused($0) }
            }
        }
    }
}

let foreign: @convention(c) (UnsafePointer<CChar>?) -> Void = { text in
    guard let text, let object = try? JSONSerialization.jsonObject(with: Data(String(cString: text).utf8)) as? [String: Any] else { return }
    Resizer.shared?.carryOut(object)
}

final class Server: @unchecked Sendable {
    let socketFD: Int32
    let token: String
    let mobileUID: uid_t
    /// The app that may connect as `mobile`. nil for the app inside Dopamine, which connects as root.
    let appExecutable: String?
    /// Set by the first request with the right token: the app is there.
    private let served = NSLock()
    private var wasServed = false
    var hasServed: Bool { served.lock(); defer { served.unlock() }; return wasServed }

    init(appExecutable: String?) throws {
        self.appExecutable = appExecutable
        mobileUID = getpwnam("mobile")?.pointee.pw_uid ?? 501
        let mobileGID = getpwnam("mobile")?.pointee.pw_gid ?? 501
        try FileManager.default.createDirectory(atPath: ipcDirectory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        guard chown(ipcDirectory, geteuid(), mobileGID) == 0, chmod(ipcDirectory, 0o750) == 0 else {
            throw ResizeError("Cannot protect the helper's socket directory.")
        }
        var bytes = [UInt8](repeating: 0, count: 32)
        arc4random_buf(&bytes, bytes.count)
        token = bytes.map { String(format: "%02x", $0) }.joined()
        let credentials = ipcDirectory + "/client.token"
        try Data(token.utf8).write(to: URL(fileURLWithPath: credentials), options: .atomic)
        guard chmod(credentials, 0o640) == 0, chown(credentials, geteuid(), mobileGID) == 0 else {
            throw ResizeError("Cannot protect the helper's token.")
        }
        let path = ipcDirectory + "/helper.sock"
        unlink(path)
        socketFD = socket(AF_UNIX, SOCK_STREAM, 0)
        guard socketFD >= 0 else { throw ResizeError("Cannot create the helper's socket.") }
        _ = fcntl(socketFD, F_SETFD, FD_CLOEXEC)
        var address = try LocalFrames.address(path)
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(socketFD, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard bound == 0, chmod(path, 0o660) == 0, chown(path, geteuid(), mobileGID) == 0, listen(socketFD, 8) == 0 else {
            throw ResizeError("Cannot publish the helper's socket.")
        }
    }

    func run() -> Never {
        while true {
            let fd = accept(socketFD, nil, nil)
            if fd < 0 { continue }
            _ = fcntl(fd, F_SETFD, FD_CLOEXEC)
            Thread.detachNewThread { [self] in
                serve(fd)
                close(fd)
            }
        }
    }

    private func constantTimeEqual(_ lhs: String, _ rhs: String) -> Bool {
        let a = Array(lhs.utf8), b = Array(rhs.utf8)
        guard a.count == b.count else { return false }
        return zip(a, b).reduce(UInt8(0)) { $0 | ($1.0 ^ $1.1) } == 0
    }

    /// One connection: the installer app (or root), then a request and its
    /// answer at a time. The engine's answer goes back as it is.
    private func serve(_ fd: Int32) {
        var noSigpipe: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSigpipe, socklen_t(MemoryLayout<Int32>.size))
        var uid: uid_t = 0
        guard raf_peer_uid(fd, &uid) == 0, uid == mobileUID || uid == 0,
              uid == 0 || appExecutable.map({ raf_peer_is_executable(fd, $0) == 1 }) == true else { return }
        while let frame = try? LocalFrames.read(from: fd) {
            let refused = { (message: String) in
                (try? JSONSerialization.data(withJSONObject: ["error": ["message": message]])) ?? Data()
            }
            guard let object = try? JSONSerialization.jsonObject(with: frame) as? [String: Any],
                  let given = object["token"] as? String, constantTimeEqual(given, token) else {
                try? LocalFrames.write(refused("Invalid helper credentials."), to: fd)
                return
            }
            served.lock(); wasServed = true; served.unlock()
            let answer: Data = String(decoding: frame, as: UTF8.self).withCString { request in
                guard let raw = oi_request(request) else { return refused("The installer did not answer.") }
                defer { oi_free(raw) }
                return Data(String(cString: raw).utf8)
            }
            do { try LocalFrames.write(answer, to: fd) } catch { return }
        }
    }
}

signal(SIGPIPE, SIG_IGN)
do {
    // The resize engine runs each storage step in a worker it starts from
    // this same executable, released only by its supervising helper.
    if let job = argument("--worker") {
        guard let gate = argument("--gate-fd").flatMap(Int32.init), raf_wait_for_release(gate) == 0 else {
            throw ResizeError("Worker was not released by its supervising helper; no mutation was attempted.")
        }
        close(gate)
        try DeviceWorker.runFile(URL(fileURLWithPath: job))
        exit(0)
    }
    guard geteuid() == 0 else { throw ResizeError("The installer's helper must run as root.") }
    // Whom the helper serves: the app inside Dopamine (--embedded IDENTIFIER PROCESS, started
    // as a launch daemon by that app, which connects as root), or the app at --ui-path.
    var embedded: (identifier: String, process: String)?
    if let index = CommandLine.arguments.firstIndex(of: "--embedded") {
        guard index + 2 < CommandLine.arguments.count else { throw ResizeError("--embedded needs the app's identifier and process name.") }
        embedded = (CommandLine.arguments[index + 1], CommandLine.arguments[index + 2])
    }
    let appExecutable = try argument("--ui-path").map(SessionTrust.validatedUIPath)
    guard (embedded == nil) != (appExecutable == nil) else { throw ResizeError("The helper serves the app --embedded or --ui-path names.") }
    if embedded != nil {
        // Nothing here works before the first unlock since the device started.
        FirstUnlock.wait()
        // That app asks for no terminal password, so Dopamine's default one must not stay.
        Thread.detachNewThread { TerminalPassword.replaceDefault(log: log) }
    }
    let executable = URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL.path
    let product = machine()
    let pad = product.hasPrefix("iPad")
    let config: [String: Any] = [
        "cache": cacheDirectory,
        "disk": "/dev/disk0",
        "tools": (executable as NSString).deletingLastPathComponent + "/e2fsprogs",
        "index": argument("--index") ?? defaultIndex,
        "system": argument("--system") ?? defaultSystem,
        "machine": product,
        "kind": pad ? "iPad" : product.hasPrefix("iPod") ? "iPod touch" : "iPhone",
        "os": pad ? "iPadOS" : "iOS",
        "timezone": TimeZone.current.identifier,
        "allow_unsupported": CommandLine.arguments.contains("--allow-unsupported"),
    ]
    let json = String(decoding: try JSONSerialization.data(withJSONObject: config), as: UTF8.self)
    if let problem = json.withCString({ oi_start($0, foreign) }) {
        defer { oi_free(problem) }
        throw ResizeError(String(cString: problem))
    }
    let resizer = try Resizer(executable: executable)
    Resizer.shared = resizer
    let server = try Server(appExecutable: appExecutable)
    resizer.run()
    if let embedded {
        // On the lock screen iOS starts the app's process without showing the app: only its
        // first request counts as open.
        let opener = AppOpener(identifier: embedded.identifier, processName: embedded.process)
        opener.opened = { server.hasServed }
        opener.start(log: log)
        log("helper serving \(embedded.identifier)")
    } else if let appExecutable {
        AppOpener(executable: appExecutable, registerWithUICache: false).start(log: log)
        log("helper serving \(appExecutable)")
    }
    server.run()
} catch {
    fputs("Orchard installer helper stopped: \(error.localizedDescription)\n", stderr)
    exit(1)
}
