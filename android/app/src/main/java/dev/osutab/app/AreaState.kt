package dev.osutab.app

import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.max
import kotlin.math.min
import kotlin.math.sin

const val TABLET_LONG_MM = 313.76
const val TABLET_SHORT_MM = 195.89
const val MIN_AREA_MM = 10.0

/** The pen area and what's needed to show it, as sent by the PC. All sizes are in mm. */
data class AreaState(
    val width: Double = TABLET_LONG_MM,
    val height: Double = TABLET_SHORT_MM,
    val offsetX: Double = 0.0,
    val offsetY: Double = 0.0,
    val rotationDeg: Double = 0.0,
    val tabletWidth: Double = TABLET_LONG_MM,
    val tabletHeight: Double = TABLET_SHORT_MM,
    val aspectLock: Boolean = true,
    val screenAspect: Double = 1.6,
    val preset: String = "",
    val orientation: Int = 90,
    val screenWidthPx: Double = 2560.0,
) {
    val centerX: Double get() = tabletWidth / 2 + offsetX
    val centerY: Double get() = tabletHeight / 2 + offsetY
    val pxPerMm: Double get() = screenWidthPx / width
    val pxPerMmDown: Double get() = screenWidthPx / screenAspect / height
    /** True when the area's shape isn't the screen's, so the pen moves faster one way than the other. */
    val hasTwoSpeeds: Boolean get() = abs(pxPerMm / pxPerMmDown - 1) > 0.01

    /** Says how far the cursor moves. Any shape is fine, so a different one isn't called out. */
    fun sensitivityLabel(): String =
        if (hasTwoSpeeds) {
            "1 mm = %.1f px across, %.1f px down".format(pxPerMm, pxPerMmDown)
        } else {
            "1 mm = %.1f px on screen".format(pxPerMm)
        }

    /** Corners in mm on the tablet, clockwise from the area's top-left. */
    fun corners(): List<Pair<Double, Double>> {
        val r = Math.toRadians(rotationDeg)
        val (s, c) = sin(r) to cos(r)
        val (hw, hh) = width / 2 to height / 2
        return listOf(-hw to -hh, hw to -hh, hw to hh, -hw to hh).map { (x, y) ->
            (centerX + x * c - y * s) to (centerY + x * s + y * c)
        }
    }

    /** Keeps the area on the tablet, the same way the PC does. */
    fun fitted(): AreaState {
        val w = width.coerceIn(MIN_AREA_MM, tabletWidth)
        val h = height.coerceIn(MIN_AREA_MM, tabletHeight)
        val r = Math.toRadians(rotationDeg)
        val halfX = (w * abs(cos(r)) + h * abs(sin(r))) / 2
        val halfY = (w * abs(sin(r)) + h * abs(cos(r))) / 2
        fun fit(wanted: Double, half: Double, size: Double) =
            if (half * 2 >= size) size / 2 else wanted.coerceIn(half, size - half)
        return copy(
            width = w,
            height = h,
            offsetX = fit(tabletWidth / 2 + offsetX, halfX, tabletWidth) - tabletWidth / 2,
            offsetY = fit(tabletHeight / 2 + offsetY, halfY, tabletHeight) - tabletHeight / 2,
        )
    }

    fun movedBy(dx: Double, dy: Double): AreaState =
        copy(offsetX = offsetX + dx, offsetY = offsetY + dy).fitted()

    /** Pinch: scales about the centre. A locked area keeps the screen's shape. */
    fun scaledBy(factor: Double): AreaState =
        if (aspectLock) withLockedWidth(width * factor) else copy(width = width * factor, height = height * factor).fitted()

    /**
     * Dragging a corner. A locked area scales by the larger of the two directions compared with
     * its size when the drag began. Comparing with the current size instead makes the choice flip
     * between directions from one touch to the next, and the area visibly shakes.
     */
    fun resizedFromCorner(newWidth: Double, newHeight: Double, startWidth: Double, startHeight: Double): AreaState {
        if (!aspectLock) return copy(width = newWidth, height = newHeight).fitted()
        return withLockedWidth(startWidth * max(newWidth / startWidth, newHeight / startHeight))
    }

    private fun withLockedWidth(wanted: Double): AreaState {
        // Both limits apply to the same shape, so take whichever is tighter.
        val w = wanted
            .coerceAtMost(min(tabletWidth, tabletHeight * screenAspect))
            .coerceAtLeast(max(MIN_AREA_MM, MIN_AREA_MM * screenAspect))
        return copy(width = w, height = w / screenAspect).fitted()
    }

    fun toLine(): String = "set $width $height $offsetX $offsetY $rotationDeg"

    companion object {
        /** Reads one `state ...` line from the PC, or null if it isn't one. */
        fun parse(line: String): AreaState? {
            val parts = line.trim().split(' ', limit = 13)
            if (parts.size != 13 || parts[0] != "state") return null
            return try {
                AreaState(
                    width = parts[1].toDouble(),
                    height = parts[2].toDouble(),
                    offsetX = parts[3].toDouble(),
                    offsetY = parts[4].toDouble(),
                    rotationDeg = parts[5].toDouble(),
                    tabletWidth = parts[6].toDouble(),
                    tabletHeight = parts[7].toDouble(),
                    aspectLock = parts[8] == "1",
                    screenAspect = parts[9].toDouble(),
                    orientation = parts[10].toInt(),
                    screenWidthPx = parts[11].toDouble(),
                    preset = parts[12],
                )
            } catch (_: NumberFormatException) {
                null
            }
        }
    }
}
