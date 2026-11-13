# Tauri looks up the plugin class by name and the native library looks up
# CoreBridge by name (JNI), so nothing in this package may be renamed.
-keep class io.github.irgendwer92.lebenszeichen.droid.** { *; }
