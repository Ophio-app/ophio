// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Environment
import android.provider.MediaStore
import java.io.File
import java.io.FileFilter
import java.io.RandomAccessFile
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.thread
import org.json.JSONArray
import org.json.JSONObject

/**
 * What a process killed mid-share leaves in the test app's own share directory,
 * then a restart. The shared file is a fixture row in the test app's Downloads.
 */
internal object SharedChecks {
    private const val MIB = 1024L * 1024

    fun run(context: Context) {
        val root = File(context.filesDir, "shared")
        val made = mutableListOf<Uri>()
        try {
            root.deleteRecursively()
            root.mkdirs()

            // Two 50 MiB copies cut short, then a one-byte share after restart.
            val cut = List(2) { bytes(root, id(), 50 * MIB) }
            var shares = SharedFiles(context)
            val source = insert(context, byteArrayOf(7)).also(made::add)
            shares.accept(Intent(Intent.ACTION_SEND).setType("application/octet-stream").putExtra(Intent.EXTRA_STREAM, source))
            val item = only(shares.pending())
            check(!item.has("error")) { "a one-byte share after restart was refused: ${item.optString("error")}" }
            check(cut.none { it.exists() }) { "recovery kept a copy cut short" }
            check(item.getLong("size") == 1L) { "the one-byte share reports ${item.getLong("size")} bytes" }
            check(shares.read(item.getString("id"), 0).contentEquals(byteArrayOf(7))) { "the one-byte share changed" }
            shares.acknowledge(item.getString("id"))
            check(root.list().orEmpty().isEmpty()) { "acknowledging left ${root.list()?.size} files" }

            // Committed shares stay as they are.
            val file = id().also { bytes(root, it, 3); write(root, "$it.json", fileItem(it, 3)) }
            val text = id().also { write(root, "$it.json", textItem(it)) }
            val error = id().also { write(root, "$it.json", textItem(it).apply { remove("text"); put("error", "fixture") }) }
            // Metadata cut short before its rename: a whole record is published, a torn one goes with its bytes.
            val synced = id().also { bytes(root, it, 5); write(root, "$it.pending", fileItem(it, 5)) }
            val note = id().also { write(root, "$it.pending", textItem(it)) }
            val torn = id().also { bytes(root, it, 5); write(root, "$it.pending", fileItem(it, 5).toString().take(12)) }
            val short = id().also { bytes(root, it, 2); write(root, "$it.pending", fileItem(it, 5)) }
            // Acknowledgement cut short after its bytes went, and a record that cannot be read.
            val acked = id().also { write(root, "$it.json", fileItem(it, 4)) }
            val corrupt = id().also { bytes(root, it, 4); write(root, "$it.json", "{") }
            shares = SharedFiles(context)
            val kept = setOf("$file.bin", "$file.json", "$text.json", "$error.json", "$synced.bin", "$synced.json", "$note.json")
            val left = root.list().orEmpty().toSet()
            check(left == kept) { "recovery left ${left - kept} and removed ${kept - left} (torn $torn, short $short, acked $acked, corrupt $corrupt)" }
            val shown = shares.pending().let { a -> List(a.length()) { a.getJSONObject(it).getString("id") }.toSet() }
            check(shown == setOf(file, text, error, synced, note)) { "the inbox shows ${shown.size} of 5 items" }
            check(shares.read(synced, 0).size == 5) { "a recovered share's bytes changed" }

            // Another instance's import still running in this process is left alone.
            val copying = id().also { bytes(root, it, 2) }
            val publishing = id().also { bytes(root, it, 2); write(root, "$it.pending", fileItem(it, 2).toString().take(12)) }
            SharedFiles.importing.addAll(listOf(copying, publishing))
            try {
                SharedFiles(context)
            } finally {
                SharedFiles.importing.removeAll(listOf(copying, publishing))
            }
            val running = setOf("$copying.bin", "$publishing.bin", "$publishing.pending")
            check(root.list().orEmpty().toSet() == kept + running) { "recovery touched an import still running" }
            SharedFiles(context)
            check(root.list().orEmpty().toSet() == kept) { "recovery kept an import once it had stopped" }

            publicationDuringRecovery(context, source, alreadyCopying = false)
            publicationDuringRecovery(context, source, alreadyCopying = true)
            val failures = listOf("json", "pending", "rename").mapNotNull { fault ->
                runCatching { recoveryFailure(context, fault) }.exceptionOrNull()?.let { "$fault: ${it.message}" }
            }
            check(failures.isEmpty()) { failures.joinToString("; ") }
        } finally {
            for (uri in made) runCatching { context.contentResolver.delete(uri, null, null) }
            root.deleteRecursively()
        }
    }

    /** An I/O failure must preserve complete shares for a later recovery attempt. */
    private fun recoveryFailure(context: Context, fault: String) {
        val root = File(context.filesDir, "shared-failure").apply { mkdirs() }
        try {
            val committed = id().also { bytes(root, it, 3); write(root, "$it.json", fileItem(it, 3)) }
            val staged = id().also { bytes(root, it, 5); write(root, "$it.pending", fileItem(it, 5)) }
            val orphan = bytes(root, id(), 2)
            var injected = false
            val failing = object : File(root.path) {
                override fun listFiles(filter: FileFilter?): Array<File>? {
                    if (!injected && fault != "rename" && filter?.accept(File(this, "probe.$fault")) == true) {
                        injected = true
                        return null
                    }
                    return super.listFiles(filter)?.map { file ->
                        if (fault == "rename" && file.name == "$staged.pending") {
                            object : File(file.path) {
                                override fun renameTo(dest: File): Boolean {
                                    injected = true
                                    return false
                                }
                            }
                        } else file
                    }?.toTypedArray()
                }
            }
            SharedFiles(context, failing)
            check(injected) { "the recovery fault was not injected" }
            check(File(root, "$committed.bin").isFile && File(root, "$committed.bin").length() == 3L) {
                "failed recovery removed a committed share's bytes"
            }
            check(File(root, "$committed.json").readText() == fileItem(committed, 3).toString())
            check(File(root, "$staged.bin").isFile && File(root, "$staged.bin").length() == 5L) {
                "failed recovery removed a complete staged share's bytes"
            }
            if (fault == "rename") {
                check(File(root, "$staged.pending").readText() == fileItem(staged, 5).toString())
                check(!File(root, "$staged.json").exists())
            }
            check(orphan.exists()) { "recovery swept bytes after an I/O failure" }

            val restarted = SharedFiles(context, root)
            val shown = restarted.pending().let { items ->
                List(items.length()) { items.getJSONObject(it).getString("id") }.toSet()
            }
            check(shown == setOf(committed, staged)) { "a later restart did not recover both complete shares" }
            check(restarted.read(committed, 0).contentEquals(ByteArray(3)))
            check(restarted.read(staged, 0).contentEquals(ByteArray(5)))
            check(!File(root, "$staged.pending").exists() && !orphan.exists()) {
                "a later restart did not finish publication and cleanup"
            }
        } finally {
            root.deleteRecursively()
        }
    }

    /** Freeze the record snapshot while a new or already active import publishes. */
    private fun publicationDuringRecovery(context: Context, source: Uri, alreadyCopying: Boolean) {
        val root = File(context.filesDir, "shared-race").apply { mkdirs() }
        val scanned = CountDownLatch(1)
        val resume = CountDownLatch(1)
        val copying = CountDownLatch(1)
        val continueCopy = CountDownLatch(1)
        val pauseCopy = AtomicBoolean(false)
        val failure = AtomicReference<Throwable?>()
        val pausedRecovery = object : File(root.path) {
            override fun listFiles(filter: FileFilter?): Array<File>? {
                val files = super.listFiles(filter)
                if (filter?.accept(File(this, "probe.json")) == true) {
                    scanned.countDown()
                    check(resume.await(10, TimeUnit.SECONDS)) { "cleanup was not resumed" }
                }
                return files
            }
        }
        val pausedImport = object : File(root.path) {
            override fun listFiles(filter: FileFilter?): Array<File>? {
                val files = super.listFiles(filter)
                if (filter?.accept(File(this, "probe.json")) == true && pauseCopy.compareAndSet(true, false)) {
                    // importFile registered its id but has not copied or published yet.
                    copying.countDown()
                    check(continueCopy.await(10, TimeUnit.SECONDS)) { "copy was not resumed" }
                }
                return files
            }
        }
        val shares = SharedFiles(context, pausedImport)
        fun startImport() = thread(name = "share-import") {
            try {
                shares.accept(Intent(Intent.ACTION_SEND).setType("application/octet-stream").putExtra(Intent.EXTRA_STREAM, source))
            } catch (e: Throwable) { failure.set(e) }
        }
        var importer: Thread? = null
        var recovery: Thread? = null
        try {
            try {
                if (alreadyCopying) {
                    pauseCopy.set(true)
                    importer = startImport()
                    check(copying.await(10, TimeUnit.SECONDS)) { "the import did not start copying" }
                }
                recovery = thread(name = "share-recovery") {
                    try { SharedFiles(context, pausedRecovery) } catch (e: Throwable) { failure.set(e) }
                }
                check(scanned.await(10, TimeUnit.SECONDS)) { "cleanup did not scan its records" }
                if (alreadyCopying) continueCopy.countDown() else importer = startImport()
                val worker = checkNotNull(importer)
                // The old implementation finishes against the stale snapshot.
                // A coordinated import blocks until cleanup resumes instead.
                worker.join(1000)
                check(!worker.isAlive || worker.state == Thread.State.BLOCKED) {
                    "the import neither finished nor blocked on cleanup"
                }
            } finally {
                continueCopy.countDown()
                resume.countDown()
                recovery?.join(10_000)
                importer?.join(10_000)
            }
            check(recovery?.isAlive != true && importer?.isAlive != true) { "share workers did not finish" }
            failure.get()?.let { throw it }
            val item = only(shares.pending())
            check(!item.has("error")) { "the concurrent share failed: ${item.optString("error")}" }
            check(shares.read(item.getString("id"), 0).contentEquals(byteArrayOf(7))) {
                "cleanup removed bytes published after its record snapshot"
            }
            SharedFiles(context, root).let { restarted ->
                check(only(restarted.pending()).getString("id") == item.getString("id"))
                check(restarted.read(item.getString("id"), 0).contentEquals(byteArrayOf(7)))
                restarted.acknowledge(item.getString("id"))
            }
        } finally {
            root.deleteRecursively()
        }
    }

    private fun id() = UUID.randomUUID().toString()

    private fun bytes(root: File, id: String, size: Long) =
        File(root, "$id.bin").also { f -> RandomAccessFile(f, "rw").use { it.setLength(size) } }

    private fun write(root: File, name: String, body: Any) = File(root, name).writeText(body.toString())

    private fun fileItem(id: String, size: Long) =
        JSONObject().put("id", id).put("name", "fixture.bin").put("mime", "application/octet-stream").put("size", size)

    private fun textItem(id: String) =
        JSONObject().put("id", id).put("name", "Shared text").put("mime", "text/plain").put("size", 0).put("text", "fixture")

    private fun only(items: JSONArray): JSONObject {
        check(items.length() == 1) { "the inbox shows ${items.length()} items, not 1" }
        return items.getJSONObject(0)
    }

    private fun insert(context: Context, bytes: ByteArray): Uri {
        val values =
            ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, "ophio-check-share-${UUID.randomUUID()}.bin")
                put(MediaStore.Downloads.RELATIVE_PATH, Environment.DIRECTORY_DOWNLOADS)
                put(MediaStore.Downloads.IS_PENDING, 1)
            }
        val uri = checkNotNull(context.contentResolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values))
        checkNotNull(context.contentResolver.openOutputStream(uri, "w")).use { it.write(bytes) }
        val update = ContentValues().apply { put(MediaStore.Downloads.IS_PENDING, 0) }
        check(context.contentResolver.update(uri, update, null, null) == 1)
        return uri
    }
}
