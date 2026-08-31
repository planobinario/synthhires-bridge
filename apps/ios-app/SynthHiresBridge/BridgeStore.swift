import Foundation
import Combine
import Security

final class BridgeStore: ObservableObject {
    @Published var backendURL = "https://synthhires.com"
    @Published var deviceID: String?
    @Published var connected = false
    @Published var scopes = BridgeScopes(capabilities: [], alwaysAllowPaths: [])

    private let service = "synthhires:bridge:keychain"
    private let defaults = UserDefaults.standard
    private enum Key {
        static let backend = "synthhires:bridge:backendURL"
        static let device = "synthhires:bridge:deviceID"
        static let scopes = "synthhires:bridge:scopes"
    }

    init() {
        backendURL = defaults.string(forKey: Key.backend) ?? backendURL
        deviceID = defaults.string(forKey: Key.device)
        if let data = defaults.data(forKey: Key.scopes), let decoded = try? JSONDecoder().decode(BridgeScopes.self, from: data) { scopes = decoded }
    }

    func savePair(deviceID: String, token: String, backendURL: String, scopes: BridgeScopes) {
        self.deviceID = deviceID
        self.backendURL = backendURL.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        self.scopes = scopes
        defaults.set(deviceID, forKey: Key.device)
        defaults.set(self.backendURL, forKey: Key.backend)
        defaults.set(try? JSONEncoder().encode(scopes), forKey: Key.scopes)
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: deviceID,
            kSecValueData as String: Data(token.utf8),
        ]
        SecItemDelete(query as CFDictionary)
        SecItemAdd(query as CFDictionary, nil)
    }

    func token() -> String? {
        guard let id = deviceID else { return nil }
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: id,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var result: AnyObject?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess, let data = result as? Data else { return nil }
        return String(data: data, encoding: .utf8)
    }
}
