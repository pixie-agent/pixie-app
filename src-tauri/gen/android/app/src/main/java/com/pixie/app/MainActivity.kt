package com.pixie.app

import android.os.Bundle
import android.view.View
import android.view.ViewTreeObserver
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsAnimationCompat
import androidx.core.view.ViewCompat

class MainActivity : TauriActivity() {
  private var webView: WebView? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)

    // When enableEdgeToEdge() is active (especially on Android 15+ / API 35),
    // the system forces adjustNothing behaviour, meaning the soft keyboard
    // overlays the WebView without resizing it. The visualViewport API in
    // WebView is unreliable, so we detect keyboard visibility at the native
    // level and inject the keyboard height as bottom padding on the WebView
    // so the HTML content scrolls up above the keyboard.
    val rootView = window.decorView.findViewById<View>(android.R.id.content)
    rootView.viewTreeObserver.addOnGlobalLayoutListener(object : ViewTreeObserver.OnGlobalLayoutListener {
      override fun onGlobalLayout() {
        val insets = ViewCompat.getRootWindowInsets(rootView)
        if (insets != null) {
          val imeVisible = insets.isVisible(WindowInsetsCompat.Type.ime())
          val imeHeight = if (imeVisible) insets.getInsets(WindowInsetsCompat.Type.ime()).bottom else 0
          val navHeight = insets.getInsets(WindowInsetsCompat.Type.systemBars()).bottom
          // When IME is visible, we need to pad the WebView by the IME height
          // minus the navigation bar height (since edge-to-edge already accounts
          // for the nav bar). When IME is hidden, clear the extra padding.
          val extraBottom = if (imeVisible) imeHeight - navHeight else 0
          webView?.setPadding(0, 0, 0, Math.max(0, extraBottom))
        }
      }
    })
  }

  override fun onWebViewCreate(webView: WebView) {
    this.webView = webView
  }
}
