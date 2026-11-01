package dev.po.remote

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.wifi.WifiManager
import android.os.Build
import uniffi.po_mobile.PoCore
import java.util.concurrent.Executors

/**
 * The app's single core (keys, account, links) and its state, alive as long as the process.
 * A network change is passed on to the core, which moves the links to the new network.
 */
class PoApp : Application() {
    lateinit var core: PoCore
        private set
    lateinit var state: AppState
        private set
    private var multicast: WifiManager.MulticastLock? = null
    val io = Executors.newCachedThreadPool()

    override fun onCreate() {
        super.onCreate()
        instance = this
        val prefs = getSharedPreferences("po", Context.MODE_PRIVATE)
        // A debug build may point at a test GitHub (the fake one, on the PC).
        val api = prefs.getString("github_api", null)?.takeIf { BuildConfig.DEBUG && it.isNotBlank() } ?: BuildConfig.GITHUB_API
        val web = prefs.getString("github_web", null)?.takeIf { BuildConfig.DEBUG && it.isNotBlank() } ?: BuildConfig.GITHUB_WEB
        val hosts = BuildConfig.EXTRA_HOSTS.split(',').map { it.trim() }.filter { it.isNotEmpty() }
        // Secrets go through the Android Keystore; an older version's plain files move there.
        core = PoCore(filesDir.resolve("po").absolutePath, deviceName(), api, web, hosts, KeystoreSecrets(filesDir.resolve("secrets")))
        state = AppState(this)
        core.setListener(state)
        channels()
        // mDNS: the PC's announcement on the local network is multicast, which Wi-Fi drops
        // unless an app holds this lock.
        multicast = (applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager)
            ?.createMulticastLock("po-link")?.apply { setReferenceCounted(false); acquire() }
        val connectivity = getSystemService(ConnectivityManager::class.java)
        connectivity?.registerDefaultNetworkCallback(object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                io.execute { core.networkChanged() }
            }
        })
        state.start()
    }

    private fun deviceName(): String {
        val model = Build.MODEL ?: "Android"
        val maker = Build.MANUFACTURER ?: ""
        return if (model.startsWith(maker, ignoreCase = true)) model else "$maker $model".trim()
    }

    private fun channels() {
        val manager = getSystemService(NotificationManager::class.java) ?: return
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_ALERTS, getString(R.string.channel_alerts), NotificationManager.IMPORTANCE_HIGH)
        )
        manager.createNotificationChannel(
            NotificationChannel(CHANNEL_LINK, getString(R.string.channel_link), NotificationManager.IMPORTANCE_MIN)
        )
    }

    companion object {
        const val CHANNEL_ALERTS = "po_alerts"
        const val CHANNEL_LINK = "po_link"
        lateinit var instance: PoApp
            private set
    }
}
