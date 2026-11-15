import groovy.json.JsonSlurper
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

// Android library module of the Kotlin layer. Tauri includes it in the
// generated Gradle project (gen/android) because the Rust crate next to it
// is a Tauri plugin; plugin versions come from that project.
plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "io.github.irgendwer92.lebenszeichen.droid"
    compileSdk = 36

    defaultConfig {
        minSdk = 24
        consumerProguardFiles("consumer-rules.pro")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_1_8
    }
}

// The Kotlin part of rustls-platform-verifier (certificate check of the
// system, see crates/client). The crate rustls-platform-verifier-android
// ships it as a Maven folder; cargo metadata finds that folder in the Cargo
// cache, with exactly the version from app/src-tauri/Cargo.lock.
val rustlsVerifier: Pair<File, String> by lazy {
    val metadata = providers.exec {
        commandLine(
            "cargo", "metadata", "--format-version", "1", "--locked",
            "--filter-platform", "aarch64-linux-android",
            "--manifest-path", file("../../src-tauri/Cargo.toml").path,
        )
    }.standardOutput.asText.get()
    @Suppress("UNCHECKED_CAST")
    val packages = (JsonSlurper().parseText(metadata) as Map<String, Any>)["packages"] as List<Map<String, Any>>
    val component = packages.single { it["name"] == "rustls-platform-verifier-android" }
    Pair(File(File(component["manifest_path"] as String).parentFile, "maven"), component["version"] as String)
}

// In every project: the app module resolves the dependencies of this
// library with its own repositories.
rootProject.allprojects {
    repositories {
        maven {
            url = uri(rustlsVerifier.first)
            // The .pom names the packaging (aar); without it Gradle looks for a .jar.
            metadataSources { mavenPom() }
        }
    }
}

// CameraX, for scanning invitation QR codes; all artifacts need the same version.
val cameraVersion = "1.4.2"

dependencies {
    implementation("rustls:rustls-platform-verifier:${rustlsVerifier.second}")
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("androidx.work:work-runtime:2.10.0")
    implementation("androidx.camera:camera-camera2:$cameraVersion")
    implementation("androidx.camera:camera-lifecycle:$cameraVersion")
    implementation("androidx.camera:camera-view:$cameraVersion")
    implementation("org.unifiedpush.android:connector:3.3.5")
    // Needed to compile against the plugin API of Tauri (Invoke uses Jackson types).
    implementation("com.fasterxml.jackson.core:jackson-databind:2.15.3")
    implementation(project(":tauri-android"))
}
