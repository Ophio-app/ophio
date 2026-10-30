// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.Signature
import java.security.spec.ECGenParameterSpec
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

internal object Keys {
    private const val PREFIX = "device."

    private fun store() = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }

    private fun alias(id: String): String {
        require(id.matches(Regex("[0-9a-f-]{36}"))) { "Invalid device key reference." }
        return PREFIX + id
    }

    @Synchronized
    fun create(id: String): String? {
        val name = alias(id)
        check(!store().containsAlias(name) && !store().containsAlias("$name.wrap"))
        if (Build.VERSION.SDK_INT < 33) return null
        try {
            val generator = KeyPairGenerator.getInstance("EC", "AndroidKeyStore")
            generator.initialize(
                KeyGenParameterSpec.Builder(
                        name,
                        KeyProperties.PURPOSE_SIGN or KeyProperties.PURPOSE_VERIFY,
                    )
                    .setAlgorithmParameterSpec(ECGenParameterSpec("ed25519"))
                    .setDigests(KeyProperties.DIGEST_NONE)
                    .build()
            )
            val pair = generator.generateKeyPair()
            check(pair.private.encoded == null) { "Device key must not be exportable." }
            val public = pair.public.encoded
            // RFC 8410 SubjectPublicKeyInfo: Ed25519 OID, no parameters, 32-byte BIT STRING.
            val prefix =
                byteArrayOf(0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00)
            check(public.size == 44 && public.copyOfRange(0, 12).contentEquals(prefix))
            // Some vendors accept generation but cannot sign with the curve.
            val probe = "device-key-check".toByteArray()
            val signed =
                Signature.getInstance("Ed25519").run {
                    initSign(pair.private)
                    update(probe)
                    sign()
                }
            check(
                Signature.getInstance("Ed25519").run {
                    initVerify(pair.public)
                    update(probe)
                    verify(signed)
                }
            )
            return encode(public.copyOfRange(12, 44))
        } catch (_: Exception) {
            // Vendor curve probes may also fail with ProviderException.
            // Remove the unusable key before trying non-exportable AES wrapping.
            store().deleteEntry(name)
            return null
        }
    }

    @Synchronized
    fun sign(id: String, transcript: ByteArray): String {
        require(transcript.size <= 4096)
        val private =
            store().getKey(alias(id), null) as? java.security.PrivateKey
                ?: error("Device key is missing.")
        check(private.encoded == null)
        return encode(
            Signature.getInstance("Ed25519").run {
                initSign(private)
                update(transcript)
                sign()
            }
        )
    }

    @Synchronized
    fun wrap(id: String, seed: ByteArray): String {
        require(seed.size == 32)
        val name = alias(id) + ".wrap"
        check(!store().containsAlias(name))
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        generator.init(
            KeyGenParameterSpec.Builder(
                    name,
                    KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
                )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setRandomizedEncryptionRequired(true)
                .setKeySize(256)
                .build()
        )
        val key = generator.generateKey()
        check(key.encoded == null)
        return try {
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.ENCRYPT_MODE, key)
            cipher.updateAAD(alias(id).toByteArray(Charsets.UTF_8))
            val encrypted = cipher.doFinal(seed)
            check(cipher.iv.size == 12)
            encode(byteArrayOf(1) + cipher.iv + encrypted)
        } finally {
            seed.fill(0)
        }
    }

    @Synchronized
    fun unwrap(id: String, sealed: ByteArray): ByteArray {
        require(sealed.size == 61 && sealed[0] == 1.toByte())
        val key =
            store().getKey(alias(id) + ".wrap", null) as? SecretKey
                ?: error("Wrapping key is missing.")
        check(key.encoded == null)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, sealed.copyOfRange(1, 13)))
        cipher.updateAAD(alias(id).toByteArray(Charsets.UTF_8))
        return cipher.doFinal(sealed, 13, 48)
    }

    @Synchronized
    fun delete(id: String) {
        val name = alias(id)
        store().deleteEntry(name)
        store().deleteEntry("$name.wrap")
    }

    fun encode(bytes: ByteArray): String =
        Base64.encodeToString(bytes, Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING)

    fun decode(text: String): ByteArray =
        Base64.decode(text, Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING)
}
