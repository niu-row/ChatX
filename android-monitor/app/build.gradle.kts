plugins {
    id("com.android.application")
}

android {
    namespace = "com.chatx.monitor"
    compileSdk = 37

    defaultConfig {
        applicationId = "com.chatx.monitor"
        minSdk = 26
        targetSdk = 37
        versionCode = 8
        versionName = "0.3.5"
    }
    buildFeatures {
        buildConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    testOptions {
        unitTests.isReturnDefaultValues = true
    }
}

dependencies {
    implementation("com.google.android.gms:play-services-code-scanner:16.1.0")
    implementation("com.squareup.okhttp3:okhttp:4.12.0")
    testImplementation("junit:junit:4.13.2")
}
