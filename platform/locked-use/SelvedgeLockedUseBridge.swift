import Darwin
import Foundation
import OpenComputerUseKit
import Security

/// A single MCP run. The parent retains stdin until its native operations are
/// drained. It holds the same lease across all desktop actions in the run.
enum SelvedgeLockedUseBridge {
    private enum Command: String {
        case begin, check, finish
    }

    private enum State: String, Encodable {
        case ordinary, protected, ok, locked
    }

    private struct Reply: Encodable {
        let state: State?
        let error: String?
    }

    private enum Failure: LocalizedError {
        case protocolViolation, untrustedParent, sessionUnavailable, sessionChanged, anotherLease, recoveryUnconfirmed
        case interrupted(LockedUseStateMachine.StopReason?)

        var errorDescription: String? {
            switch self {
            case .protocolViolation: return "Invalid Locked Use bridge command."
            case .untrustedParent: return "Locked Use requires a hardened, same-team signed Selvedge CUA MCP parent."
            case .sessionUnavailable: return "Locked Use requires a completed login in the current macOS console session."
            case .sessionChanged: return "The macOS console session changed or locked during this run."
            case .anotherLease: return "Locked Use is active or recovering for another connection."
            case .recoveryUnconfirmed: return "Locked Use did not confirm both relocking and release of the protective shields."
            case .interrupted(let reason):
                return "The protected desktop session stopped before the run finished (\(reason?.rawValue ?? "unknown reason"))."
            }
        }
    }

    static func run() throws {
        // DEFENSE: The parent can exit while a reply is being written. Handle
        // EPIPE through the same lease cleanup path as stdin disconnection.
        signal(SIGPIPE, SIG_IGN)
        switch try readCommand() {
        case .begin: try serveRun()
        case .finish: try send(.ok)
        case .check: try send(error: Failure.protocolViolation)
        case nil: return
        }
    }

    private static func serveRun() throws {
        var connection: LockedUseConnection?
        var acquisitionAttempted = false
        var observer: BrokerObserver?
        var peer: Int32 = -1
        var monitor: DispatchSourceTimer?
        var finished = false
        let monitorQueue = DispatchQueue(label: "selvedge.locked-use.parent")
        defer {
            monitor?.cancel()
            monitorQueue.sync {}
            if !finished { connection?.end(disconnecting: true) }
            if peer >= 0 { Darwin.close(peer) }
        }

        var admittedSession: LockedUseSession?
        var protectedSession: LockedUseSession?
        let shouldFinish: Bool
        do {
            let parent = try authenticatedParent()
            let session = LockedUseSession.current()
            guard session.state != .unavailable else { throw Failure.sessionUnavailable }
            let broker = try BrokerObserver()
            observer = broker
            if session.state == .locked {
                try broker.requireIdle()
                guard LockedUseSession.current() == session else { throw Failure.sessionChanged }
                // The installed OCU app identity legitimately serves both roles.
                // Broker authenticates this process and independently verifies
                // the passed AF_UNIX peer with the kernel audit token.
                var descriptors: [Int32] = [-1, -1]
                guard socketpair(AF_UNIX, SOCK_STREAM, 0, &descriptors) == 0 else {
                    throw POSIXError(.init(rawValue: errno) ?? .EIO)
                }
                peer = descriptors[1]
                _ = fcntl(peer, F_SETFD, FD_CLOEXEC)
                let client = LockedUseConnection(clientSocket: descriptors[0])
                Darwin.close(descriptors[0])
                connection = client

                // Detect a closed parent pipe during native acquisition as
                // well as while the command reader is waiting for stdin.
                let source = DispatchSource.makeTimerSource(queue: monitorQueue)
                source.schedule(deadline: .now(), repeating: .milliseconds(100))
                source.setEventHandler {
                    var input = pollfd(fd: STDIN_FILENO, events: 0, revents: 0)
                    let closed = poll(&input, 1, 0) > 0 &&
                        input.revents & Int16(POLLHUP | POLLERR | POLLNVAL) != 0
                    if closed || getppid() != parent {
                        client.end(disconnecting: true)
                        source.cancel()
                    }
                }
                monitor = source
                source.resume()

                guard getppid() == parent else { throw Failure.untrustedParent }
                acquisitionAttempted = true
                protectedSession = session
                try client.acquire(session: session)
                shouldFinish = try client.withInFlightAction(requiresLease: true) {
                    try client.validateAction()
                    admittedSession = session
                    try send(.protected)
                    return try serveChecks {
                        guard getppid() == parent else { throw Failure.untrustedParent }
                        try client.validateAction()
                    }
                }
            } else {
                try checkOrdinary(session, broker: broker)
                admittedSession = session
                try send(.ordinary)
                shouldFinish = try serveChecks {
                    guard getppid() == parent else { throw Failure.untrustedParent }
                    try checkOrdinary(session, broker: broker)
                }
            }
        } catch {
            // DEFENSE: Loss of stdin/stdout after admission happens when the
            // MCP parent exits. The defer above disconnects the lease then.
            if admittedSession != nil { throw error }
            try send(error: error)
            shouldFinish = try serveChecks { throw Failure.protocolViolation }
        }

        guard shouldFinish else { return }
        guard acquisitionAttempted, let session = protectedSession, let client = connection, let broker = observer else {
            if let session = admittedSession, let broker = observer {
                do { try checkOrdinary(session, broker: broker) }
                catch { try send(.ok, error: error); return }
            }
            try send(.ok)
            return
        }

        // The parent sends finish only after all outstanding native work has
        // stopped. Relocking and guard release remain the Broker's decisions.
        let deadline = ProcessInfo.processInfo.systemUptime + 10
        let stopReason = client.end()
        finished = true
        do {
            try broker.requireRelocked(session, by: deadline)
            if admittedSession != nil, stopReason != .turnEnded {
                try send(.locked, error: Failure.interrupted(stopReason))
            } else {
                try send(.locked)
            }
        } catch {
            try send(error: error)
        }
    }

    private static func authenticatedParent() throws -> pid_t {
        let parent = getppid()
        guard parent > 1, let team = (try? LockedUseSigningIdentity.current())?.teamIdentifier else {
            throw Failure.untrustedParent
        }
        let expression = #"identifier "dev.selvedge.cua-mcp" and anchor apple generic and certificate leaf[subject.OU] = "\#(team)""#
        var guest: SecCode?
        var requirement: SecRequirement?
        let attributes = [kSecGuestAttributePid as String: NSNumber(value: parent)] as CFDictionary
        guard SecRequirementCreateWithString(expression as CFString, [], &requirement) == errSecSuccess,
              let requirement,
              SecCodeCopyGuestWithAttributes(nil, attributes, [], &guest) == errSecSuccess,
              let guest,
              SecCodeCheckValidity(guest, [], requirement) == errSecSuccess else {
            throw Failure.untrustedParent
        }

        var staticCode: SecStaticCode?
        var signingInfo: CFDictionary?
        let infoFlags = SecCSFlags(rawValue: kSecCSSigningInformation)
        guard SecCodeCopyStaticCode(guest, [], &staticCode) == errSecSuccess,
              let staticCode,
              SecCodeCopySigningInformation(staticCode, infoFlags, &signingInfo) == errSecSuccess,
              let info = signingInfo as? [String: Any],
              let flags = info[kSecCodeInfoFlags as String] as? NSNumber,
              flags.uint32Value & SecCodeSignatureFlags.runtime.rawValue != 0,
              !hasInjectionEntitlements(info),
              SecCodeCheckValidity(guest, [], requirement) == errSecSuccess,
              getppid() == parent, kill(parent, 0) == 0 else {
            throw Failure.untrustedParent
        }
        return parent
    }

    private static func hasInjectionEntitlements(_ signingInfo: [String: Any]) -> Bool {
        let entitlements = signingInfo[kSecCodeInfoEntitlementsDict as String] as? [String: Any] ?? [:]
        return [
            "com.apple.security.get-task-allow", "get-task-allow",
            "com.apple.security.cs.disable-library-validation",
            "com.apple.security.cs.allow-dyld-environment-variables",
            "com.apple.security.cs.allow-unsigned-executable-memory",
            "com.apple.security.cs.disable-executable-page-protection",
        ].contains { (entitlements[$0] as? NSNumber)?.boolValue == true }
    }

    private static func checkOrdinary(_ session: LockedUseSession, broker: BrokerObserver) throws {
        guard LockedUseSession.current() == session else { throw Failure.sessionChanged }
        try broker.requireIdle(observedSession: session)
        guard LockedUseSession.current() == session else { throw Failure.sessionChanged }
    }

    private static func serveChecks(_ validate: () throws -> Void) throws -> Bool {
        while let command = try readCommand() {
            switch command {
            case .finish:
                return true
            case .check:
                do {
                    try validate()
                } catch {
                    try send(error: error)
                    continue
                }
                try send(.ok)
            case .begin:
                try send(error: Failure.protocolViolation)
            }
        }
        return false
    }

    private static func readCommand() throws -> Command? {
        var line: [UInt8] = []
        while line.count <= 6 {
            var byte: UInt8 = 0
            let count = Darwin.read(STDIN_FILENO, &byte, 1)
            if count == 0 { return nil }
            // DEFENSE: A signal can interrupt a blocking stdin read.
            if count < 0 && errno == EINTR { continue }
            guard count == 1 else { throw POSIXError(.init(rawValue: errno) ?? .EIO) }
            if byte == 10 {
                guard let command = Command(rawValue: String(decoding: line, as: UTF8.self)) else {
                    throw Failure.protocolViolation
                }
                return command
            }
            line.append(byte)
        }
        throw Failure.protocolViolation
    }

    private static func send(_ state: State, error: Error? = nil) throws {
        try send(.init(state: state, error: error?.localizedDescription))
    }

    private static func send(error: Error) throws {
        try send(.init(state: nil, error: error.localizedDescription))
    }

    private static func send(_ reply: Reply) throws {
        var bytes = try JSONEncoder().encode(reply)
        bytes.append(10)
        try FileHandle.standardOutput.write(contentsOf: bytes)
    }

    private final class BrokerObserver {
        private let requirement: String
        private var client: LockedUseIPCClient?

        init() throws {
            // An ad-hoc binary cannot select or trust a Broker signing team.
            requirement = try LockedUseSigningIdentity.brokerRequirement()
        }

        func requireIdle(observedSession: LockedUseSession? = nil) throws {
            let reply = try status(session: observedSession)
            guard reply.result == .ok, reply.phase == .idle,
                  reply.guardsReleased == true else { throw Failure.anotherLease }
        }

        func requireRelocked(_ session: LockedUseSession, by deadline: TimeInterval) throws {
            while ProcessInfo.processInfo.systemUptime < deadline {
                let reply = try status()
                let current = LockedUseSession.current()
                if reply.result != .denied, reply.guardsReleased == true,
                   reply.phase == .idle || reply.phase == .awaitingManualUnlock,
                   current == session {
                    return
                }
                Thread.sleep(forTimeInterval: 0.1)
            }
            throw Failure.recoveryUnconfirmed
        }

        private func status(session: LockedUseSession? = nil) throws -> LockedUseIPCReply {
            if client == nil {
                client = try LockedUseIPCClient(endpoint: .observer, brokerRequirement: requirement)
            }
            guard let client else { throw Failure.sessionUnavailable }
            return try client.request(.init(operation: .status, session: session))
        }
    }
}
