// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import org.junit.Assert.assertEquals
import org.junit.Test

class WindowCoordinatesTest {
    @Test fun translatesTheActualWebViewOriginWithoutDoubleInsets() {
        val css = WindowCoordinates(30, 120, 1800.0, 2400.0, 600.0, 800.0, 0.0, 0.0)
        assertEquals(300.0, css.x(930), 0.00001)
        assertEquals(0.0, css.y(120), 0.00001)
        assertEquals(-40.0, css.y(0), 0.00001)
        assertEquals(0.0, css.width(0), 0.0)
        assertEquals(800.0, css.height(2400), 0.00001)
    }

    @Test fun supportsRotationAndAnOccludingHorizontalHinge() {
        val css = WindowCoordinates(0, 60, 2400.0, 1800.0, 800.0, 600.0, 0.0, 0.0)
        assertEquals(300.0, css.y(960), 0.00001)
        assertEquals(8.0, css.height(24), 0.00001)
        assertEquals(800.0, css.width(2400), 0.00001)
    }

    @Test fun usesRealViewportScaleAndOffsetsForPageZoom() {
        val css = WindowCoordinates(10, 30, 1200.0, 1800.0, 200.0, 300.0, 75.0, 100.0)
        assertEquals(175.0, css.x(610), 0.00001)
        assertEquals(250.0, css.y(930), 0.00001)
        assertEquals(2.0, css.width(12), 0.00001)
    }

    @Test fun samplesTheResizedWebViewInsteadOfScreenDimensions() {
        val css = WindowCoordinates(0, 0, 1000.0, 900.0, 500.0, 450.0, 0.0, 0.0)
        assertEquals(400.0, css.y(800), 0.00001)
        assertEquals(0.0, css.height(0), 0.0)
    }
}
