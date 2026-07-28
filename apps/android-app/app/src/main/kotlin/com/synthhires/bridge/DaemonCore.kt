package com.synthhires.bridge

object DaemonCore {
    init {
        System.loadLibrary("synthhires_bridge")
    }

    external fun runBridge(token: String, deviceId: String, backendUrl: String)
}
