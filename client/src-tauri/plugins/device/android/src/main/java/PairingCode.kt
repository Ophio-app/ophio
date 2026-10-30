// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import com.google.zxing.BarcodeFormat
import com.google.zxing.BinaryBitmap
import com.google.zxing.DecodeHintType
import com.google.zxing.LuminanceSource
import com.google.zxing.MultiFormatReader
import com.google.zxing.ReaderException
import com.google.zxing.Result
import com.google.zxing.common.HybridBinarizer

/**
 * Reads a pairing code from one camera frame. A terminal draws the code in its
 * text colour, so on a dark theme it arrives light on dark, which ZXing does not
 * read by itself; a frame that does not read as it is is tried inverted.
 */
internal object PairingCode {
    fun reader() =
        MultiFormatReader().apply {
            setHints(
                mapOf(
                    DecodeHintType.POSSIBLE_FORMATS to listOf(BarcodeFormat.QR_CODE),
                    DecodeHintType.TRY_HARDER to true,
                )
            )
        }

    fun read(decoder: MultiFormatReader, frame: LuminanceSource): Result? {
        for (source in arrayOf(frame, frame.invert())) {
            try {
                return decoder.decodeWithState(BinaryBitmap(HybridBinarizer(source)))
            } catch (_: ReaderException) {} finally {
                decoder.reset()
            }
        }
        return null
    }
}
