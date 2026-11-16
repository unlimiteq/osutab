plugins {
    id("com.android.application")
}

android {
    namespace = "dev.osutab.app"
    compileSdk = 35

    defaultConfig {
        applicationId = "dev.osutab.app"
        minSdk = 30
        targetSdk = 35
        versionCode = 5
        versionName = "1.0.0"
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
    testOptions {
        unitTests.isReturnDefaultValues = true
    }
}

dependencies {
    testImplementation("junit:junit:4.13.2")
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}
