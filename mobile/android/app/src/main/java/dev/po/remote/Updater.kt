package dev.po.remote

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.os.Build
import android.widget.Toast
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.security.MessageDigest

/** A newer app in Po's latest GitHub release. */
data class AppUpdate(val version: String, val apk: String, val name: String, val sums: String?, val page: String)

/**
 * The app updating itself from Po's GitHub releases, like the `po` binary does: the latest
 * release is read at launch; when it is newer, the home screen offers it; the APK is downloaded,
 * checked against the release's `SHA256SUMS.txt`, then installed through Android's package
 * installer, which asks the user (an app outside a store cannot install silently). Release builds only: a debug build
 * is signed with another key and could not take the update anyway.
 */
object Updater {
    /** `1.2.3` from `v1.2.3`; a pre-release (`1.2.3-rc.1`) is not one. */
    fun parse(text: String): List<Int>? {
        val clean = text.trim().removePrefix("v")
        if (clean.contains('-')) return null
        val parts = clean.split('.').map { it.toIntOrNull() ?: return null }
        return parts.takeIf { it.size == 3 }
    }

    fun newer(candidate: String, current: String): Boolean {
        val a = parse(candidate) ?: return false
        val b = parse(current) ?: return true
        for (i in 0 until 3) if (a[i] != b[i]) return a[i] > b[i]
        return false
    }

    /** The SHA-256 a `sha256sum` file gives for `name`. */
    fun expectedSum(sums: String, name: String): String? = sums.lineSequence().mapNotNull { line ->
        val parts = line.trim().split(Regex("\\s+"))
        if (parts.size >= 2 && parts[1].removePrefix("*") == name && parts[0].length == 64) parts[0].lowercase() else null
    }.firstOrNull()

    private fun get(url: String): HttpURLConnection = (URL(url).openConnection() as HttpURLConnection).apply {
        connectTimeout = 15_000
        readTimeout = 60_000
        setRequestProperty("Accept", "application/vnd.github+json")
        setRequestProperty("User-Agent", "po-android/${BuildConfig.VERSION_NAME}")
    }

    /** The latest release's app, whatever its version. Blocking: off the main thread. */
    fun latest(): AppUpdate? {
        val api = BuildConfig.UPDATE_API.ifEmpty { BuildConfig.GITHUB_API }
        val connection = get("$api/repos/${BuildConfig.UPSTREAM}/releases/latest")
        try {
            if (connection.responseCode != 200) return null
            val release = JSONObject(connection.inputStream.bufferedReader().readText())
            val assets = release.optJSONArray("assets") ?: return null
            var apk: Pair<String, String>? = null
            var sums: String? = null
            for (i in 0 until assets.length()) {
                val asset = assets.getJSONObject(i)
                val name = asset.optString("name")
                val url = asset.optString("browser_download_url")
                if (name.endsWith("-android.apk")) apk = name to url
                if (name == "SHA256SUMS.txt") sums = url
            }
            val (name, url) = apk ?: return null
            return AppUpdate(release.optString("tag_name").removePrefix("v"), url, name, sums, release.optString("html_url"))
        } finally {
            connection.disconnect()
        }
    }

    /** Downloads the APK into the app's cache and checks it; `progress` goes from 0 to 1. */
    fun download(context: Context, update: AppUpdate, progress: (Float) -> Unit): File {
        val expected = update.sums?.let { url ->
            val connection = get(url)
            try {
                expectedSum(connection.inputStream.bufferedReader().readText(), update.name)
            } finally {
                connection.disconnect()
            }
        } ?: throw IllegalStateException("no checksum for ${update.name}")
        val dir = File(context.cacheDir, "updates").apply { mkdirs() }
        dir.listFiles()?.forEach { it.delete() }
        val file = File(dir, update.name)
        val digest = MessageDigest.getInstance("SHA-256")
        val connection = get(update.apk)
        try {
            val total = connection.contentLengthLong
            connection.inputStream.use { input ->
                file.outputStream().use { output ->
                    val buffer = ByteArray(64 * 1024)
                    var read = 0L
                    while (true) {
                        val n = input.read(buffer)
                        if (n < 0) break
                        output.write(buffer, 0, n)
                        digest.update(buffer, 0, n)
                        read += n
                        if (total > 0) progress(read.toFloat() / total)
                    }
                }
            }
        } finally {
            connection.disconnect()
        }
        val actual = digest.digest().joinToString("") { "%02x".format(it) }
        if (actual != expected) {
            file.delete()
            throw IllegalStateException("checksum mismatch")
        }
        return file
    }

    /**
     * Hands the downloaded APK to Android's package installer, in a session of this app: Android
     * shows its own confirmation (no chooser among the phone's other installers), then replaces
     * the app. [UpdateReceiver] hears how it went.
     */
    fun install(context: Context, file: File) {
        val installer = context.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        params.setAppPackageName(context.packageName)
        val id = installer.createSession(params)
        installer.openSession(id).use { session ->
            session.openWrite("po.apk", 0, file.length()).use { out ->
                file.inputStream().use { it.copyTo(out) }
                session.fsync(out)
            }
            val intent = Intent(context, UpdateReceiver::class.java)
            val flags = PendingIntent.FLAG_UPDATE_CURRENT or (if (Build.VERSION.SDK_INT >= 31) PendingIntent.FLAG_MUTABLE else 0)
            session.commit(PendingIntent.getBroadcast(context, id, intent, flags).intentSender)
        }
    }
}

/** How an update's install goes: Android asks the user first, then reports the outcome. */
class UpdateReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                @Suppress("DEPRECATION")
                val confirm = intent.getParcelableExtra<Intent>(Intent.EXTRA_INTENT) ?: return
                context.startActivity(confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            }
            PackageInstaller.STATUS_SUCCESS -> {}
            else -> {
                val message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE).orEmpty()
                Toast.makeText(context, context.getString(R.string.update_failed, message), Toast.LENGTH_LONG).show()
            }
        }
    }
}
