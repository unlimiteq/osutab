package dev.osutab.app

import android.animation.ValueAnimator
import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Path
import android.graphics.RectF
import android.graphics.Typeface
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
 * anything else on the tablet. The pen never arrives here: the helper holds it for the PC, which
 * is why nothing drawn or animated here can affect pen latency.
 */
class AreaView(
    context: Context,
    private val onEdit: (AreaState) -> Unit,
    private val onEditingChanged: (Boolean) -> Unit,
) : View(context) {
    var state = AreaState()
        set(value) {
            if (field == value) return
            val previous = field
            field = value
            // A new preset glides into place. Anything else, a finger above all, is followed exactly.
            if (previous.preset != value.preset && !holdsArea) glideFrom(previous) else glideProgress = null
            invalidate()
        }

    /** Editing needs the PC, so losing the connection ends it and shows one steady message. */
    var connected = false
        set(value) {
            if (field == value) return
            field = value
            if (!value) {
                cancelHold()
                setEditing(false)
            }
            fadeContentIn()
            invalidate()
        }
    /** In a floating window the area would show in the wrong place, so it isn't drawn at all. */
    var floating = false
        set(value) {
            if (field == value) return
            field = value
            if (value) {
                cancelHold()
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
    var theme = AppTheme()
        set(value) {
            if (field != value) {
                field = value
                applyTheme()
                invalidate()
            }
        }
    var prefs = AppPrefs()
        set(value) {
            if (field != value) {
                field = value
                if (!value.editable) {
                    cancelHold()
                    setEditing(false)
                }
                invalidate()
            }
        }

    /** True while updates from the PC would fight the area's own edits. */
    val holdsArea: Boolean
        get() = gesture != Gesture.NONE || SystemClock.uptimeMillis() - lastEditAt < ECHO_GUARD_MS

    private var editing = false

    private val density = resources.displayMetrics.density
    private val fillPaint = Paint()
    private val outlinePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply { style = Paint.Style.STROKE }
    private val handlePaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val regular = Typeface.createFromAsset(context.assets, "fonts/Geist-Regular.ttf")
    private val semibold = Typeface.createFromAsset(context.assets, "fonts/Geist-SemiBold.ttf")
    private val textPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textSize = 40f
        typeface = semibold
    }
    private val smallPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textSize = 26f
        typeface = regular
    }
    private val buttonPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val buttonTextPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        textSize = 24f
        typeface = semibold
    }
    private val holdPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = 5f
        strokeCap = Paint.Cap.ROUND
    }
    private val logoPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = 12f
        color = LOGO_COLOR
    }
    private val handleEdgePaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        style = Paint.Style.STROKE
        strokeWidth = 4f
    }
    private val pillRect = RectF()
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

    // Everything animated. Each runs once and stops: nothing here loops or draws while idle.
    private var glideFrom: List<Pair<Double, Double>>? = null
    private var glideProgress: Float? = null
    private var glide: ValueAnimator? = null
    private var handleAlpha = 0f
    private var contentAlpha = 1f
    private var holdProgress = 0f
    private var holdAnimator: ValueAnimator? = null
    private var handleAnimator: ValueAnimator? = null
    private var contentAnimator: ValueAnimator? = null

    init {
        applyTheme()
    }

    private fun applyTheme() {
        fillPaint.color = theme.background
        outlinePaint.color = theme.outline
        // Scaled for the screen's density, so a theme looks the same on the PC's preview.
        outlinePaint.strokeWidth = theme.outlineThickness * density
        handlePaint.color = theme.accent
        textPaint.color = theme.text
        smallPaint.color = theme.muted
        // A tablet with the same shade for panels and background still needs to show the button.
        buttonPaint.color = if (theme.panel == theme.outside) theme.line else theme.panel
        buttonTextPaint.color = theme.text
        handleEdgePaint.color = theme.background
        holdPaint.color = theme.accent
        if (!theme.animations) {
            glide?.cancel()
            contentAnimator?.cancel()
            handleAnimator?.cancel()
            holdAnimator?.cancel()
            glideProgress = null
            contentAlpha = 1f
            handleAlpha = if (editing) 1f else 0f
        }
    }

    private fun animate(durationMs: Long, start: Float, end: Float, update: (Float) -> Unit): ValueAnimator =
        ValueAnimator.ofFloat(start, end).apply {
            duration = durationMs
            addUpdateListener {
                update(it.animatedValue as Float)
                invalidate()
            }
            start()
        }

    private fun glideFrom(previous: AreaState) {
        glide?.cancel()
        if (!theme.animations || !connected) {
            glideProgress = null
            return
        }
        glideFrom = previous.corners()
        glideProgress = 0f
        glide = animate(GLIDE_MS, 0f, 1f) { value -> glideProgress = if (value < 1f) value else null }
    }

    private fun fadeContentIn() {
        contentAnimator?.cancel()
        if (!theme.animations) {
            contentAlpha = 1f
            return
        }
        contentAlpha = 0f
        contentAnimator = animate(FADE_MS, 0f, 1f) { contentAlpha = it }
    }

    private fun startHold() {
        postDelayed(enterEditing, prefs.holdToEditMs)
        holdAnimator?.cancel()
        if (theme.animations) holdAnimator = animate(prefs.holdToEditMs, 0f, 1f) { holdProgress = it }
    }

    private fun cancelHold() {
        removeCallbacks(enterEditing)
        holdAnimator?.cancel()
        holdProgress = 0f
        invalidate()
    }

    private fun inEditButton(x: Float, y: Float) = x < BUTTON_SIZE_PX && y > height - BUTTON_SIZE_PX

    /** Where to draw the corners, in pixels, partway along a glide if there is one. */
    private fun drawnCorners(scale: Float): List<Pair<Float, Float>> {
        val target = state.corners()
        val from = glideFrom
        val progress = glideProgress
        return target.mapIndexed { i, corner ->
            if (from != null && progress != null) {
                val eased = 1 - (1 - progress) * (1 - progress)
                ((from[i].first + (corner.first - from[i].first) * eased) * scale).toFloat() to
                    ((from[i].second + (corner.second - from[i].second) * eased) * scale).toFloat()
            } else {
                (corner.first * scale).toFloat() to (corner.second * scale).toFloat()
            }
        }
    }

    override fun onDraw(canvas: Canvas) {
        val alpha = (contentAlpha * 255).toInt()
        // There's no area to set off here, and the theme's text is only sure to read on its background.
        canvas.drawColor(if (connected && !floating) theme.outside else theme.background)
        if (floating) {
            canvas.drawCircle(width / 2f, height / 2f - 90f, LOGO_RADIUS_PX, logoPaint)
            drawCentred(canvas, "Make this window full screen", height / 2f + 20f, textPaint)
            drawCentred(canvas, "The pen area only lines up at full screen. Use the button in the title bar.", height / 2f + 66f, smallPaint)
            return
        }
        if (!connected) {
            textPaint.alpha = alpha
            smallPaint.alpha = alpha
            logoPaint.alpha = alpha
            canvas.drawCircle(width / 2f, height / 2f - 90f, LOGO_RADIUS_PX, logoPaint)
            drawCentred(canvas, "Waiting for the PC", height / 2f + 20f, textPaint)
            drawCentred(canvas, "Start osu!tab on your computer.", height / 2f + 66f, smallPaint)
            textPaint.alpha = 255
            smallPaint.alpha = 255
            return
        }
        val scale = pxPerMm.toFloat()
        if (showOutline || editing) {
            val corners = drawnCorners(scale)
            areaPath.rewind()
            areaPath.moveTo(corners[0].first, corners[0].second)
            for (i in 1 until corners.size) areaPath.lineTo(corners[i].first, corners[i].second)
            areaPath.close()
            canvas.drawPath(areaPath, fillPaint)
            canvas.drawPath(areaPath, outlinePaint)
            if (handleAlpha > 0f) {
                handlePaint.alpha = (handleAlpha * 255).toInt()
                handleEdgePaint.alpha = handlePaint.alpha
                for (corner in corners) {
                    canvas.drawCircle(corner.first, corner.second, HANDLE_RADIUS_PX, handlePaint)
                    canvas.drawCircle(corner.first, corner.second, HANDLE_RADIUS_PX, handleEdgePaint)
                }
            }
            if (prefs.showAreaLabel || editing) {
                val cx = (corners[0].first + corners[2].first) / 2
                val cy = (corners[0].second + corners[2].second) / 2
                val label = "%.1f × %.1f mm".format(state.width, state.height)
                val detail = "${state.preset}  ·  ${state.sensitivityLabel()}"
                canvas.drawText(label, cx - textPaint.measureText(label) / 2, cy, textPaint)
                canvas.drawText(detail, cx - smallPaint.measureText(detail) / 2, cy + 42f, smallPaint)
            }
        }
        canvas.drawCircle(BUTTON_CENTRE_PX, height - BUTTON_CENTRE_PX, BUTTON_RADIUS_PX, buttonPaint)
        if (holdProgress > 0f && !editing) {
            canvas.drawArc(
                BUTTON_CENTRE_PX - BUTTON_RADIUS_PX,
                height - BUTTON_CENTRE_PX - BUTTON_RADIUS_PX,
                BUTTON_CENTRE_PX + BUTTON_RADIUS_PX,
                height - BUTTON_CENTRE_PX + BUTTON_RADIUS_PX,
                -90f,
                holdProgress * 360f,
                false,
                holdPaint,
            )
        }
        if (!prefs.editable && !editing) return
        val button = if (editing) "Done" else "Hold"
        canvas.drawText(button, BUTTON_CENTRE_PX - buttonTextPaint.measureText(button) / 2, height - BUTTON_CENTRE_PX + 8f, buttonTextPaint)
        if (editing) drawEditHint(canvas)
    }

    private fun drawEditHint(canvas: Canvas) {
        val text = "Drag to move. Drag a corner dot or pinch to resize."
        val halfWidth = smallPaint.measureText(text) / 2 + 28f
        // Low enough to clear the window handle Samsung's tablets show at the top of the screen.
        pillRect.set(width / 2f - halfWidth, 90f, width / 2f + halfWidth, 150f)
        canvas.drawRoundRect(pillRect, 30f, 30f, buttonPaint)
        drawCentred(canvas, text, 129f, smallPaint)
    }

    private fun drawCentred(canvas: Canvas, text: String, y: Float, paint: Paint) {
        canvas.drawText(text, (width - paint.measureText(text)) / 2, y, paint)
    }

    // During play, palms and stray fingers stop here. Only holding the corner button responds.
    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (!connected || floating) return true
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> {
                if (inEditButton(event.x, event.y) && (prefs.editable || editing)) {
                    if (editing) setEditing(false) else startHold()
                } else if (editing) {
                    // Delivers each touch as it happens, instead of once a frame.
                    requestUnbufferedDispatch(InputDevice.SOURCE_TOUCHSCREEN)
                    beginDrag(event)
                }
            }
            MotionEvent.ACTION_POINTER_DOWN -> {
                // A second contact means a palm or a pinch, never a deliberate hold of the button.
                cancelHold()
                if (editing && event.pointerCount == 2) {
                    gesture = Gesture.PINCH
                    pinchDistance = distance(event)
                }
            }
            MotionEvent.ACTION_MOVE -> {
                if (!inEditButton(event.x, event.y)) cancelHold()
                if (editing) drag(event)
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> {
                cancelHold()
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
        holdAnimator?.cancel()
        holdProgress = 0f
        noteActivity()
        if (changed) {
            onEditingChanged(on)
            handleAnimator?.cancel()
            val target = if (on) 1f else 0f
            if (theme.animations) {
                handleAnimator = animate(FADE_MS, handleAlpha, target) { handleAlpha = it }
            } else {
                handleAlpha = target
            }
            invalidate()
        }
    }

    /** Edit mode ends by itself, so a forgotten edit can't let a palm move the area in play. */
    private fun noteActivity() {
        removeCallbacks(leaveEditing)
        if (editing) postDelayed(leaveEditing, prefs.editTimeoutMs)
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
        const val BUTTON_CENTRE_PX = 70f
        const val BUTTON_RADIUS_PX = 48f
        const val LOGO_RADIUS_PX = 34f
        val LOGO_COLOR = Color.rgb(0xff, 0x5f, 0xa2)
        const val ECHO_GUARD_MS = 500L
        const val GLIDE_MS = 150L
        const val FADE_MS = 200L
    }
}
