// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.AudioRecordingConfiguration
import android.media.MediaRecorder
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.Executor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.math.sqrt

internal class Microphone {
    private class Session(val id: Long, val recorder: AudioRecord) {
        val running = AtomicBoolean(true)
        val queue = ArrayBlockingQueue<ByteArray>(8)
        @Volatile var error: String? = null
        var thread: Thread? = null
    }

    @Volatile private var session: Session? = null
    private var generation = 0L

    @Synchronized
    fun prepare(): Long {
        generation += 1
        return generation
    }

    @Synchronized
    fun cancelPending() {
        generation += 1
    }

    @SuppressLint("MissingPermission")
    @Synchronized
    fun start(id: Long, expectedGeneration: Long) {
        check(generation == expectedGeneration) { "This recording request was cancelled." }
        check(session == null) { "The microphone is already in use." }
        val minimum =
            AudioRecord.getMinBufferSize(
                16000,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
            )
        check(minimum > 0) { "The microphone cannot record this audio format." }
        val recorder =
            AudioRecord.Builder()
                .setAudioSource(MediaRecorder.AudioSource.VOICE_RECOGNITION)
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setSampleRate(16000)
                        .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .build()
                )
                .setBufferSizeInBytes(maxOf(minimum, 8192))
                .build()
        if (recorder.state != AudioRecord.STATE_INITIALIZED) {
            recorder.release()
            error("The microphone could not start.")
        }
        val s = Session(id, recorder)
        session = s
        val callback =
            object : AudioManager.AudioRecordingCallback() {
                override fun onRecordingConfigChanged(
                    configs: MutableList<AudioRecordingConfiguration>
                ) {
                    if (
                        configs.any {
                            it.clientAudioSessionId == recorder.audioSessionId &&
                                it.isClientSilenced
                        }
                    ) {
                        interrupt("Recording stopped because Android silenced the microphone.")
                    }
                }
            }
        recorder.registerAudioRecordingCallback(Executor { it.run() }, callback)
        try {
            recorder.startRecording()
        } catch (e: Exception) {
            recorder.unregisterAudioRecordingCallback(callback)
            recorder.release()
            session = null
            throw e
        }
        s.thread =
            Thread(
                    {
                        try {
                            while (s.running.get()) {
                                val bytes = ByteArray(2048)
                                val n =
                                    recorder.read(bytes, 0, bytes.size, AudioRecord.READ_BLOCKING)
                                if (!s.running.get()) break
                                if (n <= 0) {
                                    s.error = "The microphone was interrupted."
                                    break
                                }
                                if (!s.queue.offer(bytes.copyOf(n), 500, TimeUnit.MILLISECONDS)) {
                                    s.error =
                                        "Recording stopped because the connection could not keep up."
                                    break
                                }
                            }
                        } catch (_: Exception) {
                            s.error = "The microphone was interrupted."
                        } finally {
                            s.running.set(false)
                            try {
                                recorder.stop()
                            } catch (_: Exception) {}
                            recorder.unregisterAudioRecordingCallback(callback)
                            recorder.release()
                        }
                    },
                    "phone-microphone",
                )
                .also { it.start() }
    }

    fun read(id: Long): Map<String, Any?> {
        val s = session ?: return mapOf("error" to "Recording ended.")
        if (s.id != id) return mapOf("error" to "This recording has ended.")
        s.error?.let {
            return mapOf("error" to it)
        }
        val bytes = s.queue.poll(100, TimeUnit.MILLISECONDS) ?: return mapOf("rms" to 0.0)
        var power = 0.0
        for (i in 0 until bytes.size - 1 step 2) {
            val sample =
                ((bytes[i].toInt() and 255) or (bytes[i + 1].toInt() shl 8)).toShort().toDouble() /
                    32768.0
            power += sample * sample
        }
        return mapOf(
            "pcm" to Keys.encode(bytes),
            "rms" to sqrt(power / (bytes.size / 2).coerceAtLeast(1)),
        )
    }

    @Synchronized
    fun stop(id: Long, drain: Boolean): ByteArray {
        generation += 1
        val s = session ?: return byteArrayOf()
        if (s.id != id) return byteArrayOf()
        s.running.set(false)
        try {
            s.recorder.stop()
        } catch (_: Exception) {}
        s.thread?.join(1000)
        val remaining = ArrayList<ByteArray>()
        s.queue.drainTo(remaining)
        session = null
        return if (drain) remaining.fold(byteArrayOf()) { a, b -> a + b } else byteArrayOf()
    }

    @Synchronized
    fun interrupt(reason: String) {
        val s = session ?: return
        generation += 1
        s.error = reason
        s.running.set(false)
        try {
            s.recorder.stop()
        } catch (_: Exception) {}
        s.queue.clear()
    }

    fun shutdown() {
        session?.let { stop(it.id, false) }
    }
}
