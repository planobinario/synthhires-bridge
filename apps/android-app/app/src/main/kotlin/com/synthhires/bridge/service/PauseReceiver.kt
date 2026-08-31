package com.synthhires.bridge.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

class PauseReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        context.stopService(Intent(context, BridgeService::class.java))
    }
}
