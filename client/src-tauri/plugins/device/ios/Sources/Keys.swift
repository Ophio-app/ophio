// SPDX-License-Identifier: Apache-2.0
//
// The pairing key's protection. The iPhone has no Ed25519 in its Secure
// Enclave, so `create` answers with no public key and the app falls back, as
// on Android phones without hardware Ed25519, to a seed the Secure Enclave
// seals: an enclave P-256 key, usable only on this phone while it's unlocked,
// encrypts the seed with ECIES. The sealed seed is opaque to the app.

import Foundation
import Security

enum Keys {
  private static let lock = NSLock()
  /// ECIES with a P-256 enclave key: the seed only opens on this phone.
  private static let algorithm = SecKeyAlgorithm.eciesEncryptionCofactorVariableIVX963SHA256AESGCM
  /// Leads a sealed seed, so a later format can be told apart.
  private static let version: UInt8 = 1

  private static func tag(_ alias: String, wrap: Bool) throws -> Data {
    let allowed = alias.unicodeScalars.allSatisfy {
      ("a"..."z").contains($0) || ("A"..."Z").contains($0) || ("0"..."9").contains($0)
        || $0 == "-" || $0 == "_" || $0 == "."
    }
    guard !alias.isEmpty, alias.utf8.count <= 128, allowed else { throw Failure("Invalid key alias.") }
    return Data("app.ophio.\(alias)\(wrap ? ".wrap" : "")".utf8)
  }

  private static func query(_ tag: Data) -> [String: Any] {
    [
      kSecClass as String: kSecClassKey,
      kSecAttrApplicationTag as String: tag,
      kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
    ]
  }

  private static func exists(_ tag: Data) -> Bool {
    SecItemCopyMatching(query(tag) as CFDictionary, nil) == errSecSuccess
  }

  private static func remove(_ tag: Data) -> Bool {
    let status = SecItemDelete(query(tag) as CFDictionary)
    return status == errSecSuccess || status == errSecItemNotFound
  }

  /// No hardware Ed25519 here: nil, so the app seals a seed with `wrap`.
  static func create(_ alias: String) throws -> String? {
    let plain = try tag(alias, wrap: false)
    let wrapped = try tag(alias, wrap: true)
    try lock.sync {
      guard !exists(plain), !exists(wrapped) else { throw Failure("The key already exists.") }
    }
    return nil
  }

  static func wrap(_ alias: String, _ seed: Data) throws -> String {
    guard seed.count == 32 else { throw Failure("Invalid seed.") }
    let tag = try tag(alias, wrap: true)
    return try lock.sync {
      guard !exists(tag) else { throw Failure("The key already exists.") }
      var error: Unmanaged<CFError>?
      #if targetEnvironment(simulator)
        // The simulator has no Secure Enclave; its keys stay in its keychain.
        let attributes: [String: Any] = [
          kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
          kSecAttrKeySizeInBits as String: 256,
          kSecPrivateKeyAttrs as String: [
            kSecAttrIsPermanent as String: true,
            kSecAttrApplicationTag as String: tag,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
          ] as [String: Any],
        ]
      #else
        guard
          let access = SecAccessControlCreateWithFlags(
            nil, kSecAttrAccessibleWhenUnlockedThisDeviceOnly, .privateKeyUsage, &error)
        else { throw Failure("The Secure Enclave is unavailable.") }
        let attributes: [String: Any] = [
          kSecAttrKeyType as String: kSecAttrKeyTypeECSECPrimeRandom,
          kSecAttrKeySizeInBits as String: 256,
          kSecAttrTokenID as String: kSecAttrTokenIDSecureEnclave,
          kSecPrivateKeyAttrs as String: [
            kSecAttrIsPermanent as String: true,
            kSecAttrApplicationTag as String: tag,
            kSecAttrAccessControl as String: access,
          ] as [String: Any],
        ]
      #endif
      guard let key = SecKeyCreateRandomKey(attributes as CFDictionary, &error),
        let open = SecKeyCopyPublicKey(key),
        SecKeyIsAlgorithmSupported(open, .encrypt, algorithm),
        let sealed = SecKeyCreateEncryptedData(open, algorithm, seed as CFData, &error) as Data?
      else {
        _ = remove(tag)
        throw Failure("The seed could not be sealed.")
      }
      return Base64URL.encode(Data([version]) + sealed)
    }
  }

  static func unwrap(_ alias: String, _ sealed: String) throws -> Data {
    let bytes = try Base64URL.decode(sealed)
    guard bytes.count > 1, bytes.count <= 1024, bytes.first == version else {
      throw Failure("Invalid sealed seed.")
    }
    let tag = try tag(alias, wrap: true)
    return try lock.sync {
      var search = query(tag)
      search[kSecAttrKeyClass as String] = kSecAttrKeyClassPrivate
      search[kSecReturnRef as String] = true
      var item: CFTypeRef?
      guard SecItemCopyMatching(search as CFDictionary, &item) == errSecSuccess,
        let found = item, CFGetTypeID(found) == SecKeyGetTypeID()
      else { throw Failure("The key is missing.") }
      let key = found as! SecKey
      var error: Unmanaged<CFError>?
      guard
        let seed = SecKeyCreateDecryptedData(key, algorithm, Data(bytes.dropFirst()) as CFData, &error)
          as Data?,
        seed.count == 32
      else { throw Failure("The seed could not be opened.") }
      return seed
    }
  }

  static func delete(_ alias: String) throws {
    let plain = try tag(alias, wrap: false)
    let wrapped = try tag(alias, wrap: true)
    try lock.sync {
      guard remove(plain), remove(wrapped) else { throw Failure("The key could not be removed.") }
    }
  }
}
