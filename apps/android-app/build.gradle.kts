// SynthHires Mobile Bridge — Android Gradle root.
//
// Sideload-only: the Play Store rejects apps with
// BIND_NOTIFICATION_LISTENER_SERVICE foreground automation. Users
// install via F-Droid or direct APK download from /space/connections.
// The README in this directory spells out the install flow.

plugins {
    id("com.android.application") version "8.5.2" apply false
    id("org.jetbrains.kotlin.android") version "1.9.24" apply false
    id("org.jetbrains.kotlin.plugin.serialization") version "1.9.24" apply false
}

allprojects {
    repositories {
        google()
        mavenCentral()
    }
}

subprojects {
    afterEvaluate {
        if (project.hasProperty("android")) {
            android {
                compileSdk = 34
                defaultConfig {
                    minSdk = 26 // Android 8.0; covers NotificationListenerService API + foreground services
                    targetSdk = 34
                }
            }
        }
    }
}