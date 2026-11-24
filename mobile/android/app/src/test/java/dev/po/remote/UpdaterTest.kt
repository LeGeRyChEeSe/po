package dev.po.remote

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class UpdaterTest {
    @Test
    fun versionsAreReadFromTags() {
        assertEquals(listOf(0, 2, 0), Updater.parse("v0.2.0"))
        assertNull("a pre-release is not offered", Updater.parse("v1.0.0-rc.1"))
        assertNull(Updater.parse("1.0"))
    }

    @Test
    fun onlyANewerVersionIsOffered() {
        assertTrue(Updater.newer("0.10.0", "0.9.9"))
        assertTrue(Updater.newer("v1.0.0", "0.9.0"))
        assertFalse(Updater.newer("0.1.0", "0.1.0"))
        assertFalse(Updater.newer("0.0.9", "0.1.0"))
        assertFalse(Updater.newer("1.0.0-rc.1", "0.1.0"))
    }

    @Test
    fun theApkSumIsFound() {
        val sums = "${"a".repeat(64)}  po-v1.0.0-x86_64-unknown-linux-gnu.tar.gz\n${"B".repeat(64)}  po-v1.0.0-android.apk\n"
        assertEquals("b".repeat(64), Updater.expectedSum(sums, "po-v1.0.0-android.apk"))
        assertNull(Updater.expectedSum(sums, "other.apk"))
    }
}
