package dev.osutab.app

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AreaStateTest {
    private val area = AreaState(width = 100.0, height = 62.5, aspectLock = true, screenAspect = 1.6)

    @Test
    fun parsesAStateLineFromThePc() {
        val s = AreaState.parse("state 120.0 75.0 1.5 -2.5 10.0 313.76 195.89 1 1.6 90 2560 My preset name")!!
        assertEquals(120.0, s.width, 1e-9)
        assertEquals(-2.5, s.offsetY, 1e-9)
        assertEquals(10.0, s.rotationDeg, 1e-9)
        assertTrue(s.aspectLock)
        assertEquals(90, s.orientation)
        assertEquals("My preset name", s.preset)
    }

    @Test
    fun rejectsOtherLines() {
        assertNull(AreaState.parse("outline 1"))
        assertNull(AreaState.parse("state 1 2"))
        assertNull(AreaState.parse("state a b c d e f g 1 h 90 2560 x"))
    }

    @Test
    fun moveStaysOnTheTablet() {
        val moved = area.movedBy(1000.0, 1000.0)
        assertEquals(TABLET_LONG_MM / 2 - 50.0, moved.offsetX, 1e-9)
        assertEquals(TABLET_SHORT_MM / 2 - 31.25, moved.offsetY, 1e-9)
    }

    @Test
    fun pinchKeepsTheShapeWhenLocked() {
        val scaled = area.scaledBy(1.5)
        assertEquals(150.0, scaled.width, 1e-9)
        assertEquals(93.75, scaled.height, 1e-9)
    }

    @Test
    fun lockedSizeStopsAtTheTabletsLimit() {
        val huge = area.scaledBy(100.0)
        assertTrue(huge.height <= TABLET_SHORT_MM + 1e-9)
        assertEquals(1.6, huge.width / huge.height, 1e-9)
        val tiny = area.scaledBy(0.001)
        assertTrue(tiny.height >= MIN_AREA_MM - 1e-9)
        assertEquals(1.6, tiny.width / tiny.height, 1e-9)
    }

    @Test
    fun unlockedSizesAreIndependent() {
        val s = area.copy(aspectLock = false).resizedFromCorner(120.0, 40.0, 100.0, 62.5)
        assertEquals(120.0, s.width, 1e-9)
        assertEquals(40.0, s.height, 1e-9)
    }

    @Test
    fun cornerDragDoesNotShakeWhenDirectionsDisagree() {
        // A finger moving along a line that isn't the area's diagonal, so width and height pull
        // differently. Each step must grow steadily: the old code flipped between the two.
        var s = area
        var previous = s.width
        for (i in 1..40) {
            val width = 100.0 + i * 1.0
            val height = 62.5 + i * 0.9
            s = s.resizedFromCorner(width, height, 100.0, 62.5)
            assertTrue("step $i went from $previous to ${s.width}", s.width >= previous)
            assertEquals(1.6, s.width / s.height, 1e-9)
            previous = s.width
        }
    }

    @Test
    fun cornersMatchTheAreaWithNoRotation() {
        val corners = area.corners()
        assertEquals(TABLET_LONG_MM / 2 - 50.0, corners[0].first, 1e-9)
        assertEquals(TABLET_SHORT_MM / 2 + 31.25, corners[2].second, 1e-9)
    }

    @Test
    fun stretchedAreaIsCalledOut() {
        val stretched = AreaState(width = 88.0, height = 75.2, screenAspect = 1.6, screenWidthPx = 2560.0)
        assertTrue(stretched.isStretched)
        assertEquals(29.09, stretched.pxPerMm, 0.01)
        assertEquals(21.28, stretched.pxPerMmDown, 0.01)
        assertTrue(stretched.sensitivityLabel().startsWith("Stretched"))
        assertTrue(!area.isStretched)
    }

    @Test
    fun editLineIsWhatThePcExpects() {
        assertEquals("set 100.0 62.5 0.0 0.0 0.0", area.toLine())
    }
}
