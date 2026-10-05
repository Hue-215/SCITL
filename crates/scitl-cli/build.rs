//! Windows(MSVC)の配布用ビルドで、VCランタイムを静的にリンクする。既定では`VCRUNTIME140.dll`を
//! 求め、Visual C++の再頒布可能パッケージが入っていないPCで起動できない。UCRTはWindowsの部品
//! なので動的のままにする。
//!
//! GUIには`tauri build`が同じことをしており(tauri-buildの`static_vcruntime.rs`)、ここはその処理を
//! 写したもの(Copyright 2019-2024 Tauri Programme within The Commons Conservancy、Apache-2.0 OR MIT。
//! 元は<https://github.com/ChrisDenton/static_vcruntime/>)。GUIと同じリンクにするため、クレートを
//! 使わずに写す。

use std::io::Write;
use std::path::Path;
use std::{env, fs};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // 配布するのはreleaseだけ。開発用のビルドとテストは既定のリンクのままにする。
    let release = env::var("PROFILE").as_deref() == Ok("release");
    let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    if release && msvc {
        link_vcruntime_statically();
    }
}

fn link_vcruntime_statically() {
    override_msvcrt_lib();

    // Rustが決め打ちしていない、ぶつかるライブラリを外す。
    for lib in [
        "libvcruntimed.lib",
        "vcruntime.lib",
        "vcruntimed.lib",
        "libcmtd.lib",
        "msvcrt.lib",
        "msvcrtd.lib",
        "libucrt.lib",
        "libucrtd.lib",
    ] {
        println!("cargo:rustc-link-arg=/NODEFAULTLIB:{lib}");
    }
    // 使うライブラリ。
    for lib in ["libcmt.lib", "libvcruntime.lib", "ucrt.lib"] {
        println!("cargo:rustc-link-arg=/DEFAULTLIB:{lib}");
    }
}

/// Rustが決め打ちでリンクする`msvcrt.lib`を、(ほぼ)空のオブジェクトファイルで置き換える。
/// `/NODEFAULTLIB`は名指しでリンクされるライブラリには効かないため。
fn override_msvcrt_lib() {
    // 空のライブラリに書く機種の種別。
    let machine: &[u8] = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86_64") => &[0x64, 0x86],
        Ok("x86") => &[0x4C, 0x01],
        _ => return,
    };
    let bytes: &[u8] = &[
        1, 0, 94, 3, 96, 98, 60, 0, 0, 0, 1, 0, 0, 0, 0, 0, 132, 1, 46, 100, 114, 101, 99, 116,
        118, 101, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 10, 16, 0, 46, 100, 114, 101, 99, 116, 118, 101, 0, 0, 0, 0, 1, 0, 0, 0, 3, 0, 4, 0,
        0, 0,
    ];

    // 空の`msvcrt.lib`を出力先に書き、そこをライブラリを探す場所に足す。
    let out_dir = env::var("OUT_DIR").expect("cargo sets OUT_DIR for build scripts");
    let path = Path::new(&out_dir).join("msvcrt.lib");
    if let Ok(mut file) = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        file.write_all(machine).expect("write the empty msvcrt.lib");
        file.write_all(bytes).expect("write the empty msvcrt.lib");
    }
    println!("cargo:rustc-link-search=native={out_dir}");
}
