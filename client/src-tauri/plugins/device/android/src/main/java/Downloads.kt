// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.provider.MediaStore
import java.io.OutputStream
import java.util.UUID

internal class Downloads(private val context: Context) {
    private data class Pending(
        val uri: Uri,
        val stream: OutputStream,
        val size: Long,
        var written: Long = 0,
    )

    private val active = mutableMapOf<String, Pending>()
    private val journal = context.getSharedPreferences("pending-downloads", Context.MODE_PRIVATE)

    init {
        // A process crash must not leave abandoned IS_PENDING rows forever. An entry can
        // also outlive its published row (a crash after finish published it, or a removal
        // that didn't persist), so only rows still pending are deleted. An entry whose
        // state can't be read stays for the next start.
        val settled = journal.edit()
        for ((id, entry) in journal.all) try {
            val uri = Uri.parse(entry as String)
            if (pending(uri) == true) {
                check(context.contentResolver.delete(includingPending(uri), null, null) == 1)
            }
            settled.remove(id)
        } catch (_: Exception) {}
        settled.commit()
    }

    /** The row's IS_PENDING, or null once the row is gone. */
    private fun pending(uri: Uri): Boolean? {
        val projection = arrayOf(MediaStore.Downloads.IS_PENDING)
        val cursor =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                val args =
                    Bundle().apply {
                        putInt(MediaStore.QUERY_ARG_MATCH_PENDING, MediaStore.MATCH_INCLUDE)
                    }
                context.contentResolver.query(uri, projection, args, null)
            } else {
                context.contentResolver.query(includingPending(uri), projection, null, null, null)
            } ?: error("Downloads storage is unavailable.")
        return cursor.use { if (it.moveToFirst()) it.getInt(0) != 0 else null }
    }

    /** Android 10 hides pending rows unless the URI asks for them. */
    @Suppress("DEPRECATION")
    private fun includingPending(uri: Uri) =
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) MediaStore.setIncludePending(uri)
        else uri

    @Synchronized
    fun begin(name: String, mime: String, size: Long): String {
        require(
            name.isNotBlank() &&
                name.length <= 255 &&
                name.none { it == '/' || it == '\\' || it.code < 32 }
        )
        require(size in 0..1024L * 1024 * 1024 && active.size < 4)
        val id = UUID.randomUUID().toString()
        val values =
            ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, name)
                put(
                    MediaStore.Downloads.MIME_TYPE,
                    if (mime.matches(Regex("[a-zA-Z0-9.+-]+/[a-zA-Z0-9.+-]+"))) mime
                    else "application/octet-stream",
                )
                put(MediaStore.Downloads.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS)
                put(MediaStore.Downloads.IS_PENDING, 1)
            }
        val uri =
            context.contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                ?: error("Downloads storage is unavailable.")
        try {
            check(journal.edit().putString(id, uri.toString()).commit())
            val stream =
                context.contentResolver.openOutputStream(uri, "w")
                    ?: error("Downloads storage is unavailable.")
            active[id] = Pending(uri, stream, size)
        } catch (e: Exception) {
            context.contentResolver.delete(uri, null, null)
            journal.edit().remove(id).commit()
            throw e
        }
        return id
    }

    @Synchronized
    fun write(id: String, bytes: ByteArray) {
        val p = active[id] ?: error("The download is no longer open.")
        require(bytes.size <= 256 * 1024 && p.written + bytes.size <= p.size)
        try {
            p.stream.write(bytes)
            p.written += bytes.size
        } catch (e: Exception) {
            cancel(id)
            throw e
        }
    }

    @Synchronized
    fun finish(id: String): String {
        val p = active[id] ?: error("The download is no longer open.")
        check(p.written == p.size) { "The download is incomplete." }
        p.stream.flush()
        p.stream.close()
        val update = ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }
        check(context.contentResolver.update(p.uri, update, null, null) == 1)
        active.remove(id)
        journal.edit().remove(id).commit()
        val name =
            context.contentResolver
                .query(p.uri, arrayOf(MediaStore.Downloads.DISPLAY_NAME), null, null, null)
                ?.use { if (it.moveToFirst()) it.getString(0) else null } ?: "Downloaded file"
        return "Downloads/$name"
    }

    @Synchronized
    fun cancel(id: String) {
        active.remove(id)?.let { p ->
            try {
                p.stream.close()
            } finally {
                context.contentResolver.delete(p.uri, null, null)
            }
        }
        journal.edit().remove(id).commit()
    }
}
