package dev.po.remote

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

/** On a device: the secrets are sealed by the Keystore, and only open under their own name. */
@RunWith(AndroidJUnit4::class)
class KeystoreSecretsTest {
    private val dir = File(InstrumentationRegistry.getInstrumentation().targetContext.cacheDir, "secrets-test").apply { deleteRecursively() }
    private val secrets = KeystoreSecrets(dir)

    @Test
    fun aSecretComesBackAndIsNotInClearOnDisk() {
        val token = "gho_secret_token_123".toByteArray()
        assertTrue(secrets.save("github", token))
        assertArrayEquals(token, secrets.load("github"))
        val onDisk = File(dir, "github.sealed").readBytes()
        assertFalse(String(onDisk, Charsets.ISO_8859_1).contains("gho_secret"))
        // A fresh instance (the app relaunched) opens it with the same Keystore key.
        assertArrayEquals(token, KeystoreSecrets(dir).load("github"))
    }

    @Test
    fun aRenamedOrAlteredFileDoesNotOpen() {
        assertTrue(secrets.save("github", "abc".toByteArray()))
        File(dir, "github.sealed").copyTo(File(dir, "identity.sealed"), overwrite = true)
        assertNull("bound to its name", secrets.load("identity"))
        val bytes = File(dir, "github.sealed").readBytes()
        bytes[bytes.size - 1] = (bytes[bytes.size - 1].toInt() xor 1).toByte()
        File(dir, "github.sealed").writeBytes(bytes)
        assertNull("altered", secrets.load("github"))
    }

    @Test
    fun removedIsGone() {
        assertTrue(secrets.save("directory", "{}".toByteArray()))
        secrets.remove("directory")
        assertNull(secrets.load("directory"))
    }
}
