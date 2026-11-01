package dev.po.remote

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import uniffi.po_mobile.SecretStore
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The phone's secrets (its private key, its GitHub token, the directory's keys), each in a file
 * encrypted with AES-256-GCM under a key of the Android Keystore. That key is made inside the
 * Keystore and never leaves it: a copy of the app's folder (a backup, a rooted read of the
 * files) gives only ciphertext. The secret's name is bound to its ciphertext (associated data),
 * so a file renamed does not open as another secret.
 */
class KeystoreSecrets(private val dir: File) : SecretStore {
    private val lock = Any()

    private fun key(): SecretKey {
        val store = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        (store.getEntry(ALIAS, null) as? KeyStore.SecretKeyEntry)?.let { return it.secretKey }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                // The service keeps the link while the screen is locked: no user authentication.
                .setUserAuthenticationRequired(false)
                .build()
        )
        return generator.generateKey()
    }

    private fun file(name: String) = File(dir, "$name.sealed")

    override fun load(name: String): ByteArray? = synchronized(lock) {
        val bytes = runCatching { file(name).readBytes() }.getOrNull() ?: return null
        if (bytes.size < IV + 16) return null
        runCatching {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, bytes, 0, IV))
            cipher.updateAAD(name.toByteArray())
            cipher.doFinal(bytes, IV, bytes.size - IV)
        }.getOrNull()
    }

    override fun save(name: String, value: ByteArray): Boolean = synchronized(lock) {
        runCatching {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            // The Keystore picks the nonce itself (it refuses one given from outside).
            cipher.init(Cipher.ENCRYPT_MODE, key())
            cipher.updateAAD(name.toByteArray())
            val sealed = cipher.iv + cipher.doFinal(value)
            dir.mkdirs()
            val temporary = File(dir, "$name.tmp")
            temporary.writeBytes(sealed)
            temporary.renameTo(file(name)) || run { file(name).delete(); temporary.renameTo(file(name)) }
        }.getOrDefault(false)
    }

    override fun remove(name: String) {
        synchronized(lock) { file(name).delete() }
    }

    private companion object {
        const val KEYSTORE = "AndroidKeyStore"
        const val ALIAS = "po-secrets"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val IV = 12
    }
}
