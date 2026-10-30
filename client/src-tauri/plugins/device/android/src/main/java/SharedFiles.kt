// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.provider.OpenableColumns
import java.io.File
import java.util.UUID
import org.json.JSONArray
import org.json.JSONObject

internal class SharedFiles(
    private val context: Context,
    private val root: File = File(context.filesDir, "shared"),
) {
    private val delivered = mutableSetOf<String>()

    companion object {
        const val ITEM_LIMIT = 50L * 1024 * 1024
        const val TOTAL_LIMIT = 100L * 1024 * 1024
        const val ITEM_COUNT = 16
        // Ids being copied or published by any instance in this process. The set
        // monitor also makes recovery and publication atomic with respect to
        // each other; the potentially slow content URI copy stays outside it.
        internal val importing: MutableSet<String> = java.util.Collections.synchronizedSet(mutableSetOf())
    }

    init {
        root.mkdirs()
        recover()
    }

    /**
     * Finishes or discards what an earlier process left half done. An import
     * writes `<id>.bin`, then `<id>.pending`, then renames that to `<id>.json`,
     * and only a `.json` record reaches the inbox, so bytes without one would
     * hold the share quota for good. Imports still running here are left alone.
     */
    private fun recover() = synchronized(importing) {
        // A failed scan is not an empty directory. Leave its bytes for another
        // restart, when we can establish which records own them.
        val staged = root.listFiles { f -> f.extension == "pending" } ?: return
        for (temp in staged) {
            val id = temp.nameWithoutExtension
            if (id in importing) continue
            // A record written in full but not yet renamed is as good as published.
            if (whole(id, temp)) {
                // Keep a complete staged share if publication fails; without a
                // JSON record the byte sweep would mistake it for an orphan.
                if (!temp.renameTo(File(root, "$id.json"))) return
            } else temp.delete()
        }
        val kept = mutableSetOf<String>()
        val records = root.listFiles { f -> f.extension == "json" } ?: return
        for (record in records) {
            val id = record.nameWithoutExtension
            // A file record without its bytes was acknowledged when the process ended.
            if (whole(id, record)) kept.add(id) else record.delete()
        }
        for (bytes in root.listFiles { f -> f.extension == "bin" }.orEmpty()) {
            val id = bytes.nameWithoutExtension
            if (id !in kept && id !in importing) bytes.delete()
        }
    }

    /** Whether a record reads back and, for a file, all its bytes are here. */
    private fun whole(id: String, record: File): Boolean {
        val item = try {
            JSONObject(record.readText())
        } catch (_: Exception) {
            return false
        }
        if (item.optString("id") != id) return false
        if (item.has("text") || item.has("error")) return true
        val bytes = File(root, "$id.bin")
        return bytes.isFile && bytes.length() == item.optLong("size", -1)
    }

    @Synchronized
    fun accept(intent: Intent) {
        if (intent.action != Intent.ACTION_SEND && intent.action != Intent.ACTION_SEND_MULTIPLE)
            return
        val uris = mutableListOf<Uri>()
        @Suppress("DEPRECATION")
        if (intent.action == Intent.ACTION_SEND_MULTIPLE) {
            uris.addAll(intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM).orEmpty())
        } else {
            @Suppress("DEPRECATION") val uri = intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM)
            if (uri != null) uris.add(uri)
        }
        intent.clipData?.let { clip ->
            for (i in 0 until minOf(clip.itemCount, ITEM_COUNT + 1)) clip.getItemAt(i).uri?.let {
                if (it !in uris) uris.add(it)
            }
        }
        intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()?.let { text ->
            if (text.length > 1024 * 1024)
                record("Shared text", "text/plain", error = "The shared text exceeds 1 MB.")
            else if (text.isNotBlank()) record("Shared text", "text/plain", text = text)
        }
        if (uris.size > ITEM_COUNT)
            record(
                "Shared files",
                "application/octet-stream",
                error = "Share up to 16 files at a time.",
            )
        for (uri in uris.take(ITEM_COUNT)) importFile(uri, intent.type)
        // The same intent may be presented again during a configuration change.
        intent.action = Intent.ACTION_MAIN
        intent.removeExtra(Intent.EXTRA_STREAM)
        intent.removeExtra(Intent.EXTRA_TEXT)
        intent.clipData = null
    }

    private fun importFile(uri: Uri, fallbackMime: String?) {
        var name = "Shared file"
        var mime = fallbackMime ?: "application/octet-stream"
        val id = UUID.randomUUID().toString()
        val bytes = File(root, "$id.bin")
        importing.add(id)
        try {
            require(uri.scheme == "content") { "Only a shared content URI can be imported." }
            require(uri.authority != "${context.packageName}.fileprovider") {
                "Share this file from its original app."
            }
            mime = context.contentResolver.getType(uri) ?: mime
            context.contentResolver
                .query(
                    uri,
                    arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE),
                    null,
                    null,
                    null,
                )
                ?.use { c ->
                    if (c.moveToFirst()) {
                        c.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                            .takeIf { it >= 0 }
                            ?.let { name = c.getString(it)?.take(255) ?: name }
                        c.getColumnIndex(OpenableColumns.SIZE)
                            .takeIf { it >= 0 && !c.isNull(it) }
                            ?.let {
                                require(c.getLong(it) <= ITEM_LIMIT) {
                                    "This file exceeds the 50 MB share limit."
                                }
                            }
                    }
                }
            require(root.listFiles { f -> f.extension == "json" }.orEmpty().size < 64) {
                "The phone’s share inbox is full."
            }
            val used = root.listFiles().orEmpty().sumOf { it.length() }
            var count = 0L
            context.contentResolver.openInputStream(uri)?.use { input ->
                bytes.outputStream().use { out ->
                    val buffer = ByteArray(64 * 1024)
                    while (true) {
                        val n = input.read(buffer)
                        if (n < 0) break
                        count += n
                        require(count <= ITEM_LIMIT) { "This file exceeds the 50 MB share limit." }
                        require(used + count <= TOTAL_LIMIT) { "The phone’s share inbox is full." }
                        out.write(buffer, 0, n)
                    }
                    out.fd.sync()
                }
            } ?: error("This shared file cannot be read.")
            record(name, mime, id = id, size = count)
        } catch (e: Exception) {
            bytes.delete()
            val message =
                if (e is IllegalArgumentException) e.message ?: "This shared file cannot be read."
                else "The sharing app did not allow this file to be read. Share it again."
            record(name, mime, error = message)
        } finally {
            importing.remove(id)
        }
    }

    private fun record(
        name: String,
        mime: String,
        text: String? = null,
        error: String? = null,
        id: String = UUID.randomUUID().toString(),
        size: Long = 0,
    ) = synchronized(importing) {
        // Retain bounded error information even when byte storage is full.
        if (root.listFiles { f -> f.extension == "json" }.orEmpty().size >= 80) return
        val item = JSONObject().put("id", id).put("name", name).put("mime", mime).put("size", size)
        if (text != null) item.put("text", text)
        if (error != null) item.put("error", error)
        val temp = File(root, "$id.pending")
        importing.add(id)
        try {
            temp.outputStream().use {
                it.write(item.toString().toByteArray())
                it.fd.sync()
            }
            check(temp.renameTo(File(root, "$id.json")))
        } finally {
            importing.remove(id)
        }
    }

    @Synchronized
    fun pending(): JSONArray {
        val items = JSONArray()
        root
            .listFiles { f -> f.extension == "json" }
            .orEmpty()
            .sortedBy { it.lastModified() }
            .forEach { file ->
                if (file.nameWithoutExtension !in delivered) {
                    try {
                        items.put(JSONObject(file.readText()))
                        delivered.add(file.nameWithoutExtension)
                    } catch (_: Exception) {
                        file.delete()
                        File(root, "${file.nameWithoutExtension}.bin").delete()
                    }
                }
            }
        return items
    }

    @Synchronized
    fun ready() {
        delivered.clear()
    }

    @Synchronized
    fun read(id: String, offset: Long): ByteArray {
        require(id.matches(Regex("[0-9a-f-]{36}")) && offset >= 0)
        val file = File(root, "$id.bin")
        require(offset <= file.length())
        java.io.RandomAccessFile(file, "r").use { f ->
            f.seek(offset)
            val out = ByteArray(minOf(256 * 1024L, f.length() - offset).toInt())
            f.readFully(out)
            return out
        }
    }

    @Synchronized
    fun acknowledge(id: String) {
        require(id.matches(Regex("[0-9a-f-]{36}")))
        File(root, "$id.bin").delete()
        File(root, "$id.json").delete()
        delivered.remove(id)
    }
}
