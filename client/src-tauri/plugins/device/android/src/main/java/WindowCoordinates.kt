// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

/** Physical app-window pixels to layout-viewport CSS pixels. Origins already
 * include native insets through getLocationInWindow; do not subtract them again.
 * The visual viewport accounts for page zoom without using display density. */
internal data class WindowCoordinates(
    val webLeft: Int, val webTop: Int,
    val webWidth: Double, val webHeight: Double,
    val visibleWidth: Double, val visibleHeight: Double,
    val offsetX: Double, val offsetY: Double,
) {
    private val sx = visibleWidth / webWidth
    private val sy = visibleHeight / webHeight
    fun x(windowX: Int) = (windowX - webLeft) * sx + offsetX
    fun y(windowY: Int) = (windowY - webTop) * sy + offsetY
    fun width(pixels: Int) = pixels * sx
    fun height(pixels: Int) = pixels * sy
}
