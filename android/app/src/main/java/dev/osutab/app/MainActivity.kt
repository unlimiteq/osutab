package dev.osutab.app

import android.app.Activity
import android.content.Context
import android.content.pm.ActivityInfo
import android.content.res.Configuration
import android.graphics.Rect
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.View
import android.view.WindowInsets
import android.view.WindowInsetsController
import android.view.WindowManager
import java.io.BufferedReader
import java.io.BufferedWriter
import java.io.InputStreamReader
import java.io.OutputStreamWriter
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.LinkedBlockingQueue

/**
 * The tablet side of the pen area: a full-screen surface showing the area and taking finger edits.
 * It talks to the PC over a socket that `adb reverse` carries through the USB cable. Pen input
 * doesn't pass through here, so nothing in this app can add latency to it.
 */
class MainActivity : Activity() {
    private lateinit var view: AreaView
    private val main = Handler(Looper.getMainLooper())
    private val outgoing = LinkedBlockingQueue<String>()
    private var link: Link? = null

    // A brief drop shouldn't flash the screen, so the waiting message only shows after this.
    private val showDisconnected = Runnable { view.connected = false }
    private val hideBars = Runnable { hideSystemBars() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        window.attributes = window.attributes.apply {
            layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
        }
        view = AreaView(this, onEdit = { edited -> outgoing.offer(edited.toLine()) }, onEditingChanged = ::setSmoothMotion)
        // The waiting screen has no PC to ask, so it uses the theme from last time.
        getPreferences(Context.MODE_PRIVATE).getString(SAVED_THEME, null)?.let { saved ->
            AppTheme.parse(saved)?.let { view.theme = it }
        }
        setContentView(view)
        // Stops edge swipes from a resting palm opening the system's gestures where Android allows.
        view.addOnLayoutChangeListener { v: View, l, t, r, b, _, _, _, _ ->
            v.systemGestureExclusionRects = listOf(Rect(0, 0, r - l, b - t))
        }
        // Android lets a swipe from the edge bring the bars back, and no ordinary app can stop that.
        // This sends them away again as soon as they appear.
        view.setOnApplyWindowInsetsListener { v, insets ->
            // Checked one at a time: systemBars() also includes the caption bar, which never shows
            // here, and isVisible only says yes when every type asked about is visible.
            if (insets.isVisible(WindowInsets.Type.navigationBars()) || insets.isVisible(WindowInsets.Type.statusBars())) {
                main.removeCallbacks(hideBars)
                main.postDelayed(hideBars, REHIDE_MS)
            }
            v.onApplyWindowInsets(insets)
        }
    }

    override fun onResume() {
        super.onResume()
        hideSystemBars()
        link = Link().also { it.start() }
    }

    override fun onPause() {
        super.onPause()
        link?.shutdown()
        link = null
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) hideSystemBars()
    }

    override fun onTopResumedActivityChanged(isTopResumed: Boolean) {
        super.onTopResumedActivityChanged(isTopResumed)
        if (isTopResumed) askForFullScreen()
    }

    override fun onMultiWindowModeChanged(isInMultiWindowMode: Boolean, newConfig: Configuration) {
        super.onMultiWindowModeChanged(isInMultiWindowMode, newConfig)
        view.floating = isInMultiWindowMode
    }

    /**
     * The area only lines up with the pen at full screen, but a tablet in its desktop mode opens
     * apps in a floating window. If Android says no, the view asks the user instead.
     */
    private fun askForFullScreen() {
        view.floating = isInMultiWindowMode
        if (isInMultiWindowMode && Build.VERSION.SDK_INT >= Build.VERSION_CODES.VANILLA_ICE_CREAM) {
            requestFullscreenMode(FULLSCREEN_MODE_REQUEST_ENTER, null)
        }
    }

    private fun hideSystemBars() {
        window.setDecorFitsSystemWindows(false)
        window.insetsController?.let {
            // With the default behaviour the bars really appear when swiped, which reports them to
            // this app. The transient behaviour hides that, leaving them up for several seconds.
            it.systemBarsBehavior = WindowInsetsController.BEHAVIOR_DEFAULT
            it.hide(WindowInsets.Type.systemBars())
        }
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        reportRotation()
    }

    /** When the pen follows the screen, the PC needs to know each time the screen turns. */
    private fun reportRotation() {
        if (view.prefs.followScreen) outgoing.offer("rotation ${(display?.rotation ?: 0) * 90}")
    }

    private fun applyTheme(theme: AppTheme) {
        if (view.theme == theme) return
        view.theme = theme
        getPreferences(Context.MODE_PRIVATE).edit().putString(SAVED_THEME, theme.toLine()).apply()
    }

    private fun applyPrefs(prefs: AppPrefs) {
        view.prefs = prefs
        if (prefs.followScreen) {
            requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_FULL_USER
            reportRotation()
        }
    }

    /** Edit mode runs at the display's top rate so dragging is smooth. Play doesn't need the power. */
    private fun setSmoothMotion(on: Boolean) {
        window.attributes = window.attributes.apply {
            preferredRefreshRate = if (on) display?.supportedModes?.maxOfOrNull { it.refreshRate } ?: 0f else 0f
        }
    }

    private fun applyOrientation(rotation: Int) {
        val wanted = when (rotation) {
            0 -> ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
            90 -> ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
            180 -> ActivityInfo.SCREEN_ORIENTATION_REVERSE_PORTRAIT
            270 -> ActivityInfo.SCREEN_ORIENTATION_REVERSE_LANDSCAPE
            else -> ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
        }
        if (requestedOrientation != wanted) requestedOrientation = wanted
    }

    /** Runs on the main thread. A connection only counts once the PC has really said something. */
    private fun onPcSpoke() {
        main.removeCallbacks(showDisconnected)
        view.connected = true
    }

    private fun onPcGone() {
        main.removeCallbacks(showDisconnected)
        main.postDelayed(showDisconnected, DISCONNECT_GRACE_MS)
    }

    private fun handleLine(line: String) {
        when {
            line.startsWith("state ") -> AreaState.parse(line)?.let { state ->
                val current = view.state
                // Don't let an echo of an earlier edit fight a finger, or undo where it stopped.
                view.state = if (view.holdsArea) {
                    state.copy(
                        width = current.width,
                        height = current.height,
                        offsetX = current.offsetX,
                        offsetY = current.offsetY,
                        rotationDeg = current.rotationDeg,
                    )
                } else {
                    state
                }
                if (!view.prefs.followScreen) applyOrientation(state.orientation)
            }
            line.startsWith("theme ") -> AppTheme.parse(line)?.let(::applyTheme)
            line.startsWith("prefs ") -> AppPrefs.parse(line)?.let(::applyPrefs)
            line == "outline 1" -> view.showOutline = true
            line == "outline 0" -> view.showOutline = false
        }
    }

    /** One connection attempt loop. Only one exists at a time, even across pausing and resuming. */
    private inner class Link : Thread("osutab-link") {
        @Volatile private var stopped = false
        @Volatile private var socket: Socket? = null

        fun shutdown() {
            stopped = true
            try {
                socket?.close()
            } catch (_: Exception) {
            }
            interrupt()
        }

        override fun run() {
            while (!stopped) {
                var spoke = false
                try {
                    Socket().use { s ->
                        socket = s
                        s.tcpNoDelay = true
                        s.connect(InetSocketAddress("127.0.0.1", PC_PORT), CONNECT_TIMEOUT_MS)
                        val reader = BufferedReader(InputStreamReader(s.getInputStream()))
                        val writer = BufferedWriter(OutputStreamWriter(s.getOutputStream()))
                        val sender = startSender(writer)
                        try {
                            while (!stopped) {
                                val line = reader.readLine() ?: break
                                if (!spoke) {
                                    spoke = true
                                    main.post { onPcSpoke() }
                                }
                                main.post { handleLine(line) }
                            }
                        } finally {
                            sender.interrupt()
                        }
                    }
                } catch (_: Exception) {
                }
                if (spoke) main.post { onPcGone() }
                if (stopped) return
                try {
                    sleep(RECONNECT_MS)
                } catch (_: InterruptedException) {
                    return
                }
            }
        }

        private fun startSender(writer: BufferedWriter): Thread {
            outgoing.clear()
            return Thread({
                try {
                    while (true) {
                        val batch = mutableListOf(outgoing.take())
                        outgoing.drainTo(batch)
                        // Only the newest size matters while a finger is moving.
                        val newestSet = batch.lastOrNull { it.startsWith("set ") }
                        for (line in batch) {
                            if (line.startsWith("set ") && line !== newestSet) continue
                            writer.write(line)
                            writer.write("\n")
                        }
                        writer.flush()
                    }
                } catch (_: Exception) {
                }
            }, "osutab-send").also { it.start() }
        }
    }

    companion object {
        const val PC_PORT = 27192
        const val RECONNECT_MS = 500L
        const val CONNECT_TIMEOUT_MS = 1000
        const val DISCONNECT_GRACE_MS = 1500L
        const val REHIDE_MS = 150L
        const val SAVED_THEME = "theme"
    }
}
