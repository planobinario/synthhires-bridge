package com.synthhires.bridge.service

import android.accessibilityservice.AccessibilityService
import android.graphics.Rect
import android.view.accessibility.AccessibilityNodeInfo
import android.view.accessibility.AccessibilityEvent
import android.os.Bundle

class AutomationAccessibilityService : AccessibilityService() {
    override fun onAccessibilityEvent(event: AccessibilityEvent?) { }
    override fun onInterrupt() { }

    fun perform(action: String, text: String? = null, resourceId: String? = null): Boolean {
        val root = rootInActiveWindow ?: return false
        val node = when {
            !resourceId.isNullOrBlank() -> root.findAccessibilityNodeInfosByViewId(resourceId).firstOrNull()
            !text.isNullOrBlank() -> root.findAccessibilityNodeInfosByText(text).firstOrNull()
            else -> root
        } ?: return false
        return when (action) {
            "click" -> node.performAction(AccessibilityNodeInfo.ACTION_CLICK)
            "focus" -> node.performAction(AccessibilityNodeInfo.ACTION_FOCUS)
            "write" -> node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, Bundle().apply {
                putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, text.orEmpty())
            })
            "back" -> performGlobalAction(GLOBAL_ACTION_BACK)
            "home" -> performGlobalAction(GLOBAL_ACTION_HOME)
            else -> false
        }
    }

    companion object {
        @Volatile private var instance: AutomationAccessibilityService? = null
        fun current(): AutomationAccessibilityService? = instance
    }

    override fun onServiceConnected() {
        super.onServiceConnected()
        instance = this
    }
}
