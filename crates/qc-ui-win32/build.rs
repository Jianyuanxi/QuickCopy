fn main() {
    // 嵌入清单：启用 v6 通用控件（系统原生外观）和每显示器 DPI 感知
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let manifest = dir.join("app.manifest");
    println!("cargo:rerun-if-changed=app.manifest");
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{}", manifest.display());
    // exe 图标（资源序号 1），窗口图标也从这里取
    let ico = dir.join("../../assets/app.ico").canonicalize().unwrap();
    let rc = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("app.rc");
    std::fs::write(&rc, format!("1 ICON \"{}\"\n", ico.display().to_string().replace('\\', "/"))).unwrap();
    embed_resource::compile(&rc, embed_resource::NONE).manifest_optional().unwrap();
    println!("cargo:rerun-if-changed=../../assets/app.ico");
}
