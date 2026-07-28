// Top-level build file for SynthHires Bridge Android App.
// The native Rust libraries (.so) are compiled by cargo-ndk in CI and
// placed under app/src/main/jniLibs/ before Gradle runs.
plugins {
    id("com.android.application") version "8.7.0" apply false
    id("org.jetbrains.kotlin.android") version "2.0.0" apply false
}
