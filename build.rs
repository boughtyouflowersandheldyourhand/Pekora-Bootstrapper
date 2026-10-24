#![allow(non_snake_case)]

use std::env::var;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=icon.png");
    println!("cargo:rerun-if-changed=build.rs");

    if var("CARGO_CFG_TARGET_OS").unwrap_or_default() != "windows" {
        return;
    }
    let TargetEnv = var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    if TargetEnv == "gnu" {
        for Arg in ["-l:libssp.a", "-lmsvcrt", "-ladvapi32"] {
            println!("cargo:rustc-link-arg={Arg}");
        }
    }

    let Png = PathBuf::from(var("CARGO_MANIFEST_DIR").unwrap()).join("icon.png");
    if !Png.is_file() {
        println!("cargo:warning=icon.png missing, skipping .exe icon embed");
        return;
    }

    let Out = PathBuf::from(var("OUT_DIR").unwrap());
    let Ico = Out.join("icon.ico");
    if let Err(Error) = BuildIco(&Png, &Ico) {
        println!("cargo:warning=could not build icon.ico: {Error}");
        return;
    }

    let Rc = Out.join("icon.rc");
    if let Err(Error) = std::fs::write(&Rc, format!("1 ICON \"{}\"\n", Ico.to_string_lossy().replace('\\', "/"))) {
        println!("cargo:warning=could not write icon.rc: {Error}");
        return;
    }

    if TargetEnv == "msvc" {
        let Res = Out.join("icon.res");
        let RcExe = var("RC").unwrap_or_else(|_| "rc.exe".to_string());
        match Command::new(&RcExe).arg("/nologo").arg("/fo").arg(&Res).arg(&Rc).status() {
            Ok(Status) if Status.success() => println!("cargo:rustc-link-arg={}", Res.to_string_lossy()),
            Ok(Status) => println!("cargo:warning={RcExe} exited with {Status}"),
            Err(Error) => println!("cargo:warning=failed to run {RcExe}: {Error}"),
        }
        return;
    }

    let Windres = if var("HOST").map_or(false, |Host| Host.contains("windows")) {
        "windres"
    } else {
        "x86_64-w64-mingw32-windres"
    };
    let Object = Out.join("icon_res.o");
    match Command::new(Windres).arg("-I").arg(&Out).arg(&Rc).arg("-O").arg("coff").arg("-o").arg(&Object).status() {
        Ok(Status) if Status.success() => println!("cargo:rustc-link-arg={}", Object.to_string_lossy()),
        Ok(Status) => println!("cargo:warning=windres exited with {Status}"),
        Err(Error) => println!("cargo:warning=failed to run {Windres}: {Error}"),
    }
}

fn BuildIco(Png: &Path, Ico: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let Image = image::open(Png)?.to_rgba8();
    let mut Dir = ico::IconDir::new(ico::ResourceType::Icon);
    for Size in [16u32, 24, 32, 48, 64, 128, 256] {
        let Resized = image::imageops::resize(&Image, Size, Size, image::imageops::FilterType::Lanczos3);
        Dir.add_entry(ico::IconDirEntry::encode(&ico::IconImage::from_rgba_data(Size, Size, Resized.into_raw()))?);
    }
    Dir.write(std::io::BufWriter::new(std::fs::File::create(Ico)?))?;
    Ok(())
}
