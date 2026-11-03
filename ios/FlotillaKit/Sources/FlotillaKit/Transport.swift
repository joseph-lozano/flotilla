import Foundation

/// Why a connection couldn't be made or ended.
public enum TransportError: Error, Sendable, Hashable, CustomStringConvertible {
    /// The server doesn't know this device's token (401): pair it again.
    case unauthorized
    /// The server speaks another protocol (426).
    case otherProtocol
    /// Anything else: try again later.
    case failed(String)

    public var description: String {
        switch self {
        case .unauthorized: "This device isn't paired with this server any more. Pair it again."
        case .otherProtocol: "This server runs a different version of Flotilla. Update the app or the server."
        case .failed(let why): why
        }
    }
}

/// A WebSocket to a server, carrying text frames.
public protocol WebSocket: Sendable {
    func send(_ text: String) async throws(TransportError)
    /// The next text frame (other frames are skipped).
    func receive() async throws(TransportError) -> String
    /// Ping, returning when the pong comes.
    func ping() async throws(TransportError)
    func close()
}

/// How a client opens its WebSocket: `URLSessionTransport` in the app, a stand-in in tests.
public protocol Transport: Sendable {
    func open(_ request: URLRequest) async throws(TransportError) -> any WebSocket
}

/// The system's WebSocket (`URLSessionWebSocketTask`): TLS against the system's trusted
/// certificates for an `https://` server (`tailscale serve`), plain for `http://`.
public struct URLSessionTransport: Transport {
    private let session: URLSession

    public init() {
        let config = URLSessionConfiguration.ephemeral
        config.waitsForConnectivity = false
        // Nothing is cached or kept: the token goes in a header on each upgrade.
        config.urlCache = nil
        config.httpCookieStorage = nil
        session = URLSession(configuration: config)
    }

    public func open(_ request: URLRequest) async throws(TransportError) -> any WebSocket {
        let task = session.webSocketTask(with: request)
        // A thread's first view can be large (its whole history): more than the default 1 MB.
        task.maximumMessageSize = 64 << 20
        task.resume()
        return URLSessionSocket(task: task)
    }
}

final class URLSessionSocket: WebSocket {
    private let task: URLSessionWebSocketTask

    init(task: URLSessionWebSocketTask) {
        self.task = task
    }

    func send(_ text: String) async throws(TransportError) {
        do {
            try await task.send(.string(text))
        } catch {
            throw failure(error)
        }
    }

    func receive() async throws(TransportError) -> String {
        while true {
            let message: URLSessionWebSocketTask.Message
            do {
                message = try await task.receive()
            } catch {
                throw failure(error)
            }
            if case .string(let text) = message { return text }
        }
    }

    func ping() async throws(TransportError) {
        let task = self.task
        let result: TransportError? = await withCheckedContinuation { c in
            task.sendPing { error in c.resume(returning: error.map { TransportError.failed($0.localizedDescription) }) }
        }
        if let result { throw result }
    }

    func close() {
        task.cancel(with: .normalClosure, reason: nil)
    }

    /// What went wrong, from the upgrade's answer when there was one.
    private func failure(_ error: any Error) -> TransportError {
        if let http = task.response as? HTTPURLResponse {
            switch http.statusCode {
            case 401: return .unauthorized
            case 426: return .otherProtocol
            case 101: break
            case let code: return .failed("The server said \(code)")
            }
        }
        if task.closeCode != .invalid, task.closeCode != .abnormalClosure {
            return .failed("The server closed the connection")
        }
        let host = task.originalRequest?.url?.host() ?? "the server"
        let ns = error as NSError
        if ns.domain == NSURLErrorDomain {
            switch ns.code {
            case NSURLErrorCannotConnectToHost, NSURLErrorCannotFindHost, NSURLErrorTimedOut, NSURLErrorNotConnectedToInternet, NSURLErrorNetworkConnectionLost:
                return .failed("Can't reach \(host): \(ns.localizedDescription)")
            case NSURLErrorSecureConnectionFailed, NSURLErrorServerCertificateUntrusted, NSURLErrorServerCertificateHasBadDate, NSURLErrorServerCertificateNotYetValid:
                return .failed("Can't reach \(host) securely: \(ns.localizedDescription)")
            default: break
            }
        }
        return .failed(ns.localizedDescription)
    }
}
