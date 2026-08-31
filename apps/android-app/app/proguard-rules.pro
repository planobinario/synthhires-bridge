# Keep kotlinx.serialization serializers discovered by generated metadata.
-keepclassmembers class **$$serializer { *; }
-keepclassmembers class kotlinx.serialization.** { *; }
-keep class com.synthhires.bridge.core.protocol.** { *; }
-keep class com.synthhires.bridge.service.** { *; }
