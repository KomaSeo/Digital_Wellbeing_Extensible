// Responsibility: Build the phone app: its version comes from installer/RELEASE_VERSION, and a
// release build is signed with the key the release workflow gives it (ANDROID_SIGNING_KEY_FILE).
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

// "0.1.0-rc.105" → versionCode 100105; a final "0.1.0" counts as rc 999.
val releaseVersion: String = rootProject.file("../installer/RELEASE_VERSION").readText().trim()
val releaseVersionCode: Int = run {
    val (numbers, candidate) = releaseVersion.split("-rc.").let { parts ->
        parts[0] to (parts.getOrNull(1)?.toInt() ?: 999)
    }
    val (major, minor, patch) = numbers.split(".").map(String::toInt)
    major * 10_000_000 + minor * 100_000 + patch * 1_000 + candidate
}
val signingKeyFile: String? = System.getenv("ANDROID_SIGNING_KEY_FILE")

android {
    namespace = "io.github.komaseo.digitalwellbeing"
    compileSdk = 35

    defaultConfig {
        applicationId = "io.github.komaseo.digitalwellbeing"
        minSdk = 27
        // 34, not 35: Android 15 draws apps that target 35 under the status bar and the keyboard.
        targetSdk = 34
        versionCode = releaseVersionCode
        versionName = releaseVersion
    }

    signingConfigs {
        if (signingKeyFile != null) {
            create("release") {
                storeFile = file(signingKeyFile)
                storeType = "pkcs12"
                storePassword = System.getenv("ANDROID_SIGNING_PASSWORD")
                keyAlias = "digital-wellbeing-phone"
                keyPassword = System.getenv("ANDROID_SIGNING_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            if (signingKeyFile != null) {
                signingConfig = signingConfigs.getByName("release")
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    lint {
        // The release workflow builds without the network checks lint makes.
        checkReleaseBuilds = false
    }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
}
