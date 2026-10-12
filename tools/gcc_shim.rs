// gcc 占位 shim：只为满足 windres 的预处理调用（gcc -E -xc-header ... input.rc）。
// 版本信息 rc 无需真正预处理，原样输出即可。
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "-o" {
            output = args.get(i + 1).cloned();
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            // 形如 -DXXX 的 flag 直接跳过；带空格参数的常见 flag 多跳一项
            if a == "-D" || a == "-I" || a == "-U" || a == "-include" {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        if std::path::Path::new(a).exists() {
            input = Some(a.clone());
        }
        i += 1;
    }
    let content = match &input {
        Some(p) => std::fs::read(p).unwrap_or_default(),
        None => {
            // 无输入文件：从 stdin 读
            let mut buf = Vec::new();
            let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut buf);
            buf
        }
    };
    match &output {
        Some(p) => {
            let _ = std::fs::write(p, &content);
        }
        None => {
            let _ = std::io::stdout().write_all(&content);
        }
    }
}
