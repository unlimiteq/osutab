package dev.osutab.app

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
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
) : View(context) {
    var state = AreaState()
        set(value) {
            field = value
            invalidate()
        }
    var connected = false
        set(value) {
            field = value
            invalidate()
        }
    var showOutline = true
        set(value) {
            field = value
            invalidate()
        }

    /** True while a finger is changing the area, when updates from the PC would fight it. */
    val gestureActive: Boolean get() = gesture != Gesture.NONE

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

    private val enterEditing = Runnable { setEditing(true) }
    private val leaveEditing = Runnable { setEditing(false) }

    private fun inEditButton(x: Float, y: Float) = x < BUTTON_SIZE_PX && y > height - BUTTON_SIZE_PX

    override fun onDraw(canvas: Canvas) {
        canvas.drawColor(outsidePaint)
        val scale = pxPerMm.toFloat()
        if (showOutline || editing) {
            val corners = state.corners().map { (it.first * scale).toFloat() to (it.second * scale).toFloat() }
            areaPath.reset()
            areaPath.moveTo(corners[0].first, corners[0].second)
            corners.drop(1).forEach { areaPath.lineTo(it.first, it.second) }
            areaPath.close()
            canvas.drawPath(areaPath, fillPaint)
            canvas.drawPath(areaPath, outlinePaint)
            if (editing) {
                corners.forEach { canvas.drawCircle(it.first, it.second, HANDLE_RADIUS_PX, handlePaint) }
            }
            val cx = (state.centerX * scale).toFloat()
            val cy = (state.centerY * scale).toFloat()
            val label = "%.1f × %.1f mm".format(state.width, state.height)
            val detail = state.sensitivityLabel()
            canvas.drawText(label, cx - textPaint.measureText(label) / 2, cy - 8f, textPaint)
            canvas.drawText(detail, cx - smallPaint.measureText(detail) / 2, cy + 28f, smallPaint)
            canvas.drawText(state.preset, cx - smallPaint.measureText(state.preset) / 2, cy + 62f, smallPaint)
        }
        if (!connected) {
            val message = "Waiting for the PC. Start osutab on your computer."
            val hint = "The S Pen works normally on this tablet until osutab connects."
            canvas.drawText(message, (width - textPaint.measureText(message)) / 2, 60f, textPaint)
            canvas.drawText(hint, (width - smallPaint.measureText(hint)) / 2, 100f, smallPaint)
            return
        }
        canvas.drawCircle(70f, height - 70f, 60f, buttonPaint)
        val label = if (editing) "Done" else "Hold"
        canvas.drawText(label, 70f - smallPaint.measureText(label) / 2, height - 61f, smallPaint)
        if (editing) {
            val help = "Drag to move. Drag a corner dot or pinch to resize."
            canvas.drawText(help, (width - smallPaint.measureText(help)) / 2, 44f, smallPaint)
        }
    }

    // During play, palms and stray fingers stop here. Only holding the corner button responds.
    override fun onTouchEvent(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                if (inEditButton(event.x, event.y)) {
                    if (editing) setEditing(false) else postDelayed(enterEditing, HOLD_TO_EDIT_MS)
                } else if (editing) {
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
        editing = on
        gesture = Gesture.NONE
        noteActivity()
        invalidate()
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
                apply(state.withSize(along * 2, across * 2))
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
    }
}
