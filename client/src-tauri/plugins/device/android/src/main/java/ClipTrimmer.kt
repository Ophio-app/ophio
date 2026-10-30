// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.content.Context
import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaMuxer
import java.io.File
import java.io.RandomAccessFile
import java.nio.ByteBuffer
import java.security.MessageDigest
import java.util.UUID

internal class ClipFailure(message: String) : Exception(message)

/** Only app-private copies of explicitly selected imports; no capture or storage permission. */
internal class ClipTrimmer(context: Context) {
    private val directory = File(context.cacheDir, "clip-exports").apply { mkdirs() }
    private data class Job(val input: File, val output: File, val size: Long, val hash: String, var ready: Boolean = false)
    private val jobs = mutableMapOf<String, Job>()
    init { directory.listFiles()?.forEach { it.delete() } }

    @Synchronized fun begin(size: Long, hash: String): String {
        if (jobs.isNotEmpty()) throw ClipFailure("Another clip is being prepared. Finish or cancel it first.")
        if (size !in 1..MAX_BYTES || !hash.matches(Regex("[0-9a-f]{64}"))) throw ClipFailure("Choose a clip smaller than 128 MB.")
        val id = UUID.randomUUID().toString()
        val input = File(directory, "$id.input")
        if (!input.createNewFile()) throw ClipFailure("The clip could not be staged on this device.")
        jobs[id] = Job(input, File(directory, "$id.mp4"), size, hash)
        return id
    }
    @Synchronized fun write(id: String, bytes: ByteArray) {
        val job = job(id)
        if (job.ready || bytes.isEmpty() || bytes.size > CHUNK || job.input.length() + bytes.size > job.size) throw ClipFailure("The imported clip is incomplete or changed.")
        job.input.appendBytes(bytes)
    }
    @Synchronized fun trim(id: String, startMs: Long, endMs: Long): Map<String, Any> {
        val job = job(id)
        if (job.ready || job.input.length() != job.size || hash(job.input) != job.hash) throw ClipFailure("The imported clip changed. Import it again.")
        if (startMs < 0 || endMs <= startMs || endMs - startMs > 60_000 || endMs > Long.MAX_VALUE / 1000) throw ClipFailure("Choose a range of up to 60 seconds.")
        val extractor = MediaExtractor()
        var muxer: MediaMuxer? = null
        try {
            extractor.setDataSource(job.input.absolutePath)
            val videos = (0 until extractor.trackCount).filter { extractor.getTrackFormat(it).getString(MediaFormat.KEY_MIME)?.startsWith("video/") == true }
            if (videos.size != 1) throw ClipFailure("This clip needs a single video track. Use screenshots instead.")
            val track = videos.single()
            val format = extractor.getTrackFormat(track)
            val duration = format.getLong(MediaFormat.KEY_DURATION)
            if (endMs * 1000 > duration) throw ClipFailure("Choose a range inside this clip.")
            // MP4 remux is deliberately limited; unsupported formats remain available for stills.
            if (format.getString(MediaFormat.KEY_MIME) !in listOf("video/avc", "video/hevc", "video/mp4v-es")) throw ClipFailure("Android can’t trim this format here. Use screenshots instead.")
            extractor.selectTrack(track)
            extractor.seekTo(startMs * 1000, MediaExtractor.SEEK_TO_PREVIOUS_SYNC)
            val actualStart = extractor.sampleTime
            if (actualStart < 0 || actualStart > startMs * 1000 || extractor.sampleFlags and MediaExtractor.SAMPLE_FLAG_SYNC == 0) throw ClipFailure("No usable keyframe was found. Use screenshots instead.")
            muxer = MediaMuxer(job.output.absolutePath, MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4)
            if (format.containsKey(MediaFormat.KEY_ROTATION)) muxer.setOrientationHint(format.getInteger(MediaFormat.KEY_ROTATION))
            val outputTrack = muxer.addTrack(format)
            muxer.start()
            val buffer = ByteBuffer.allocateDirect(MAX_SAMPLE)
            val info = MediaCodec.BufferInfo()
            var previous = -1L
            var count = 0
            while (extractor.sampleTime >= 0 && extractor.sampleTime < endMs * 1000) {
                val at = extractor.sampleTime
                val flags = extractor.sampleFlags
                if (at <= previous || flags and (MediaExtractor.SAMPLE_FLAG_ENCRYPTED or MediaExtractor.SAMPLE_FLAG_PARTIAL_FRAME) != 0 || extractor.sampleSize !in 1..MAX_SAMPLE.toLong()) throw ClipFailure("This clip’s frame order or encoding can’t be trimmed here. Use screenshots instead.")
                buffer.clear()
                val length = extractor.readSampleData(buffer, 0)
                if (length <= 0 || length > MAX_SAMPLE) throw ClipFailure("Android couldn’t read a complete video frame.")
                info.set(0, length, at - actualStart, if (flags and MediaExtractor.SAMPLE_FLAG_SYNC != 0) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0)
                muxer.writeSampleData(outputTrack, buffer, info)
                previous = at
                count++
                extractor.advance()
                if (job.output.length() > MAX_BYTES || count > 36_000) throw ClipFailure("The trimmed clip is too large. Choose a shorter range.")
            }
            val actualEnd = if (extractor.sampleTime >= 0) extractor.sampleTime else duration
            if (count == 0 || actualEnd <= previous || actualEnd > endMs * 1000 + 1_000_000 || actualEnd - actualStart > 60_000_000) throw ClipFailure("This range can’t be cut within 60 seconds. Choose a shorter range.")
            // Explicit terminal timestamp gives the final sample its true duration.
            info.set(0, 0, actualEnd - actualStart, MediaCodec.BUFFER_FLAG_END_OF_STREAM)
            muxer.writeSampleData(outputTrack, ByteBuffer.allocate(0), info)
            muxer.stop()
            muxer.release()
            muxer = null
            if (job.output.length() !in 1..MAX_BYTES) throw ClipFailure("Android did not produce a complete clip.")
            val check = MediaExtractor()
            try {
                check.setDataSource(job.output.absolutePath)
                if (check.trackCount != 1 || kotlin.math.abs(check.getTrackFormat(0).getLong(MediaFormat.KEY_DURATION) - (actualEnd - actualStart)) > 2_000) throw ClipFailure("Android couldn’t verify the trimmed duration. Use screenshots instead.")
            } finally { check.release() }
            job.ready = true
            return mapOf("startMs" to actualStart / 1000.0, "endMs" to actualEnd / 1000.0, "size" to job.output.length(), "sha256" to hash(job.output))
        } catch (error: ClipFailure) { job.output.delete(); throw error }
        catch (_: Exception) { job.output.delete(); throw ClipFailure("Android can’t trim this clip. Use screenshots instead.") }
        finally { runCatching { muxer?.release() }; extractor.release() }
    }
    @Synchronized fun read(id: String, offset: Long): ByteArray {
        val job = job(id)
        if (!job.ready || offset < 0 || offset >= job.output.length()) throw ClipFailure("The trimmed clip is no longer available.")
        return RandomAccessFile(job.output, "r").use { file ->
            file.seek(offset)
            ByteArray(minOf(CHUNK.toLong(), file.length() - offset).toInt()).also { file.readFully(it) }
        }
    }
    @Synchronized fun discard(id: String) { jobs.remove(id)?.let { it.input.delete(); it.output.delete() } }
    @Synchronized fun close() { jobs.keys.toList().forEach { discard(it) } }
    private fun job(id: String) = jobs[id] ?: throw ClipFailure("This clip is no longer staged. Try again.")
    private fun hash(file: File): String {
        val digest = MessageDigest.getInstance("SHA-256")
        file.inputStream().use { input ->
            val buffer = ByteArray(CHUNK)
            while (true) { val count = input.read(buffer); if (count < 0) break; digest.update(buffer, 0, count) }
        }
        return digest.digest().joinToString("") { "%02x".format(it) }
    }
    companion object { private const val MAX_BYTES = 128L * 1024 * 1024; private const val CHUNK = 256 * 1024; private const val MAX_SAMPLE = 8 * 1024 * 1024 }
}
