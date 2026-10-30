// SPDX-License-Identifier: Apache-2.0
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}
android {
    namespace = "app.ophio.device"
    compileSdk = 37
    defaultConfig { minSdk = 29; testInstrumentationRunner = "app.ophio.device.KeyChecks"; consumerProguardFiles("consumer-rules.pro") }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
}
kotlin { compilerOptions { jvmTarget = JvmTarget.JVM_1_8 } }
dependencies {
    implementation("androidx.core:core-ktx:1.16.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.zxing:core:3.5.3")
    implementation(project(":tauri-android"))
    // PreviewChecks builds the Invoke that tauri-android's Jackson types.
    androidTestImplementation("com.fasterxml.jackson.core:jackson-databind:2.15.3")
}
