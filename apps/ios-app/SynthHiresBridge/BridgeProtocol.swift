import Foundation

let bridgeProtocolVersion = 1

struct HelloFrame: Codable {
    let v: Int
    let kind = "hello"
    let tokenHash: String
    let fingerprint: String
    let deviceKind = "mobile"
    let deviceName: String
    let clientVersion: String
    let os = "ios"
    let arch: String
}

struct HeartbeatFrame: Codable {
    let v: Int
    let kind = "heartbeat"
    let t: UInt64
}

struct ActionRequestFrame: Codable {
    let v: Int
    let kind: String
    let id: String
    let capability: String
    let params: [String: JSONValue]
    let conversationId: String?
    let skipConsentPrompt: Bool
}

struct ActionResultFrame: Codable {
    let v: Int
    let kind = "action_result"
    let id: String
    let ok: Bool
    let output: JSONValue?
    let error: ActionError?
    let durationMs: UInt64
}

struct ActionError: Codable { let code: String; let message: String }
struct ConsentResponseFrame: Codable { let v: Int; let kind = "consent_response"; let id: String; let approved: Bool; let remember: Bool }
struct ScopeUpdateFrame: Codable { let scopes: BridgeScopes }
struct BridgeScopes: Codable { let capabilities: [String]; let alwaysAllowPaths: [String] }

enum JSONValue: Codable, Equatable {
    case string(String)
    case int(Int)
    case double(Double)
    case bool(Bool)
    case object([String: JSONValue])
    case array([JSONValue])
    case null

    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let value = try? c.decode(Bool.self) { self = .bool(value) }
        else if let value = try? c.decode(Int.self) { self = .int(value) }
        else if let value = try? c.decode(Double.self) { self = .double(value) }
        else if let value = try? c.decode(String.self) { self = .string(value) }
        else if let value = try? c.decode([String: JSONValue].self) { self = .object(value) }
        else if let value = try? c.decode([JSONValue].self) { self = .array(value) }
        else { self = .null }
    }

    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .string(let value): try c.encode(value)
        case .int(let value): try c.encode(value)
        case .double(let value): try c.encode(value)
        case .bool(let value): try c.encode(value)
        case .object(let value): try c.encode(value)
        case .array(let value): try c.encode(value)
        case .null: try c.encodeNil()
        }
    }

    var stringValue: String? { if case .string(let value) = self { return value }; return nil }
    var intValue: Int? { if case .int(let value) = self { return value }; return nil }
}

func jsonData<T: Encodable>(_ value: T) throws -> Data { try JSONEncoder().encode(value) }
func jsonObject(_ data: Data) -> [String: Any] { (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] ?? [:] }
