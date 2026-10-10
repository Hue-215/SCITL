# Add project specific ProGuard rules here.
# You can control the set of applied configuration files using the
# proguardFiles setting in build.gradle.
#
# For more details, see
#   http://developer.android.com/guide/developing/tools/proguard.html

# If your project uses WebView with JS, uncomment the following
# and specify the fully qualified class name to the JavaScript interface
# class:
#-keepclassmembers class fqcn.of.javascript.interface.for.webview {
#   public *;
#}

# Uncomment this to preserve the line number information for
# debugging stack traces.
#-keepattributes SourceFile,LineNumberTable

# If you keep the line number information, uncomment this to
# hide the original source file name.
#-renamesourcefileattribute SourceFile

# HTTPSの証明書の検証でRustからJNIで呼ぶ(`rustls-platform-verifier`)。JNIからの呼び出しは
# 見えないので、使われていないとみなして消されないようにする。
-keep, includedescriptorclasses class org.rustls.platformverifier.** { *; }

# 応答が終わったことの通知と、通知を押して開く会話の引き取りで、RustからJNIで呼ぶ
# (`scitl_core::reply_notification`)。クラスも名前で引くので、名前を変えさせない。
-keep class net.niigo.scitl.ReplyNotifier {
    public static void post(android.content.Context, long, java.lang.String, java.lang.String, java.lang.String);
}
-keep class net.niigo.scitl.MainActivity {
    public static long takeRequestedChat();
}
