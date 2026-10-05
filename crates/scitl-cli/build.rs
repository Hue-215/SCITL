//! Windows(MSVC)の配布用ビルドで、VCランタイムを静的にリンクする。既定では`VCRUNTIME140.dll`を
//! 求め、Visual C++の再頒布可能パッケージが入っていないPCで起動できない。UCRTはWindowsの部品
//! なので動的のままにする。MSVC以外では何もしない。

fn main() {
    // 配布するのはreleaseだけ。開発用のビルドとテストは既定のリンクのままにする。
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        static_vcruntime::metabuild();
    }
}
