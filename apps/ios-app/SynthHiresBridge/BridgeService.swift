import Foundation
import Combine
import Contacts
import CoreLocation
import CryptoKit
import UIKit

@MainActor
final class BridgeService: NSObject, ObservableObject, URLSessionWebSocketDelegate, CLLocationManagerDelegate {
    @Published var connected = false
    @Published var lastError: String?
    @Published var pendingConsent: ActionRequestFrame?

    let store: BridgeStore
    private var socket: URLSessionWebSocketTask?
    private var session: URLSession!
    private var heartbeat: Timer?
    private var pendingToken: String?
    private let locationManager = CLLocationManager()

    init(store: BridgeStore) {
        self.store = store
        super.init()
        locationManager.delegate = self
        session = URLSession(configuration: .default, delegate: self, delegateQueue: OperationQueue.main)
    }

    func start() {
        stop()
        guard let token = store.token(), let url = URL(string: store.backendURL + "/api/devices/ws") else {
            lastError = "No hay credenciales de pairing"
            return
        }
        pendingToken = token
        let request = NSMutableURLRequest(url: url)
        request.setValue("bearer.\(token)", forHTTPHeaderField: "Sec-WebSocket-Protocol")
        request.setValue(sha256(token), forHTTPHeaderField: "X-Bridge-Token-Hash")
        socket = session.webSocketTask(with: request as URLRequest)
        socket?.resume()
        receive()
    }

    func stop() {
        heartbeat?.invalidate()
        heartbeat = nil
        socket?.cancel(with: .normalClosure, reason: nil)
        socket = nil
        pendingToken = nil
        connected = false
    }

    func approveConsent(_ approved: Bool, remember: Bool = false) {
        guard let action = pendingConsent else { return }
        send(ConsentResponseFrame(v: bridgeProtocolVersion, id: action.id, approved: approved, remember: remember))
        pendingConsent = nil
        if !approved { sendResult(id: action.id, ok: false, error: "consent_denied") }
        else { handle(action, consentApproved: true) }
    }

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask, didOpenWithProtocol protocol: String?) {
        guard let token = pendingToken else { return }
        sendHello(token: token)
    }

    func urlSession(_ session: URLSession, webSocketTask: URLSessionWebSocketTask, didCloseWith closeCode: URLSessionWebSocketTask.CloseCode, reason: Data?) {
        connected = false
        heartbeat?.invalidate()
        heartbeat = nil
    }

    private func sendHello(token: String) {
        send(HelloFrame(
            v: bridgeProtocolVersion,
            tokenHash: sha256(token),
            fingerprint: sha256(UIDevice.current.identifierForVendor?.uuidString ?? UUID().uuidString),
            deviceName: UIDevice.current.name,
            clientVersion: "0.1.0",
            arch: "arm64",
        ))
    }

    private func receive() {
        socket?.receive { [weak self] result in
            Task { @MainActor in
                guard let self else { return }
                switch result {
                case .failure(let error):
                    self.connected = false
                    self.lastError = error.localizedDescription
                case .success(let message):
                    switch message {
                    case .string(let text): self.process(Data(text.utf8))
                    case .data(let data): self.process(data)
                    @unknown default: break
                    }
                    self.receive()
                }
            }
        }
    }

    private func process(_ data: Data) {
        switch jsonObject(data)["kind"] as? String {
        case "hello_ack":
            connected = true
            lastError = nil
            heartbeat?.invalidate()
            heartbeat = Timer.scheduledTimer(withTimeInterval: 30, repeats: true) { [weak self] _ in
                self?.send(HeartbeatFrame(v: bridgeProtocolVersion, t: UInt64(Date().timeIntervalSince1970 * 1000)))
            }
        case "scope_update":
            if let scopes = try? JSONDecoder().decode(ScopeUpdateFrame.self, from: data) { store.scopes = scopes.scopes }
        case "revoke": stop()
        case "action_request":
            if let action = try? JSONDecoder().decode(ActionRequestFrame.self, from: data) { handle(action) }
        default: break
        }
    }

    private func handle(_ action: ActionRequestFrame, consentApproved: Bool = false) {
        guard store.scopes.capabilities.contains(action.capability) else {
            sendResult(id: action.id, ok: false, error: "capability_not_granted"); return
        }
        let needsConsent = action.capability == "mobile.clipboard.write" || action.capability == "mobile.sms.send"
        if needsConsent && !action.skipConsentPrompt && !consentApproved { pendingConsent = action; return }
        let start = Date()
        do { sendResult(id: action.id, ok: true, output: try perform(action), duration: UInt64(Date().timeIntervalSince(start) * 1000)) }
        catch { sendResult(id: action.id, ok: false, error: error.localizedDescription, duration: UInt64(Date().timeIntervalSince(start) * 1000)) }
    }

    private func perform(_ action: ActionRequestFrame) throws -> JSONValue {
        switch action.capability {
        case "mobile.clipboard.read": return .object(["text": .string(UIPasteboard.general.string ?? "")])
        case "mobile.clipboard.write":
            let text = action.params["text"]?.stringValue ?? ""
            UIPasteboard.general.string = text
            return .object(["written": .bool(true), "length": .int(text.count)])
        case "mobile.contacts.read":
            let contacts = CNContactStore()
            var result: [JSONValue] = []
            var allowed = false
            let semaphore = DispatchSemaphore(value: 0)
            contacts.requestAccess(for: .contacts) { value, _ in allowed = value; semaphore.signal() }
            semaphore.wait()
            guard allowed else { throw BridgeError.permission("contacts") }
            let keys: [CNKeyDescriptor] = [CNContactGivenNameKey as NSString, CNContactFamilyNameKey as NSString, CNContactPhoneNumbersKey as NSString]
            try contacts.enumerateContacts(with: CNFetchRequest(keysToFetch: keys)) { contact, _ in
                result.append(.object(["name": .string("\(contact.givenName) \(contact.familyName)"), "phone": .string(contact.phoneNumbers.first?.value.stringValue ?? "")]))
            }
            return .object(["contacts": .array(result)])
        case "mobile.location.read":
            let auth = CLLocationManager.authorizationStatus()
            guard auth == .authorizedWhenInUse || auth == .authorizedAlways else { locationManager.requestWhenInUseAuthorization(); throw BridgeError.permission("location") }
            guard let location = locationManager.location else { throw BridgeError.unavailable("No hay ubicación reciente") }
            return .object(["latitude": .double(location.coordinate.latitude), "longitude": .double(location.coordinate.longitude), "accuracyMeters": .double(location.horizontalAccuracy), "at": .double(location.timestamp.timeIntervalSince1970 * 1000)])
        case "mobile.notifications.read", "mobile.notifications.dismiss", "mobile.sms.read", "mobile.sms.send", "mobile.automation.perform":
            throw BridgeError.unsupported("Esta capability no está disponible en iOS por las restricciones de Apple")
        default: throw BridgeError.unsupported("Capability no soportada en iOS")
        }
    }

    private func sendResult(id: String, ok: Bool, output: JSONValue? = nil, error: String? = nil, duration: UInt64 = 0) {
        send(ActionResultFrame(v: bridgeProtocolVersion, id: id, ok: ok, output: output, error: error.map { ActionError(code: "action_failed", message: $0) }, durationMs: duration))
    }

    private func send<T: Encodable>(_ value: T) {
        guard let data = try? jsonData(value), let text = String(data: data, encoding: .utf8) else { return }
        socket?.send(.string(text)) { [weak self] error in
            if let error { Task { @MainActor in self?.lastError = error.localizedDescription } }
        }
    }

    private func sha256(_ value: String) -> String { SHA256.hash(data: Data(value.utf8)).map { String(format: "%02x", $0) }.joined() }
}

enum BridgeError: LocalizedError {
    case permission(String)
    case unsupported(String)
    case unavailable(String)

    var errorDescription: String? {
        switch self {
        case .permission(let value): return "permission_required:\(value)"
        case .unsupported(let value), .unavailable(let value): return value
        }
    }
}
