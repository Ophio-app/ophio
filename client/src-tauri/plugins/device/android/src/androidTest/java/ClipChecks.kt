// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.content.Context
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaExtractor
import android.media.MediaFormat
import android.media.MediaMuxer
import android.os.SystemClock
import java.io.File
import java.security.MessageDigest

/** Synthetic pixels only; no desktop, microphone, account, network or key access. */
internal object ClipChecks {
    fun run(context: Context) {
        val fixture = File.createTempFile("clip-fixture-", ".mp4", context.cacheDir)
        val trimmer = ClipTrimmer(context)
        try {
            encode(fixture)
            val bytes = fixture.readBytes()
            val id = trimmer.begin(bytes.size.toLong(), hash(bytes))
            bytes.asList().chunked(256 * 1024).forEach { trimmer.write(id, it.toByteArray()) }
            val result = trimmer.trim(id, 2000, 5000)
            val start = (result.getValue("startMs") as Number).toDouble()
            val end = (result.getValue("endMs") as Number).toDouble()
            check(start in 1000.0..2000.0 && end in 5000.0..5100.0)
            val size = (result.getValue("size") as Number).toLong()
            val output = java.io.ByteArrayOutputStream()
            while (output.size() < size) output.write(trimmer.read(id, output.size().toLong()))
            check(output.size().toLong() == size && hash(output.toByteArray()) == result["sha256"])
            fixture.writeBytes(output.toByteArray())
            val extractor = MediaExtractor()
            try {
                extractor.setDataSource(fixture.absolutePath)
                check(extractor.trackCount == 1)
                val format = extractor.getTrackFormat(0)
                check(format.getString(MediaFormat.KEY_MIME) == "video/avc")
                check(kotlin.math.abs(format.getLong(MediaFormat.KEY_DURATION) / 1000.0 - (end - start)) <= 2)
                extractor.selectTrack(0)
                check(extractor.sampleFlags and MediaExtractor.SAMPLE_FLAG_SYNC != 0)
            } finally { extractor.release() }
            trimmer.discard(id)
            check(runCatching { trimmer.read(id, 0) }.isFailure)
            val changed = trimmer.begin(bytes.size.toLong(), "0".repeat(64))
            bytes.asList().chunked(256 * 1024).forEach { trimmer.write(changed, it.toByteArray()) }
            check(runCatching { trimmer.trim(changed, 2000, 5000) }.isFailure)
            trimmer.discard(changed)
            val invalid = byteArrayOf(1, 2, 3)
            val broken = trimmer.begin(invalid.size.toLong(), hash(invalid))
            trimmer.write(broken, invalid)
            check(runCatching { trimmer.trim(broken, 0, 1000) }.isFailure)
            check(runCatching { trimmer.read(broken, 0) }.isFailure)
            trimmer.discard(broken)
            check(runCatching { trimmer.begin(0, "0".repeat(64)) }.isFailure)
            check(runCatching { trimmer.begin(129L * 1024 * 1024, "0".repeat(64)) }.isFailure)
        } finally { trimmer.close(); fixture.delete() }
    }
    private fun hash(bytes: ByteArray) = MessageDigest.getInstance("SHA-256").digest(bytes).joinToString("") { "%02x".format(it) }
    private fun encode(file: File) {
        val codec = MediaCodec.createEncoderByType("video/avc")
        val muxer = MediaMuxer(file.absolutePath, MediaMuxer.OutputFormat.MUXER_OUTPUT_MPEG_4)
        var started = false
        try {
            val format = MediaFormat.createVideoFormat("video/avc", 128, 96).apply {
                setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible)
                setInteger(MediaFormat.KEY_BIT_RATE, 128_000)
                setInteger(MediaFormat.KEY_FRAME_RATE, 10)
                setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 1)
                setInteger(MediaFormat.KEY_MAX_B_FRAMES, 0)
            }
            codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
            codec.start()
            val info = MediaCodec.BufferInfo()
            var frame = 0
            var track = -1
            var ended = false
            val deadline = SystemClock.elapsedRealtime() + 30_000
            while (!ended) {
                check(SystemClock.elapsedRealtime() < deadline) { "Fixture encoding timed out" }
                if (frame <= 60) {
                    val input = codec.dequeueInputBuffer(10_000)
                    if (input >= 0) {
                        val pixels = ByteArray(128 * 96 * 3 / 2) { if (it < 128 * 96) (16 + frame).toByte() else 128.toByte() }
                        val buffer = checkNotNull(codec.getInputBuffer(input)); buffer.clear()
                        if (frame < 60) buffer.put(pixels)
                        codec.queueInputBuffer(input, 0, if (frame == 60) 0 else pixels.size, frame * 100_000L, if (frame == 60) MediaCodec.BUFFER_FLAG_END_OF_STREAM else 0)
                        frame++
                    }
                }
                val out = codec.dequeueOutputBuffer(info, 10_000)
                if (out == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                    check(!started)
                    track = muxer.addTrack(codec.outputFormat); muxer.start(); started = true
                } else if (out >= 0) {
                    val buffer = checkNotNull(codec.getOutputBuffer(out))
                    if (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG == 0 && info.size > 0) muxer.writeSampleData(track, buffer, info)
                    ended = info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0
                    codec.releaseOutputBuffer(out, false)
                }
            }
            check(started)
            muxer.stop(); started = false
        } finally {
            runCatching { codec.stop() }; codec.release()
            if (started) runCatching { muxer.stop() }
            muxer.release()
        }
    }
}
