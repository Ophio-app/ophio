// SPDX-License-Identifier: Apache-2.0
package app.ophio.device

import android.app.Activity
import android.app.Dialog
import android.content.res.ColorStateList
import android.content.res.Configuration
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.ColorDrawable
import android.graphics.drawable.GradientDrawable
import android.graphics.drawable.StateListDrawable
import android.net.Uri
import android.text.Editable
import android.text.InputType
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.TextUtils
import android.text.TextWatcher
import android.text.style.ForegroundColorSpan
import android.text.style.RelativeSizeSpan
import android.util.TypedValue
import android.view.Gravity
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import android.view.ViewGroup
import android.view.Window
import android.view.WindowManager
import android.view.accessibility.AccessibilityNodeInfo
import android.view.inputmethod.InputMethodManager
import android.webkit.CookieManager
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.FrameLayout
import android.widget.ImageButton
import android.widget.ImageView
import android.widget.LinearLayout
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.ScrollView
import android.widget.TextView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import app.tauri.plugin.Invoke
import java.io.ByteArrayInputStream
import java.io.File
import java.util.UUID
import org.json.JSONArray
import org.json.JSONObject

/** This WebView never receives Tauri scripts, JavaScript interfaces or device credentials. */
internal class PreviewBrowser(private val screen: () -> Activity) {
    /** One question's answer in progress: chosen labels, then the person's own words. */
    private class Answer(val labels: MutableList<String>, var own: String)

    private var dialog: Dialog? = null
    private var active: String? = null
    private val root = File(screen().filesDir, "preview-pictures").apply { mkdirs() }

    fun show(invoke: Invoke) {
        val activity = screen()
        activity.runOnUiThread {
            if (dialog != null) {
                invoke.reject("A preview is already open.")
                return@runOnUiThread
            }
            try {
                val args = invoke.getArgs()
                val url = Uri.parse(args.getString("url"))
                require(
                    url.scheme == "http" &&
                        url.host == "127.0.0.1" &&
                        url.port > 0 &&
                        url.userInfo == null
                )
                val origin = "http://127.0.0.1:${url.port}"
                active = args.getString("id")
                val pictures = mutableListOf<Map<String, Any>>()
                // The picture the person chose to annotate; the app opens it on close.
                var annotate: String? = null
                val density = activity.resources.displayMetrics.density
                fun px(dp: Int) = (dp * density).toInt()
                val dark =
                    (activity.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK) ==
                        Configuration.UI_MODE_NIGHT_YES
                // The app's own surface, ink, line and accent text, as its tokens set them.
                val surface = if (dark) 0xFF120C0A.toInt() else 0xFFFAF7F0.toInt()
                val ink = if (dark) 0xFFF1EEE6.toInt() else 0xFF211526.toInt()
                val line = if (dark) 0xFF49374F.toInt() else 0xFFDCD1DF.toInt()
                val accent = if (dark) 0xFFC3FF24.toInt() else 0xFF3B4D00.toInt()
                fun saved() =
                    when (pictures.size) {
                        0 -> "Private preview"
                        1 -> "Picture saved"
                        else -> "${pictures.size} pictures saved"
                    }
                val view = WebView(activity)
                view.settings.javaScriptEnabled = true
                view.settings.domStorageEnabled = true
                view.settings.allowFileAccess = false
                view.settings.allowContentAccess = false
                view.settings.setSupportMultipleWindows(false)
                view.settings.javaScriptCanOpenWindowsAutomatically = false
                view.settings.mixedContentMode =
                    android.webkit.WebSettings.MIXED_CONTENT_NEVER_ALLOW
                // Pages lay out as the phone's browser lays them out: at the width
                // their viewport asks for, fitted to the screen, and pinch to zoom.
                view.settings.useWideViewPort = true
                view.settings.loadWithOverviewMode = true
                view.settings.setSupportZoom(true)
                view.settings.builtInZoomControls = true
                view.settings.displayZoomControls = false
                CookieManager.getInstance().setAcceptThirdPartyCookies(view, false)
                val title =
                    TextView(activity).apply {
                        text = "Opening preview…"
                        textSize = 16f
                        setTextColor(ink)
                        maxLines = 1
                        ellipsize = TextUtils.TruncateAt.END
                        accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE
                    }
                // Under the bar only while something has gone wrong.
                val notice =
                    TextView(activity).apply {
                        textSize = 14f
                        setTextColor(ink)
                        setBackgroundColor(line)
                        setPadding(px(16), px(10), px(16), px(10))
                        visibility = View.GONE
                        accessibilityLiveRegion = View.ACCESSIBILITY_LIVE_REGION_POLITE
                    }
                fun say(message: String?) {
                    notice.text = message
                    notice.visibility = if (message == null) View.GONE else View.VISIBLE
                }
                val leaves = "This link leaves the private preview."
                fun allowed(uri: Uri): Boolean =
                    uri.scheme == "http" &&
                        uri.host == "127.0.0.1" &&
                        uri.port == url.port &&
                        uri.userInfo == null
                view.webViewClient =
                    object : WebViewClient() {
                        override fun shouldOverrideUrlLoading(
                            view: WebView,
                            request: WebResourceRequest,
                        ): Boolean {
                            if (allowed(request.url)) return false
                            say(leaves)
                            notice.postDelayed({ if (notice.text == leaves) say(null) }, 4_000)
                            return true
                        }

                        override fun shouldInterceptRequest(
                            view: WebView,
                            request: WebResourceRequest,
                        ): WebResourceResponse? {
                            if (allowed(request.url)) return null
                            return WebResourceResponse(
                                "text/plain",
                                "UTF-8",
                                403,
                                "Forbidden",
                                emptyMap(),
                                ByteArrayInputStream(ByteArray(0)),
                            )
                        }

                        override fun onPageStarted(view: WebView, url: String, favicon: Bitmap?) {
                            title.text = "Opening preview…"
                            say(null)
                        }

                        // A page that failed still finishes, so its notice is kept.
                        override fun onPageFinished(view: WebView, url: String) {
                            title.text = saved()
                        }

                        override fun onReceivedError(
                            view: WebView,
                            request: WebResourceRequest,
                            error: android.webkit.WebResourceError,
                        ) {
                            if (request.isForMainFrame)
                                say("The preview connection was lost. Close and open it again.")
                        }

                        override fun onReceivedHttpError(
                            view: WebView,
                            request: WebResourceRequest,
                            response: WebResourceResponse,
                        ) {
                            if (request.isForMainFrame && response.statusCode >= 400)
                                say("The computer could not open this preview.")
                        }
                    }
                fun icon(drawable: Int, label: String) =
                    ImageButton(activity).apply {
                        setImageResource(drawable)
                        imageTintList = ColorStateList.valueOf(ink)
                        contentDescription = label
                        tooltipText = label
                        val ripple = TypedValue()
                        activity.theme.resolveAttribute(
                            android.R.attr.selectableItemBackgroundBorderless,
                            ripple,
                            true,
                        )
                        setBackgroundResource(ripple.resourceId)
                    }
                val close = icon(R.drawable.preview_close, "Close preview")
                close.setOnClickListener { dialog?.dismiss() }
                val annotation =
                    Button(activity, null, android.R.attr.borderlessButtonStyle).apply {
                        text = "Annotate"
                        isAllCaps = false
                        textSize = 16f
                        setTextColor(accent)
                        minHeight = px(48)
                        visibility = View.GONE
                        setOnClickListener {
                            annotate = pictures.lastOrNull()?.get("id") as String?
                            dialog?.dismiss()
                        }
                    }
                val picture =
                    icon(R.drawable.preview_camera, "Take picture").apply {
                        setOnClickListener {
                            try {
                                require(
                                    view.width > 0 &&
                                        view.height > 0 &&
                                        view.width.toLong() * view.height <= 16L * 1024 * 1024
                                )
                                require(
                                    root.listFiles().orEmpty().sumOf { it.length() } <
                                        32L * 1024 * 1024
                                )
                                val bitmap =
                                    Bitmap.createBitmap(
                                        view.width,
                                        view.height,
                                        Bitmap.Config.ARGB_8888,
                                    )
                                val id = UUID.randomUUID().toString()
                                val file = File(root, "$id.png")
                                val capturedAt = System.currentTimeMillis()
                                val metadata =
                                    JSONObject()
                                        .put("id", id)
                                        .put("profileId", args.getString("profileId"))
                                        .put("previewId", args.getString("previewId"))
                                        .put("capturedAt", capturedAt)
                                        .put(
                                            "taskId",
                                            args.optString("taskId").ifEmpty { JSONObject.NULL },
                                        )
                                try {
                                    File(root, "$id.json").outputStream().use {
                                        it.write(metadata.toString().toByteArray())
                                        it.fd.sync()
                                    }
                                    view.draw(Canvas(bitmap))
                                    val temporary = File(root, "$id.part")
                                    temporary.outputStream().use { output ->
                                        check(
                                            bitmap.compress(Bitmap.CompressFormat.PNG, 100, output)
                                        )
                                        output.fd.sync()
                                    }
                                    check(temporary.renameTo(file))
                                    pictures.add(
                                        mapOf(
                                            "id" to id,
                                            "size" to file.length(),
                                            "width" to view.width,
                                            "height" to view.height,
                                            "capturedAt" to capturedAt,
                                        ),
                                    )
                                } finally {
                                    bitmap.recycle()
                                    File(root, "$id.part").delete()
                                    if (!file.exists()) File(root, "$id.json").delete()
                                }
                                title.text = saved()
                                say(null)
                                annotation.visibility = View.VISIBLE
                            } catch (_: Exception) {
                                say("The picture could not be saved. Free some space and try again.")
                            }
                        }
                    }
                // What the agent asked about the page, answered under it as the person
                // tries it. The answers go back to the app as the preview closes.
                val asked = args.optJSONArray("questions") ?: JSONArray()
                val given = args.optJSONArray("answers") ?: JSONArray()
                val answers =
                    List(minOf(asked.length(), 6)) { i ->
                        val question = asked.getJSONObject(i)
                        val offered = question.optJSONArray("options") ?: JSONArray()
                        val labels =
                            List(offered.length()) { offered.getJSONObject(it).getString("label") }
                        val saved = given.optJSONObject(i)
                        val chosen = saved?.optJSONArray("labels") ?: JSONArray()
                        Answer(
                            List(chosen.length()) { chosen.optString(it) }
                                .filter { it in labels }
                                .toMutableList(),
                            saved?.optString("own").orEmpty().take(4000),
                        )
                    }
                var submitted = false
                val muted = (ink and 0x00FFFFFF) or 0xB3000000.toInt()
                val submit =
                    Button(activity).apply {
                        text = "Submit answers"
                        isAllCaps = false
                        textSize = 16f
                        minHeight = px(48)
                        setTextColor(surface)
                        backgroundTintList = ColorStateList.valueOf(accent)
                        setOnClickListener {
                            submitted = true
                            dialog?.dismiss()
                        }
                    }
                fun answered() =
                    answers.any { it.labels.isNotEmpty() || it.own.isNotBlank() }
                fun refresh() {
                    submit.isEnabled = answered()
                    submit.alpha = if (submit.isEnabled) 1f else 0.5f
                }
                val questions =
                    LinearLayout(activity).apply {
                        orientation = LinearLayout.VERTICAL
                        setPadding(px(16), 0, px(16), px(8))
                    }
                answers.forEachIndexed { i, answer ->
                    val question = asked.getJSONObject(i)
                    val multi = question.optBoolean("multi_select", false)
                    val offered = question.optJSONArray("options") ?: JSONArray()
                    question.optString("header").takeIf { it.isNotBlank() }?.let {
                        questions.addView(
                            TextView(activity).apply {
                                text = it
                                textSize = 13f
                                setTextColor(muted)
                                setPadding(0, px(12), 0, 0)
                            }
                        )
                    }
                    questions.addView(
                        TextView(activity).apply {
                            text = question.getString("question")
                            textSize = 16f
                            setTextColor(ink)
                            setPadding(0, px(if (question.has("header")) 2 else 12), 0, px(4))
                        }
                    )
                    if (multi && offered.length() > 0)
                        questions.addView(
                            TextView(activity).apply {
                                text = "Choose one or more"
                                textSize = 13f
                                setTextColor(muted)
                            }
                        )
                    // The person's own words, as the app's own question cards take them.
                    val own =
                        EditText(activity).apply {
                            hint =
                                when {
                                    offered.length() == 0 -> "Your answer"
                                    multi -> "Add your own answer"
                                    else -> "Or answer in your own words"
                                }
                            setText(answer.own)
                            textSize = 16f
                            setTextColor(ink)
                            setHintTextColor(muted)
                            backgroundTintList = ColorStateList.valueOf(accent)
                            inputType =
                                InputType.TYPE_CLASS_TEXT or
                                    InputType.TYPE_TEXT_FLAG_MULTI_LINE or
                                    InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
                            maxLines = 4
                            minHeight = px(48)
                        }
                    fun option(label: String, description: String): SpannableStringBuilder =
                        SpannableStringBuilder(label).apply {
                            if (description.isNotBlank()) {
                                val start = length
                                append("\n").append(description)
                                setSpan(RelativeSizeSpan(0.85f), start, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
                                setSpan(ForegroundColorSpan(muted), start, length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
                            }
                        }
                    if (multi) {
                        for (k in 0 until offered.length()) {
                            val o = offered.getJSONObject(k)
                            val label = o.getString("label")
                            questions.addView(
                                CheckBox(activity).apply {
                                    text = option(label, o.optString("description"))
                                    setTextColor(ink)
                                    textSize = 16f
                                    minHeight = px(48)
                                    buttonTintList = ColorStateList.valueOf(accent)
                                    isChecked = label in answer.labels
                                    setOnCheckedChangeListener { _, on ->
                                        answer.labels.remove(label)
                                        if (on) answer.labels.add(label)
                                        refresh()
                                    }
                                }
                            )
                        }
                    } else if (offered.length() > 0) {
                        val group = RadioGroup(activity)
                        for (k in 0 until offered.length()) {
                            val o = offered.getJSONObject(k)
                            val label = o.getString("label")
                            group.addView(
                                RadioButton(activity).apply {
                                    id = View.generateViewId()
                                    tag = label
                                    text = option(label, o.optString("description"))
                                    setTextColor(ink)
                                    textSize = 16f
                                    minHeight = px(48)
                                    buttonTintList = ColorStateList.valueOf(accent)
                                    isChecked = label in answer.labels
                                }
                            )
                        }
                        // One choice only: an option, or the person's own words instead.
                        group.setOnCheckedChangeListener { g, checked ->
                            if (checked == View.NO_ID) return@setOnCheckedChangeListener
                            answer.labels.clear()
                            answer.labels.add(g.findViewById<View>(checked).tag as String)
                            if (own.text.isNotEmpty()) own.setText("")
                            refresh()
                        }
                        questions.addView(group)
                        own.tag = group
                    }
                    own.addTextChangedListener(
                        object : TextWatcher {
                            override fun beforeTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}

                            override fun onTextChanged(s: CharSequence?, a: Int, b: Int, c: Int) {}

                            override fun afterTextChanged(text: Editable?) {
                                answer.own = text?.toString().orEmpty().take(4000)
                                if (!multi && answer.own.isNotBlank() && answer.labels.isNotEmpty()) {
                                    answer.labels.clear()
                                    (own.tag as? RadioGroup)?.clearCheck()
                                }
                                refresh()
                            }
                        }
                    )
                    questions.addView(own)
                }
                refresh()
                val maxAnswers = (activity.resources.displayMetrics.heightPixels * 0.4).toInt()
                val answering =
                    object : ScrollView(activity) {
                        override fun onMeasure(width: Int, height: Int) {
                            super.onMeasure(
                                width,
                                View.MeasureSpec.makeMeasureSpec(maxAnswers, View.MeasureSpec.AT_MOST),
                            )
                        }
                    }
                answering.addView(questions)
                val asks =
                    TextView(activity).apply {
                        text = if (answers.size == 1) "The agent asks" else "The agent asks ${answers.size} questions"
                        textSize = 15f
                        setTextColor(ink)
                        typeface = Typeface.DEFAULT_BOLD
                        maxLines = 2
                        ellipsize = TextUtils.TruncateAt.END
                    }
                val minimize = icon(R.drawable.preview_minimize, "Minimize questions")
                val asking =
                    LinearLayout(activity).apply {
                        orientation = LinearLayout.VERTICAL
                        visibility = if (answers.isEmpty()) View.GONE else View.VISIBLE
                    }
                asking.addView(
                    View(activity).apply { setBackgroundColor(line) },
                    LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 1),
                )
                val heading =
                    LinearLayout(activity).apply {
                        orientation = LinearLayout.HORIZONTAL
                        gravity = Gravity.CENTER_VERTICAL
                        setPadding(px(4), 0, px(12), 0)
                    }
                heading.addView(minimize, LinearLayout.LayoutParams(px(48), px(48)))
                heading.addView(
                    asks,
                    LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                        marginStart = px(4)
                        marginEnd = px(8)
                    },
                )
                heading.addView(submit)
                asking.addView(heading)
                asking.addView(answering)
                // Minimized, the questions wait in a small bubble over the page, which
                // the person can drag out of the way; a tap brings them back.
                val badge =
                    TextView(activity).apply {
                        textSize = 12f
                        typeface = Typeface.DEFAULT_BOLD
                        gravity = Gravity.CENTER
                        setTextColor(surface)
                        minWidth = px(20)
                        setPadding(px(5), 0, px(5), 0)
                        background =
                            GradientDrawable().apply {
                                cornerRadius = px(10).toFloat()
                                setColor(ink)
                                setStroke(px(2), surface)
                            }
                        importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
                        elevation = px(7).toFloat()
                    }
                val bubble =
                    FrameLayout(activity).apply {
                        visibility = View.GONE
                        isClickable = true
                        isFocusable = true
                        // The square default highlight would frame the round bubble; a
                        // ring on the circle shows keyboard focus instead.
                        defaultFocusHighlightEnabled = false
                        // Its shadows fall past its frame; clipped, they drew a box.
                        clipChildren = false
                        addView(
                            ImageView(activity).apply {
                                setImageResource(R.drawable.preview_questions)
                                imageTintList = ColorStateList.valueOf(surface)
                                scaleType = ImageView.ScaleType.CENTER_INSIDE
                                setPadding(px(14), px(14), px(14), px(14))
                                isDuplicateParentStateEnabled = true
                                fun disc(ring: Boolean) =
                                    GradientDrawable().apply {
                                        shape = GradientDrawable.OVAL
                                        setColor(accent)
                                        if (ring) setStroke(px(3), ink)
                                    }
                                background =
                                    StateListDrawable().apply {
                                        addState(intArrayOf(android.R.attr.state_focused), disc(true))
                                        addState(intArrayOf(), disc(false))
                                    }
                                elevation = px(6).toFloat()
                                importantForAccessibility = View.IMPORTANT_FOR_ACCESSIBILITY_NO
                            },
                            FrameLayout.LayoutParams(px(52), px(52), Gravity.BOTTOM or Gravity.START),
                        )
                        addView(
                            badge,
                            FrameLayout.LayoutParams(
                                ViewGroup.LayoutParams.WRAP_CONTENT,
                                px(20),
                                Gravity.TOP or Gravity.END,
                            ),
                        )
                    }
                fun counted() {
                    val open = answers.count { it.labels.isEmpty() && it.own.isBlank() }
                    badge.text = if (open == 0) "✓" else open.toString()
                    bubble.contentDescription =
                        (if (answers.size == 1) "Show the agent’s question" else "Show the agent’s ${answers.size} questions") +
                            when (open) {
                                0 -> ", all answered"
                                answers.size -> ""
                                else -> ", $open not answered"
                            }
                }
                fun fold(away: Boolean) {
                    if (away) {
                        // A field being typed in gives the keyboard back as it folds.
                        val keys = activity.getSystemService(InputMethodManager::class.java)
                        keys?.hideSoftInputFromWindow(asking.windowToken, 0)
                        asking.findFocus()?.clearFocus()
                        counted()
                    }
                    asking.visibility = if (away) View.GONE else View.VISIBLE
                    bubble.visibility = if (away) View.VISIBLE else View.GONE
                    (if (away) bubble else minimize).post {
                        (if (away) bubble else minimize)
                            .performAccessibilityAction(AccessibilityNodeInfo.ACTION_ACCESSIBILITY_FOCUS, null)
                    }
                }
                minimize.setOnClickListener { fold(true) }
                bubble.setOnClickListener { fold(false) }
                // Dragged, the bubble follows the finger inside the page and then
                // settles against the nearer side; a tap without a drag opens it.
                val slop = ViewConfiguration.get(activity).scaledTouchSlop
                val inset = px(12)
                fun keep(settle: Boolean) {
                    val stage = bubble.parent as? View ?: return
                    val left = -(stage.width - bubble.width - 2 * inset).coerceAtLeast(0).toFloat()
                    val top = -(stage.height - bubble.height - 2 * inset).coerceAtLeast(0).toFloat()
                    val x = bubble.translationX.coerceIn(left, 0f)
                    val y = bubble.translationY.coerceIn(top, 0f)
                    if (settle) bubble.animate().translationX(if (x < left / 2) left else 0f).translationY(y).setDuration(160).start()
                    else {
                        bubble.translationX = x
                        bubble.translationY = y
                    }
                }
                var downX = 0f
                var downY = 0f
                var fromX = 0f
                var fromY = 0f
                var dragging = false
                bubble.setOnTouchListener { v, e ->
                    when (e.actionMasked) {
                        MotionEvent.ACTION_DOWN -> {
                            downX = e.rawX
                            downY = e.rawY
                            fromX = v.translationX
                            fromY = v.translationY
                            dragging = false
                            v.isPressed = true
                        }
                        MotionEvent.ACTION_MOVE -> {
                            val dx = e.rawX - downX
                            val dy = e.rawY - downY
                            if (!dragging && dx * dx + dy * dy > slop * slop) {
                                dragging = true
                                v.isPressed = false
                            }
                            if (dragging) {
                                v.translationX = fromX + dx
                                v.translationY = fromY + dy
                                keep(false)
                            }
                        }
                        MotionEvent.ACTION_UP -> {
                            v.isPressed = false
                            if (dragging) keep(true) else v.performClick()
                        }
                        MotionEvent.ACTION_CANCEL -> {
                            v.isPressed = false
                            if (dragging) keep(true)
                        }
                    }
                    true
                }
                // One slim bar over the page: close, what this is, and its actions.
                val bar =
                    LinearLayout(activity).apply {
                        orientation = LinearLayout.HORIZONTAL
                        gravity = Gravity.CENTER_VERTICAL
                        minimumHeight = px(56)
                        setPadding(px(4), 0, px(4), 0)
                    }
                bar.addView(close, LinearLayout.LayoutParams(px(48), px(48)))
                bar.addView(
                    title,
                    LinearLayout.LayoutParams(0, ViewGroup.LayoutParams.WRAP_CONTENT, 1f).apply {
                        marginStart = px(8)
                        marginEnd = px(4)
                    },
                )
                bar.addView(annotation)
                bar.addView(picture, LinearLayout.LayoutParams(px(48), px(48)))
                val content =
                    LinearLayout(activity).apply {
                        orientation = LinearLayout.VERTICAL
                        setBackgroundColor(surface)
                    }
                content.addView(bar)
                content.addView(
                    View(activity).apply { setBackgroundColor(line) },
                    LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 1),
                )
                content.addView(notice)
                // The page, with the minimized questions' bubble over its lower corner.
                val stage = FrameLayout(activity).apply { clipChildren = false }
                stage.addView(view)
                stage.addView(
                    bubble,
                    FrameLayout.LayoutParams(px(60), px(60), Gravity.BOTTOM or Gravity.END).apply {
                        setMargins(inset, inset, inset, inset)
                    },
                )
                // A turn or the keyboard changes the page's size; the bubble stays on it.
                stage.addOnLayoutChangeListener { _, _, _, _, _, _, _, _, _ -> keep(false) }
                content.addView(
                    stage,
                    LinearLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, 0, 1f),
                )
                content.addView(asking)
                // The whole screen, edge to edge, kept clear of the system bars,
                // the camera cutout and the keyboard.
                ViewCompat.setOnApplyWindowInsetsListener(content) { v, insets ->
                    val clear =
                        insets.getInsets(
                            WindowInsetsCompat.Type.systemBars() or
                                WindowInsetsCompat.Type.displayCutout() or
                                WindowInsetsCompat.Type.ime()
                        )
                    v.setPadding(clear.left, clear.top, clear.right, clear.bottom)
                    WindowInsetsCompat.CONSUMED
                }
                val opened =
                    Dialog(
                        activity,
                        if (dark) android.R.style.Theme_DeviceDefault_NoActionBar
                        else android.R.style.Theme_DeviceDefault_Light_NoActionBar,
                    ).apply {
                        setContentView(content)
                        window?.let { frame(it, content, dark, surface) }
                        setOnDismissListener {
                            view.stopLoading()
                            view.loadUrl("about:blank")
                            view.clearHistory()
                            view.destroy()
                            // Remove only this preview's gate cookie. Other previews and the main
                            // app remain independent.
                            CookieManager.getInstance().getCookie(origin)?.split(';')?.forEach {
                                cookie ->
                                val name = cookie.trim().substringBefore('=')
                                if (name.startsWith("preview_${url.port}_"))
                                    CookieManager.getInstance()
                                        .setCookie(
                                            origin,
                                            "$name=; Max-Age=0; Path=/; HttpOnly; SameSite=Strict",
                                            null,
                                        )
                            }
                            CookieManager.getInstance().flush()
                            dialog = null
                            active = null
                            invoke.resolveObject(
                                mapOf(
                                    "pictures" to pictures,
                                    "annotate" to annotate,
                                    "answers" to
                                        answers.map {
                                            mapOf("labels" to it.labels.toList(), "own" to it.own)
                                        },
                                    "submitted" to submitted,
                                )
                            )
                        }
                    }
                dialog = opened
                opened.show()
                opened.window?.setLayout(
                    ViewGroup.LayoutParams.MATCH_PARENT,
                    ViewGroup.LayoutParams.MATCH_PARENT,
                )
                view.loadUrl(url.toString())
            } catch (_: Exception) {
                active = null
                invoke.reject("The private preview could not open.")
            }
        }
    }

    /** The preview's window fills the screen and draws under clear system bars. */
    @Suppress("DEPRECATION")
    private fun frame(window: Window, content: View, dark: Boolean, surface: Int) {
        WindowCompat.setDecorFitsSystemWindows(window, false)
        window.setBackgroundDrawable(ColorDrawable(surface))
        window.statusBarColor = Color.TRANSPARENT
        window.navigationBarColor = Color.TRANSPARENT
        window.isNavigationBarContrastEnforced = false
        // The keyboard's height arrives as an inset, which the content keeps clear of.
        window.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE)
        window.attributes =
            window.attributes.apply {
                layoutInDisplayCutoutMode =
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
            }
        WindowCompat.getInsetsController(window, content).apply {
            isAppearanceLightStatusBars = !dark
            isAppearanceLightNavigationBars = !dark
        }
    }

    fun close(id: String? = null) {
        screen().runOnUiThread { if (id == null || id == active) dialog?.dismiss() }
    }

    @Synchronized
    fun read(id: String, offset: Long): ByteArray {
        require(id.matches(Regex("[0-9a-f-]{36}")) && offset >= 0)
        java.io.RandomAccessFile(File(root, "$id.png"), "r").use { file ->
            require(offset <= file.length())
            file.seek(offset)
            val bytes = ByteArray(minOf(256 * 1024L, file.length() - offset).toInt())
            file.readFully(bytes)
            return bytes
        }
    }

    @Synchronized
    fun pending(profile: String, preview: String): List<Map<String, Any?>> =
        root
            .listFiles()
            .orEmpty()
            .filter { it.extension == "json" }
            .mapNotNull { file ->
                try {
                    val metadata = JSONObject(file.readText())
                    val id = metadata.getString("id")
                    require(id.matches(Regex("[0-9a-f-]{36}")))
                    val image = File(root, "$id.png")
                    if (
                        metadata.getString("profileId") != profile ||
                            metadata.getString("previewId") != preview ||
                            !image.isFile
                    )
                        null
                    else
                        mapOf(
                            "id" to id,
                            "size" to image.length(),
                            "capturedAt" to
                                if (metadata.has("capturedAt")) metadata.getLong("capturedAt")
                                else null,
                            "taskId" to
                                if (metadata.isNull("taskId")) null
                                else metadata.getString("taskId"),
                        )
                } catch (_: Exception) {
                    null
                }
            }

    @Synchronized
    fun acknowledge(id: String) {
        require(id.matches(Regex("[0-9a-f-]{36}")))
        check(!File(root, "$id.png").exists() || File(root, "$id.png").delete())
        File(root, "$id.json").delete()
        File(root, "$id.part").delete()
    }
}
