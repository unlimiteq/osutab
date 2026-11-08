package dev.osutab.app

import android.graphics.Color
import kotlin.math.roundToInt

/** The colors and options the PC sends. Until it does, the app shows the default theme. */
data class AppTheme(
    val background: Int = Color.rgb(0x14, 0x16, 0x1a),
    val panel: Int = Color.rgb(0x1d, 0x20, 0x26),
    val text: Int = Color.rgb(0xec, 0xee, 0xf2),
    val muted: Int = Color.rgb(0x9a, 0xa1, 0xad),
    val accent: Int = Color.rgb(0xff, 0x5f, 0xa2),
    val outline: Int = Color.rgb(0xff, 0x5f, 0xa2),
    val outlineThickness: Float = 2f,
    val outsideDim: Int = 55,
    val animations: Boolean = true,
) {
    /** The background darkened by the theme's amount. */
    val outside: Int get() = blend(background, Color.BLACK, outsideDim / 100f)

    /** A hairline that shows on this theme's panels. */
    val line: Int get() = blend(panel, text, 0.16f)

    fun toLine(): String = "theme ${hex(background)} ${hex(panel)} ${hex(text)} ${hex(muted)} ${hex(accent)} " +
        "${hex(outline)} $outlineThickness $outsideDim ${if (animations) 1 else 0}"

    companion object {
        /** Reads one `theme ...` line from the PC, or null if it isn't one. */
        fun parse(line: String): AppTheme? {
            val parts = line.trim().split(' ')
            if (parts.size != 10 || parts[0] != "theme") return null
            return try {
                AppTheme(
                    background = color(parts[1]),
                    panel = color(parts[2]),
                    text = color(parts[3]),
                    muted = color(parts[4]),
                    accent = color(parts[5]),
                    outline = color(parts[6]),
                    outlineThickness = parts[7].toFloat(),
                    outsideDim = parts[8].toInt(),
                    animations = parts[9] == "1",
                )
            } catch (_: NumberFormatException) {
                null
            }
        }

        private fun color(hex: String): Int {
            if (hex.length != 6) throw NumberFormatException(hex)
            val value = hex.toInt(16)
            return Color.rgb(value shr 16 and 0xff, value shr 8 and 0xff, value and 0xff)
        }

        private fun hex(color: Int): String =
            "%02x%02x%02x".format(Color.red(color), Color.green(color), Color.blue(color))

        fun blend(from: Int, to: Int, amount: Float): Int {
            fun mix(a: Int, b: Int) = (a + (b - a) * amount).roundToInt()
            return Color.rgb(
                mix(Color.red(from), Color.red(to)),
                mix(Color.green(from), Color.green(to)),
                mix(Color.blue(from), Color.blue(to)),
            )
        }
    }
}

/** The app's other settings from the PC. */
data class AppPrefs(
    val showAreaLabel: Boolean = true,
    val holdToEditMs: Long = 1000,
    val editTimeoutMs: Long = 20_000,
    /** The pen follows the screen's rotation, so the app mustn't lock its orientation. */
    val followScreen: Boolean = false,
    /** False when another program sets the area, as OpenTabletDriver does, so it can't be edited here. */
    val editable: Boolean = true,
) {
    companion object {
        /** Reads one `prefs ...` line from the PC, or null if it isn't one. */
        fun parse(line: String): AppPrefs? {
            val parts = line.trim().split(' ')
            // The last field is newer, so a PC from before it leaves editing on.
            if (parts.size !in 5..6 || parts[0] != "prefs") return null
            return try {
                AppPrefs(parts[1] == "1", parts[2].toLong(), parts[3].toLong(), parts[4] == "1", parts.getOrNull(5) != "0")
            } catch (_: NumberFormatException) {
                null
            }
        }
    }
}
