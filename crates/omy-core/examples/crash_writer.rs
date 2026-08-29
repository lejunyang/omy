//! 独立验证原子写入在进程被强杀时的行为。
//!
//! 由外部脚本调用：本程序开始写入一个大文件，写到一半时主动 abort 进程，
//! 模拟断电 / 崩溃。随后由脚本检查目标文件是否存在——
//! 若原子写入实现正确，目标文件**绝不应存在**，只应留下 .tmp 残file。
//!
//! 这是与单元测试完全不同的验证通道：单元测试无法真正杀死进程，
//! 只能测试正常的 commit / abort 路径。

use omy_core::fsatomic::AtomicWriter;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(target) = args.get(1) else {
        eprintln!("用法: crash_writer <目标路径> <模式:kill|commit|abort>");
        std::process::exit(2);
    };
    let mode = args.get(2).map(String::as_str).unwrap_or("kill");

    let path = std::path::Path::new(target);
    let mut w = match AtomicWriter::create(path) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("创建失败: {e}");
            std::process::exit(1);
        }
    };

    // 分多次写入，制造"写到一半"的状态
    let chunk = vec![0xABu8; 64 * 1024];
    for i in 0..8 {
        if let Err(e) = w.write_all(&chunk) {
            eprintln!("写入第 {i} 块失败: {e}");
            std::process::exit(1);
        }
        if i == 3 && mode == "kill" {
            // 写到一半强行终止进程，不走任何析构或清理逻辑。
            // process::abort 不会运行 Drop，最接近真实断电。
            eprintln!("已写入 4/8 块，强制终止进程");
            std::process::abort();
        }
    }

    match mode {
        "commit" => match w.commit() {
            Ok(()) => println!("committed"),
            Err(e) => {
                eprintln!("提交失败: {e}");
                std::process::exit(1);
            }
        },
        "abort" => {
            w.abort();
            println!("aborted");
        }
        _ => {
            eprintln!("未知模式: {mode}");
            std::process::exit(2);
        }
    }
}
