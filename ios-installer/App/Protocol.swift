// The helper's side of PROTOCOL.md: the view it returns and the socket it
// answers on.
import Foundation

struct InstallerView: Codable, Equatable {
    struct Device: Codable, Equatable {
        /// The model's name, once the board data has been read.
        var name: String?
        var kind: String
        var os: String
    }
    struct Welcome: Codable, Equatable {
        /// The storage, the release and the board are known.
        var checked: Bool
    }
    struct Partition: Codable, Equatable {
        var name: String
        var filesystem: String
        var bytes: Int64
    }
    struct Storage: Codable, Equatable {
        var totalBytes: Int64
        var iosBytes: Int64
        var iosUsedBytes: Int64
        var outsideBytes: Int64
        var outside: [Partition]
    }
    struct Split: Codable, Equatable {
        var minIosBytes: Int64?
        var maxIosBytes: Int64
        var suggestedIosBytes: Int64
        var neededBytes: Int64
        var fits: Bool
        var refused: String?
    }
    struct Step: Codable, Equatable {
        var title: String
        var state: String
    }
    struct Splitting: Codable, Equatable {
        var targetIosBytes: Int64
        var steps: [Step]
        var confirmedSecondsAgo: Double?
        var elapsedSeconds: Double
        var reconnecting: Bool
    }
    struct Account: Codable, Equatable {
        var user: String
        var download: String
        var erases: [Partition]
    }
    struct Installing: Codable, Equatable {
        var phase: String
        var percent: Double?
    }
    struct Installed: Codable, Equatable {
        var seconds: Double
    }
    struct Failed: Codable, Equatable {
        var message: String
        var detail: String?
    }

    var screen: String
    var system: String?
    /// Whether this device can be installed to, once that is known.
    var supported: Bool?
    var device: Device?
    var storage: Storage?
    var welcome: Welcome?
    var split: Split?
    var splitting: Splitting?
    var account: Account?
    var installing: Installing?
    var installed: Installed?
    var failed: Failed?

    static func decode(_ object: Any) -> InstallerView? {
        guard let data = try? JSONSerialization.data(withJSONObject: object) else { return nil }
        return decode(data)
    }
    static func decode(_ data: Data) -> InstallerView? {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try? decoder.decode(InstallerView.self, from: data)
    }
}

/// One request at a time over the helper's Unix socket, off the main thread.
final class HelperClient {
    enum Answer {
        /// The value of `ok`.
        case ok(Any)
        /// The message of `error`: the helper refused, nothing changed.
        case refused(String)
        /// No token, no socket, or no answer.
        case unreachable
    }

    #if targetEnvironment(simulator)
    // The simulator talks to a helper the developer runs on the Mac.
    static let usualDirectory = ProcessInfo.processInfo.environment["ORCHARD_HELPER_DIR"] ?? "/var/run/orchard-installer"
    #else
    static let usualDirectory = "/var/run/orchard-installer"
    #endif
    private let directory: String
    /// Runs its argument where the helper's socket can be opened, and returns after it: as
    /// root, for the app inside Dopamine (EmbeddedApp.swift). The open connection is used outside.
    private let elevate: ((() -> Void) -> Void)?

    init(directory: String = HelperClient.usualDirectory, elevate: ((() -> Void) -> Void)? = nil) {
        self.directory = directory
        self.elevate = elevate
    }
    private static let limit = 1 << 20
    private let queue = DispatchQueue(label: "org.orchardlinux.installer.helper")
    private var descriptor: Int32 = -1
    private var token = ""

    /// Sends `{"token": …, name: arguments}` and calls back on the main thread.
    /// Only `view` is sent again when the connection turns out to be stale;
    /// requests that change something are sent exactly once.
    func request(_ name: String, _ arguments: [String: Any] = [:], completion: @escaping (Answer) -> Void) {
        queue.async {
            var answer = self.exchange(name, arguments)
            if case .unreachable = answer, name == "view" { answer = self.exchange(name, arguments) }
            DispatchQueue.main.async { completion(answer) }
        }
    }

    private func exchange(_ name: String, _ arguments: [String: Any]) -> Answer {
        guard descriptor >= 0 || connect() else { return .unreachable }
        guard let body = try? JSONSerialization.data(withJSONObject: ["token": token, name: arguments]),
              body.count <= HelperClient.limit else { return .unreachable }
        var length = UInt32(body.count).bigEndian
        let header = Data(bytes: &length, count: 4)
        guard write(header + body), let prefix = read(4) else { disconnect(); return .unreachable }
        let count = prefix.reduce(0) { $0 << 8 | Int($1) }
        guard count <= HelperClient.limit, let data = read(count),
              let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { disconnect(); return .unreachable }
        if let value = object["ok"] { return .ok(value) }
        if let error = object["error"] as? [String: Any] { return .refused(error["message"] as? String ?? "") }
        disconnect()
        return .unreachable
    }

    private func connect() -> Bool {
        guard let elevate = elevate else { return open() }
        var opened = false
        elevate { opened = self.open() }
        return opened
    }

    private func open() -> Bool {
        // The helper makes the token anew each time it starts.
        guard let text = try? String(contentsOfFile: directory + "/client.token", encoding: .utf8) else { return false }
        token = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        var on: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &on, socklen_t(MemoryLayout<Int32>.size))
        var timeout = timeval(tv_sec: 15, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        withUnsafeMutablePointer(to: &address.sun_path) {
            $0.withMemoryRebound(to: CChar.self, capacity: capacity) { _ = strncpy($0, directory + "/helper.sock", capacity - 1) }
        }
        let result = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) }
        }
        guard result == 0 else { close(fd); return false }
        descriptor = fd
        return true
    }

    private func disconnect() {
        if descriptor >= 0 { close(descriptor) }
        descriptor = -1
    }

    private func write(_ data: Data) -> Bool {
        var sent = 0
        while sent < data.count {
            let n = data.withUnsafeBytes { Darwin.send(descriptor, $0.baseAddress! + sent, data.count - sent, 0) }
            if n <= 0 { return false }
            sent += n
        }
        return true
    }

    private func read(_ count: Int) -> Data? {
        var data = Data(count: count)
        var got = 0
        while got < count {
            let n = data.withUnsafeMutableBytes { Darwin.recv(descriptor, $0.baseAddress! + got, count - got, 0) }
            if n <= 0 { return nil }
            got += n
        }
        return data
    }
}
