use std::env;

/// 任务管理器 / 资源管理器里显示的名字：winresource 默认拿包名（sysmon），
/// 这里统一改成和 exe 一致的 AA 前缀名。
const APP_NAME: &str = "AAsysmon";

fn main() {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // 图标变了要重新跑构建脚本，否则 cargo 会复用旧资源
    println!("cargo:rerun-if-changed=assets/icon.ico");

    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico");
    // FileDescription 是 Windows 各种列表里显示的「应用程序名」
    res.set("FileDescription", APP_NAME)
        .set("ProductName", APP_NAME)
        .set("InternalName", APP_NAME)
        .set("OriginalFilename", "AAsysmon.exe")
        .set("ProductVersion", env!("CARGO_PKG_VERSION"))
        .set("FileVersion", env!("CARGO_PKG_VERSION"));

    if let Err(e) = res.compile() {
        // 单独提示，避免原因淹没在一长串编译输出里
        eprintln!("build.rs: 嵌入图标/版本信息失败：{e}");
        eprintln!("提示：MSVC 工具链需要 Windows SDK 中的 rc.exe；GNU 工具链需要 windres。");
        std::process::exit(1);
    }
}
