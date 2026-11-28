package io.github.komaseo.digitalwellbeing

import android.annotation.SuppressLint
import android.app.Activity
import android.content.Context
import android.os.Build
import android.os.Bundle
import android.webkit.JavascriptInterface
import android.webkit.WebChromeClient
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import android.window.OnBackInvokedDispatcher
import org.json.JSONObject
import java.util.concurrent.Executors

/**
 * Responsibility: Show the to-do page (assets/phone.html) and carry its requests to the sync
 * server (the owner: "브라우저 말고 휴대폰 앱으로 만들자"). The page keeps its token and the
 * server address in this app's preferences through [Bridge]; nothing else is loaded.
 */
class MainActivity : Activity() {
    private lateinit var webView: WebView
    private val requests = Executors.newCachedThreadPool()

    @SuppressLint("SetJavaScriptEnabled")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        webView = WebView(this)
        webView.settings.javaScriptEnabled = true
        webView.settings.allowFileAccess = false
        webView.settings.allowContentAccess = false
        // The page asks with confirm() before a phone disconnects.
        webView.webChromeClient = WebChromeClient()
        webView.webViewClient = object : WebViewClient() {
            // The page never leaves itself.
            override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest) = true
        }
        webView.addJavascriptInterface(Bridge(), "DigitalWellbeing")
        setContentView(webView)
        webView.loadUrl("file:///android_asset/phone.html")
        if (Build.VERSION.SDK_INT >= 33) {
            onBackInvokedDispatcher.registerOnBackInvokedCallback(
                OnBackInvokedDispatcher.PRIORITY_DEFAULT,
            ) { goBack() }
        }
    }

    @Deprecated("Android 13 and newer use the back callback registered in onCreate")
    override fun onBackPressed() {
        goBack()
    }

    /** Back closes a form or an opened to-do first, then the app. */
    private fun goBack() {
        webView.evaluateJavascript("window.phoneBack ? window.phoneBack() : false") { handled ->
            if (handled != "true") finish()
        }
    }

    override fun onResume() {
        super.onResume()
        webView.onResume()
        webView.resumeTimers()
        webView.evaluateJavascript("window.phoneResume && window.phoneResume()", null)
    }

    override fun onPause() {
        // No polling while the app is not on the screen.
        webView.onPause()
        webView.pauseTimers()
        super.onPause()
    }

    override fun onDestroy() {
        requests.shutdownNow()
        webView.destroy()
        super.onDestroy()
    }

    /** What the page may call, as `DigitalWellbeing.…` (only the bundled page is ever loaded). */
    inner class Bridge {
        private val preferences = getSharedPreferences("phone", Context.MODE_PRIVATE)

        /** A stored value ("" when there is none): "token" or "server". */
        @JavascriptInterface
        fun get(key: String): String = preferences.getString(key, "") ?: ""

        /** Store a value; "" removes it. */
        @JavascriptInterface
        fun set(key: String, value: String) {
            preferences.edit().apply { if (value.isEmpty()) remove(key) else putString(key, value) }.apply()
        }

        /** Store the server address the user typed; answers it as stored, or "" when it is not one. */
        @JavascriptInterface
        fun setServer(typed: String): String {
            val address = PhoneServer.normalize(typed) ?: return ""
            set("server", address)
            return address
        }

        @JavascriptInterface
        fun version(): String = packageManager.getPackageInfo(packageName, 0).versionName ?: ""

        /**
         * Send `body` to the server on another thread; the answer comes back as
         * `window.phoneReply(callId, status, text)` (status 0: the server was not reached).
         */
        @JavascriptInterface
        fun post(callId: String, body: String) {
            val server = get("server")
            requests.execute {
                val reply = if (server.isEmpty()) {
                    PhoneServer.Reply(0, "no server address")
                } else {
                    PhoneServer.post(server, body)
                }
                val script = "window.phoneReply(${JSONObject.quote(callId)}, ${reply.status}, " +
                    "${JSONObject.quote(reply.body)})"
                runOnUiThread { if (!isDestroyed) webView.evaluateJavascript(script, null) }
            }
        }
    }
}
