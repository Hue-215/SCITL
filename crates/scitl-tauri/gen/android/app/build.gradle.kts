import groovy.json.JsonOutput
import groovy.json.JsonSlurper
import java.util.zip.ZipFile
import javax.inject.Inject
import javax.xml.parsers.DocumentBuilderFactory
import org.gradle.api.artifacts.component.ModuleComponentIdentifier
import org.gradle.api.artifacts.result.ResolvedArtifactResult
import org.gradle.api.attributes.Attribute
import org.gradle.maven.MavenModule
import org.gradle.maven.MavenPomArtifact
import org.gradle.process.ExecOperations
import org.w3c.dom.Element

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
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

// cargoの解決結果(Android向け)。下の版・依存の確認・Kotlinの部品の場所で使う。
val cargoMetadata = providers.exec {
    commandLine(
        System.getenv("CARGO") ?: "cargo",
        "metadata", "--format-version", "1", "--locked",
        "--filter-platform", "aarch64-linux-android",
        "--manifest-path", file("../../../Cargo.toml").path,
    )
}.standardOutput.asText.get().let { JsonSlurper().parseText(it) as Map<*, *> }
val cargoPackages = (cargoMetadata["packages"] as List<*>).map { it as Map<*, *> }

// 版はCargo.tomlの`[workspace.package]`の1箇所だけに書く。tauri-cliは`tauri.conf.json`に版が
// 無いと`tauri.properties`を書かないので、ここで引く。versionCodeは、新しい版ほど大きくないと
// 上書きのインストールを断られるので、版から決める(tauri-cliと同じ式)。
val scitlVersionName = cargoPackages.single { it["name"] == "scitl-tauri" }["version"] as String
val scitlVersionCode = Regex("""^(\d+)\.(\d+)\.(\d+)""").find(scitlVersionName)
    ?.destructured?.let { (major, minor, patch) ->
        check(minor.toInt() < 1000 && patch.toInt() < 1000) {
            "版のminor・patchが1000以上で、versionCodeの大小が版の順と合わなくなる: $scitlVersionName"
        }
        major.toInt() * 1_000_000 + minor.toInt() * 1_000 + patch.toInt()
    }
    ?: error("版がx.y.zの形で始まっておらず、versionCodeを決められない: $scitlVersionName")

// HTTPSの証明書の検証でRustから呼ぶKotlinの部品。クレート`rustls-platform-verifier-android`が
// Mavenの形で同梱しているので、cargoの解決結果からその場所を引き、中にある唯一の版を使う
// (docs/spec/architecture/network-secrets.md「Androidの信頼ルート」)。
val rustlsPlatformVerifierMaven = cargoPackages.let { packages ->
    // 初期化するcoreと検証するreqwestで版が分かれると、初期化が検証する側に届かず、HTTPSが
    // 使えないままになるので、ここで止める。
    packages.filter { it["name"] == "rustls-platform-verifier" }.let { verifiers ->
        check(verifiers.size == 1) {
            "rustls-platform-verifierが1つの版にまとまっていない: ${verifiers.map { it["version"] }}"
        }
    }
    // 秘密情報の保存先が読む`ndk-context`は、coreが1回だけ初期化する(network-secrets.md
    // 「Androidの保存先」)。版が分かれると初期化が保存先に届かず、保存先が初期化前の読み出しで
    // panicする。ほかのクレート(taoの新しい版等)が使い始めると、二重の初期化でpanicしうる。
    // どちらも動かすまで気付けないので、ここで止める。
    packages.filter { it["name"] == "ndk-context" }.let { contexts ->
        check(contexts.size == 1) {
            "ndk-contextが1つの版にまとまっていない: ${contexts.map { it["version"] }}"
        }
        val id = contexts.single()["id"]
        val users = ((cargoMetadata["resolve"] as Map<*, *>)["nodes"] as List<*>)
            .map { it as Map<*, *> }
            .filter { node -> (node["deps"] as List<*>).any { (it as Map<*, *>)["pkg"] == id } }
            .map { node -> packages.single { it["id"] == node["id"] }["name"] }
            .toSet()
        check(users == setOf("scitl-core", "android-native-keyring-store")) {
            "ndk-contextを使うクレートが想定と違う(初期化の重複を確かめる): $users"
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
        versionCode = scitlVersionCode
        versionName = scitlVersionName
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
            // ここでは署名しない(署名の無いAPKは端末に入れられない)。配布物は
            // `scripts/release-build-android.sh`が署名する。鍵のパスワードをGradleに渡すと、ビルドの
            // 後も残るデーモンの環境変数に載るため(.claude/skills/release-build「Androidの配布物」)。
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

// 第三者ライセンスの一覧のために、リリースのAPKに入るMavenの依存と、それぞれのPOMが書いている
// ライセンス、jarの中に持っている表示のファイル(META-INFのNOTICE・LICENSE)を書き出す。表示の
// ファイルは、Androidのビルドが既定でAPKから除くものがあるので、一覧の側に載せる。プロジェクトとして
// 入るTauri本体とプラグインのKotlin側は、Rustのクレートの中身なので、クレートの一覧で足りる。
val scitlReleaseDependencies = tasks.register("scitlReleaseDependencies") {
    val output = layout.buildDirectory.file("scitl/release-dependencies.json")
    outputs.file(output)
    outputs.upToDateWhen { false }
    doLast {
        val ids = configurations["universalReleaseRuntimeClasspath"].incoming.resolutionResult
            .allComponents.map { it.id }.filterIsInstance<ModuleComponentIdentifier>()
        val poms = dependencies.createArtifactResolutionQuery()
            .forComponents(ids)
            .withArtifacts(MavenModule::class.java, MavenPomArtifact::class.java)
            .execute().resolvedComponents
            .associate { component ->
                component.id to component.getArtifacts(MavenPomArtifact::class.java)
                    .filterIsInstance<ResolvedArtifactResult>().single().file
            }
        // aarは、中のclasses.jarを取り出した形で受け取る(クラスパスに載るものと同じ)。
        val notices = configurations["universalReleaseRuntimeClasspath"].incoming.artifactView {
            attributes { attribute(Attribute.of("artifactType", String::class.java), "android-classes-jar") }
        }.artifacts.groupBy({ it.id.componentIdentifier }) { artifact ->
            ZipFile(artifact.file).use { jar ->
                jar.entries().asSequence()
                    .filter { Regex("""META-INF/[^/]*(NOTICE|LICEN[SC]E)[^/]*""", RegexOption.IGNORE_CASE).matches(it.name) }
                    .map { mapOf("file" to it.name, "text" to jar.getInputStream(it).readBytes().toString(Charsets.UTF_8)) }
                    .toList()
            }
        }.mapValues { it.value.flatten() }
        // POMは外から取ってきたファイルなので、外部の実体を読ませない。
        val pomParser = DocumentBuilderFactory.newInstance().apply {
            setFeature("http://apache.org/xml/features/disallow-doctype-decl", true)
        }
        val list = ids.map { id ->
            val pom = poms[id] ?: error("POMを引けない: $id")
            val project = pomParser.newDocumentBuilder().parse(pom).documentElement
            fun Element.children(name: String) = (0 until childNodes.length)
                .map { childNodes.item(it) }.filterIsInstance<Element>().filter { it.tagName == name }
            fun Element.text(name: String) = children(name).firstOrNull()?.textContent?.trim()
            mapOf(
                "group" to id.group,
                "name" to id.module,
                "version" to id.version,
                "url" to project.text("url"),
                "licenses" to project.children("licenses").flatMap { it.children("license") }
                    .map { mapOf("name" to it.text("name"), "url" to it.text("url")) },
                "notices" to notices[id].orEmpty(),
            )
        }.sortedBy { "${it["group"]}:${it["name"]}" }
        output.get().asFile.writeText(JsonOutput.prettyPrint(JsonOutput.toJson(list)))
    }
}

// リリースのAPKには、ライセンスと第三者ライセンスの一覧を中(assets/licenses)に入れる。APKは
// 1つのファイルで渡るので、隣に置いても一緒に届かない。一覧は`scripts/assemble-dist.mjs`が
// 組み立てる(画面のビルドが出すnpmの一覧を読むので、`tauri android build`を通して走らせる)。
abstract class ScitlLicensesTask : DefaultTask() {
    @get:InputFile
    abstract val dependencies: RegularFileProperty

    @get:Internal
    abstract val script: RegularFileProperty

    @get:OutputDirectory
    abstract val outputDir: DirectoryProperty

    @get:Inject
    abstract val execOperations: ExecOperations

    @TaskAction
    fun assemble() {
        execOperations.exec {
            commandLine(
                "node", script.get().asFile.path, "--android-licenses",
                dependencies.get().asFile.path, outputDir.get().asFile.resolve("licenses").path,
            )
        }
    }
}
val scitlLicenses = tasks.register<ScitlLicensesTask>("scitlLicenses") {
    dependsOn(scitlReleaseDependencies)
    dependencies.set(layout.buildDirectory.file("scitl/release-dependencies.json"))
    script.set(file("../../../../../scripts/assemble-dist.mjs"))
    // 入力はクレート・npm・licenses/の写しに跨るので、毎回組み立て直す。
    outputs.upToDateWhen { false }
}
androidComponents {
    onVariants(selector().withBuildType("release")) { variant ->
        variant.sources.assets?.addGeneratedSourceDirectory(scitlLicenses, ScitlLicensesTask::outputDir)
    }
}

apply(from = "tauri.build.gradle.kts")