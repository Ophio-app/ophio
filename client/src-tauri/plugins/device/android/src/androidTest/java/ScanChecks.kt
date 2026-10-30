// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import com.google.zxing.BarcodeFormat
import com.google.zxing.EncodeHintType
import com.google.zxing.PlanarYUVLuminanceSource
import com.google.zxing.qrcode.QRCodeWriter
import com.google.zxing.qrcode.decoder.ErrorCorrectionLevel
import android.content.Intent
import android.net.Uri

/** Synthetic frames only; no camera, network or key access. */
internal object ScanChecks {
    // A fixture offer in the computer's packed form (crates/companion/src/cli.rs).
    private const val LINK =
        "ophio://pair?c=AEHYW3A6FU5EWXENTYFBWLB5JZPV6HE2BZ6TWKSMN2HQUGZ4LV7J6KSLNSGQ4HZ2LN6J2HR7LJ5ZYDIAAEBAGBAFAYDQQCIKBMGA2DQPCAIREEYUCULBOGAZDINRYHI6D4AAAAN2MDJTQAAAAEAAECCCYO6HE3ZNKBBQEERRHEZC4MJWHAXDCLRSGA5DINZRGAYBSZDFONVS4ZLYMFWXA3DFFZ2HGLTOMV2DUNBXGEYDA"

    fun run() {
        val decoder = PairingCode.reader()
        // Dark on light, then a dark terminal's light text on its dark background.
        check(PairingCode.read(decoder, frame(ink = 20, paper = 235))?.text == LINK)
        check(PairingCode.read(decoder, frame(ink = 225, paper = 30))?.text == LINK)
        check(PairingCode.read(decoder, PlanarYUVLuminanceSource(ByteArray(640 * 480) { 30 }, 640, 480, 0, 0, 640, 480, false)) == null)
        // Contribution entry uses the same camera decoder. The full handoff
        // stays at its reference; it never needs to fit inside the QR.
        val contribution = "ophio-contribution://open/v1?packet=" + "eA".repeat(200) + "&handoff=https%3A%2F%2Fcatalog.example.com%2Fhandoff.json"
        check(PairingCode.read(decoder, frame(20, 235, contribution))?.text == contribution)
        val links = AppLinks()
        val intent = Intent(Intent.ACTION_VIEW, Uri.parse(contribution))
        links.accept(intent)
        check(intent.data == null)
        check(links.drain() == listOf(contribution))
        links.accept(intent) // Activity recreation cannot replay the launch.
        check(links.drain().isEmpty())
        links.accept(Intent(Intent.ACTION_VIEW, Uri.parse(CONTRIBUTION_LINK_PREFIX + "x".repeat(96 * 1024))))
        check(links.drain().isEmpty())
    }

    /** The code as the companion prints it: error correction L, two modules of margin, four pixels to a module. */
    private fun frame(ink: Int, paper: Int, link: String = LINK): PlanarYUVLuminanceSource {
        val hints = mapOf(EncodeHintType.ERROR_CORRECTION to ErrorCorrectionLevel.L, EncodeHintType.MARGIN to 2)
        val code = QRCodeWriter().encode(link, BarcodeFormat.QR_CODE, 0, 0, hints)
        val scale = 4
        val border = 40
        val width = code.width * scale + border * 2
        val height = code.height * scale + border * 2
        val y = ByteArray(width * height) { paper.toByte() }
        for (row in 0 until code.height) for (col in 0 until code.width) {
            if (!code[col, row]) continue
            for (dy in 0 until scale) for (dx in 0 until scale)
                y[(border + row * scale + dy) * width + border + col * scale + dx] = ink.toByte()
        }
        return PlanarYUVLuminanceSource(y, width, height, 0, 0, width, height, false)
    }
}
