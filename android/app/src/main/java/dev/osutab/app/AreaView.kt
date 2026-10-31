package dev.osutab.app

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.os.SystemClock
import android.view.InputDevice
import android.view.MotionEvent
import android.view.View
import kotlin.math.abs
import kotlin.math.cos
import kotlin.math.hypot
import kotlin.math.sin

/**
 * Draws the pen area and takes finger gestures. It consumes every touch, so a palm can't reach
 * anything else on the tablet. The pen never arrives here: the helper holds it for the PC.
 */
class AreaView(
    context: Context,
    private val onEdit: (AreaState) -> Unit,
    private val onEditingChanged: (Boolean) -> Unit,
) : View(context) {
    var state = AreaState()
        set(value) {
            if (field != value) {
                field = value
                invalidate()
            }
        }

    /** Editing needs the PC, so losing the connection ends it and shows one steady message. */
    var connected = false
        set(value) {
            if (field == value) return
            field = value
            if (!value) {
                removeCallbacks(enterEditing)
                setEditing(false)
            }
            invalidate()
        }
    var showOutline = true
        set(value) {
            if (field != value) {
                field = value
                invalidate()
            }
        }

    /** True while updates from the PC would fight the area's own edits. */
    val holdsArea: Boolean
        get() = gesture != Gesture.NONE || SystemClock.uptimeMillis() - lastEditAt < ECHO_GUARD_MS

    private var editing = false

    private val outsidePaint = Color.rgb(8, 9, 11)
    private val fillPaint = Paint().apply { color = Color.rgb(28, 33, 41) }
    private val outlinePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.rgb(90, 170, 255)
        style = Paint.Style.STROKE
        strokeWidth = 4f
    }
    private val handlePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.rgb(90, 170, 255) }
    private val textPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.argb(220, 255, 255, 255)
        textSize = 34f
    }
    private val smallPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.argb(170, 255, 255, 255)
        textSize = 26f
    }
    private val buttonPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { color = Color.argb(90, 255, 255, 255) }
    private val areaPath = Path()

    private val pxPerMm: Double get() = width / state.tabletWidth

    private enum class Gesture { NONE, MOVE, CORNER, PINCH }

    private var gesture = Gesture.NONE
    private var lastX = 0f
    private var lastY = 0f
    private var pinchDistance = 0f
    private var startWidth = 0.0
    private var startHeight = 0.0
    private var lastEditAt = 0L

    private val enterEditing = Runnable { setEditing(true) }
    private val leaveEditing = Runnable { setEditing(false) }

    private fun inEditButton(x: Float, y: Float) = x < BUTTON_SIZE_PX && y > height - BUTTON_SIZE_PX

    override fun onDraw(canvas: Canvas) {
        canvas.drawColor(outsidePaint)
        if (!connected) {
            drawCentred(canvas, "Waiting for the PC", height / 2f - 10f, textPaint)
            drawCentred(canvas, "Start osutab on your computer. You can edit the area once it connects.", height / 2f + 30f, smallPaint)
            return
        }
        val scale = pxPerMm.toFloat()
        if (showOutline || editing) {
            val corners = state.corners().map { (it.first * scale).toFloat() to (it.second * scale).toFloat() }
            areaPath.rewind()
            areaPath.moveTo(corners[0].first, corners[0].second)
            for (i in 1 until corners.size) areaPath.lineTo(corners[i].first, corners[i].second)
            areaPath.close()
            canvas.drawPath(areaPath, fillPaint)
            canvas.drawPath(areaPath, outlinePaint)
            if (editing) {
                for (corner in corners) canvas.drawCircle(corner.first, corner.second, HANDLE_RADIUS_PX, handlePaint)
            }
            val cx = (state.centerX * scale).toFloat()
            val cy = (state.centerY * scale).toFloat()
            val label = "%.1f × %.1f mm".format(state.width, state.height)
            val detail = state.sensitivityLabel()
            canvas.drawText(label, cx - textPaint.measureText(label) / 2, cy - 8f, textPaint)
            canvas.drawText(detail, cx - smallPaint.measureText(detail) / 2, cy + 28f, smallPaint)
            canvas.drawText(state.preset, cx - smallPaint.measureText(state.preset) / 2, cy + 62f, smallPaint)
        }
        canvas.drawCircle(70f, height - 70f, 60f, buttonPaint)
        val button = if (editing) "Done" else "Hold"
        canvas.drawText(button, 70f - smallPaint.measureText(button) / 2, height - 61f, smallPaint)
        if (editing) drawCentred(canvas, "Drag to move. Drag a corner dot or pinch to resize.", 44f, smallPaint)
    }

    private fun drawCentred(canvas: Canvas, text: String, y: Float, paint: Paint) {
        canvas.drawText(text, (width - paint.measureText(text)) / 2, y, paint)
    }

    // During play, palms and stray fingers stop here. Only holding the corner button responds.
    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (!connected) return true
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                if (inEditButton(event.x, event.y)) {
                    if (editing) setEditing(false) else postDelayed(enterEditing, HOLD_TO_EDIT_MS)
                } else if (editing) {
                    // Delivers each touch as it happens, instead of once a frame.
                    requestUnbufferedDispatch(InputDevice.SOURCE_TOUCHSCREEN)
                    beginDrag(event)
                }
            }
            MotionEvent.ACTION_POINTER_DOWN -> {
                // A second contact means a palm or a pinch, never a deliberate hold of the button.
                removeCallbacks(enterEditing)
                if (editing && event.pointerCount == 2) {
                    gesture = Gesture.PINCH
                    pinchDistance = distance(event)
                }
            }
            MotionEvent.ACTION_MOVE -> {
                if (!inEditButton(event.x, event.y)) removeCallbacks(enterEditing)
                if (editing) drag(event)
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                removeCallbacks(enterEditing)
                gesture = Gesture.NONE
                noteActivity()
            }
            MotionEvent.ACTION_POINTER_UP -> {
                gesture = Gesture.NONE
            }
        }
        return true
    }

    private fun setEditing(on: Boolean) {
        val changed = editing != on
        editing = on
        gesture = Gesture.NONE
        noteActivity()
        if (changed) {
            onEditingChanged(on)
            invalidate()
        }
    }

    /** Edit mode ends by itself, so a forgotten edit can't let a palm move the area in play. */
    private fun noteActivity() {
        removeCallbacks(leaveEditing)
        if (editing) postDelayed(leaveEditing, EDIT_TIMEOUT_MS)
    }

    private fun distance(e: MotionEvent) = hypot(e.getX(0) - e.getX(1), e.getY(0) - e.getY(1))

    private fun beginDrag(e: MotionEvent) {
        lastX = e.x
        lastY = e.y
        startWidth = state.width
        startHeight = state.height
        val scale = pxPerMm
        val nearCorner = state.corners().any { (x, y) ->
            hypot(x * scale - e.x, y * scale - e.y) <= HANDLE_RADIUS_PX * 3
        }
        gesture = if (nearCorner) Gesture.CORNER else Gesture.MOVE
        noteActivity()
    }

    private fun drag(e: MotionEvent) {
        val scale = pxPerMm
        when (gesture) {
            Gesture.MOVE -> {
                val next = state.movedBy((e.x - lastX) / scale, (e.y - lastY) / scale)
                lastX = e.x
                lastY = e.y
                apply(next)
            }
            Gesture.CORNER -> {
                // The area stays centred, so a corner sets half the size. It works in the
                // area's own axes so a rotated area resizes along its sides.
                val r = Math.toRadians(state.rotationDeg)
                val dx = e.x / scale - state.centerX
                val dy = e.y / scale - state.centerY
                val along = abs(dx * cos(r) + dy * sin(r))
                val across = abs(-dx * sin(r) + dy * cos(r))
                apply(state.resizedFromCorner(along * 2, across * 2, startWidth, startHeight))
            }
            Gesture.PINCH -> {
                if (e.pointerCount >= 2 && pinchDistance > 0f) {
                    val d = distance(e)
                    apply(state.scaledBy((d / pinchDistance).toDouble()))
                    pinchDistance = d
                }
            }
            Gesture.NONE -> {}
        }
    }

    private fun apply(next: AreaState) {
        noteActivity()
        lastEditAt = SystemClock.uptimeMillis()
        if (next != state) {
            state = next
            onEdit(next)
        }
    }

    private companion object {
        const val HANDLE_RADIUS_PX = 18f
        const val BUTTON_SIZE_PX = 150f
        const val HOLD_TO_EDIT_MS = 1000L
        const val EDIT_TIMEOUT_MS = 20_000L
        const val ECHO_GUARD_MS = 500L
    }
}
