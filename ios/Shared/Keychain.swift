import Foundation
import Security

/// Legacy Keychain reader, kept only to migrate credentials that older builds stored
/// in the Keychain into the shared App Group (see `Settings`).
///
/// Why we no longer *store* here: an app and its extensions only share Keychain items
/// via a `keychain-access-groups` entitlement, which needs valid provisioning to take
/// effect on a real device. When it doesn't (free/misconfigured signing), the widget
/// reads an empty token and renders only "--". The App Group is the reliable channel
/// for app→widget config and already carries `baseURL`/`unit`, so credentials live
/// there now too. This reads the app's *default* Keychain group (no access group) to
/// recover anything a previous version saved.
enum Keychain {
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
