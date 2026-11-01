plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "dev.osutab.app"
    compileSdk = 35
    buildToolsVersion = "35.0.0"

    defaultConfig {
        applicationId = "dev.osutab.app"
        minSdk = 30
        targetSdk = 35
        versionCode = 2
        versionName = "0.2"
    }

    buildTypes {
        // The debug signature is enough for sideloading over adb, and needs no key handling.
        debug {
            isMinifyEnabled = false
        }
    }

    // The same font files the PC program builds in.
    sourceSets.getByName("main").assets.srcDir("../../assets")

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    testOptions {
        unitTests.isReturnDefaultValues = true
    }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
}
