package org.omy.app

import android.app.Activity
import android.app.Dialog
import android.content.ComponentName
import android.content.res.Configuration
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.RippleDrawable
import android.text.TextUtils
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.view.Window
import android.view.WindowManager
import android.widget.FrameLayout
import android.widget.HorizontalScrollView
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.TextView
import android.content.pm.ResolveInfo
import java.util.Locale
import kotlin.math.min

/**
 * 与 Web 端主题一致的原生“用其他应用打开”对话框。
 *
 * 只使用 Android 平台 View，不依赖 Material 控件。应用图标来自
 * ResolveInfo.loadIcon，名称与包名由系统包管理器提供。
 */
internal class OpenWithDialog(
  private val activity: Activity,
  private val handlers: List<ResolveInfo>,
  private val fileName: String,
  private val extension: String,
  private val appearance: String,
  private val remembered: ComponentName?,
  private val onOpen: (ResolveInfo, Boolean) -> Unit,
  private val onCancel: () -> Unit,
) {
  private data class Palette(
    val background: Int,
    val surface: Int,
    val border: Int,
    val foreground: Int,
    val secondary: Int,
    val accent: Int,
    val accentSolid: Int,
    val accentSoft: Int,
    val disabled: Int,
    val ripple: Int,
  )

  private val density = activity.resources.displayMetrics.density
  private val palette = palette()
  private val rows = mutableListOf<LinearLayout>()
  private val marks = mutableListOf<TextView>()
  private var selected = remembered?.let { component ->
    handlers.indexOfFirst { resolveComponent(it) == component }
  } ?: if (handlers.size == 1) 0 else -1
  private var rememberChecked = false
  private var completed = false
  private var openButton: TextView? = null

  fun show() {
    val dialog = Dialog(activity, android.R.style.Theme_DeviceDefault_Dialog_NoActionBar)
    dialog.requestWindowFeature(Window.FEATURE_NO_TITLE)
    dialog.setContentView(buildContent(dialog))
    dialog.setCanceledOnTouchOutside(true)
    dialog.setOnCancelListener { finishCancel() }
    dialog.show()

    val screen = activity.resources.displayMetrics
    val width = min(screen.widthPixels - dp(16), dp(600))
    dialog.window?.apply {
      setBackgroundDrawable(android.graphics.drawable.ColorDrawable(Color.TRANSPARENT))
      addFlags(WindowManager.LayoutParams.FLAG_DIM_BEHIND)
      attributes = attributes.apply { dimAmount = 0.58f }
      setLayout(width, WindowManager.LayoutParams.WRAP_CONTENT)
      decorView.elevation = dp(24).toFloat()
    }
  }

  private fun buildContent(dialog: Dialog): View {
    val root = LinearLayout(activity).apply {
      orientation = LinearLayout.VERTICAL
      setPadding(dp(20), dp(20), dp(20), dp(18))
      background = rounded(palette.background, 18f, 1, palette.border)
    }

    val titleLine = LinearLayout(activity).apply {
      orientation = LinearLayout.HORIZONTAL
      gravity = Gravity.CENTER_VERTICAL
      layoutParams = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        ViewGroup.LayoutParams.WRAP_CONTENT,
      )
    }
    titleLine.addView(TextView(activity).apply {
      text = activity.getString(R.string.file_chooser_title)
      setTextColor(palette.foreground)
      textSize = 21f
      typeface = Typeface.create(Typeface.DEFAULT, Typeface.BOLD)
      includeFontPadding = false
      maxLines = 1
    })
    titleLine.addView(TextView(activity).apply {
      text = fileName
      setTextColor(palette.secondary)
      textSize = 13f
      maxLines = 1
      ellipsize = TextUtils.TruncateAt.MIDDLE
      includeFontPadding = false
      gravity = Gravity.END or Gravity.CENTER_VERTICAL
      layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
        marginStart = dp(16)
      }
    })
    titleLine.addView(TextView(activity).apply {
      text = extensionLabel()
      setTextColor(palette.accent)
      textSize = 11f
      typeface = Typeface.create(Typeface.DEFAULT, Typeface.BOLD)
      gravity = Gravity.CENTER
      setPadding(dp(9), dp(4), dp(9), dp(4))
      background = rounded(palette.accentSoft, 999f, 1, palette.accent)
      layoutParams = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.WRAP_CONTENT,
        ViewGroup.LayoutParams.WRAP_CONTENT,
      ).apply { marginStart = dp(10) }
    })
    root.addView(titleLine)

    root.addView(TextView(activity).apply {
      text = activity.getString(R.string.file_chooser_available_apps)
      setTextColor(palette.secondary)
      textSize = 12f
      typeface = Typeface.create(Typeface.DEFAULT, Typeface.BOLD)
      includeFontPadding = false
      layoutParams = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        ViewGroup.LayoutParams.WRAP_CONTENT,
      ).apply {
        topMargin = dp(18)
        bottomMargin = dp(10)
      }
    })

    val list = LinearLayout(activity).apply {
      orientation = LinearLayout.HORIZONTAL
      gravity = Gravity.CENTER_VERTICAL
    }
    handlers.forEachIndexed { index, info -> list.addView(appTile(index, info)) }

    root.addView(HorizontalScrollView(activity).apply {
      isFillViewport = false
      isHorizontalScrollBarEnabled = handlers.size > 4
      overScrollMode = View.OVER_SCROLL_IF_CONTENT_SCROLLS
      addView(list)
      layoutParams = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        dp(112),
      )
    })

    root.addView(rememberRow())

    val actions = LinearLayout(activity).apply {
      orientation = LinearLayout.HORIZONTAL
      gravity = Gravity.END or Gravity.CENTER_VERTICAL
      layoutParams = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        ViewGroup.LayoutParams.WRAP_CONTENT,
      ).apply { topMargin = dp(18) }
    }
    val cancel = actionButton(
      activity.getString(R.string.file_chooser_cancel),
      primary = false,
      enabled = true,
    ).apply {
      setOnClickListener {
        finishCancel()
        dialog.dismiss()
      }
    }
    val open = actionButton(
      activity.getString(R.string.file_chooser_open),
      primary = true,
      enabled = selected >= 0,
    )
    openButton = open
    open.setOnClickListener {
      val target = handlers.getOrNull(selected) ?: return@setOnClickListener
      completed = true
      onOpen(target, rememberChecked)
      dialog.dismiss()
    }
    actions.addView(cancel)
    actions.addView(open, LinearLayout.LayoutParams(dp(108), dp(46)).apply { marginStart = dp(10) })
    root.addView(actions)

    refreshSelection()
    return root
  }

  private fun appTile(index: Int, info: ResolveInfo): View {
    val component = resolveComponent(info)
    val isRemembered = component == remembered
    val tile = LinearLayout(activity).apply {
      orientation = LinearLayout.VERTICAL
      gravity = Gravity.CENTER_HORIZONTAL
      setPadding(dp(8), dp(8), dp(8), dp(7))
      isClickable = true
      isFocusable = true
      contentDescription = info.loadLabel(activity.packageManager).toString()
      layoutParams = LinearLayout.LayoutParams(dp(92), dp(92)).apply {
        marginEnd = dp(8)
      }
      setOnClickListener {
        selected = index
        refreshSelection()
      }
    }

    val iconBox = FrameLayout(activity).apply {
      layoutParams = LinearLayout.LayoutParams(dp(48), dp(48))
    }
    iconBox.addView(ImageView(activity).apply {
      setImageDrawable(runCatching { info.loadIcon(activity.packageManager) }.getOrNull())
      scaleType = ImageView.ScaleType.FIT_CENTER
      contentDescription = null
      layoutParams = FrameLayout.LayoutParams(dp(42), dp(42), Gravity.CENTER)
    })
    val mark = TextView(activity).apply {
      gravity = Gravity.CENTER
      textSize = 10f
      typeface = Typeface.create(Typeface.DEFAULT, Typeface.BOLD)
      includeFontPadding = false
      layoutParams = FrameLayout.LayoutParams(dp(19), dp(19), Gravity.TOP or Gravity.END)
    }
    iconBox.addView(mark)
    tile.addView(iconBox)

    tile.addView(TextView(activity).apply {
      text = info.loadLabel(activity.packageManager).toString()
      setTextColor(if (isRemembered) palette.accent else palette.foreground)
      textSize = 11.5f
      typeface = Typeface.create(Typeface.DEFAULT, Typeface.BOLD)
      gravity = Gravity.CENTER
      maxLines = 2
      ellipsize = TextUtils.TruncateAt.END
      includeFontPadding = false
      layoutParams = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        0,
        1f,
      ).apply { topMargin = dp(4) }
    })

    rows.add(tile)
    marks.add(mark)
    return tile
  }

  private fun rememberRow(): View {
    val row = LinearLayout(activity).apply {
      orientation = LinearLayout.HORIZONTAL
      gravity = Gravity.CENTER_VERTICAL
      setPadding(dp(12), dp(10), dp(12), dp(10))
      background = rounded(palette.surface, 10f, 1, palette.border)
      isClickable = true
      isFocusable = true
      layoutParams = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        ViewGroup.LayoutParams.WRAP_CONTENT,
      ).apply { topMargin = dp(8) }
    }
    val mark = TextView(activity).apply {
      gravity = Gravity.CENTER
      textSize = 13f
      typeface = Typeface.create(Typeface.DEFAULT, Typeface.BOLD)
      includeFontPadding = false
      layoutParams = LinearLayout.LayoutParams(dp(22), dp(22)).apply { marginEnd = dp(11) }
    }
    val copy = LinearLayout(activity).apply {
      orientation = LinearLayout.VERTICAL
      layoutParams = LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f)
    }
    copy.addView(TextView(activity).apply {
      text = if (extension == NO_EXTENSION) {
        activity.getString(R.string.file_chooser_remember_no_ext)
      } else {
        activity.getString(R.string.file_chooser_remember, extension)
      }
      setTextColor(palette.foreground)
      textSize = 13.5f
      includeFontPadding = false
    })
    copy.addView(TextView(activity).apply {
      text = activity.getString(R.string.file_chooser_manage_hint)
      setTextColor(palette.secondary)
      textSize = 11.5f
      includeFontPadding = false
      layoutParams = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        ViewGroup.LayoutParams.WRAP_CONTENT,
      ).apply { topMargin = dp(3) }
    })
    fun refresh() {
      mark.text = if (rememberChecked) "✓" else ""
      mark.setTextColor(Color.WHITE)
      mark.background = if (rememberChecked) {
        rounded(palette.accentSolid, 6f, 0, Color.TRANSPARENT)
      } else {
        rounded(Color.TRANSPARENT, 6f, 2, palette.secondary)
      }
    }
    row.setOnClickListener {
      rememberChecked = !rememberChecked
      refresh()
    }
    refresh()
    row.addView(mark)
    row.addView(copy)
    return row
  }

  private fun refreshSelection() {
    rows.forEachIndexed { index, row ->
      val selectedRow = index == selected
      val content = rounded(
        if (selectedRow) palette.accentSoft else palette.surface,
        11f,
        if (selectedRow) 2 else 1,
        if (selectedRow) palette.accent else palette.border,
      )
      row.background = RippleDrawable(
        android.content.res.ColorStateList.valueOf(palette.ripple),
        content,
        null,
      )
      marks.getOrNull(index)?.apply {
        text = if (selectedRow) "✓" else ""
        setTextColor(Color.WHITE)
        background = if (selectedRow) {
          rounded(palette.accentSolid, 999f, 0, Color.TRANSPARENT)
        } else {
          rounded(Color.TRANSPARENT, 999f, 2, palette.border)
        }
      }
    }
    updateActionButton(openButton, selected >= 0)
  }

  private fun actionButton(label: String, primary: Boolean, enabled: Boolean): TextView =
    TextView(activity).apply {
      text = label
      gravity = Gravity.CENTER
      textSize = 14f
      typeface = Typeface.create(Typeface.DEFAULT, Typeface.BOLD)
      isAllCaps = false
      isClickable = true
      isFocusable = true
      setTextColor(if (primary) Color.WHITE else palette.foreground)
      layoutParams = LinearLayout.LayoutParams(dp(if (primary) 108 else 88), dp(46))
      updateActionButton(this, enabled, primary)
    }

  private fun updateActionButton(view: TextView?, enabled: Boolean, primary: Boolean = true) {
    view ?: return
    view.isEnabled = enabled
    val fill = when {
      primary && enabled -> palette.accentSolid
      primary -> palette.disabled
      else -> Color.TRANSPARENT
    }
    val stroke = if (primary) 0 else 1
    view.background = RippleDrawable(
      android.content.res.ColorStateList.valueOf(palette.ripple),
      rounded(fill, 10f, stroke, palette.border),
      null,
    )
    view.alpha = if (enabled) 1f else 0.62f
  }

  private fun finishCancel() {
    if (completed) return
    completed = true
    onCancel()
  }

  private fun extensionLabel(): String =
    if (extension == NO_EXTENSION) activity.getString(R.string.file_chooser_no_extension)
    else ".${extension.uppercase(Locale.ROOT)}"

  private fun resolveComponent(info: ResolveInfo): ComponentName =
    ComponentName(info.activityInfo.packageName, info.activityInfo.name)

  private fun palette(): Palette {
    val systemDark = activity.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK ==
      Configuration.UI_MODE_NIGHT_YES
    val dark = appearance == "dark" || (appearance != "light" && systemDark)
    return if (dark) {
      Palette(
        background = Color.rgb(22, 27, 34),
        surface = Color.rgb(28, 33, 40),
        border = Color.rgb(48, 54, 61),
        foreground = Color.rgb(230, 237, 243),
        secondary = Color.rgb(139, 148, 158),
        accent = Color.rgb(68, 147, 248),
        accentSolid = Color.rgb(31, 111, 235),
        accentSoft = Color.rgb(22, 58, 100),
        disabled = Color.rgb(67, 74, 83),
        ripple = Color.argb(42, 68, 147, 248),
      )
    } else {
      Palette(
        background = Color.WHITE,
        surface = Color.rgb(247, 248, 250),
        border = Color.rgb(229, 231, 235),
        foreground = Color.rgb(31, 35, 40),
        secondary = Color.rgb(101, 109, 118),
        accent = Color.rgb(37, 99, 235),
        accentSolid = Color.rgb(37, 99, 235),
        accentSoft = Color.rgb(239, 246, 255),
        disabled = Color.rgb(156, 163, 175),
        ripple = Color.argb(32, 37, 99, 235),
      )
    }
  }

  private fun rounded(color: Int, radiusDp: Float, strokeDp: Int, strokeColor: Int): GradientDrawable =
    GradientDrawable().apply {
      shape = GradientDrawable.RECTANGLE
      cornerRadius = dp(radiusDp).toFloat()
      setColor(color)
      if (strokeDp > 0) setStroke(dp(strokeDp), strokeColor)
    }

  private fun dp(value: Int): Int = (value * density + 0.5f).toInt()
  private fun dp(value: Float): Int = (value * density + 0.5f).toInt()

  companion object {
    private const val NO_EXTENSION = "_no_extension"
  }
}
