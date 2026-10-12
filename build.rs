// 构建脚本：手写 VERSIONINFO 资源，用 windres 编译后以 link-arg 直接链接。
// （winres 走静态库方式链接，无符号的资源对象会被链接器按需策略跳过，故改为直传 .o）
// 失败不阻塞构建（仅影响 exe 元数据）。
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if let Err(e) = embed_version_info() {
        println!("cargo:warning=版本资源嵌入失败（已跳过）：{}", e);
    }
}

fn embed_version_info() -> Result<(), String> {
    let out_dir = std::env::var("OUT_DIR").map_err(|e| e.to_string())?;
    let rc_path = std::path::Path::new(&out_dir).join("version.rc");
    let obj_path = std::path::Path::new(&out_dir).join("version.o");

    let rc = r#"
#include <windows.h>
1 VERSIONINFO
FILEVERSION 0,1,0,0
PRODUCTVERSION 0,1,0,0
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904b0"
        BEGIN
            VALUE "CompanyName", "SteamSweep Project"
            VALUE "FileDescription", "SteamSweep - Steam leftover scanner (read-only)"
            VALUE "FileVersion", "0.1.0"
            VALUE "InternalName", "steamsweep"
            VALUE "LegalCopyright", "MIT License"
            VALUE "OriginalFilename", "steamsweep.exe"
            VALUE "ProductName", "SteamSweep"
            VALUE "ProductVersion", "0.1.0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1200
    END
END
"#;
    std::fs::write(&rc_path, rc).map_err(|e| e.to_string())?;

    let status = Command::new("windres.exe")
        .arg("-i")
        .arg(&rc_path)
        .arg("-o")
        .arg(&obj_path)
        .arg("-O")
        .arg("coff")
        .status()
        .map_err(|e| format!("调用 windres 失败：{}", e))?;
    if !status.success() {
        return Err(format!("windres 退出码：{:?}", status.code()));
    }

    // 直接把 .o 作为链接参数传给链接器（不经静态库，确保 .rsrc 一定被链入）
    let obj = obj_path.to_string_lossy().replace('\\', "/");
    println!("cargo:rustc-link-arg={}", obj);
    Ok(())
}
