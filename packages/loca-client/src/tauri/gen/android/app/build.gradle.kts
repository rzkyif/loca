import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

// Release signing. The release pipeline (scripts/release/android.ts) materializes
// keystore.properties (+ the .jks) from secrets before building; when it is
// absent (a plain local `deno task build:android`) the release build falls back
// to debug signing so the APK still installs for testing.
val keystoreProperties = Properties().apply {
    val propFile = rootProject.file("keystore.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}
val hasReleaseSigning = keystoreProperties.getProperty("storeFile") != null

// The channel drives both the applicationId suffix (side-by-side install) and the
// on-device app label. The label is the Android analogue of iOS's
// LOCA_DISPLAY_NAME: stable reads "Loca", dev / latest read "Loca (dev)" /
// "Loca (latest)", matching the desktop naming. It is generated here rather than
// declared in res/values/strings.xml because it must vary per channel and Tauri
// only regenerates strings.xml at `tauri android init`, never on a build.
val locaChannel = System.getenv("LOCA_CHANNEL") ?: "stable"
val locaAppLabel = if (locaChannel == "stable") "Loca" else "Loca ($locaChannel)"

android {
    compileSdk = 36
    namespace = "com.rzkyif.loca"
    defaultConfig {
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "com.rzkyif.loca"
        minSdk = 24
        targetSdk = 36
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
        // Consumed by AndroidManifest.xml as @string/app_name (launcher label) and
        // @string/main_activity_title (the recents-screen task title).
        resValue("string", "app_name", locaAppLabel)
        resValue("string", "main_activity_title", locaAppLabel)
    }
    signingConfigs {
        if (hasReleaseSigning) {
            create("release") {
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("keyPassword")
                storeFile = rootProject.file(keystoreProperties.getProperty("storeFile"))
                storePassword = keystoreProperties.getProperty("storePassword")
            }
        }
    }
    buildTypes {
        getByName("debug") {
            manifestPlaceholders["usesCleartextTraffic"] = "true"
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            // Per-channel install identity, applied to release artifacts only:
            // stable keeps the bare applicationId; dev / latest get a suffix so
            // they install alongside stable on one device (build-android.ts sets
            // LOCA_CHANNEL). The debug variant that `tauri android dev` builds and
            // launches stays bare com.rzkyif.loca so the CLI's `am start` target
            // matches the installed package - a suffix there would install the app
            // under com.rzkyif.loca.<channel> while the launch still targets
            // com.rzkyif.loca, so the freshly installed build never comes up. The java
            // namespace stays com.rzkyif.loca (the MainActivity package path is fixed
            // at `tauri android init`), so only the applicationId varies - which is
            // why the suffix is used rather than overriding the Tauri identifier.
            if (locaChannel != "stable") {
                applicationIdSuffix = ".$locaChannel"
            }
            isMinifyEnabled = true
            // Use the release keystore when the pipeline provided one; otherwise
            // fall back to debug signing so a keyless local build still installs.
            signingConfig = if (hasReleaseSigning) {
                signingConfigs.getByName("release")
            } else {
                signingConfigs.getByName("debug")
            }
            proguardFiles(
                *fileTree(".") { include("**/*.pro") }
                    .plus(getDefaultProguardFile("proguard-android-optimize.txt"))
                    .toList().toTypedArray()
            )
        }
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
    buildFeatures {
        buildConfig = true
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.16.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.core:core-splashscreen:1.2.0")
    implementation("androidx.activity:activity-ktx:1.13.0")
    implementation("com.google.android.material:material:1.14.0")
    implementation("androidx.lifecycle:lifecycle-process:2.11.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.7.0")
}

apply(from = "tauri.build.gradle.kts")