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
    // The sherpa-onnx runtime libraries, packaged from the vendored tree rather
    // than copied into `src/main/jniLibs`.
    //
    // `libloca_lib.so` links these two dynamically (the Android arm of
    // sherpa-onnx-sys is the crate's `shared` mode, because upstream's android
    // release ships exactly this pair and no static archive), so without them in
    // the APK the app fails at dlopen with an unresolved dependency: the process
    // dies on launch, before any Loca code runs.
    //
    // Read straight out of `vendor/` because that is where scripts/vendor-sherpa.ts
    // puts them and the digest it pins is the only thing vouching for these
    // bytes. Copying into the source tree would put unpinned binaries a build
    // step away from the signed package. AGP merges srcDirs and matches ABI
    // directory names, so the four-ABI universal build gets the right pair per
    // ABI with no per-ABI wiring here.
    //
    // The archive also carries two libraries this app never calls; the
    // `packaging` block below drops them.
    //
    // The NDK's `libc++_shared.so` is NOT staged here and must not be: the Tauri
    // CLI already symlinks it into `src/main/jniLibs/<abi>/` on every build,
    // alongside `libloca_lib.so`. Adding a second source for it fails the build
    // with "Duplicate resources" at the jniLibs merge, which is how this comment
    // came to exist. Those symlinks only appear after a build has run, so an
    // untouched tree looks like the runtime is missing when it is not.
    //
    // The path is asserted rather than trusted: a `srcDir` that does not exist
    // contributes nothing SILENTLY, so the first version of this line was one
    // directory short (gen/android sits six levels below the repo root, not
    // five) and produced an APK that built cleanly, shipped no sherpa libraries,
    // and would have died at launch on the `DT_NEEDED` for
    // `libsherpa-onnx-c-api.so`. That failure is exactly the kind this project
    // keeps finding, so it fails the build here instead.
    val sherpaJniLibs = rootProject.file("../../../../../../vendor/sherpa-onnx/android/jniLibs")
    if (!sherpaJniLibs.isDirectory) {
        throw GradleException(
            "vendor/sherpa-onnx is not staged (looked in $sherpaJniLibs). The APK would build " +
                "without the speech libraries and fail at launch.\n" +
                "Run: deno run -A scripts/vendor-sherpa.ts",
        )
    }
    sourceSets {
        getByName("main") {
            jniLibs.srcDir(sherpaJniLibs)
        }
    }
    packaging {
        jniLibs {
            // The android archive also carries `libsherpa-onnx-jni.so` (the JNI
            // wrapper, a different API surface from the C API this binds) and
            // `libsherpa-onnx-cxx-api.so`. Roughly 5 MB per ABI of code nothing
            // in this app calls, so they are dropped at packaging rather than
            // filtered at the source set: AGP's `jniLibs` source directory set
            // has no `exclude`, and this is the DSL that does.
            excludes += setOf(
                "**/libsherpa-onnx-jni.so",
                "**/libsherpa-onnx-cxx-api.so",
            )
        }
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
    // Per-ABI release APKs. The universal APK is 56 MiB and 94.9% of it is four
    // copies of libloca_lib.so (measured: 55,782,804 of 58,787,702 bytes), so an
    // arm64-only APK is roughly 17 MiB. That headroom is what makes room for the
    // Rust core, llama.cpp and sherpa: four copies of those would be untenable,
    // while one is smaller than what ships today.
    //
    // AGP's `splits` block is variant-wide: there is no per-buildType splits DSL,
    // so it is gated on the invoked task instead. `tauri android dev` builds and
    // installs a single APK by name, and emitting four from the debug variant
    // would leave the CLI installing whichever it happened to find first. The
    // dev loop is not worth a release-size win.
    //
    // `isUniversalApk = true` keeps publishing the fat APK alongside the splits.
    // It costs one artifact and it is what an older client falls back to when it
    // cannot report its own ABI (see lib/platform/mobile.ts): without it, such a
    // client declines every update instead of taking the safe one.
    val splitAbis = gradle.startParameter.taskNames.any { it.contains("Release") }
    splits {
        abi {
            isEnable = splitAbis
            reset()
            include("arm64-v8a", "armeabi-v7a", "x86_64", "x86")
            isUniversalApk = true
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