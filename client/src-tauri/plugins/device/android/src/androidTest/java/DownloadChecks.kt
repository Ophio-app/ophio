// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Bundle
import android.os.Environment
import android.provider.MediaStore
import java.util.UUID

/** Fixture bytes in this test app's own Downloads rows; no other app's files are touched. */
internal object DownloadChecks {
    fun run(context: Context) {
        val journal = context.getSharedPreferences("pending-downloads", Context.MODE_PRIVATE)
        val made = mutableListOf<Uri>()
        try {
            Downloads(context)
            check(journal.all.isEmpty()) { "recovery left journal entries" }

            // Process death right after IS_PENDING became 0, before the journal entry went.
            val published = insert(context, "published", byteArrayOf(1, 2, 3), pending = false).also(made::add)
            check(journal.edit().putString(UUID.randomUUID().toString(), published.toString()).commit())
            Downloads(context)
            check(pending(context, published) == false) { "recovery deleted a published download" }
            check(read(context, published).contentEquals(byteArrayOf(1, 2, 3))) { "published bytes changed" }
            check(journal.all.isEmpty()) { "recovery kept a published download's entry" }

            // finish() succeeded, but its journal removal did not persist.
            val downloads = Downloads(context)
            val bytes = ByteArray(300 * 1024) { (it % 251).toByte() }
            val id = downloads.begin("ophio-check-${UUID.randomUUID()}.bin", "application/octet-stream", bytes.size.toLong())
            val finished = Uri.parse(journal.getString(id, null)).also(made::add)
            bytes.asList().chunked(256 * 1024).forEach { downloads.write(id, it.toByteArray()) }
            check(downloads.finish(id).startsWith("Downloads/"))
            check(journal.edit().putString(id, finished.toString()).commit())
            Downloads(context)
            check(pending(context, finished) == false) { "recovery deleted a finished download" }
            check(read(context, finished).contentEquals(bytes)) { "finished bytes changed" }
            check(journal.all.isEmpty()) { "recovery kept a finished download's entry" }

            // Process death mid-download: the row is still pending and must go.
            val abandoned = Downloads(context)
            val partial = abandoned.begin("ophio-check-${UUID.randomUUID()}.bin", "application/octet-stream", 10)
            val unfinished = Uri.parse(journal.getString(partial, null)).also(made::add)
            abandoned.write(partial, byteArrayOf(1, 2, 3, 4))
            check(pending(context, unfinished) == true)
            Downloads(context)
            check(pending(context, unfinished) == null) { "recovery kept an abandoned pending download" }
            check(journal.all.isEmpty()) { "recovery kept an abandoned download's entry" }
            abandoned.cancel(partial)

            // A journaled row that no longer exists only clears its entry.
            check(journal.edit().putString(UUID.randomUUID().toString(), unfinished.toString()).commit())
            Downloads(context)
            check(journal.all.isEmpty()) { "recovery kept a missing download's entry" }
        } finally {
            for (uri in made) runCatching { context.contentResolver.delete(uri, null, null) }
            journal.edit().clear().commit()
        }
    }

    private fun insert(context: Context, label: String, bytes: ByteArray, pending: Boolean): Uri {
        val values =
            ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, "ophio-check-$label-${UUID.randomUUID()}.bin")
                put(MediaStore.Downloads.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS)
                put(MediaStore.Downloads.IS_PENDING, 1)
            }
        val uri = checkNotNull(context.contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values))
        checkNotNull(context.contentResolver.openOutputStream(uri, "w")).use { it.write(bytes) }
        if (!pending) {
            val update = ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }
            check(context.contentResolver.update(uri, update, null, null) == 1)
        }
        return uri
    }

    /** True or false for a row's IS_PENDING, null once the row is gone. */
    private fun pending(context: Context, uri: Uri): Boolean? {
        val args = Bundle().apply { putInt(MediaStore.QUERY_ARG_MATCH_PENDING, MediaStore.MATCH_INCLUDE) }
        return checkNotNull(context.contentResolver.query(uri, arrayOf(MediaStore.Downloads.IS_PENDING), args, null))
            .use { if (it.moveToFirst()) it.getInt(0) != 0 else null }
    }

    private fun read(context: Context, uri: Uri) =
        checkNotNull(context.contentResolver.openInputStream(uri)).use { it.readBytes() }
}
