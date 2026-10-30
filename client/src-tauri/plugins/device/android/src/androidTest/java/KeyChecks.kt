// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.app.Activity
import android.app.Instrumentation
import android.os.Bundle
import java.security.KeyStore
import java.util.UUID

/** Device-only checks: no mock Keystore and no external test service. */
class KeyChecks : Instrumentation() {
    private var downloadsOnly = false
    private var clipsOnly = false
    private var scanOnly = false
    private var alertsOnly = false
    private var exitsOnly = false
    private var sharesOnly = false
    private var preview: String? = null
    override fun onCreate(arguments: Bundle?) {
        super.onCreate(arguments)
        downloadsOnly = arguments?.getString("downloads") == "true"
        sharesOnly = arguments?.getString("shares") == "true"
        clipsOnly = arguments?.getString("clips") == "true"
        scanOnly = arguments?.getString("scan") == "true"
        alertsOnly = arguments?.getString("alerts") == "true"
        exitsOnly = arguments?.getString("exits") == "true"
        preview = arguments?.getString("preview")
        start()
    }

    override fun onStart() {
        if (exitsOnly) {
            try {
                ExitChecks.run()
                finish(Activity.RESULT_OK, Bundle().apply { putString("stream", "Exit checks passed: crashes always count, Android closing a backgrounded app does not.\n") })
            } catch (e: Throwable) {
                finish(Activity.RESULT_CANCELED, Bundle().apply { putString("stream", "Exit checks failed: ${e.message}\n") })
            }
            return
        }
        if (downloadsOnly) {
            try {
                DownloadChecks.run(targetContext)
                finish(Activity.RESULT_OK, Bundle().apply { putString("stream", "Download checks passed: published and finished downloads survive recovery, abandoned pending rows are removed, stale entries clear.\n") })
            } catch (e: Throwable) {
                finish(Activity.RESULT_CANCELED, Bundle().apply { putString("stream", "Download checks failed: ${e.message}\n") })
            }
            return
        }
        if (sharesOnly) {
            try {
                SharedChecks.run(targetContext)
                finish(Activity.RESULT_OK, Bundle().apply { putString("stream", "Share checks passed: a restart frees bytes from copies cut short, publishes whole records left unrenamed, removes torn and short ones, finishes cut-short acknowledgements, keeps committed shares and imports starting or publishing during cleanup, and preserves complete shares through failed metadata scans and renames for the next restart.\n") })
            } catch (e: Throwable) {
                val at = e.stackTrace.firstOrNull { it.className.startsWith("app.ophio") }
                finish(Activity.RESULT_CANCELED, Bundle().apply { putString("stream", "Share checks failed: ${e.message} (${at?.fileName}:${at?.lineNumber})\n") })
            }
            return
        }

        preview?.let { name ->
            try {
                val seen = PreviewChecks.run(this, name)
                finish(Activity.RESULT_OK, Bundle().apply { putString("stream", "Preview checks passed: $seen.\n") })
            } catch (e: Throwable) {
                finish(Activity.RESULT_CANCELED, Bundle().apply { putString("stream", "Preview checks failed: ${e.message}\n") })
            }
            return
        }
        if (alertsOnly) {
            try {
                AlertChecks.run(targetContext)
                finish(Activity.RESULT_OK, Bundle().apply { putString("stream", "Alert checks passed: answered alerts refused, reopened ones posted, clears forgotten, groups counted while Android catches up, routes kept while showing, reused, replaced and used once, alerts showing hidden when app lock turns on, also by the next process should this one end first, and questions shown with their answers, sent once, asked again when not sent, and hidden with app lock.\n") })
            } catch (e: Throwable) {
                // The checks' own words and where, only; they carry no data of the phone's.
                val at = e.stackTrace.firstOrNull { it.className.startsWith("app.ophio") }
                finish(Activity.RESULT_CANCELED, Bundle().apply { putString("stream", "Alert checks failed: ${e.message} (${at?.fileName}:${at?.lineNumber})\n") })
            }
            return
        }
        if (scanOnly) {
            try {
                ScanChecks.run()
                finish(Activity.RESULT_OK, Bundle().apply { putString("stream", "Scan checks passed: dark on light, light on dark, empty frame.\n") })
            } catch (_: Throwable) {
                finish(Activity.RESULT_CANCELED, Bundle().apply { putString("stream", "Scan checks failed.\n") })
            }
            return
        }
        if (clipsOnly) {
            try {
                ClipChecks.run(targetContext)
                finish(Activity.RESULT_OK, Bundle().apply { putString("stream", "Clip checks passed: remux, duration, hashes, malformed input, bounds and cleanup.\n") })
            } catch (_: Throwable) {
                finish(Activity.RESULT_CANCELED, Bundle().apply { putString("stream", "Clip checks failed.\n") })
            }
            return
        }
        val direct = UUID.randomUUID().toString()
        val wrapped = UUID.randomUUID().toString()
        try {
            val public = Keys.create(direct)
            if (public != null) {
                check(Keys.decode(public).size == 32)
                check(Keys.decode(Keys.sign(direct, "fixture-transcript".toByteArray())).size == 64)
            }
            val seed = ByteArray(32) { it.toByte() }
            val sealed = Keys.decode(Keys.wrap(wrapped, seed.copyOf()))
            check(Keys.unwrap(wrapped, sealed).contentEquals(seed))
            val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
            check(store.getKey("device.$wrapped.wrap", null).encoded == null)
            sealed[60] = (sealed[60].toInt() xor 1).toByte()
            check(runCatching { Keys.unwrap(wrapped, sealed) }.isFailure)
            Keys.delete(wrapped)
            check(!store.containsAlias("device.$wrapped.wrap"))
            finish(
                Activity.RESULT_OK,
                Bundle().apply {
                    putString(
                        "stream",
                        "Keystore checks passed: non-exportable wrapping key, round trip, tamper rejection, deletion. Ed25519 supported: ${public != null}\n",
                    )
                },
            )
        } catch (_: Throwable) {
            finish(
                Activity.RESULT_CANCELED,
                Bundle().apply { putString("stream", "Keystore checks failed.\n") },
            )
        } finally {
            Keys.delete(direct)
            Keys.delete(wrapped)
        }
    }
}
