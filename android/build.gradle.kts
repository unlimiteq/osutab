plugins {
    id("com.android.application") version "9.5.1" apply false
    // Android Gradle plugin 9 builds Kotlin itself. Declaring this picks the Kotlin version it uses.
    id("org.jetbrains.kotlin.android") version "2.4.21" apply false
}
