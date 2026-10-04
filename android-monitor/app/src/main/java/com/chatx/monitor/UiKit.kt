package com.chatx.monitor

import android.content.Context
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.text.TextUtils
import android.util.TypedValue
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.LinearLayout
import android.widget.TextView

class UiKit(private val context: Context) {
    private val compact: Boolean =
        context.resources.configuration.screenWidthDp < 380 ||
            context.resources.configuration.fontScale > 1.15f

    val pageHorizontalPadding: Int
        get() = if (compact) 14 else 16

    fun dp(value: Int): Int =
        (value * context.resources.displayMetrics.density).toInt()

    fun color(res: Int): Int = context.getColor(res)

    fun column(): LinearLayout = LinearLayout(context).apply {
        orientation = LinearLayout.VERTICAL
    }

    fun row(): LinearLayout = LinearLayout(context).apply {
        orientation = LinearLayout.HORIZONTAL
        gravity = Gravity.CENTER_VERTICAL
        isBaselineAligned = false
        clipChildren = false
        clipToPadding = false
    }
    fun card(padding: Int = if (compact) 14 else 16): LinearLayout = column().apply {
        setPadding(dp(padding), dp(padding), dp(padding), dp(padding))
        background = rounded(color(R.color.cx_surface), 16f, color(R.color.cx_border))
        elevation = dp(1).toFloat()
    }

    fun heroCard(): LinearLayout = column().apply {
        val padding = if (compact) 14 else 16
        setPadding(dp(padding), dp(padding), dp(padding), dp(padding))
        background = rounded(color(R.color.cx_surface_soft), 18f, color(R.color.cx_border))
    }

    fun title(text: String, size: Float = 20f): TextView = TextView(context).apply {
        this.text = text
        textSize = size
        includeFontPadding = false
        setTextColor(color(R.color.cx_text))
        setTypeface(typeface, Typeface.BOLD)
    }

    fun body(text: String, size: Float = 14f): TextView = TextView(context).apply {
        this.text = text
        textSize = size
        includeFontPadding = false
        setTextColor(color(R.color.cx_text))
        setLineSpacing(0f, 1.15f)
    }
    fun muted(text: String, size: Float = 13f): TextView = TextView(context).apply {
        this.text = text
        textSize = size
        includeFontPadding = false
        setTextColor(color(R.color.cx_text_muted))
        setLineSpacing(0f, 1.15f)
    }

    fun eyebrow(text: String): TextView = TextView(context).apply {
        this.text = text.uppercase()
        textSize = 11f
        includeFontPadding = false
        letterSpacing = 0.08f
        setTextColor(color(R.color.cx_text_muted))
        setTypeface(typeface, Typeface.BOLD)
    }

    fun metric(value: String, label: String): LinearLayout = column().apply {
        addView(title(value, 20f))
        addView(muted(label, 11f), margin(top = 4))
    }

    fun statusTile(
        label: String,
        value: String,
        tone: Tone = Tone.NEUTRAL,
    ): LinearLayout = column().apply {
        background = rounded(
            toneBackground(tone),
            12f,
            color(R.color.cx_border),
        )
        minimumHeight = dp(if (compact) 56 else 60)
        setPadding(dp(10), dp(10), dp(10), dp(10))
        clipChildren = false
        clipToPadding = false
        addView(muted(label, 11f))
        addView(
            title(value, if (compact) 16f else 17f).apply {
                setSingleLine(true)
                setAutoSizeTextTypeUniformWithConfiguration(
                    13,
                    if (compact) 16 else 17,
                    1,
                    TypedValue.COMPLEX_UNIT_SP,
                )
            },
            margin(top = 4),
        )
    }

    fun pill(text: String, tone: Tone = Tone.NEUTRAL): TextView = TextView(context).apply {
        this.text = text
        textSize = 11f
        includeFontPadding = false
        gravity = Gravity.CENTER
        setTypeface(typeface, Typeface.BOLD)
        setPadding(dp(8), dp(5), dp(8), dp(5))
        setTextColor(toneForeground(tone))
        background = rounded(toneBackground(tone), 999f)
    }
    fun button(
        text: String,
        primary: Boolean = false,
        danger: Boolean = false,
        action: () -> Unit,
    ): TextView = TextView(context).apply {
        this.text = text
        textSize = 13f
        includeFontPadding = false
        gravity = Gravity.CENTER
        minHeight = dp(48)
        isClickable = true
        isFocusable = true
        setTypeface(typeface, Typeface.BOLD)
        val backgroundColor = when {
            danger -> color(R.color.cx_danger_bg)
            primary -> color(R.color.cx_primary)
            else -> color(R.color.cx_surface)
        }
        val foregroundColor = when {
            danger -> color(R.color.cx_danger)
            primary -> Color.WHITE
            else -> color(R.color.cx_text)
        }
        setTextColor(foregroundColor)
        background = rounded(
            backgroundColor,
            12f,
            color(R.color.cx_border),
        )
        setPadding(dp(12), dp(10), dp(12), dp(10))
        setOnClickListener { action() }
    }

    fun compactLine(text: String, size: Float = 12f): TextView =
        muted(text, size).apply {
            setSingleLine(true)
            ellipsize = TextUtils.TruncateAt.MIDDLE
        }

    fun divider(): View = View(context).apply {
        setBackgroundColor(color(R.color.cx_border))
    }
    fun rounded(fill: Int, radiusDp: Float, stroke: Int? = null): GradientDrawable =
        GradientDrawable().apply {
            shape = GradientDrawable.RECTANGLE
            setColor(fill)
            val radius = dp(radiusDp.toInt()).toFloat()
            cornerRadii = floatArrayOf(
                radius, radius,
                radius, radius,
                radius, radius,
                radius, radius,
            )
            if (stroke != null) setStroke(dp(1), stroke)
        }

    fun margin(
        width: Int = ViewGroup.LayoutParams.MATCH_PARENT,
        height: Int = ViewGroup.LayoutParams.WRAP_CONTENT,
        left: Int = 0,
        top: Int = 0,
        right: Int = 0,
        bottom: Int = 0,
        weight: Float = 0f,
    ): LinearLayout.LayoutParams = LinearLayout.LayoutParams(width, height, weight).apply {
        setMargins(dp(left), dp(top), dp(right), dp(bottom))
    }

    private fun toneForeground(tone: Tone): Int = when (tone) {
        Tone.SUCCESS -> color(R.color.cx_success)
        Tone.WARNING -> color(R.color.cx_warning)
        Tone.DANGER -> color(R.color.cx_danger)
        Tone.PRIMARY -> color(R.color.cx_primary)
        Tone.NEUTRAL -> color(R.color.cx_text_muted)
    }

    private fun toneBackground(tone: Tone): Int = when (tone) {
        Tone.SUCCESS -> color(R.color.cx_success_bg)
        Tone.WARNING -> color(R.color.cx_warning_bg)
        Tone.DANGER -> color(R.color.cx_danger_bg)
        Tone.PRIMARY -> color(R.color.cx_surface_soft)
        Tone.NEUTRAL -> color(R.color.cx_chip)
    }

    enum class Tone { SUCCESS, WARNING, DANGER, PRIMARY, NEUTRAL }
}
