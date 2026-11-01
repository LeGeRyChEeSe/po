package dev.po.remote

import android.Manifest
import android.content.Intent
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import dev.po.remote.ui.App
import dev.po.remote.ui.PoTheme

class MainActivity : ComponentActivity() {
    private val state get() = PoApp.instance.state

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        if (Build.VERSION.SDK_INT >= 33) {
            registerForActivityResult(ActivityResultContracts.RequestPermission()) {}.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
        setContent {
            PoTheme(state.theme, state.appearance) {
                BackHandler(enabled = state.stack.size > 1) { state.back() }
                App(state)
            }
        }
        openFrom(intent)
    }

    /** A notification touched: its session opens. */
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        openFrom(intent)
    }

    private fun openFrom(intent: Intent?) {
        val pc = intent?.getStringExtra("pc") ?: return
        val key = intent.getStringExtra("key") ?: return
        if (state.open != pc to key) state.openConversation(pc, key)
    }

    override fun onResume() {
        super.onResume()
        state.inForeground = true
    }

    override fun onPause() {
        super.onPause()
        state.inForeground = false
    }
}
