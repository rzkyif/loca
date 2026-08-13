package com.rzkyif.loca

import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.webkit.WebView
import androidx.activity.enableEdgeToEdge
import androidx.core.splashscreen.SplashScreen.Companion.installSplashScreen
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    // Show the launch splash (Theme.loca.Starting) and swap to Theme.loca.
    // Must run before super.onCreate(); the compat library backports the
    // Android 12 splash to our minSdk so all devices get the same theme-aware
    // dark/light surface + logo instead of the plain white window background.
    installSplashScreen()
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
  }

  // Edge-to-edge keeps the activity window full-screen even while the soft
  // keyboard is up, so the WebView never resizes for the IME. On this platform
  // that means the two signals the web layer would normally use are both dead:
  // CSS env(safe-area-inset-*) reads 0 on Android WebView, and JS
  // window.visualViewport never reports the keyboard (tauri-apps/tauri#10631).
  // We therefore read the insets natively and publish them to the page as CSS
  // variables, which the mobile shell consumes for ALL inset/keyboard layout:
  //   --safe-area-inset-{top,right,bottom,left}  static device chrome
  //       (status / gesture bars + display cutout), and
  //   --keyboard-inset  the soft-keyboard height in CSS px (0 when hidden).
  // The frame pads its bottom by max(safe-bottom, keyboard) so the composer
  // rides above whichever is taller; fixed sheets lift by --keyboard-inset.
  override fun onWebViewCreate(webView: WebView) {
    ViewCompat.setOnApplyWindowInsetsListener(webView) { _, insets ->
      publishInsets(webView, insets)
      // Return the insets unconsumed (the documented pattern); we supply the page
      // its inset values via the CSS variables rather than via env().
      insets
    }
    // Kick a first inset pass now; keyboard show/hide re-fires the listener.
    webView.requestApplyInsets()

    // The dispatch above lands while wry is still on its throwaway pre-load
    // document, so the CSS variables get set on a document that is gone by the
    // time our SPA's document is in place. The static status-bar inset never
    // changes afterward, so the listener does not re-fire on its own, leaving the
    // page with no top inset (the CoreBar then sits under the status bar).
    // Re-publish onto the live document once the page has loaded. Keyboard and
    // rotation still re-fire the listener, so this only needs to cover first load.
    val handler = Handler(Looper.getMainLooper())
    val deadline = SystemClock.uptimeMillis() + 10_000
    handler.post(object : Runnable {
      override fun run() {
        val insets = ViewCompat.getRootWindowInsets(webView)
        val loaded = webView.progress >= 100 && webView.url?.startsWith("http") == true
        if (insets != null && loaded) {
          publishInsets(webView, insets)
          return
        }
        if (SystemClock.uptimeMillis() < deadline) handler.postDelayed(this, 150)
      }
    })
  }

  // Push the current window insets to the page as CSS variables the mobile shell
  // consumes for all inset/keyboard layout (see the class comment).
  private fun publishInsets(webView: WebView, insets: WindowInsetsCompat) {
    val density = webView.resources.displayMetrics.density
    fun cssPx(px: Int): Int = (px / density).toInt()
    val bars = insets.getInsets(
      WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout(),
    )
    val keyboard = insets.getInsets(WindowInsetsCompat.Type.ime()).bottom
    val js = """
      (function () {
        var s = document.documentElement.style;
        s.setProperty('--safe-area-inset-top', '${cssPx(bars.top)}px');
        s.setProperty('--safe-area-inset-right', '${cssPx(bars.right)}px');
        s.setProperty('--safe-area-inset-bottom', '${cssPx(bars.bottom)}px');
        s.setProperty('--safe-area-inset-left', '${cssPx(bars.left)}px');
        s.setProperty('--keyboard-inset', '${cssPx(keyboard)}px');
      })();
    """.trimIndent()
    webView.evaluateJavascript(js, null)
  }
}
