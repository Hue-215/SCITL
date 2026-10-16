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

# 応答が終わったことの通知と、通知を押して開く会話の引き取りと、通知の許可で、RustからJNIで呼ぶ
# (`scitl_core::reply_notification`)。クラスも名前で引くので、名前を変えさせない。
-keep class net.niigo.scitl.ReplyNotifier {
    public static void post(android.content.Context, long, java.lang.String, java.lang.String, java.lang.String, java.lang.String, java.lang.String);
    public static boolean permissionNeeded(android.content.Context);
    public static void answerPermissionOffer(android.content.Context, boolean);
}
-keep class net.niigo.scitl.MainActivity {
    public static long takeRequestedChat();
}

# 同期の相手のQRを読む画面を開き、結果を引き取る。カメラの許可を見て、設定の画面を開く
# (`scitl_core::qr_scan`)。
-keep class net.niigo.scitl.QrScanner {
    public static boolean available(android.content.Context);
    public static int cameraPermission(android.content.Context);
    public static void openAppSettings(android.content.Context);
    public static long start(android.content.Context);
    public static java.lang.String poll(long);
    public static void cancel(long);
}
