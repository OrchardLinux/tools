// The companion's root helper: answers the installer over usbmux
// (PROTOCOL.md) and splits the storage with resize-apfs's engine. It keeps
// the app informed through a status file; the app never talks to it.
import Foundation
import Darwin
import ResizeCore
import ResizeDevice
import NativeSystem

let port: UInt16 = 49720
let protocolVersion = 1
let tokenPath = "/cores/binpack/usr/share/orchard-companion/token"
let statusDirectory = "/var/run/orchard-companion"
let stateDirectory = "/var/root/Library/OrchardCompanion"
let maxLine = 1 << 20

@_silgen_name("oc_ecid") func oc_ecid(_ out: UnsafeMutablePointer<UInt64>) -> Int32
@_silgen_name("oc_running") func oc_running(_ name: UnsafePointer<CChar>) -> Int32
@_silgen_name("oc_springboard_launch") func oc_springboard_launch(_ bundleID: UnsafePointer<CChar>) -> Int32
@_silgen_name("oc_restart") func oc_restart() -> Int32

let bundleID = "org.orchardlinux.companion"
let appPath = "/cores/binpack/Applications/palera1nLoader.app"
let uicache = "/cores/binpack/usr/bin/uicache"
let started = Date()
/// When the helper started; operations before it belong to an earlier boot.
let helperStarted = started

/// A line in the helper's log with seconds since the helper started, to see
/// where the time between boot and the app goes.
func log(_ message: String) {
    print(String(format: "[%7.1fs] ", Date().timeIntervalSince(started)) + message)
    fflush(stdout)
}

/// Gets the app onto the screen without the user looking for it: registers
/// it with iPadOS as early as iPadOS takes it (palera1n's daemon does the
/// same, possibly later), then opens it until its process runs.
final class Launcher: @unchecked Sendable {
    private let lock = NSLock()
    private var wanted = true

    /// Open the app again (when the computer connects, for 2.7).
    func want() { lock.lock(); wanted = true; lock.unlock() }

    func run() {
        Thread.detachNewThread { [self] in
            var registered = false, lastRegister = Date.distantPast
            while true {
                Thread.sleep(forTimeInterval: 1)
                lock.lock(); let open = wanted; lock.unlock()
                if !registered {
                    if installed() {
                        registered = true
                        log("app registered with iPadOS")
                    } else if FileManager.default.fileExists(atPath: appPath + "/OrchardCompanion"),
                              Date().timeIntervalSince(lastRegister) > 3 {
                        lastRegister = Date()
                        log("registering the app: uicache exit \(spawn([uicache, "-p", appPath]))")
                    }
                    continue
                }
                guard open else { continue }
                if oc_running("OrchardCompanion") != 0 {
                    log("app running")
                    lock.lock(); wanted = false; lock.unlock()
                    continue
                }
                let viaLS = openWithLaunchServices()
                let viaSB = viaLS ? 0 : oc_springboard_launch(bundleID)
                log("opening the app: LaunchServices \(viaLS), SpringBoard \(viaSB)")
            }
        }
    }

    private func workspace() -> NSObject? {
        guard let type = NSClassFromString("LSApplicationWorkspace") as? NSObject.Type else { return nil }
        return type.perform(NSSelectorFromString("defaultWorkspace"))?.takeUnretainedValue() as? NSObject
    }

    private func installed() -> Bool {
        guard let ws = workspace() else { return false }
        let selector = NSSelectorFromString("applicationIsInstalled:")
        guard ws.responds(to: selector) else { return false }
        typealias Call = @convention(c) (AnyObject, Selector, NSString) -> Bool
        return unsafeBitCast(ws.method(for: selector), to: Call.self)(ws, selector, bundleID as NSString)
    }

    private func openWithLaunchServices() -> Bool {
        guard let ws = workspace() else { return false }
        let selector = NSSelectorFromString("openApplicationWithBundleID:")
        guard ws.responds(to: selector) else { return false }
        typealias Call = @convention(c) (AnyObject, Selector, NSString) -> Bool
        return unsafeBitCast(ws.method(for: selector), to: Call.self)(ws, selector, bundleID as NSString)
    }

    private func spawn(_ argv: [String]) -> Int32 {
        var pid: pid_t = 0
        let args = argv.map { strdup($0) } + [nil]
        defer { args.forEach { free($0) } }
        guard posix_spawn(&pid, argv[0], nil, nil, args, environ) == 0 else { return -1 }
        var status: Int32 = 0
        while waitpid(pid, &status, 0) < 0 && errno == EINTR {}
        return status
    }
}

func machine() -> String {
    var length = 0
    guard sysctlbyname("hw.machine", nil, &length, nil, 0) == 0, length > 0, length < 256 else { return "unknown" }
    var buffer = [CChar](repeating: 0, count: length)
    guard sysctlbyname("hw.machine", &buffer, &length, nil, 0) == 0 else { return "unknown" }
    return String(cString: buffer)
}

func ecid() -> String? {
    var value: UInt64 = 0
    guard oc_ecid(&value) == 0, value != 0 else { return nil }
    return String(value, radix: 16)
}

func constantTimeEqual(_ lhs: String, _ rhs: String) -> Bool {
    let a = Array(lhs.utf8), b = Array(rhs.utf8)
    guard a.count == b.count else { return false }
    return zip(a, b).reduce(UInt8(0)) { $0 | ($1.0 ^ $1.1) } == 0
}

/// The system the installer installs, from the overlay (the iPad app's words).
let systemName: String = {
    let path = (tokenPath as NSString).deletingLastPathComponent + "/system"
    let name = (try? String(contentsOfFile: path, encoding: .utf8))?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    return name.isEmpty ? "the system" : name
}()

final class Status: @unchecked Sendable {
    private let lock = NSLock()
    private var lastPing: Date?
    /// The engine's last snapshot, so the file can be written without waiting
    /// for the next (slow) inspection.
    private var lastSnapshot: ServiceSnapshot?
    func pinged() { lock.lock(); lastPing = Date(); lock.unlock() }
    /// A new snapshot from the engine's loop.
    func update(_ snapshot: ServiceSnapshot?) {
        lock.lock(); lastSnapshot = snapshot; lock.unlock()
        rewrite()
    }
    /// The file with the current connection and the last snapshot: on connect,
    /// and every second from its own thread (not held up by inspections).
    func rewrite() {
        lock.lock(); let snapshot = lastSnapshot; lock.unlock()
        write(snapshot)
    }
    var hostConnected: Bool {
        lock.lock(); defer { lock.unlock() }
        return lastPing.map { Date().timeIntervalSince($0) < 5 } ?? false
    }

    /// The app's view of the engine's operation.
    func write(_ snapshot: ServiceSnapshot?) {
        var phase = "idle", started: Double? = nil, message: String? = nil
        // Only a split started since the helper started (this boot) is shown;
        // an older record is history (e.g. a split that completed before the
        // disk changed, which the engine reports as stopped).
        if let op = snapshot?.operation, op.startedAt >= helperStarted {
            started = op.startedAt.timeIntervalSince1970
            switch op.status {
            case .running: phase = "splitting"
            case .completed: phase = "done"
            case .stopped, .unknown: phase = "failed"; message = op.message
            }
        }
        let value: [String: Any] = ["host": hostConnected, "phase": phase, "system": systemName,
                                    "started": started.map { $0 as Any } ?? NSNull(),
                                    "message": message.map { $0 as Any } ?? NSNull()]
        guard let data = try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) else { return }
        let url = URL(fileURLWithPath: statusDirectory).appendingPathComponent("status.json")
        try? data.write(to: url, options: .atomic)
        chmod(url.path, 0o644)
    }
}

final class Server: @unchecked Sendable {
    let engine: DeviceEngine
    let token: String
    let status = Status()
    let launcher = Launcher()
    /// The engine's latest snapshot, encoded. The engine holds its lock while
    /// a resize step runs (minutes, when shrinking), so requests answer from
    /// this copy, refreshed every second, and never wait for the resize.
    private let latestLock = NSLock()
    private var latest: Data?

    private func publish(_ snapshot: ServiceSnapshot?) {
        guard let snapshot, let data = try? JSONEncoder().encode(snapshot) else { return }
        latestLock.lock(); latest = data; latestLock.unlock()
    }

    private func latestSnapshot() throws -> Data {
        latestLock.lock(); let data = latest; latestLock.unlock()
        if let data { return data }
        let fresh = try JSONEncoder().encode(engine.snapshot())
        latestLock.lock(); latest = fresh; latestLock.unlock()
        return fresh
    }

    init() throws {
        guard geteuid() == 0 else { throw ResizeError("The companion helper must run as root.") }
        let raw = try String(contentsOfFile: tokenPath, encoding: .utf8).trimmingCharacters(in: .whitespacesAndNewlines)
        guard raw.count == 64, raw.allSatisfy(\.isHexDigit) else { throw ResizeError("The overlay's token is missing or malformed.") }
        token = raw.lowercased()
        try FileManager.default.createDirectory(atPath: stateDirectory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        try FileManager.default.createDirectory(atPath: statusDirectory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o755])
        let source = DeviceSource(path: "/dev/disk0", fixtureSectorSize: nil, writesEnabled: true)
        engine = try DeviceEngine(source: source, directory: URL(fileURLWithPath: stateDirectory),
                                  executable: URL(fileURLWithPath: CommandLine.arguments[0]).standardizedFileURL.path)
    }

    func run() throws {
        log("helper started")
        launcher.run()
        Thread.detachNewThread { [status] in
            while true {
                status.rewrite()
                Thread.sleep(forTimeInterval: 1)
            }
        }
        // The engine's own loop: inspect, then publish (its own thread; the
        // engine is synchronous and lock-guarded, iOS 12 has no async).
        Thread.detachNewThread { [self] in
            while true {
                engine.refresh()
                let snapshot = try? engine.snapshot()
                publish(snapshot)
                status.update(snapshot)
                Thread.sleep(forTimeInterval: 1)
            }
        }
        let listener = socket(AF_INET, SOCK_STREAM, 0)
        guard listener >= 0 else { throw ResizeError("Cannot create the installer's socket.") }
        var yes: Int32 = 1
        setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &yes, socklen_t(MemoryLayout<Int32>.size))
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = port.bigEndian
        address.sin_addr.s_addr = inet_addr("127.0.0.1")
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(listener, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) }
        }
        guard bound == 0, listen(listener, 4) == 0 else { throw ResizeError("Cannot listen on 127.0.0.1:\(port).") }
        log("listening on 127.0.0.1:\(port)")
        // Connections on their own threads; the main thread stays free for
        // the status loop above (a main-actor task).
        Thread.detachNewThread { [self] in
            while true {
                let fd = accept(listener, nil, nil)
                if fd < 0 { if errno == EINTR { continue }; log("accept failed: \(errno)"); continue }
                Thread.detachNewThread { [self] in
                    serve(fd)
                    forget(fd)
                    close(fd)
                }
            }
        }
        dispatchMain()
    }

    /// The installer's current connection. A newer one replaces it (the
    /// installer reconnected), so a dead connection never blocks the next.
    private let connections = NSLock()
    private var current: Int32?

    private func adopt(_ fd: Int32) {
        connections.lock()
        if let old = current, old != fd {
            shutdown(old, SHUT_RDWR)
            log("a newer connection replaced the previous one")
        }
        current = fd
        connections.unlock()
    }

    private func forget(_ fd: Int32) {
        connections.lock()
        if current == fd { current = nil; log("the computer disconnected") }
        connections.unlock()
    }

    private func serve(_ fd: Int32) {
        var timeout = timeval(tv_sec: 10, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        var noSigpipe: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &noSigpipe, socklen_t(MemoryLayout<Int32>.size))
        var buffer = Data()
        var authenticated = false
        while let line = readLine(fd, &buffer) {
            guard let object = try? JSONSerialization.jsonObject(with: line) as? [String: Any],
                  object.count == 1, let (name, body) = object.first else { return }
            let args = body as? [String: Any] ?? [:]
            if !authenticated {
                guard name == "hello", let given = args["token"] as? String,
                      constantTimeEqual(given.lowercased(), token) else { return }
                authenticated = true
                adopt(fd)
                status.pinged()
                // The app shows the computer as connected right away.
                status.rewrite()
                log("the computer connected")
                launcher.want()
                reply(fd, ok: ["protocol": protocolVersion, "ecid": ecid() as Any? ?? NSNull(), "machine": machine()])
                continue
            }
            status.pinged()
            do {
                switch name {
                case "ping": reply(fd, ok: [:])
                case "restart":
                    // The installer quit: back to normal iPadOS (palera1n's own reboot).
                    guard engine.idleForRestart else { throw ResizeError("The storage is being split; restart later.") }
                    reply(fd, ok: [:])
                    log("restarting for the installer")
                    Thread.detachNewThread {
                        Thread.sleep(forTimeInterval: 0.5)
                        // The kernel's own restart (no tool needed: works on any route).
                        log("restart: \(oc_restart())")
                    }
                case "log":
                    // The helper's own log (timings from boot to the app), its last 64 KiB.
                    let data = FileManager.default.contents(atPath: "/cores/orchard-companion.log") ?? Data()
                    reply(fd, ok: ["text": String(decoding: data.suffix(65536), as: UTF8.self)])
                case "snapshot": try reply(fd, okJSON: latestSnapshot())
                case "split":
                    guard let bytes = (args["apfs_bytes"] as? NSNumber)?.int64Value,
                          let revisionText = args["revision"] as? String, let revision = UUID(uuidString: revisionText)
                    else { throw ResizeError("A split needs apfs_bytes and the layout's revision.") }
                    // Partitions after APFS the user approved erasing, when the size needs their space.
                    let replacing = (args["replacing"] as? [String] ?? []).compactMap(UUID.init(uuidString:))
                    guard replacing.count == (args["replacing"] as? [String] ?? []).count else { throw ResizeError("replacing holds an invalid partition id.") }
                    let plan = try engine.plan(PlanRequest(revision: revision, requestedBytes: bytes, createLinux: false, replacing: replacing))
                    let data = try JSONEncoder().encode(engine.start(StartRequest(planID: plan.id, requestID: UUID())))
                    try reply(fd, okJSON: data)
                    // The copy shows the running split from now on.
                    publish(try? engine.snapshot())
                default: throw ResizeError("Unknown request.")
                }
            } catch {
                reply(fd, error: error.localizedDescription)
            }
        }
    }

    private func readLine(_ fd: Int32, _ buffer: inout Data) -> Data? {
        while true {
            if let newline = buffer.firstIndex(of: 0x0a) {
                let line = buffer[buffer.startIndex..<newline]
                buffer.removeSubrange(buffer.startIndex...newline)
                return Data(line)
            }
            guard buffer.count < maxLine else { return nil }
            var chunk = [UInt8](repeating: 0, count: 65536)
            let n = recv(fd, &chunk, chunk.count, 0)
            if n < 0 && errno == EINTR { continue }
            guard n > 0 else { return nil }
            buffer.append(contentsOf: chunk[0..<n])
        }
    }

    private func send(_ fd: Int32, _ data: Data) {
        var line = data
        line.append(0x0a)
        line.withUnsafeBytes { raw in
            var done = 0
            while done < raw.count {
                let n = Darwin.send(fd, raw.baseAddress!.advanced(by: done), raw.count - done, 0)
                if n < 0 && errno == EINTR { continue }
                if n <= 0 { return }
                done += n
            }
        }
    }

    private func reply(_ fd: Int32, ok: [String: Any]) {
        if let data = try? JSONSerialization.data(withJSONObject: ["ok": ok]) { send(fd, data) }
    }

    /// An answer whose body is already JSON (the engine's own encoding).
    private func reply(_ fd: Int32, okJSON body: Data) throws {
        var data = Data("{\"ok\":".utf8)
        data.append(body)
        data.append(Data("}".utf8))
        send(fd, data)
    }

    private func reply(_ fd: Int32, error message: String) {
        if let data = try? JSONSerialization.data(withJSONObject: ["error": ["message": message]]) { send(fd, data) }
    }
}

extension DeviceEngine {
    /// No split running: a restart can't cut one off.
    var idleForRestart: Bool {
        (try? snapshot())?.operation.map { $0.status != .running } ?? true
    }
}

func argument(_ name: String) -> String? {
    let args = CommandLine.arguments
    return args.firstIndex(of: name).flatMap { $0 + 1 < args.count ? args[$0 + 1] : nil }
}

signal(SIGPIPE, SIG_IGN)
do {
    // The engine runs each storage step in a worker it starts from this same
    // executable, released only by its supervising helper.
    if let job = argument("--worker") {
        guard let gate = argument("--gate-fd").flatMap(Int32.init), raf_wait_for_release(gate) == 0 else {
            throw ResizeError("Worker was not released by its supervising helper; no mutation was attempted.")
        }
        close(gate)
        try DeviceWorker.runFile(URL(fileURLWithPath: job))
        exit(0)
    }
    try Server().run()
} catch {
    fputs("Orchard companion stopped: \(error.localizedDescription)\n", stderr)
    exit(1)
}
