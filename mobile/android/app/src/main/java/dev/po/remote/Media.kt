package dev.po.remote

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.net.Uri
import android.provider.OpenableColumns
import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Records the microphone as whisper wants it — 16 kHz, mono, 16-bit — and gives it as a WAV
 * file, for the PC to transcribe. At most three minutes.
 */
class VoiceRecorder(private val context: Context) {
    private var record: AudioRecord? = null
    private var thread: Thread? = null
    private val pcm = ByteArrayOutputStream()
    @Volatile private var running = false

    @SuppressLint("MissingPermission")
    fun start() {
        if (context.checkSelfPermission(android.Manifest.permission.RECORD_AUDIO) != android.content.pm.PackageManager.PERMISSION_GRANTED) {
            throw IllegalStateException(context.getString(R.string.voice_permission))
        }
        val min = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        val size = maxOf(min, RATE) // half a second of room
        val audio = AudioRecord(MediaRecorder.AudioSource.VOICE_RECOGNITION, RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT, size)
        if (audio.state != AudioRecord.STATE_INITIALIZED) {
            audio.release()
            throw IllegalStateException("AudioRecord")
        }
        record = audio
        running = true
        audio.startRecording()
        thread = Thread {
            val buffer = ByteArray(size)
            while (running) {
                val n = audio.read(buffer, 0, buffer.size)
                if (n > 0) synchronized(pcm) { if (pcm.size() < MAX_BYTES) pcm.write(buffer, 0, n) }
            }
        }.apply { isDaemon = true; start() }
    }

    /** Stops; the WAV recorded. */
    fun stop(): ByteArray {
        running = false
        thread?.join(1000)
        record?.runCatching { stop(); release() }
        record = null
        val body = synchronized(pcm) { pcm.toByteArray() }
        return wav(body)
    }

    private fun wav(body: ByteArray): ByteArray {
        val header = ByteBuffer.allocate(44).order(ByteOrder.LITTLE_ENDIAN)
        header.put("RIFF".toByteArray()).putInt(36 + body.size).put("WAVE".toByteArray())
        header.put("fmt ".toByteArray()).putInt(16).putShort(1).putShort(1).putInt(RATE).putInt(RATE * 2).putShort(2).putShort(16)
        header.put("data".toByteArray()).putInt(body.size)
        return header.array() + body
    }

    companion object {
        const val RATE = 16000
        const val MAX_BYTES = RATE * 2 * 180
    }
}

/** A file chosen on the phone, read to be sent: a photo made smaller, the rest as it is. */
object Uploads {
    class File(val name: String, val bytes: ByteArray, val image: Boolean)

    /** What the PC accepts (`po_wire::MAX_UPLOAD`). */
    private const val MAX = 10 * 1024 * 1024
    /** A photo's longest side: what a model reads well, a fraction of the camera's size. */
    private const val SIDE = 1600

    fun read(context: Context, uri: Uri): File {
        val resolver = context.contentResolver
        val type = resolver.getType(uri) ?: ""
        var name = resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        } ?: uri.lastPathSegment?.substringAfterLast('/') ?: "file"
        if (type.startsWith("image/") && type != "image/gif" && type != "image/svg+xml") {
            val bytes = resolver.openInputStream(uri)?.use { it.readBytes() } ?: throw IllegalStateException(name)
            val smaller = shrink(bytes)
            if (smaller != null) {
                name = name.substringBeforeLast('.') + ".jpg"
                return File(name, smaller, true)
            }
            return File(name, bytes.also { check(it.size <= MAX) { context.getString(R.string.file_too_big) } }, true)
        }
        val bytes = resolver.openInputStream(uri)?.use { input ->
            val out = ByteArrayOutputStream()
            val buffer = ByteArray(64 * 1024)
            while (true) {
                val n = input.read(buffer)
                if (n < 0) break
                out.write(buffer, 0, n)
                check(out.size() <= MAX) { context.getString(R.string.file_too_big) }
            }
            out.toByteArray()
        } ?: throw IllegalStateException(name)
        return File(name, bytes, false)
    }

    /** A JPEG at most [SIDE] pixels on its longest side; null if it cannot be decoded. */
    private fun shrink(bytes: ByteArray): ByteArray? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeByteArray(bytes, 0, bytes.size, bounds)
        if (bounds.outWidth <= 0) return null
        var sample = 1
        while (maxOf(bounds.outWidth, bounds.outHeight) / (sample * 2) >= SIDE) sample *= 2
        val bitmap = BitmapFactory.decodeByteArray(bytes, 0, bytes.size, BitmapFactory.Options().apply { inSampleSize = sample }) ?: return null
        val scale = SIDE.toFloat() / maxOf(bitmap.width, bitmap.height)
        // A camera photo says how it was held: turned the right way before its marks are lost.
        val degrees = runCatching {
            when (android.media.ExifInterface(java.io.ByteArrayInputStream(bytes)).getAttributeInt(android.media.ExifInterface.TAG_ORIENTATION, 1)) {
                android.media.ExifInterface.ORIENTATION_ROTATE_90 -> 90f
                android.media.ExifInterface.ORIENTATION_ROTATE_180 -> 180f
                android.media.ExifInterface.ORIENTATION_ROTATE_270 -> 270f
                else -> 0f
            }
        }.getOrDefault(0f)
        val matrix = android.graphics.Matrix().apply {
            if (scale < 1f) postScale(scale, scale)
            if (degrees != 0f) postRotate(degrees)
        }
        val sized = if (scale < 1f || degrees != 0f) Bitmap.createBitmap(bitmap, 0, 0, bitmap.width, bitmap.height, matrix, true) else bitmap
        val out = ByteArrayOutputStream()
        sized.compress(Bitmap.CompressFormat.JPEG, 85, out)
        return out.toByteArray()
    }
}
