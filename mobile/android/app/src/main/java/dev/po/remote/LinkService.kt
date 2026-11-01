package dev.po.remote

import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat

/**
 * Keeps the process (and so the link to the PC) alive while a session is followed with the app
 * in the background: the end of a turn or an approval then arrives as a local notification,
 * with no push service in between.
 */
class LinkService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE
        )
        val notification = NotificationCompat.Builder(this, PoApp.CHANNEL_LINK)
            .setSmallIcon(R.drawable.ic_po)
            .setContentTitle(getString(R.string.link_notification))
            .setContentText(getString(R.string.link_notification_text))
            .setOngoing(true)
            .setSilent(true)
            .setContentIntent(open)
            .build()
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(1, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
        } else {
            startForeground(1, notification)
        }
        return START_NOT_STICKY
    }

    /** The user swiped the app away: nothing to keep the link for. */
    override fun onTaskRemoved(rootIntent: Intent?) {
        stopSelf()
    }
}
