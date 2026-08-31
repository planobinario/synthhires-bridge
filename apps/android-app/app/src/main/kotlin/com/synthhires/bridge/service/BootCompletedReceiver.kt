package com.synthhires.bridge.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import com.synthhires.bridge.core.security.TokenStore

class BootCompletedReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        if (intent?.action != Intent.ACTION_BOOT_COMPLETED) return
        val store = TokenStore(context)
        if (store.loadActiveDeviceId() != null) BridgeService.start(context)
    }
}
