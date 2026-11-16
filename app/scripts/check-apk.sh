#!/usr/bin/env bash
# Checks a built APK: JNI functions in the Rust library, manifest entries,
# classes in the bytecode, signature. For the workflows Android debug and
# Android release; needs ANDROID_HOME and NDK_HOME.
#
# Usage: scripts/check-apk.sh <apk>

set -euo pipefail
apk="$1"
build_tools="$(ls -d "$ANDROID_HOME"/build-tools/* | sort -V | tail -n 1)"
unpacked="$(mktemp -d "${RUNNER_TEMP:-/tmp}/apk.XXXXXX")"
work="$(mktemp -d "${RUNNER_TEMP:-/tmp}/check.XXXXXX")"
unzip -q -o "$apk" -d "$unpacked"

echo "::group::Native library"
library="$unpacked/lib/arm64-v8a/liblebenszeichen_lib.so"
ls -l "$unpacked"/lib/*/
"$NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/llvm-nm" -D --defined-only "$library" > "$work/symbols.txt"
for symbol in nativeFetch nativeOpenRequests nativeRespond nativeSetPushUrl nativeDecodeQr nativeSetForeground nativeLanguage; do
  grep -F "Java_io_github_irgendwer92_lebenszeichen_droid_CoreBridge_$symbol" "$work/symbols.txt" \
    || { echo "missing in the library: $symbol"; exit 1; }
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
  grep -m 1 -F "$needle" "$work/manifest.txt" || { echo "missing in the manifest: $needle"; exit 1; }
done
# allowBackup must be false (0x0); the private keys must not leave the device.
grep -E "android:allowBackup.*=(false|0x0)" "$work/manifest.txt" || { echo "allowBackup is not false"; exit 1; }
"$build_tools/aapt2" dump badging "$apk" | grep -E "^(package|sdkVersion|targetSdkVersion|native-code)" || true
echo "::endgroup::"

echo "::group::Certificate check (rustls-platform-verifier)"
grep -a -l -F "Lorg/rustls/platformverifier/CertificateVerifier;" "$unpacked"/classes*.dex \
  || { echo "missing in the bytecode: CertificateVerifier"; exit 1; }
echo "::endgroup::"

echo "::group::Classes of the Kotlin layer"
for class in CoreBridge PushServiceImpl ReactionReceiver RequestActivity AnswerWorker FetchWorker ExpiryWorker PushUrlWorker ScanActivity LebenszeichenPlugin; do
  grep -a -l -F "Lio/github/irgendwer92/lebenszeichen/droid/$class;" "$unpacked"/classes*.dex \
    || { echo "missing in the bytecode: $class"; exit 1; }
done
echo "::endgroup::"

echo "::group::Signature and versions"
"$build_tools/apksigner" verify --print-certs "$apk" | grep -E "SHA-256|DN" || true
find ~/.gradle/caches \( -name "connector-*.aar" -o -name "work-runtime-*.aar" \) | sed 's|.*/||' | sort -u
ls -l "$apk"
echo "::endgroup::"
