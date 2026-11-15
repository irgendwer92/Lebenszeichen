#!/usr/bin/env bash
# Prüft eine gebaute APK: JNI-Funktionen in der Rust-Bibliothek, Einträge im
# Manifest, Klassen im Bytecode, Signatur. Für die Abläufe Android-Prototyp
# (Debug) und Android-Release; braucht ANDROID_HOME und NDK_HOME.
#
# Aufruf: scripts/check-apk.sh <apk>

set -euo pipefail
apk="$1"
build_tools="$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -n 1)"
unpacked="$(mktemp -d "${RUNNER_TEMP:-/tmp}/apk.XXXXXX")"
work="$(mktemp -d "${RUNNER_TEMP:-/tmp}/check.XXXXXX")"
unzip -q -o "$apk" -d "$unpacked"

echo "::group::Native Bibliothek"
library="$unpacked/lib/arm64-v8a/liblebenszeichen_lib.so"
ls -l "$unpacked"/lib/*/
"$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-nm" -D --defined-only "$library" > "$work/symbols.txt"
for symbol in nativeFetch nativeOpenRequests nativeRespond nativeSetPushUrl nativeDecodeQr nativeSetForeground; do
  grep -F "Java_io_github_irgendwer92_lebenszeichen_droid_CoreBridge_$symbol" "$work/symbols.txt" \
    || { echo "fehlt in der Bibliothek: $symbol"; exit 1; }
done
echo "::endgroup::"

echo "::group::Manifest"
"$build_tools/aapt2" dump xmltree --file AndroidManifest.xml "$apk" > "$work/manifest.txt"
for needle in \
  droid.PushServiceImpl droid.ReactionReceiver droid.RequestActivity droid.ScanActivity android.permission.CAMERA \
  android.permission.USE_FULL_SCREEN_INTENT android.permission.POST_NOTIFICATIONS \
  org.unifiedpush.android.connector.MESSAGE org.unifiedpush.android.connector.PUSH_EVENT \
  androidx.work.impl.background.systemjob.SystemJobService \
  android:networkSecurityConfig android:dataExtractionRules; do
  grep -m 1 -F "$needle" "$work/manifest.txt" || { echo "fehlt im Manifest: $needle"; exit 1; }
done
# allowBackup must be false (0x0); the private keys must not leave the device.
grep -E "android:allowBackup.*=(false|0x0)" "$work/manifest.txt" || { echo "allowBackup ist nicht false"; exit 1; }
"$build_tools/aapt2" dump badging "$apk" | grep -E "^(package|sdkVersion|targetSdkVersion|native-code)" || true
echo "::endgroup::"

echo "::group::Zertifikatsprüfung (rustls-platform-verifier)"
grep -a -l -F "Lorg/rustls/platformverifier/CertificateVerifier;" "$unpacked"/classes*.dex \
  || { echo "fehlt im Bytecode: CertificateVerifier"; exit 1; }
echo "::endgroup::"

echo "::group::Klassen der Kotlin-Schicht"
for class in CoreBridge PushServiceImpl ReactionReceiver RequestActivity AnswerWorker FetchWorker ExpiryWorker PushUrlWorker ScanActivity LebenszeichenPlugin; do
  grep -a -l -F "Lio/github/irgendwer92/lebenszeichen/droid/$class;" "$unpacked"/classes*.dex \
    || { echo "fehlt im Bytecode: $class"; exit 1; }
done
echo "::endgroup::"

echo "::group::Signatur und Versionen"
"$build_tools/apksigner" verify --print-certs "$apk" | grep -E "SHA-256|DN" || true
find ~/.gradle/caches \( -name "connector-*.aar" -o -name "work-runtime-*.aar" \) | sed 's|.*/||' | sort -u
ls -l "$apk"
echo "::endgroup::"
