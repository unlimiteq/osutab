package dev.osutab.app

import android.app.Activity
import android.content.pm.ActivityInfo
import android.graphics.Rect
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
    @Volatile private var running = false
    private var worker: Thread? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        window.attributes = window.attributes.apply {
            layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_ALWAYS
        }
        view = AreaView(this) { edited -> outgoing.offer(edited.toLine()) }
        setContentView(view)
        // Stops edge swipes from a resting palm opening the system's gestures where Android allows.
        view.addOnLayoutChangeListener { v: View, l, t, r, b, _, _, _, _ ->
            v.systemGestureExclusionRects = listOf(Rect(0, 0, r - l, b - t))
        }
    }

    override fun onResume() {
        super.onResume()
        hideSystemBars()
        running = true
        worker = Thread(::connectLoop, "osutab-link").also { it.start() }
    }

    override fun onPause() {
        super.onPause()
        running = false
        worker?.interrupt()
        worker = null
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus) hideSystemBars()
    }

    private fun hideSystemBars() {
        window.setDecorFitsSystemWindows(false)
        window.insetsController?.let {
            it.hide(WindowInsets.Type.systemBars())
            it.systemBarsBehavior = WindowInsetsController.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
    }

    private fun applyOrientation(rotation: Int) {
        requestedOrientation = when (rotation) {
            0 -> ActivityInfo.SCREEN_ORIENTATION_PORTRAIT
            90 -> ActivityInfo.SCREEN_ORIENTATION_LANDSCAPE
            180 -> ActivityInfo.SCREEN_ORIENTATION_REVERSE_PORTRAIT
            270 -> ActivityInfo.SCREEN_ORIENTATION_REVERSE_LANDSCAPE
            else -> ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
        }
    }

    private fun connectLoop() {
        while (running) {
            try {
                Socket().use { socket ->
                    socket.tcpNoDelay = true
                    socket.connect(InetSocketAddress("127.0.0.1", PC_PORT), 1000)
                    main.post { view.connected = true }
                    val reader = BufferedReader(InputStreamReader(socket.getInputStream()))
                    val writer = BufferedWriter(OutputStreamWriter(socket.getOutputStream()))
                    val sender = Thread({
                        try {
                            while (true) {
                                var line = outgoing.take()
                                // Only the newest size matters while a finger is moving.
                                while (true) line = outgoing.poll() ?: break
                                writer.write(line)
                                writer.write("\n")
                                writer.flush()
                            }
                        } catch (_: Exception) {
                        }
                    }, "osutab-send")
                    sender.start()
                    try {
                        while (running) {
                            val line = reader.readLine() ?: break
                            handleLine(line)
                        }
                    } finally {
                        sender.interrupt()
                    }
                }
            } catch (_: Exception) {
            }
            main.post { view.connected = false }
            if (running) {
                try {
                    Thread.sleep(RECONNECT_MS)
                } catch (_: InterruptedException) {
                    return
                }
            }
        }
    }

    private fun handleLine(line: String) {
        when {
            line.startsWith("state ") -> AreaState.parse(line)?.let { state ->
                main.post {
                    // Don't let an echo of an earlier edit fight a finger that's still moving.
                    val current = view.state
                    view.state = if (view.gestureActive) {
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
                    applyOrientation(state.orientation)
                }
            }
            line == "outline 1" -> main.post { view.showOutline = true }
            line == "outline 0" -> main.post { view.showOutline = false }
        }
    }

    companion object {
        const val PC_PORT = 27192
        const val RECONNECT_MS = 500L
    }
}
