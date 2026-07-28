package com.synthhires.bridge

import android.app.Activity
import android.os.Bundle
import android.widget.Button
import android.widget.TextView

class MainActivity : Activity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        findViewById<TextView>(R.id.status_text).text =
            "SynthHires Bridge v0.1.0\nTap below to start the daemon."

        findViewById<Button>(R.id.start_button).setOnClickListener {
            startBridgeService()
        }
    }

    private fun startBridgeService() {
        // In production, device_id and token come from pairing with the web UI.
        // For now, the user enters them via a settings screen.
        val prefs = getSharedPreferences("bridge", MODE_PRIVATE)
        val token = prefs.getString("device_token", null)
        val deviceId = prefs.getString("device_id", null)

        if (token == null || deviceId == null) {
            findViewById<TextView>(R.id.status_text).text =
                "No device paired.\nPair from app.synthhires.com/space/connections first."
            return
        }

        val intent = android.content.Intent(this, BridgeService::class.java).apply {
            putExtra("device_token", token)
            putExtra("device_id", deviceId)
        }

        if (android.os.Build.VERSION.SDK_INT >= android.os.Build.VERSION_CODES.O) {
            startForegroundService(intent)
        } else {
            startService(intent)
        }

        findViewById<TextView>(R.id.status_text).text = "Bridge running..."
    }
}
