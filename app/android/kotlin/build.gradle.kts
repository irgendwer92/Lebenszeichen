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

dependencies {
    implementation("androidx.core:core-ktx:1.15.0")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("androidx.work:work-runtime:2.10.0")
    implementation("org.unifiedpush.android:connector:3.3.5")
    // Needed to compile against the plugin API of Tauri (Invoke uses Jackson types).
    implementation("com.fasterxml.jackson.core:jackson-databind:2.15.3")
    implementation(project(":tauri-android"))
}
