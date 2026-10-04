package com.chatx.monitor

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build

object NotificationCenter {
    const val FOREGROUND_ID = 100
    private const val MONITOR_CHANNEL = "chatx_monitor"
    private const val ALERT_CHANNEL = "chatx_alerts"

    fun createChannels(context: Context) {
        if (Build.VERSION.SDK_INT < 26) return
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(
                MONITOR_CHANNEL,
                context.getString(R.string.monitor_channel),
                NotificationManager.IMPORTANCE_LOW,
            ),
        )
        manager.createNotificationChannel(
            NotificationChannel(
                ALERT_CHANNEL,
                context.getString(R.string.alert_channel),
                NotificationManager.IMPORTANCE_HIGH,
            ),
        )
    }
    fun foreground(context: Context, text: String): Notification {
        val builder = if (Build.VERSION.SDK_INT >= 26) {
            Notification.Builder(context, MONITOR_CHANNEL)
        } else {
            Notification.Builder(context)
        }
        return builder
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentTitle("ChatX Monitor")
            .setContentText(text)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setContentIntent(contentIntent(context))
            .build()
    }

    fun updateForeground(context: Context, text: String) {
        val manager = context.getSystemService(NotificationManager::class.java)
        manager.notify(FOREGROUND_ID, foreground(context, text))
    }

    fun postAlert(context: Context, event: AlertEvent) {
        val builder = if (Build.VERSION.SDK_INT >= 26) {
            Notification.Builder(context, ALERT_CHANNEL)
        } else {
            Notification.Builder(context)
        }
        val notification = builder
            .setSmallIcon(
                if (event.type == AlertType.RECOVERED) {
                    android.R.drawable.stat_sys_download_done
                } else {
                    android.R.drawable.stat_notify_error
                },
            )
            .setContentTitle(event.title)
            .setContentText(event.message)
            .setStyle(Notification.BigTextStyle().bigText(event.message))
            .setAutoCancel(true)
            .setContentIntent(contentIntent(context))
            .build()
        context.getSystemService(NotificationManager::class.java)
            .notify(event.id, notification)
    }

    private fun contentIntent(context: Context): PendingIntent {
        val intent = Intent(context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP)
        return PendingIntent.getActivity(
            context,
            0,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
    }
}
