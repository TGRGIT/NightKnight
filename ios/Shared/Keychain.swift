import Foundation
import Security

/// Minimal Keychain wrapper for the few secrets the app stores (device token, CF
/// Access service-token id/secret). Shared with the widget via an access group is
/// possible later; for now each target keeps its own.
enum Keychain {
    static func set(_ key: String, _ value: String) {
        let data = Data(value.utf8)
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrAccount as String: key,
        ]
        SecItemDelete(query as CFDictionary)
        var attrs = query
        attrs[kSecValueData as String] = data
        // `ThisDeviceOnly` keeps the token/secret out of encrypted backups and iCloud
        // Keychain sync, so the credential can't leave this device. Still readable in
        // the background (after first unlock) for the widget/watch. It won't migrate on
        // device restore — acceptable, since a device token is simply re-issued.
        attrs[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        SecItemAdd(attrs as CFDictionary, nil)
    }

    static func get(_ key: String) -> String {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrAccount as String: key,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
              let data = item as? Data, let s = String(data: data, encoding: .utf8)
        else { return "" }
        return s
    }
}
