# Tauri looks up the plugin class by name and the native library looks up
# CoreBridge by name (JNI), so nothing in this package may be renamed.
-keep class io.github.irgendwer92.lebenszeichen.droid.** { *; }

# rustls-platform-verifier: the Rust core calls CertificateVerifier through
# JNI by name; its library ships no rules of its own. Without this, R8
# renames or removes it in the release build and every TLS connection fails.
-keep class org.rustls.platformverifier.** { *; }
