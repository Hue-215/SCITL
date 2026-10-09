import groovy.json.JsonSlurper
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

// minSdkはtauri.conf.jsonの`bundle.android.minSdkVersion`だけに書く。tauri-cliはRust側のビルドで
// NDKのclangを選ぶのにその値を使うので、APKの下限と食い違わないよう、ここでも同じ値を読む
// (docs/spec/architecture/tech-stack.md「AndroidのSDKの版」)。
val tauriMinSdk = JsonSlurper().parse(file("../../../tauri.conf.json")).let { config ->
    val bundle = (config as? Map<*, *>)?.get("bundle") as? Map<*, *>
    val android = bundle?.get("android") as? Map<*, *>
    android?.get("minSdkVersion") as? Int
        ?: error("tauri.conf.jsonにbundle.android.minSdkVersionが無い(無いとtauri-cliは黙って24を使う)")
}

// HTTPSの証明書の検証でRustから呼ぶKotlinの部品。クレート`rustls-platform-verifier-android`が
// Mavenの形で同梱しているので、cargoの解決結果からその場所を引き、中にある唯一の版を使う
// (docs/spec/architecture/network-secrets.md「Androidの信頼ルート」)。
val rustlsPlatformVerifierMaven = providers.exec {
    commandLine(
        System.getenv("CARGO") ?: "cargo",
        "metadata", "--format-version", "1", "--locked",
        "--filter-platform", "aarch64-linux-android",
        "--manifest-path", file("../../../Cargo.toml").path,
    )
}.standardOutput.asText.get().let { metadata ->
    val packages = ((JsonSlurper().parseText(metadata) as Map<*, *>)["packages"] as List<*>)
        .map { it as Map<*, *> }
    // 初期化するcoreと検証するreqwestで版が分かれると、初期化が検証する側に届かず、HTTPSが
    // 使えないままになるので、ここで止める。
    packages.filter { it["name"] == "rustls-platform-verifier" }.let { verifiers ->
        check(verifiers.size == 1) {
            "rustls-platform-verifierが1つの版にまとまっていない: ${verifiers.map { it["version"] }}"
        }
    }
    val crate = packages.single { it["name"] == "rustls-platform-verifier-android" }
    File(crate["manifest_path"] as String).parentFile.resolve("maven")
}
// 同梱のMavenは`maven-metadata.xml`を持たず、`latest.release`では版を引けないので、版のフォルダを見る。
val rustlsPlatformVerifierVersion = rustlsPlatformVerifierMaven
    .resolve("rustls/rustls-platform-verifier")
    .listFiles { file -> file.isDirectory }!!
    .single()
    .name

repositories {
    // このグループは同梱のMavenからだけ取る。GoogleやMaven Centralに同じ名前のものが出ても使わない。
    exclusiveContent {
        forRepository {
            maven { url = uri(rustlsPlatformVerifierMaven) }
        }
        filter { includeGroup("rustls") }
    }
}

android {
    compileSdk = 36
    namespace = "net.niigo.scitl"
    defaultConfig {
        // WebViewとJava側の一部のHTTPライブラリが従う決まりで、Rustの通信には掛からない。Rust側の
        // 平文httpの可否は`net::classify_host`が決める。LANへつなぐために`true`にしない
        // (`docs/spec/architecture/network-secrets.md`「Androidでの平文http」)。
        manifestPlaceholders["usesCleartextTraffic"] = "false"
        applicationId = "net.niigo.scitl"
        minSdk = tauriMinSdk
        targetSdk = 36
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")
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
            isMinifyEnabled = true
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
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    implementation("rustls:rustls-platform-verifier:$rustlsPlatformVerifierVersion")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = "tauri.build.gradle.kts")